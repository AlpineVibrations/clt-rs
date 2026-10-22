//! Deterministic recovery for a misplaced Markdown Done entry. The reviewed
//! manifest stays immutable; only an unpublished task-owned branch tip qualifies.
use super::{
    AgentGitFinalizationLease, AgentGitWorktreeBaseline, configure_agent_git_identity,
    git_commit_is_ancestor, git_commit_matches_agent_staged_manifest, git_commit_task_trailers,
    git_commit_uses_agent_identity, git_nul_separated_paths, git_stdout,
    local_agent_git_task_commit_is_retained, resolve_git_commit, run_agent_git_projection_command,
    run_agent_git_remote_command, verify_agent_git_resume_branch,
};
use crate::{
    agent::{self, AgentSessionControlState, GitFinalizationState},
    session_control::collect_codex_session_tasks_in_board,
    task::{
        TaskStatus, acquire_board_mutation_lock, codex_session_id_from_task_content, get_tasks_dir,
        task_content_is_manual,
    },
};
use anyhow::{Context, Result};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tempfile::TempDir;

const DONE_PATH: &str = "tasks/done.md";

fn selected_line_removed(text: &str, session: &str) -> Option<(String, String)> {
    let mut selected = None;
    let mut rest = String::new();
    for line in text.split_inclusive('\n') {
        if line.starts_with("- ")
            && codex_session_id_from_task_content(line.trim()) == Some(session)
        {
            if selected.replace(line.to_string()).is_some() {
                return None;
            }
        } else {
            rest.push_str(line);
        }
    }
    Some((selected?, rest))
}

fn blob(project: &Path, tree: &str) -> Result<String> {
    let output = Command::new("git")
        .current_dir(project)
        .args(["cat-file", "blob", &format!("{tree}:{DONE_PATH}")])
        .output()?;
    anyhow::ensure!(output.status.success(), "Cannot read the sealed Done board");
    Ok(String::from_utf8(output.stdout)?)
}

fn is_only_selected_order(project: &Path, commit: &str, tree: &str, session: &str) -> Result<bool> {
    let paths = git_nul_separated_paths(
        project,
        &["diff", "--name-only", "-z", commit, tree, "--"],
        "inspect repair scope",
    )?;
    if paths != [DONE_PATH] {
        return Ok(false);
    }
    // A permission/type change is not an ordering repair.
    let old_mode = git_stdout(
        project,
        &["ls-tree", commit, DONE_PATH],
        "inspect original board mode",
    )?;
    let new_mode = git_stdout(
        project,
        &["ls-tree", tree, DONE_PATH],
        "inspect sealed board mode",
    )?;
    if old_mode.split_whitespace().next() != new_mode.split_whitespace().next() {
        return Ok(false);
    }
    Ok(matches!(
        (selected_line_removed(&blob(project, commit)?, session), selected_line_removed(&blob(project, tree)?, session)),
        (Some(old), Some(new)) if old == new
    ))
}

fn require_unpublished(
    project: &Path,
    branch: &str,
    commit: &str,
    baseline: &AgentGitWorktreeBaseline,
) -> Result<()> {
    let refs = git_stdout(
        project,
        &["for-each-ref", "--contains", commit, "--format=%(refname)"],
        "check references before board repair",
    )?;
    anyhow::ensure!(
        refs.lines().all(|reference| reference == branch),
        "Automatic board-order repair cannot rewrite a commit retained by another local or remote reference"
    );
    let remotes = git_stdout(
        project,
        &["remote"],
        "inspect configured remotes before repair",
    )?;
    let mut urls = std::collections::BTreeSet::new();
    for remote in remotes.lines() {
        for args in [
            vec!["remote", "get-url", "--all", remote],
            vec!["remote", "get-url", "--push", "--all", remote],
        ] {
            urls.extend(
                git_stdout(project, &args, "resolve repair publication checks")?
                    .lines()
                    .map(str::to_owned),
            );
        }
    }
    urls.extend(baseline.upstream_push_url.iter().cloned());
    anyhow::ensure!(
        urls.len() <= 8,
        "Automatic board repair needs a bounded publication check; inspect the configured remotes"
    );
    for url in urls {
        let output = run_agent_git_remote_command(
            Command::new("git")
                .current_dir(project)
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(["ls-remote", "--refs", &url]),
            "prove the task commit is unpublished",
        )?;
        anyhow::ensure!(
            output.status.success(),
            "Cannot prove the task commit is unpublished: remote inspection failed"
        );
        for line in String::from_utf8(output.stdout)?.lines() {
            let oid = line
                .split_whitespace()
                .next()
                .context("Remote returned an invalid reference")?;
            let tip = resolve_git_commit(project, oid, "resolve a remote tip for board repair")
                .context("Cannot prove the task commit is unpublished because a remote tip is unavailable locally; preserve history for explicit recovery")?;
            anyhow::ensure!(
                !git_commit_is_ancestor(project, commit, &tip)?,
                "The task commit is already published; automatic board-order repair will not rewrite it"
            );
        }
    }
    Ok(())
}

