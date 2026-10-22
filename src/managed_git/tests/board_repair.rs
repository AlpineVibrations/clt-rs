use super::*;
use crate::managed_git::{AgentGitWorktreeBaseline, agent_git_finalization_diagnostic};

const SESSION: &str = "session-board-repair";
const RUN: &str = "run-board-repair";

struct Fixture {
    root: PathBuf,
    state: PathBuf,
    store: agent::TursoAgentStore,
    project: agent::AgentProject,
    original: String,
    sealed: String,
}
impl Fixture {
    fn new() -> Self {
        let root = temp_root("board-order-repair");
        let state = root.join("state");
        let path = root.join("project");
        init_tasks(&path, false).unwrap();
        fs::write(
            path.join("tasks/doing.md"),
            format!(
                "# Doing Tasks\n- Repair feature — COMPLETED 2026-10-22: checked codex:{SESSION}\n"
            ),
        )
        .unwrap();
        fs::write(
            path.join("tasks/done.md"),
            "# Done Tasks\n- Previous task\n- Earlier task\n",
        )
        .unwrap();
        fs::write(path.join("tasks/todo.md"), "# Todo Tasks\n- Later task\n").unwrap();
        initialize_test_git_repository(&path);
        let path = fs::canonicalize(path).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
        store.register_project_blocking(&path, "repair").unwrap();
        store
            .set_project_git_mode_for_path_blocking(&path, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                SESSION,
                123,
                RUN,
                &root.join("run.out"),
                &root.join("run.err"),
            )
            .unwrap();
        let start = capture_agent_git_start_state(&path, AgentGitMode::Commit).unwrap();
        ensure_agent_git_working_record(&store, &project, SESSION, RUN, Some(&start)).unwrap();
        bind_agent_git_working_task_identity(&store, &project, SESSION, RUN).unwrap();
        fs::write(path.join("feature.txt"), "verified implementation\n").unwrap();
        run_test_git(&path, &["add", "feature.txt"]);
        move_task_to_done_with_agent_store(
            &path,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: RUN.into(),
            },
            &store,
        )
        .unwrap();
        let done = fs::read_to_string(path.join("tasks/done.md")).unwrap();
        let mut lines: Vec<_> = done.lines().collect();
        let selected = lines
            .iter()
            .position(|line| line.contains(SESSION))
            .unwrap();
        let task = lines.remove(selected);
        lines.push(task);
        fs::write(
            path.join("tasks/done.md"),
            format!("{}\n", lines.join("\n")),
        )
        .unwrap();
        run_test_git(&path, &["add", "--all"]);
        run_test_agent_git(
            &path,
            &[
                "commit",
                "-m",
                "Repair feature",
                "-m",
                &format!("CLT-Task: codex:{SESSION}"),
            ],
        );
        let original = run_test_git(&path, &["rev-parse", "HEAD"]);
        fs::write(path.join("tasks/done.md"), done).unwrap();
        store
            .clear_running_session_control_blocking(project.id, SESSION, Some(RUN))
            .unwrap();
        store
            .ensure_pending_git_finalization_resume_requested_blocking(project.id, SESSION)
            .unwrap();
        let journal = store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap()
            .unwrap();
        let sealed = AgentGitWorktreeBaseline::from_json(&journal.worktree_baseline)
            .unwrap()
            .staged_index_tree
            .unwrap();
        Self {
            root,
            state,
            store,
            project,
            original,
            sealed,
        }
    }
    fn journal(&self) -> agent::GitFinalizationRecord {
        self.store
            .git_finalization_blocking(self.project.id, SESSION)
            .unwrap()
            .unwrap()
    }
    fn reconcile(&self) -> anyhow::Result<agent::GitFinalizationRecord> {
        if self.journal().state.is_terminal() {
            return reconcile_agent_git_finalization(
                &self.store,
                &self.project.path,
                self.journal(),
                None,
                None,
            );
        }
        let lease =
            try_acquire_agent_git_finalization_lease(&self.state, &self.project, false)?.unwrap();
        let result = reconcile_agent_git_finalization(
            &self.store,
            &self.project.path,
            self.journal(),
            None,
            Some(&lease),
        );
        lease.release()?;
        result
    }
}

