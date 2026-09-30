use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Local;

use crate::{
    agent::{
        AGENT_MISSING_GIT_RECOVERY_TOKEN_PREFIX, AgentProject, AgentSessionControlState,
        TursoAgentStore,
    },
    runner::{agent_timestamp, agent_timestamp_after, automated_agent_child_context},
    session_control::InteractiveAgentLease,
    task::{
        TASK_STATUSES, TASK_STOPPED_MARKER, TaskBoard, TaskEntry, TaskSource, TaskStatus,
        acquire_board_mutation_lock, board_has_manual_task, get_tasks_dir,
        move_task_without_reordering_after_lock, read_task_entries,
        recoverable_codex_session_id_from_task_content,
        task_content_without_recoverable_codex_session, task_content_without_stop_marker,
        task_display_text,
    },
};

#[cfg(test)]
mod tests;

pub(crate) fn failure_has_missing_git_start(summary: &str) -> bool {
    // Stored run diagnostics from older versions remain recoverable.
    summary.contains("no frozen Git start journal") || summary.contains("no frozen start journal")
}

#[derive(Clone)]
pub(crate) struct GitRecoveryPlan {
    pub(crate) project: AgentProject,
    pub(crate) session_id: String,
    pub(crate) task: TaskEntry,
    pub(crate) status: TaskStatus,
    board_dir: PathBuf,
    run_id: i64,
}

impl GitRecoveryPlan {
    pub(crate) fn prompt(&self) -> String {
        let title: String = task_display_text(&self.task).chars().take(160).collect();
        if self.status == TaskStatus::Done {
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
        .filter(|run| {
            matches!(run.status.as_str(), "failure" | "timeout")
                && run
                    .summary
                    .as_deref()
                    .is_some_and(failure_has_missing_git_start)
        })
        .context("This project has no missing Git recovery record to recover")?;
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
        if selected
            && store
                .git_finalization_blocking(project.id, session)?
                .is_none()
        {
            candidates.push((board, status, task, session));
        }
    }
    anyhow::ensure!(
        candidates.len() == 1,
        "Cannot identify one affected task. Use clt agent recover-task --session <session-id> for the intended task; its saved error is available with l"
    );
    let (board, status, task, session) = candidates.remove(0);
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
        task: task.clone(),
        status: *status,
        board_dir: board.clone(),
        run_id: run.id,
    })
}

/// Explicit user acceptance starts a *new* attempt, preserving the old run's
/// evidence. A completed task needs only its obsolete resume request stopped.
pub(crate) fn recover_git_task(store: &TursoAgentStore, plan: &GitRecoveryPlan) -> Result<String> {
    anyhow::ensure!(
        automated_agent_child_context()?.is_none(),
        "Missing-journal recovery requires an explicit user action outside an automated CLT run"
    );
    let project_board = get_tasks_dir(&plan.project.path);
    let _lock = acquire_board_mutation_lock(&project_board)?;
    anyhow::ensure!(
        !board_has_manual_task(&project_board)?,
        "A manually owned task reserves this project; finish or hand it off before recovery"
    );
    let board = TaskBoard::new(&plan.board_dir);
    let entries = board.entries(plan.status)?;
    let index = entries
        .iter()
        .position(|task| task.source == plan.task.source && task.content == plan.task.content)
        .context(
            "The task changed while recovery was being confirmed; press r to review it again",
        )?;
    let mut linked = Vec::new();
    linked_tasks(&project_board, &mut linked)?;
    anyhow::ensure!(
        linked
            .iter()
            .filter(
                |(_, _, task)| recoverable_codex_session_id_from_task_content(&task.content)
                    == Some(plan.session_id.as_str())
            )
            .count()
            == 1,
        "The task's conversation links changed; review recovery again"
    );
    let holder = InteractiveAgentLease::holder_for_current_process();
    store.begin_missing_git_recovery_blocking(
        plan.project.id,
        &plan.project.path,
        &plan.session_id,
        plan.run_id,
        &holder,
        &agent_timestamp(),
        &agent_timestamp_after(60),
    )?;
    let result = (|| -> Result<String> {
        if plan.status != TaskStatus::Done {
            // Keep the stopped old session attached through the move. A crash
            // before the final write leaves an explicitly stopped, retryable task.
            let stopped = format!(
                "{} {TASK_STOPPED_MARKER}",
                task_content_without_stop_marker(&plan.task.content)
            );
            board.write_entry_content(plan.status, &plan.task, &stopped)?;
            if plan.status != TaskStatus::Todo {
                move_task_without_reordering_after_lock(
                    &plan.board_dir,
                    plan.status,
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
            let content = format!(
                "{original}\n\nUNBLOCKED {}: User requested recovery after the previous run lost its Git starting record. Previous Codex session: {}. Review current files and Git history first; preserve existing work and commits, verify what is already complete, and implement only what remains. This is a fresh attempt; follow the project's current Git settings. Do not recreate or duplicate earlier commits.",
                Local::now().format("%Y-%m-%d"),
                plan.session_id,
            );
            board.write_entry_content(TaskStatus::Todo, &todo, &content)?;
        }
        store.finish_missing_git_recovery_blocking(plan.project.id, &holder)
            .context("Task recovery was published, but clearing its old failure state failed; check the task board before retrying")?;
        Ok(if plan.status == TaskStatus::Done {
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