struct IndexLock(PathBuf);
impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct RepairWorktree<'a> {
    project: &'a Path,
    path: PathBuf,
}
impl Drop for RepairWorktree<'_> {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .current_dir(self.project)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
    }
}

fn correct_shared_index(project: &Path, index: &Path, sealed: &str, session: &str) -> Result<()> {
    let expected = blob(project, sealed)?;
    let (selected, _) =
        selected_line_removed(&expected, session).context("Sealed Done entry is ambiguous")?;
    let prefix = expected
        .split_once(&selected)
        .context("Sealed Done entry disappeared")?
        .0;
    anyhow::ensure!(
        !prefix.lines().any(|line| line.starts_with("- ")),
        "Automatic order repair requires the selected task to be the first Done entry"
    );
    let output = Command::new("git")
        .current_dir(project)
        .env("GIT_INDEX_FILE", index)
        .args(["show", &format!(":{DONE_PATH}")])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Shared index has no unambiguous Done board"
    );
    let staged = String::from_utf8(output.stdout)?;
    let (staged_selected, remainder) = selected_line_removed(&staged, session)
        .context("Shared index no longer has the exact selected Done entry")?;
    anyhow::ensure!(
        staged_selected == selected && remainder.starts_with(prefix),
        "Shared index changed the selected task or board heading; preserve those edits for explicit recovery"
    );
    let corrected = format!("{prefix}{selected}{}", &remainder[prefix.len()..]);
    if corrected == staged {
        return Ok(());
    }
    let entries = run_agent_git_projection_command(
        project,
        index,
        None,
        &["ls-files", "--stage", "--", DONE_PATH],
        "inspect staged board mode",
    )?;
    let fields: Vec<_> = entries.split_whitespace().collect();
    anyhow::ensure!(
        fields.len() == 4 && fields[2] == "0",
        "Shared index contains an unresolved Done-board merge"
    );
    let mut child = Command::new("git")
        .current_dir(project)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("Git blob stdin unavailable")?
        .write_all(corrected.as_bytes())?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "Cannot store corrected staged board"
    );
    let oid = String::from_utf8(output.stdout)?;
    run_agent_git_projection_command(
        project,
        index,
        None,
        &[
            "update-index",
            "--cacheinfo",
            &format!("{},{},{}", fields[0], oid.trim(), DONE_PATH),
        ],
        "stage only the selected task position",
    )?;
    Ok(())
}

