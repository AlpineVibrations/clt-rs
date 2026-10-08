//! Completed conversations leave the board unchanged. Legacy interactive-done
//! markers are restored after their exact guardian exits or is proven absent.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{
    agent::{AgentSessionControlState, TursoAgentStore},
    application::ensure_status_conversion_allowed,
    task::{
        TaskBoard, TaskEntry, TaskSource, TaskStatus, convert_status_to_directory,
        read_task_entries, recoverable_codex_session_id_from_task_content,
        task_content_is_interactive_done, task_content_without_interactive_done_marker,
        task_content_without_manual_marker,
    },
};

use super::lock_project_board;

type LinkedTask = (PathBuf, TaskStatus, usize, TaskEntry);

fn collect_linked_tasks(
    board: &Path,
    session: Option<&str>,
    tasks: &mut Vec<LinkedTask>,
) -> Result<()> {
    for status in TaskStatus::SESSION_SEARCH_ORDER {
        for (index, task) in read_task_entries(board, status)?.into_iter().enumerate() {
            let linked = recoverable_codex_session_id_from_task_content(&task.content);
            if match session {
                Some(session) => linked == Some(session),
                None => linked.is_some() && task_content_is_interactive_done(&task.content),
            } {
                tasks.push((board.to_path_buf(), status, index + 1, task.clone()));
            }
            if task.has_subtasks
                && let TaskSource::Path { path, is_dir: true } = &task.source
            {
                collect_linked_tasks(path, session, tasks)?;
            }
        }
    }
    Ok(())
}

fn linked_task(board: &Path, session: &str, destination: TaskStatus) -> Result<Option<LinkedTask>> {
    let mut tasks = Vec::new();
    collect_linked_tasks(board, Some(session), &mut tasks)?;
    if !tasks.iter().any(|task| {
        task_content_is_interactive_done(&task.3.content)
            || destination == TaskStatus::Doing && task.1 == TaskStatus::Done
    }) {
        // Ordinary planning and unfinished work keep their own lifecycle.
        return Ok(None);
    }
    if tasks.len() == 2 {
        // A Markdown move publishes its destination before removing its source.
        // Repair only an exact marked copy left at that crash boundary.
        let (left, right) = (&tasks[0], &tasks[1]);
        if left.0 == right.0
            && left.1 != right.1
            && [left.1, right.1].contains(&TaskStatus::Doing)
            && [left.1, right.1].contains(&TaskStatus::Done)
            && left.3.content.trim_end() == right.3.content.trim_end()
            && task_content_is_interactive_done(&left.3.content)
        {
            let source = tasks.iter().position(|task| task.1 != destination).unwrap();
            let (board, status, _, task) = tasks.remove(source);
            TaskBoard::new(board).remove_entry_without_reordering(status, &task)?;
        }
    }
    anyhow::ensure!(
        tasks.len() <= 1,
        "This Codex session belongs to multiple tasks; resolve duplicate links before continuing"
    );
    Ok(tasks.pop())
}

pub(super) fn prepare_completed_task_context(
    store: &TursoAgentStore,
    project_id: i64,
    session: &str,
    guardian_holder: &str,
) -> Result<bool> {
    let (board, _lock) = lock_project_board(store, project_id)?;
    let control = store
        .session_control_blocking(project_id, session)?
        .context("Interactive session control disappeared before opening task context")?;
    anyhow::ensure!(
        control.state == AgentSessionControlState::Interactive
            && control.interactive_holder.as_deref() == Some(guardian_holder)
            && control.interactive_launch_token.as_deref() == Some(guardian_holder)
            && control.child_pid.is_none(),
        "Interactive session ownership changed before opening task context"
    );
    let mut tasks = Vec::new();
    collect_linked_tasks(&board, Some(session), &mut tasks)?;
    anyhow::ensure!(
        tasks.len() <= 1,
        "This Codex session belongs to multiple tasks; resolve duplicate links before continuing"
    );
    let Some((_, status, _, _)) = tasks.first() else {
        return Ok(false);
    };
    // Provisional Done still belongs to its unfinished run. Merely reading its
    // conversation neither changes that contract nor attempts Git finalization.
    Ok(*status == TaskStatus::Done)
}

