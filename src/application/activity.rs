//! Read-only worker activity shared by the CLI, TUI and ownership diagnostics.
use crate::{
    agent::{AgentSessionControlRecord, AgentWorkerRecord},
    task::{
        TaskSource, TaskStatus, get_tasks_dir, read_task_entries,
        recoverable_codex_session_id_from_task_content, task_display_text,
    },
};
use anyhow::Result;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkerActivityPhase {
    Starting,
    Running,
    Resuming,
    Reviewing,
    Settling,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkerActivity {
    pub phase: WorkerActivityPhase,
    pub summary: String,
    pub session_id: Option<String>,
}

pub(crate) fn describe_worker_activity(
    worker: &AgentWorkerRecord,
    controls: &[AgentSessionControlRecord],
) -> WorkerActivity {
    let sessions: Vec<_> = controls
        .iter()
        .filter(|control| {
            control.project_id == worker.project_id
                && control.run_token.as_deref() == Some(worker.worker_token.as_str())
        })
        .map(|control| control.codex_session_id.as_str())
        .collect();
    let (session, ambiguous) = match sessions.as_slice() {
        [] => (worker.resume_session_id.as_deref(), false),
        [session]
            if worker
                .resume_session_id
                .as_deref()
                .is_none_or(|id| id == *session) =>
        {
            (Some(*session), false)
        }
        _ => (None, true),
    };
    let review = worker.task_selection == "supervisor_review";
    let phase = if worker.state == "finalizing" {
        WorkerActivityPhase::Settling
    } else if worker.state == "dispatching" {
        WorkerActivityPhase::Starting
    } else if review {
        WorkerActivityPhase::Reviewing
    } else if session.is_none() {
        WorkerActivityPhase::Starting
    } else if worker.task_selection == "next_todo" {
        WorkerActivityPhase::Running
    } else {
        WorkerActivityPhase::Resuming
    };
    let subject = if review {
        "supervisor review of blocked tasks".to_string()
    } else if ambiguous {
        "task ownership is ambiguous; inspect agent status".to_string()
    } else if let Some(session) = session {
        let board = get_tasks_dir(&worker.project_path);
        let mut matches = Vec::new();
        match collect_session_task_labels(&board, &board, session, &mut matches) {
            Ok(()) if matches.len() == 1 => matches.remove(0),
            Ok(()) if matches.is_empty() => {
                format!("session {session} (waiting for its task link)")
            }
            Ok(()) => format!("session {session} (multiple task links; inspect the board)"),
            Err(_) => format!("session {session} (task board unavailable)"),
        }
    } else {
        "waiting for task ownership to be registered".to_string()
    };
    let verb = match phase {
        WorkerActivityPhase::Starting => "Starting",
        WorkerActivityPhase::Running => "Working on",
        WorkerActivityPhase::Resuming => "Resuming",
        WorkerActivityPhase::Reviewing => "Running",
        WorkerActivityPhase::Settling => "Finishing worker cleanup for",
    };
    WorkerActivity {
        phase,
        summary: format!("{verb}: {subject}"),
        session_id: session.map(str::to_string),
    }
}

fn collect_session_task_labels(
    root_board: &Path,
    board: &Path,
    session: &str,
    matches: &mut Vec<String>,
) -> Result<()> {
    for status in TaskStatus::SESSION_SEARCH_ORDER {
        for (index, task) in read_task_entries(board, status)?.into_iter().enumerate() {
            if recoverable_codex_session_id_from_task_content(&task.content) == Some(session) {
                let nested = board.strip_prefix(root_board).unwrap_or(board);
                let prefix = if nested.as_os_str().is_empty() {
                    String::new()
                } else {
                    format!("{} / ", nested.display())
                };
                matches.push(format!(
                    "{prefix}{} #{} — {}",
                    status.as_str().to_uppercase(),
                    index + 1,
                    task_display_text(&task)
                ));
            }
            if task.has_subtasks
                && let TaskSource::Path { path, is_dir: true } = &task.source
            {
                collect_session_task_labels(root_board, path, session, matches)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