pub(super) fn repair_board_order(
    store: &agent::TursoAgentStore,
    project: &Path,
    journal: &agent::GitFinalizationRecord,
    commit: &str,
    lease: Option<&AgentGitFinalizationLease>,
) -> Result<bool> {
    // Only scheduler-owned finalization may mutate history. Worker exit proof
    // and read-only supervisor assessment merely report the mismatch.
    let Some(lease) = lease else { return Ok(false) };
    for key in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_NAMESPACE",
    ] {
        anyhow::ensure!(
            std::env::var_os(key).is_none(),
            "Automatic board repair requires the standard checkout environment; {key} is set"
        );
    }
    let baseline = AgentGitWorktreeBaseline::from_json(&journal.worktree_baseline)?;
    let (Some(tree), Some(branch), Some(identity)) = (
        baseline.staged_index_tree.as_deref(),
        journal.branch_ref.as_deref(),
        journal.task_identity.as_deref(),
    ) else {
        return Ok(false);
    };
    if baseline.version < 2
        || journal.state != GitFinalizationState::CommitPending
        || journal.commit_oid.is_some()
        || !is_only_selected_order(project, commit, tree, &journal.codex_session_id)?
    {
        return Ok(false);
    }
    let _board_lock = acquire_board_mutation_lock(&get_tasks_dir(project))?;
    let recheck = || -> Result<()> {
        lease.ensure_owned()?;
        anyhow::ensure!(
            store
                .list_projects_blocking()?
                .iter()
                .any(|project| project.id == journal.project_id && project.enabled),
            "Project was paused during automatic board repair"
        );
        let control =
            store.session_control_blocking(journal.project_id, &journal.codex_session_id)?;
        anyhow::ensure!(
            control.is_some_and(|control| control.state
                == AgentSessionControlState::ResumeRequested
                && control.run_token.as_deref()
                    == Some(&format!(
                        "{}{}",
                        agent::AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX,
                        journal.generation
                    ))
                && control.child_pid.is_none()
                && control.interactive_holder.is_none()
                && control.interactive_launch_token.is_none()),
            "Task session was stopped or acquired during automatic board repair"
        );
        let mut linked = Vec::new();
        collect_codex_session_tasks_in_board(
            &get_tasks_dir(project),
            &journal.codex_session_id,
            &mut linked,
        )?;
        anyhow::ensure!(
            matches!(linked.as_slice(), [(TaskStatus::Done, task)] if !crate::task::task_entry_is_stopped(task) && !task_content_is_manual(&task.content)),
            "Task was stopped, claimed, or moved during automatic board repair"
        );
        anyhow::ensure!(
            store
                .git_finalization_blocking(journal.project_id, &journal.codex_session_id)?
                .as_ref()
                == Some(journal),
            "Git journal changed during automatic board repair"
        );
        verify_agent_git_resume_branch(project, journal)?;
        anyhow::ensure!(
            resolve_git_commit(project, "HEAD", "verify repair HEAD")? == commit,
            "Automatic board-order repair requires the task commit to remain HEAD"
        );
        anyhow::ensure!(
            local_agent_git_task_commit_is_retained(
                project,
                Some(branch),
                commit,
                &journal.codex_session_id,
                identity
            )?,
            "Task ownership changed during automatic board repair"
        );
        Ok(())
    };
    recheck()?;
    require_unpublished(project, branch, commit, &baseline)?;
    recheck()?;

    let temporary = TempDir::new()?;
    let index = PathBuf::from(git_stdout(
        project,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        "resolve shared index for repair",
    )?);
    anyhow::ensure!(
        fs::symlink_metadata(&index)?.file_type().is_file(),
        "Automatic board repair requires a regular shared index"
    );
    let lock_path = index.with_extension("lock");
    let mut locked_index = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .context("Shared index is busy; retry automatic board repair after its owner exits")?;
    let lock = IndexLock(lock_path);
    let corrected_index = temporary.path().join("index");
    fs::copy(&index, &corrected_index)?;
    correct_shared_index(project, &corrected_index, tree, &journal.codex_session_id)?;
    run_agent_git_projection_command(
        project,
        &corrected_index,
        None,
        &["write-tree"],
        "verify the preserved shared index",
    )?;

    // Run ordinary hooks and signing against an isolated checkout and index.
    let checkout = temporary.path().join("checkout");
    let _checkout = RepairWorktree {
        project,
        path: checkout.clone(),
    };
    let output = run_agent_git_remote_command(
        Command::new("git")
            .current_dir(project)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["worktree", "add", "--detach", "--no-checkout"])
            .arg(&checkout)
            .arg(commit),
        "create isolated repair checkout",
    )?;
    anyhow::ensure!(
        output.status.success(),
        "Cannot create isolated board repair checkout: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    anyhow::ensure!(
        fs::canonicalize(git_stdout(
            &checkout,
            &["rev-parse", "--show-toplevel"],
            "verify isolated repair checkout"
        )?)? == fs::canonicalize(&checkout)?,
        "Git configuration redirected the repair checkout; preserve the original worktree"
    );
    let materialized = run_agent_git_remote_command(
        Command::new("git")
            .current_dir(&checkout)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["read-tree", "--reset", "-u", tree]),
        "materialize the exact sealed tree",
    )?;
    anyhow::ensure!(
        materialized.status.success(),
        "Cannot materialize the sealed task tree: {}",
        String::from_utf8_lossy(&materialized.stderr).trim()
    );
    let mut command = Command::new("git");
    command
        .current_dir(&checkout)
        .args(["commit", "--amend", "--no-edit"]);
    configure_agent_git_identity(&mut command, journal.git_mode);
    let output = run_agent_git_remote_command(
        &mut command,
        "create the sealed board-order repair with normal hooks",
    )?;
    anyhow::ensure!(
        output.status.success(),
        "Automatic board-order repair commit failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let repaired = resolve_git_commit(&checkout, "HEAD", "verify repaired task commit")?;
    anyhow::ensure!(
        git_commit_matches_agent_staged_manifest(
            project,
            &repaired,
            &journal.codex_session_id,
            identity,
            &journal.worktree_baseline,
            true
        )? && git_commit_uses_agent_identity(project, &repaired)?
            && git_commit_task_trailers(project, &repaired)?
                == [format!("codex:{}", journal.codex_session_id)],
        "Repair hooks changed the sealed task payload or identity; preserve the original task commit"
    );
    recheck()?;
    // Publish the index first. A crash here leaves only the intended order fix
    // staged; the exact-entry normalization makes the next repair idempotent. The ref
    // update compares the exact old OID so concurrent commits cannot be replaced.
    locked_index.write_all(&fs::read(&corrected_index)?)?;
    locked_index.set_permissions(fs::metadata(&index)?.permissions())?;
    locked_index.sync_all()?;
    fs::rename(&lock.0, &index)?;
    git_stdout(
        project,
        &[
            "update-ref",
            "-m",
            "clt: repair sealed task board order",
            branch,
            &repaired,
            commit,
        ],
        "publish the verified task repair",
    )?;
    lease.ensure_owned()?;
    println!(
        "Task Git board-order repair: session={} original={} repaired={}",
        journal.codex_session_id, commit, repaired
    );
    Ok(true)
}