fn prepare_destination(board: &Path, task: &TaskEntry, destination: TaskStatus) -> Result<()> {
    if matches!(task.source, TaskSource::Path { .. }) && !board.join(destination.as_str()).is_dir()
    {
        ensure_status_conversion_allowed(board, destination)?;
        convert_status_to_directory(board, destination)?;
    }
    Ok(())
}

pub(super) fn restore_completed_task_after_lock(board: &Path, session: &str) -> Result<()> {
    let Some((board, status, index, task)) = linked_task(board, session, TaskStatus::Done)? else {
        return Ok(());
    };
    if !task_content_is_interactive_done(&task.content) {
        return Ok(());
    }
    let task_board = TaskBoard::new(&board);
    if status == TaskStatus::Doing {
        prepare_destination(&board, &task, TaskStatus::Done)?;
        // Keep the marker through the move so a crash can retry restoration.
        task_board.move_task_without_reordering_after_lock(status, TaskStatus::Done, index)?;
    }
    let (_, status, _, task) = linked_task(&board, session, TaskStatus::Done)?
        .context("Reopened task disappeared while returning to Done")?;
    task_board.write_entry_content(
        status,
        &task,
        &task_content_without_manual_marker(&task_content_without_interactive_done_marker(
            &task.content,
        )),
    )
}

/// Call only after reaping the exact child, or proving its process group absent.
pub(super) fn restore_completed_task_for_guardian(
    store: &TursoAgentStore,
    project_id: i64,
    session: &str,
    guardian_holder: &str,
) -> Result<()> {
    let (board, _lock) = lock_project_board(store, project_id)?;
    let control = store.session_control_blocking(project_id, session)?;
    if control.is_some_and(|control| {
        matches!(
            control.state,
            AgentSessionControlState::Interactive | AgentSessionControlState::StopRequested
        ) && control.interactive_holder.as_deref() == Some(guardian_holder)
            && control.interactive_launch_token.as_deref() == Some(guardian_holder)
    }) {
        restore_completed_task_after_lock(&board, session)?;
    }
    Ok(())
}

pub(super) fn restore_idle_completed_tasks(store: &TursoAgentStore, project_id: i64) -> Result<()> {
    let (board, _lock) = lock_project_board(store, project_id)?;
    // Reconciliation also runs for paused/missing projects. Do not initialize
    // a replacement board merely to look for a legacy marker.
    if !crate::task::TASK_STATUSES
        .into_iter()
        .any(|status| crate::task::status_store_exists(&board, status))
    {
        return Ok(());
    }
    if store
        .list_active_workers_blocking()?
        .iter()
        .any(|worker| worker.project_id == project_id)
        || store
            .git_launch_state_for_project_blocking(project_id)?
            .is_some()
    {
        return Ok(());
    }
    let mut tasks = Vec::new();
    collect_linked_tasks(&board, None, &mut tasks)?;
    for (_, _, _, task) in tasks {
        let session = recoverable_codex_session_id_from_task_content(&task.content)
            .context("Reopened task lost its saved conversation")?;
        if store
            .session_control_blocking(project_id, session)?
            .is_none_or(|control| {
                matches!(
                    control.state,
                    AgentSessionControlState::Stopped | AgentSessionControlState::ResumeRequested
                ) && control.child_pid.is_none()
                    && control.interactive_holder.is_none()
                    && control.interactive_launch_token.is_none()
            })
            && store
                .git_finalization_blocking(project_id, session)?
                .is_none_or(|journal| {
                    journal.state.is_terminal()
                        || (journal.state == crate::agent::GitFinalizationState::Working
                            && journal.task_identity.is_none()
                            && journal.commit_oid.is_none())
                })
        {
            restore_completed_task_after_lock(&board, session)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
