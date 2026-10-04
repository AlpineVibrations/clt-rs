use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Local;

use crate::{
    agent::{
        AGENT_BRANCH_GIT_RECOVERY_REASON, AGENT_MISSING_GIT_RECOVERY_TOKEN_PREFIX, AgentProject,
        AgentSessionControlState, GitFinalizationRecord, GitFinalizationState, TursoAgentStore,
    },
    managed_git::current_agent_git_branch,
    runner::{agent_timestamp, agent_timestamp_after, automated_agent_child_context},
    session_control::InteractiveAgentLease,
    task::{
        TASK_STATUSES, TASK_STOPPED_MARKER, TaskBoard, TaskEntry, TaskSource, TaskStatus,
        acquire_board_mutation_lock, board_has_manual_task, get_tasks_dir,
        move_task_without_reordering_after_lock, read_task_entries,
        recoverable_codex_session_id_from_task_content,
        task_content_without_recoverable_codex_session, task_content_without_stop_marker,
        task_display_text, task_entry_is_stopped,
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
    })
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
        !board_has_manual_task(&project_board)?,
        "A manually owned task reserves this project; finish or hand it off before recovery"
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
    let holder = InteractiveAgentLease::holder_for_current_process();
    if plan.journal.is_some() {
        anyhow::ensure!(
            current_agent_git_branch(&plan.project.path)? == plan.current_branch,
            "The checkout branch changed while recovery was being confirmed; review recovery again"
        );
    }
    store.begin_missing_git_recovery_blocking(
        plan.project.id,
        &plan.project.path,
        &plan.session_id,
        plan.run_id,
        plan.journal.as_ref(),
        require_resume_requested,
        &holder,
        &agent_timestamp(),
        &agent_timestamp_after(60),
    )?;
    let result = (|| -> Result<String> {
        if let Some((linked, board, index)) = &selected
            && (linked.status != TaskStatus::Done || plan.journal.is_some())
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
            let todo = board
                .entries(TaskStatus::Todo)?
                .into_iter()
                .find(|task| {
                    recoverable_codex_session_id_from_task_content(&task.content)
                        == Some(plan.session_id.as_str())
                })
                .context("The recovered task disappeared before it could be queued")?;
            let original = task_content_without_recoverable_codex_session(
                task_content_without_stop_marker(&todo.content),
            );
            // Do not use a codex: marker for the previous conversation: it is
            // history, and the next attempt must get its own session and journal.
            let reason = if let Some(journal) = &plan.journal {
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