#[test]
fn board_order_repair_uses_the_seal_and_preserves_shared_work() {
    for index_already_corrected in [false, true] {
        let fixture = Fixture::new();
        let path = &fixture.project.path;
        fs::write(path.join("unrelated.txt"), "staged outside work\n").unwrap();
        run_test_git(path, &["add", "unrelated.txt"]);
        if index_already_corrected {
            run_test_git(path, &["add", "tasks/done.md"]);
        }
        let indexed = run_test_git(path, &["show", ":tasks/done.md"])
            .replace("Previous task", "Previous task with outside staged note");
        fs::write(path.join("tasks/done.md"), format!("{indexed}\n")).unwrap();
        run_test_git(path, &["add", "tasks/done.md"]);
        fs::write(
            path.join("tasks/done.md"),
            "Outside unstaged work is untouched\n",
        )
        .unwrap();
        // Retain completed identity in the worktree while making it differ from staging.
        let sealed_board = run_test_git(
            path,
            &["show", &format!("{}:tasks/done.md", fixture.sealed)],
        );
        fs::write(
            path.join("tasks/done.md"),
            format!("{sealed_board}\n- Outside unstaged task\n"),
        )
        .unwrap();
        fs::write(path.join("untracked.txt"), "outside untracked work\n").unwrap();
        let before_board = fs::read(path.join("tasks/done.md")).unwrap();
        let result = fixture.reconcile().unwrap();
        assert_eq!(result.state, GitFinalizationState::Completed);
        let repaired = run_test_git(path, &["rev-parse", "HEAD"]);
        assert_ne!(repaired, fixture.original);
        assert_eq!(
            run_test_git(path, &["rev-parse", "HEAD^{tree}"]),
            fixture.sealed
        );
        assert_eq!(
            run_test_git(path, &["rev-parse", "HEAD^"]),
            run_test_git(path, &["rev-parse", &format!("{}^", fixture.original)])
        );
        assert_eq!(fs::read(path.join("tasks/done.md")).unwrap(), before_board);
        assert!(run_test_git(path, &["show", ":tasks/done.md"]).contains("outside staged note"));
        assert_eq!(
            run_test_git(path, &["show", ":unrelated.txt"]),
            "staged outside work"
        );
        assert_eq!(
            fs::read_to_string(path.join("untracked.txt")).unwrap(),
            "outside untracked work\n"
        );
        assert_eq!(fixture.reconcile().unwrap().commit_oid, Some(repaired));
        assert_eq!(
            run_test_git(path, &["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            1
        );
    }
}

#[test]
#[cfg(unix)]
fn board_order_repair_refuses_unsafe_or_failed_corrections() {
    for scenario in [
        "implementation",
        "other_task",
        "published",
        "advanced_head",
        "hook_failure",
        "hook_mutation",
        "index_lock",
        "stopped",
        "paused",
    ] {
        let fixture = Fixture::new();
        let path = &fixture.project.path;
        match scenario {
            "implementation" | "other_task" => {
                if scenario == "implementation" {
                    fs::write(path.join("feature.txt"), "unsealed change\n").unwrap();
                    run_test_git(path, &["add", "feature.txt"]);
                } else {
                    let board = run_test_git(path, &["show", ":tasks/done.md"])
                        .replace("Previous task", "Changed prior task");
                    fs::write(path.join("tasks/done.md"), format!("{board}\n")).unwrap();
                    run_test_git(path, &["add", "tasks/done.md"]);
                }
                run_test_agent_git(path, &["commit", "--amend", "--no-edit"]);
            }
            "published" => {
                let remote = fixture.root.join("remote.git");
                fs::create_dir(&remote).unwrap();
                run_test_git(&remote, &["init", "--bare"]);
                run_test_git(path, &["remote", "add", "origin", remote.to_str().unwrap()]);
                run_test_git(path, &["push", "origin", "HEAD:refs/heads/published"]);
                run_test_git(path, &["update-ref", "-d", "refs/remotes/origin/published"]);
            }
            "advanced_head" => {
                run_test_git(
                    path,
                    &["commit", "--allow-empty", "-m", "Later user commit"],
                );
            }
            "hook_failure" | "hook_mutation" => {
                use std::os::unix::fs::PermissionsExt;
                let hook = path.join(".git/hooks/pre-commit");
                let script = if scenario == "hook_failure" {
                    "#!/bin/sh\necho required-hook-failed >&2\nexit 1\n"
                } else {
                    "#!/bin/sh\necho hook-change > feature.txt\ngit add feature.txt\n"
                };
                fs::write(&hook, script).unwrap();
                fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
            }
            "stopped" => {
                fixture
                    .store
                    .set_session_control_state_blocking(
                        fixture.project.id,
                        SESSION,
                        AgentSessionControlState::Stopped,
                    )
                    .unwrap();
            }
            "paused" => {
                fixture
                    .store
                    .set_project_enabled_for_path_blocking(path, false)
                    .unwrap();
            }
            "index_lock" => {
                fs::write(path.join(".git/index.lock"), "foreign lock").unwrap();
            }
            _ => unreachable!(),
        }
        let head = run_test_git(path, &["rev-parse", "HEAD"]);
        let index = fs::read(path.join(".git/index")).unwrap();
        let feature = fs::read(path.join("feature.txt")).unwrap();
        let error = fixture.reconcile().unwrap_err();
        assert!(!error.to_string().is_empty(), "{scenario}");
        assert_eq!(
            run_test_git(path, &["rev-parse", "HEAD"]),
            head,
            "{scenario}"
        );
        assert_eq!(
            fs::read(path.join(".git/index")).unwrap(),
            index,
            "{scenario}"
        );
        assert_eq!(
            fs::read(path.join("feature.txt")).unwrap(),
            feature,
            "{scenario}"
        );
        assert_eq!(fixture.journal().state, GitFinalizationState::CommitPending);
        if scenario == "index_lock" {
            assert_eq!(
                fs::read_to_string(path.join(".git/index.lock")).unwrap(),
                "foreign lock"
            );
        }
    }
}

#[test]
fn supervisor_reviews_sealed_failure_and_resumes_only_its_original_done_session() {
    let fixture = Fixture::new();
    let path = &fixture.project.path;
    // A code mismatch is deliberately outside automatic board-order repair.
    fs::write(path.join("feature.txt"), "unsealed code\n").unwrap();
    run_test_git(path, &["add", "feature.txt"]);
    run_test_agent_git(path, &["commit", "--amend", "--no-edit"]);
    fixture
        .store
        .set_supervisor_settings_blocking(&crate::supervisor::SupervisorSettings {
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    let diagnostic = agent_git_finalization_diagnostic(path, &fixture.journal()).unwrap();
    assert!(diagnostic.contains("differing paths: feature.txt, tasks/done.md"));
    let mut pass = run_agent_scheduler_pass(&fixture.state, false, &[]).unwrap();
    assert_eq!(pass.jobs.len(), 1);
    assert_eq!(
        pass.jobs[0].task_selection,
        AgentTaskSelection::SupervisorReview
    );
    let job = pass.jobs.remove(0);
    fixture
        .store
        .release_lease_blocking(fixture.project.id, &job.holder)
        .unwrap();
    crate::supervisor::begin_review(&fixture.store, &fixture.project).unwrap();
    let stderr = fixture.root.join("review.err");
    fs::write(crate::supervisor::output_path(&stderr), r#"{"decision":"retry","task":1,"reason":"Inspect the exact mismatch","next_action":"Inspect the existing commit and obtain the required correction authorization; never create another task commit."}"#).unwrap();
    let result = crate::runner::AgentRunResult {
        status: "success",
        exit_code: Some(0),
        log_dir: fixture.root.clone(),
        stdout_path: fixture.root.join("review.out"),
        stderr_path: stderr,
        summary: "reviewed".into(),
        codex_session_id: None,
        session_run_token: None,
        control_action: None,
    };
    crate::supervisor::finish_review(&fixture.store, &fixture.project, &result).unwrap();
    let before = fs::read(path.join("tasks/done.md")).unwrap();
    crate::supervisor::prepare_retry(&fixture.store, &fixture.project).unwrap();
    assert_eq!(fs::read(path.join("tasks/done.md")).unwrap(), before);
    let pass = run_agent_scheduler_pass(&fixture.state, false, &[]).unwrap();
    assert_eq!(pass.jobs.len(), 1);
    assert_eq!(
        pass.jobs[0].task_selection,
        AgentTaskSelection::SupervisorRetry
    );
    assert_eq!(pass.jobs[0].resume_session_id.as_deref(), Some(SESSION));
    fixture
        .store
        .release_lease_blocking(fixture.project.id, &pass.jobs[0].holder)
        .unwrap();
}

#[test]
fn scheduler_repairs_a_sealed_board_before_supervisor_holds_and_next_todo() {
    let fixture = Fixture::new();
    fixture
        .store
        .set_supervisor_settings_blocking(&crate::supervisor::SupervisorSettings {
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    let mut review = crate::supervisor::begin_review(&fixture.store, &fixture.project).unwrap();
    review.state = "held".into();
    review.error = Some("Previously stalled finalization".into());
    fixture
        .store
        .save_supervisor_review_blocking(fixture.project.id, &review)
        .unwrap();
    let pass = run_agent_scheduler_pass(&fixture.state, false, &[]).unwrap();
    assert_eq!(fixture.journal().state, GitFinalizationState::Completed);
    assert_eq!(
        run_test_git(&fixture.project.path, &["rev-parse", "HEAD^{tree}"]),
        fixture.sealed
    );
    assert_eq!(pass.jobs.len(), 1);
    assert_eq!(pass.jobs[0].task_selection, AgentTaskSelection::NextTodo);
    fixture
        .store
        .release_lease_blocking(fixture.project.id, &pass.jobs[0].holder)
        .unwrap();
    assert!(
        fixture
            .store
            .supervisor_review_blocking(fixture.project.id)
            .unwrap()
            .is_none()
    );
}
