use std::{
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::Local;

use crate::{
    agent::{
        AGENT_BRANCH_GIT_RECOVERY_REASON, AGENT_MISSING_GIT_RECOVERY_TOKEN_PREFIX, AgentProject,
        AgentRunRecord, AgentSessionControlState, GitFinalizationRecord, GitFinalizationState,
        TursoAgentStore,
    },
    managed_git::{
        current_agent_git_branch, current_agent_git_head, git_ref_active_task_identity_count,
    },
    runner::{agent_timestamp, agent_timestamp_after, automated_agent_child_context},
    session_control::InteractiveAgentLease,
    task::{
        TASK_STATUSES, TASK_STOPPED_MARKER, TaskBoard, TaskEntry, TaskSource, TaskStatus,
        acquire_board_mutation_lock, board_has_manual_session, durable_task_identity,
        get_tasks_dir, move_task_without_reordering_after_lock, prioritize_task_after_lock,
        read_task_entries, recoverable_codex_session_id_from_task_content, task_blocking_note,
        task_content_is_manual, task_content_without_recoverable_codex_session,
        task_content_without_stop_marker, task_display_text, task_entry_is_blocked,
        task_entry_is_ready, task_entry_is_stopped,
    },
};

#[cfg(test)]
mod tests;

pub(crate) fn failure_has_missing_git_start(summary: &str) -> bool {
    // Stored run diagnostics from older versions remain recoverable.
    summary.contains("no frozen Git start journal") || summary.contains("no frozen start journal")
}

pub(crate) fn failure_has_git_recovery(summary: &str) -> bool {
    failure_has_missing_git_start(summary) || summary.contains("Git task branch changed:")
}

#[derive(Clone)]
pub(crate) struct GitRecoveryPlan {
    pub(crate) project: AgentProject,
    pub(crate) session_id: String,
    linked: Option<GitRecoveryTask>,
    run_id: i64,
    journal: Option<GitFinalizationRecord>,
    current_branch: Option<String>,
    restart: bool,
    preactivation: bool,
    stale_head: Option<String>,
    writer_conflict: Option<(i64, String)>,
}

#[derive(Clone)]
struct GitRecoveryTask {
    task: TaskEntry,
    status: TaskStatus,
    board_dir: PathBuf,
}

impl GitRecoveryPlan {
    pub(crate) fn prompt(&self) -> String {
        let Some(linked) = &self.linked else {
            let journal = self
                .journal
                .as_ref()
                .expect("Orphan recovery requires a journal");
            return format!(
                "Recover {}: orphaned Git attempt\nSession {} belongs to {} and has no task on {}. Retire this old attempt and stop its retries? The current board, files, staging, commits and old journal are preserved. [y/n]",
                self.project.name,
                self.session_id,
                journal.branch_ref.as_deref().unwrap_or("detached HEAD"),
                self.current_branch.as_deref().unwrap_or("detached HEAD"),
            );
        };
        let title: String = task_display_text(&linked.task).chars().take(160).collect();
        if self.restart {
            return format!(
                "Restart {}: {}\nQueue a fresh attempt to finish this task? Files, staging, commits, the old conversation and Git journal are preserved. Only an unsealed idle attempt may be retired. [y/n]",
                self.project.name, title,
            );
        }
        if let Some(journal) = &self.journal {
            return format!(
                "Recover {}: {}\nRetire the old attempt on {} and queue a fresh Codex conversation on {} to review existing work? Files, staging, commits and the old journal are preserved. A provisional Done task returns to Todo for verification. [y/n]",
                self.project.name,
                title,
                journal.branch_ref.as_deref().unwrap_or("detached HEAD"),
                self.current_branch.as_deref().unwrap_or("detached HEAD"),
            );
        }
        if linked.status == TaskStatus::Done {
            format!(
                "Recover {}: {}\nThis task is already in Done. Accept its current completion and stop retrying the old run? Files, commits and conversation are preserved. [y/n]",
                self.project.name, title,
            )
        } else {
            format!(
                "Recover {}: {}\nKeep current files and commits, and queue a fresh Codex conversation to review existing work and finish what remains? The old conversation ID stays in the task history. [y/n]",
                self.project.name, title,
            )
        }
    }
}

fn linked_tasks(board_dir: &Path, tasks: &mut Vec<(PathBuf, TaskStatus, TaskEntry)>) -> Result<()> {
    for status in TASK_STATUSES {
        for task in read_task_entries(board_dir, status)? {
            if recoverable_codex_session_id_from_task_content(&task.content).is_some() {
                tasks.push((board_dir.to_path_buf(), status, task.clone()));
            }
            if task.has_subtasks
                && let TaskSource::Path { path, is_dir: true } = &task.source
            {
                linked_tasks(path, tasks)?;
            }
        }
    }
    Ok(())
}

fn restart_preserves_other_git_work(
    store: &TursoAgentStore,
    project_id: i64,
    session: &str,
    tasks: &[(PathBuf, TaskStatus, TaskEntry)],
) -> Result<bool> {
    for journal in store.list_pending_git_finalizations_blocking(Some(project_id))? {
        if journal.codex_session_id == session {
            continue;
        }
        let linked = tasks
            .iter()
            .filter(|(_, _, task)| {
                recoverable_codex_session_id_from_task_content(&task.content)
                    == Some(journal.codex_session_id.as_str())
            })
            .collect::<Vec<_>>();
        if journal.state != GitFinalizationState::Working
            || !matches!(linked.as_slice(), [(_, TaskStatus::Todo, task)]
                if task_entry_is_blocked(task) || task_entry_is_stopped(task))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A preview carries the exact run and task contents the user is accepting.
/// It never reconstructs an old journal or changes the checkout.
pub(crate) fn plan_git_recovery(
    store: &TursoAgentStore,
    project: &AgentProject,
    session_id: Option<&str>,
) -> Result<GitRecoveryPlan> {
    let run = store
        .latest_run_for_project_blocking(project.id)?
        .filter(|run| matches!(run.status.as_str(), "failure" | "timeout"))
        .context("This project has no failed Git attempt to recover")?;
    anyhow::ensure!(
        session_id
            .zip(run.codex_session_id.as_deref())
            .is_none_or(|(requested, recorded)| requested == recorded),
        "The selected conversation does not match the failed run; recover the task identified by that run"
    );
    let board_dir = get_tasks_dir(&project.path);
    let _lock = acquire_board_mutation_lock(&board_dir)?;
    let mut tasks = Vec::new();
    linked_tasks(&board_dir, &mut tasks)?;
    let requested_session = session_id.or(run.codex_session_id.as_deref());
    let controls = store.session_controls_for_project_blocking(project.id)?;
    let recovery_token = format!("{AGENT_MISSING_GIT_RECOVERY_TOKEN_PREFIX}{}", run.id);
    let mut candidates = Vec::new();
    let mut current_branch = None;
    for (board, status, task) in &tasks {
        let session = recoverable_codex_session_id_from_task_content(&task.content).unwrap();
        let selected = match requested_session {
            Some(requested) => requested == session,
            None => {
                *status == TaskStatus::Doing
                    || controls.iter().any(|control| {
                        control.codex_session_id == session
                            && (control.state == AgentSessionControlState::ResumeRequested
                                || control.state == AgentSessionControlState::Stopped
                                    && control.run_token.as_deref()
                                        == Some(recovery_token.as_str()))
                    })
            }
        };
        if selected {
            let journal = store.git_finalization_blocking(project.id, session)?;
            let recoverable = if let Some(journal) = &journal {
                current_branch = current_agent_git_branch(&project.path)?;
                let interrupted_recovery = journal.state == GitFinalizationState::Cancelled
                    && journal.last_error.as_deref() == Some(AGENT_BRANCH_GIT_RECOVERY_REASON)
                    && controls.iter().any(|control| {
                        control.codex_session_id == session
                            && control.state == AgentSessionControlState::Stopped
                            && control.run_token.as_deref() == Some(recovery_token.as_str())
                    });
                journal.commit_oid.is_none()
                    && current_branch.is_some()
                    && (interrupted_recovery
                        || current_branch != journal.branch_ref
                            && matches!(
                                journal.state,
                                GitFinalizationState::Working
                                    | GitFinalizationState::Tracking
                                    | GitFinalizationState::CommitPending
                            ))
            } else {
                run.summary
                    .as_deref()
                    .is_some_and(failure_has_missing_git_start)
            };
            if recoverable {
                candidates.push((board, status, task, session, journal));
            }
        }
    }
    // A branch switch may remove the old task entirely or replace its session.
    // Recover the failed journal itself; never infer a replacement task by title.
    if candidates.is_empty()
        && let Some(session) = requested_session
        && !tasks.iter().any(|(_, _, task)| {
            recoverable_codex_session_id_from_task_content(&task.content) == Some(session)
        })
        && let Some(journal) = store.git_finalization_blocking(project.id, session)?
    {
        current_branch = current_agent_git_branch(&project.path)?;
        let interrupted = journal.state == GitFinalizationState::Cancelled
            && journal.last_error.as_deref() == Some(AGENT_BRANCH_GIT_RECOVERY_REASON)
            && controls.iter().any(|control| {
                control.codex_session_id == session
                    && control.state == AgentSessionControlState::Stopped
                    && control.run_token.as_deref() == Some(recovery_token.as_str())
            });
        if journal.commit_oid.is_none()
            && current_branch.is_some()
            && current_branch != journal.branch_ref
            && (interrupted
                || matches!(
                    journal.state,
                    GitFinalizationState::Working
                        | GitFinalizationState::Tracking
                        | GitFinalizationState::CommitPending
                ))
        {
            return Ok(GitRecoveryPlan {
                project: project.clone(),
                session_id: session.to_string(),
                linked: None,
                run_id: run.id,
                journal: Some(journal),
                current_branch,
                restart: false,
                preactivation: false,
                stale_head: None,
                writer_conflict: None,
            });
        }
    }
    anyhow::ensure!(
        candidates.len() == 1,
        "Cannot identify one affected task. Use clt agent recover-task --session <session-id> for the intended task; its saved error is available with l"
    );
    let (board, status, task, session, journal) = candidates.remove(0);
    anyhow::ensure!(
        tasks
            .iter()
            .filter(
                |(_, _, task)| recoverable_codex_session_id_from_task_content(&task.content)
                    == Some(session)
            )
            .count()
            == 1,
        "The conversation is linked to several tasks; resolve the duplicate links before recovery"
    );
    anyhow::ensure!(
        matches!(
            status,
            TaskStatus::Todo | TaskStatus::Doing | TaskStatus::Done
        ),
        "Move the affected task out of Backlog before recovering it"
    );
    Ok(GitRecoveryPlan {
        project: project.clone(),
        session_id: session.to_string(),
        linked: Some(GitRecoveryTask {
            task: task.clone(),
            status: *status,
            board_dir: board.clone(),
        }),
        run_id: run.id,
        journal,
        current_branch,
        restart: false,
        preactivation: false,
        stale_head: None,
        writer_conflict: None,
    })
}

/// An explicit restart is selected by exact session, independently of unrelated
/// later supervisor runs. Never reopen a terminal journal or discard sealed proof.
pub(crate) fn plan_task_restart(
    store: &TursoAgentStore,
    project: &AgentProject,
    session: &str,
) -> Result<GitRecoveryPlan> {
    let board = get_tasks_dir(&project.path);
    let _lock = acquire_board_mutation_lock(&board)?;
    let mut tasks = Vec::new();
    linked_tasks(&board, &mut tasks)?;
    let mut selected = tasks.into_iter().filter(|(_, _, task)| {
        recoverable_codex_session_id_from_task_content(&task.content) == Some(session)
    });
    let (board_dir, status, task) = selected
        .next()
        .context("No task links to the selected session")?;
    anyhow::ensure!(
        selected.next().is_none(),
        "The conversation is linked to several tasks"
    );
    anyhow::ensure!(
        status.is_active() || status == TaskStatus::Done,
        "Move the selected task out of Backlog before restarting it"
    );
    let journal = store.git_finalization_blocking(project.id, session)?;
    anyhow::ensure!(
        journal.as_ref().is_none_or(|journal| {
            journal.commit_oid.is_none()
                && matches!(
                    journal.state,
                    GitFinalizationState::Working | GitFinalizationState::Cancelled
                )
        }),
        "Cannot restart sealed or verified Git work; finish its existing finalization first"
    );
    let owner_run = journal
        .as_ref()
        .and_then(|journal| journal.owner_run_token.as_deref())
        .map(|token| store.run_for_worker_token_blocking(project.id, token))
        .transpose()?
        .flatten();
    let run = store
        .latest_run_for_codex_session_blocking(project.id, session)?
        .or(owner_run)
        .context("The selected session has no recorded task run")?;
    Ok(GitRecoveryPlan {
        project: project.clone(),
        session_id: session.into(),
        linked: Some(GitRecoveryTask {
            task,
            status,
            board_dir,
        }),
        run_id: run.id,
        journal,
        current_branch: current_agent_git_branch(&project.path)?,
        restart: true,
        preactivation: false,
        stale_head: None,
        writer_conflict: None,
    })
}

// Only a failed startup with no agent output qualifies. Never infer this from
// task text or the generic WORKING summary, which can mask the actual error.
fn writer_conflict_at_startup(run: &AgentRunRecord, session: &str) -> bool {
    if run.status != "failure"
        || run.finished_at.is_none()
        || !run.exit_code.is_some_and(|code| code != 0)
        || run.codex_session_id.as_deref() != Some(session)
    {
        return false;
    }
    let read_small_log = |path: Option<&str>| -> Option<String> {
        let file = std::fs::File::open(path?).ok()?;
        let mut text = String::new();
        file.take(65537).read_to_string(&mut text).ok()?;
        (text.len() <= 65536).then_some(text)
    };
    let Some(stdout) = read_small_log(run.stdout_path.as_deref()) else {
        return false;
    };
    let Some(stderr) = read_small_log(run.stderr_path.as_deref()) else {
        return false;
    };
    let message = format!("thread {session} already has an active writer");
    let diagnostic = format!("Error: thread/resume: thread/resume failed: {message} (code -32600)");
    stdout.trim().is_empty()
        && stderr.lines().any(|line| {
            let line = line.trim();
            if line == diagnostic {
                return true;
            }
            line.strip_prefix("Error: Codex thread/resume: ")
                .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
                .is_some_and(|error| {
                    error.get("code").and_then(|code| code.as_i64()) == Some(-32600)
                        && error.get("message").and_then(|message| message.as_str())
                            == Some(message.as_str())
                })
        })
}

/// Two consecutive failed resumes establish a persistent startup conflict.
/// Use the normal idle-project recovery fence; never edit Codex's own locks.
pub(crate) fn recover_writer_conflict_automatically(
    store: &TursoAgentStore,
    project: &AgentProject,
) -> Result<Option<String>> {
    if !project.enabled {
        return Ok(None);
    }
    for control in store.session_controls_for_project_blocking(project.id)? {
        if control.state != AgentSessionControlState::ResumeRequested
            || control.child_pid.is_some()
            || control.interactive_holder.is_some()
            || control.interactive_launch_token.is_some()
        {
            continue;
        }
        let Some(token) = control.run_token else {
            continue;
        };
        let session = &control.codex_session_id;
        let Some(run) = store.latest_run_for_codex_session_blocking(project.id, session)? else {
            continue;
        };
        if !writer_conflict_at_startup(&run, session) {
            continue;
        }
        let Some(previous) =
            store.previous_run_for_codex_session_blocking(project.id, session, run.id)?
        else {
            continue;
        };
        if !writer_conflict_at_startup(&previous, session) {
            continue;
        }
        let journal = store.git_finalization_blocking(project.id, session)?;
        if journal.as_ref().is_some_and(|journal| {
            journal.state != GitFinalizationState::Working || journal.commit_oid.is_some()
        }) {
            continue;
        }
        let mut plan = plan_task_restart(store, project, session)?;
        if plan.journal != journal
            || plan.run_id != run.id
            || !plan.linked.as_ref().is_some_and(|linked| {
                linked.status.is_active() && task_entry_is_ready(&linked.task)
            })
        {
            continue;
        }
        // Missing managed journals cannot be replaced from a later checkout.
        if plan.journal.is_none()
            && store.session_git_mode_blocking(project.id, session)?
                != Some(crate::agent::AgentGitMode::Off)
        {
            continue;
        }
        plan.writer_conflict = Some((previous.id, token));
        return execute_git_recovery(store, &plan, false).map(Some);
    }
    Ok(None)
}

/// Retire terminal, unbound attempts whose launch checkpoint cannot activate:
/// HEAD advanced, or the selected task was absent from the checkpoint. The
/// normal launch path checkpoints the board and captures a fresh boundary.
pub(crate) fn recover_failed_activation_automatically(
    store: &TursoAgentStore,
    project: &AgentProject,
) -> Result<Option<String>> {
    if !project.enabled {
        return Ok(None);
    }
    for journal in store.list_pending_git_finalizations_blocking(Some(project.id))? {
        if journal.state != GitFinalizationState::Working
            || journal.task_identity.is_some()
            || journal.commit_oid.is_some()
        {
            continue;
        }
        let session = &journal.codex_session_id;
        // A terminal worker can lack its session ID when observation failed.
        // Select the newest exact-session or exact-owner run, never a later
        // unrelated supervisor assessment or an obsolete owner result.
        let owner_run = journal
            .owner_run_token
            .as_deref()
            .map(|token| store.run_for_worker_token_blocking(project.id, token))
            .transpose()?
            .flatten();
        let session_run = store.latest_run_for_codex_session_blocking(project.id, session)?;
        let Some(run) = owner_run
            .into_iter()
            .chain(session_run)
            .max_by_key(|run| run.id)
        else {
            continue;
        };
        if !matches!(run.status.as_str(), "failure" | "blocked" | "timeout")
            || run.finished_at.is_none()
            || journal.starting_head.is_none()
            || journal.branch_ref.is_none()
            || current_agent_git_branch(&project.path)? != journal.branch_ref
        {
            continue;
        }
        let control = store.session_control_blocking(project.id, session)?;
        if control.as_ref().is_some_and(|control| {
            control.state != AgentSessionControlState::ResumeRequested
                || control.child_pid.is_some()
                || control.interactive_holder.is_some()
                || control.interactive_launch_token.is_some()
                || control.run_token != journal.owner_run_token
        }) {
            continue;
        }
        let mut plan = plan_task_restart(store, project, session)?;
        let Some(linked) = plan.linked.as_ref() else {
            continue;
        };
        if plan.journal.as_ref() != Some(&journal)
            || !linked.status.is_active()
            || task_entry_is_stopped(&linked.task)
            || task_content_is_manual(&linked.task.content)
        {
            continue;
        }
        let head = current_agent_git_head(&project.path)?;
        let head_changed = journal.starting_head.as_deref() != Some(head.as_str());
        if !head_changed {
            let Some(identity) = durable_task_identity(&linked.task.content) else {
                continue;
            };
            // Prove the missing task from Git, not one exact wording of the
            // worker's error. Only clear a blocked note about this startup
            // failure; a later dependency or input blocker remains authoritative.
            if git_ref_active_task_identity_count(&project.path, &head, &identity)? != 0
                || task_blocking_note(&linked.task.content).is_some_and(|note| {
                    !note.contains("committed exactly once") || !note.contains("(found 0)")
                })
            {
                continue;
            }
        }
        plan.run_id = run.id;
        plan.preactivation = !head_changed;
        plan.stale_head = Some(head);
        let mut tasks = Vec::new();
        linked_tasks(&get_tasks_dir(&project.path), &mut tasks)?;
        if !restart_preserves_other_git_work(store, project.id, session, &tasks)? {
            // This is a normal wait, not a project error. Let the scheduler
            // reach another task's commit verification/publication first.
            continue;
        }
        return execute_git_recovery(store, &plan, false).map(Some);
    }
    Ok(None)
}

/// Explicit recovery or scheduler-owned branch recovery retires the old attempt.
/// Only missing-journal recovery accepts Done; a retired sealed attempt needs
/// fresh verification even if its provisional board move already reached Done.
pub(crate) fn recover_git_task(store: &TursoAgentStore, plan: &GitRecoveryPlan) -> Result<String> {
    execute_git_recovery(store, plan, false)
}

pub(crate) fn recover_changed_branch_automatically(
    store: &TursoAgentStore,
    project: &AgentProject,
    session: &str,
) -> Result<Option<String>> {
    if !store
        .session_control_blocking(project.id, session)?
        .is_some_and(|control| control.state == AgentSessionControlState::ResumeRequested)
    {
        return Ok(None);
    }
    let plan = plan_git_recovery(store, project, Some(session))?;
    if plan.journal.is_none()
        || plan
            .linked
            .as_ref()
            .is_some_and(|linked| task_entry_is_stopped(&linked.task))
    {
        return Ok(None);
    }
    execute_git_recovery(store, &plan, true).map(Some)
}

fn execute_git_recovery(
    store: &TursoAgentStore,
    plan: &GitRecoveryPlan,
    require_resume_requested: bool,
) -> Result<String> {
    anyhow::ensure!(
        automated_agent_child_context()?.is_none(),
        "Git task recovery must run outside an automated CLT task"
    );
    let project_board = get_tasks_dir(&plan.project.path);
    let _lock = acquire_board_mutation_lock(&project_board)?;
    anyhow::ensure!(
        !board_has_manual_session(&project_board, &plan.session_id)?,
        "This task is manually owned; finish or hand it off before recovery"
    );
    let mut linked_tasks_now = Vec::new();
    linked_tasks(&project_board, &mut linked_tasks_now)?;
    let link_count = linked_tasks_now
        .iter()
        .filter(|(_, _, task)| {
            recoverable_codex_session_id_from_task_content(&task.content)
                == Some(plan.session_id.as_str())
        })
        .count();
    anyhow::ensure!(
        link_count == usize::from(plan.linked.is_some()),
        "The task's conversation links changed; review recovery again"
    );
    let selected = plan.linked.as_ref().map(|linked| -> Result<_> {
        let board = TaskBoard::new(&linked.board_dir);
        let index = board.entries(linked.status)?.iter().position(|task| {
            task.source == linked.task.source && task.content == linked.task.content
        }).context("The task changed while recovery was being confirmed; press r to review it again")?;
        Ok((linked, board, index))
    }).transpose()?;
    if plan.restart {
        anyhow::ensure!(
            restart_preserves_other_git_work(
                store,
                plan.project.id,
                &plan.session_id,
                &linked_tasks_now
            )?,
            "Another unfinished task owns this project; return its dependency wait to Todo before restarting the prerequisite"
        );
    }
    let holder = InteractiveAgentLease::holder_for_current_process();
    if plan.journal.is_some() || plan.restart {
        anyhow::ensure!(
            current_agent_git_branch(&plan.project.path)? == plan.current_branch,
            "The checkout branch changed while recovery was being confirmed; review recovery again"
        );
    }
    if let Some(head) = &plan.stale_head {
        anyhow::ensure!(
            current_agent_git_head(&plan.project.path)? == *head,
            "The checkout commit changed during activation recovery; retry on the next scan"
        );
    }
    if let Some((previous_id, _)) = &plan.writer_conflict {
        let latest = store
            .latest_run_for_codex_session_blocking(plan.project.id, &plan.session_id)?
            .context("Writer conflict run disappeared")?;
        let previous = store
            .previous_run_for_codex_session_blocking(plan.project.id, &plan.session_id, latest.id)?
            .context("Previous writer conflict run disappeared")?;
        anyhow::ensure!(
            latest.id == plan.run_id
                && previous.id == *previous_id
                && writer_conflict_at_startup(&latest, &plan.session_id)
                && writer_conflict_at_startup(&previous, &plan.session_id),
            "Writer conflict evidence changed; preserving the old attempt"
        );
    }
    store.begin_missing_git_recovery_blocking(
        plan.project.id,
        &plan.project.path,
        &plan.session_id,
        plan.run_id,
        plan.journal.as_ref(),
        require_resume_requested,
        plan.restart,
        plan.stale_head.is_some(),
        plan.writer_conflict
            .as_ref()
            .map(|(run, token)| (*run, token.as_str())),
        &holder,
        &agent_timestamp(),
        &agent_timestamp_after(60),
    )?;
    let result = (|| -> Result<String> {
        if let Some((linked, board, index)) = &selected
            && (linked.status != TaskStatus::Done || plan.journal.is_some() || plan.restart)
        {
            // Keep the stopped old session attached through the move. A crash
            // before the final write leaves an explicitly stopped, retryable task.
            let stopped = format!(
                "{} {TASK_STOPPED_MARKER}",
                task_content_without_stop_marker(&linked.task.content)
            );
            board.write_entry_content(linked.status, &linked.task, &stopped)?;
            if linked.status != TaskStatus::Todo {
                move_task_without_reordering_after_lock(
                    &linked.board_dir,
                    linked.status,
                    TaskStatus::Todo,
                    index + 1,
                )?;
            }
            let mut todo = board
                .entries(TaskStatus::Todo)?
                .into_iter()
                .find(|task| {
                    recoverable_codex_session_id_from_task_content(&task.content)
                        == Some(plan.session_id.as_str())
                })
                .context("The recovered task disappeared before it could be queued")?;
            if plan.restart
                && !plan.preactivation
                && plan.stale_head.is_none()
                && plan.writer_conflict.is_none()
            {
                prioritize_task_after_lock(&linked.board_dir, TaskStatus::Todo, &todo)?;
                todo = board
                    .entries(TaskStatus::Todo)?
                    .into_iter()
                    .next()
                    .context("The restarted task disappeared before activation")?;
                anyhow::ensure!(
                    recoverable_codex_session_id_from_task_content(&todo.content)
                        == Some(plan.session_id.as_str()),
                    "Restarted task order changed"
                );
            }
            let original = task_content_without_recoverable_codex_session(
                task_content_without_stop_marker(&todo.content),
            );
            // Do not use a codex: marker for the previous conversation: it is
            // history, and the next attempt must get its own session and journal.
            let reason = if plan.writer_conflict.is_some() {
                "two consecutive resumes failed at startup because the old conversation already has an active writer; the idle attempt was retired automatically".to_string()
            } else if plan.preactivation {
                "the selected Todo was absent from the old launch checkpoint and its worker failed before activation; the unbound attempt was retired automatically".to_string()
            } else if plan.stale_head.is_some() {
                "HEAD changed before task activation and the worker ended; the stale unbound attempt was retired automatically".to_string()
            } else if plan.restart {
                "the owner explicitly restarted this unfinished task; the old attempt remains in history".to_string()
            } else if let Some(journal) = &plan.journal {
                format!(
                    "the checkout branch changed from {} to {}; the previous Git attempt was retired",
                    journal.branch_ref.as_deref().unwrap_or("detached HEAD"),
                    plan.current_branch.as_deref().unwrap_or("detached HEAD"),
                )
            } else {
                "the previous run lost its Git starting record".to_string()
            };
            let content = format!(
                "{original}\n\nUNBLOCKED {}: CLT recovered after {reason}. Previous Codex session: {}. Review current files and Git history first; preserve existing work and commits, verify what is already complete, and implement only what remains. This is a fresh attempt; follow the project's current Git settings. Do not recreate or duplicate earlier commits.",
                Local::now().format("%Y-%m-%d"),
                plan.session_id,
            );
            board.write_entry_content(TaskStatus::Todo, &todo, &content)?;
        }
        store.finish_missing_git_recovery_blocking(plan.project.id, &holder)
            .context("Task recovery was published, but clearing its old failure state failed; check the task board before retrying")?;
        Ok(if plan.linked.is_none() {
            format!(
                "Retired orphaned Git attempt in {} and stopped its obsolete retry. The current task board, files, staging, commits and old journal were preserved.",
                plan.project.name
            )
        } else if plan
            .linked
            .as_ref()
            .is_some_and(|linked| linked.status == TaskStatus::Done)
            && plan.journal.is_none()
            && !plan.restart
        {
            format!(
                "Accepted completed task in {} and stopped its obsolete retry. Files and commits were preserved.",
                plan.project.name
            )
        } else {
            format!(
                "Recovered task in {}. A fresh Codex run will review existing work and finish what remains when project scheduling is active. Files, commits and the old conversation were preserved.",
                plan.project.name
            )
        })
    })();
    let release = store.release_lease_blocking(plan.project.id, &holder);
    match result {
        Ok(message) => {
            release?;
            Ok(message)
        }
        Err(error) => {
            let _ = release;
            Err(error)
        }
    }
}
