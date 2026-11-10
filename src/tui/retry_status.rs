//! Cached scheduler retry state. Rendering never queries the registry or clock.
use super::{TuiAgentProject, TuiAgentRuntimeState, TuiApp, task_has_stopped_agent_flag};
use crate::{
    agent::{self, AgentSessionControlRecord, AgentSessionControlState},
    application::{activity::WorkerActivityPhase, git_recovery::failure_has_git_recovery},
    task::{
        TaskEntry, TaskStatus, get_tasks_dir, recoverable_codex_session_id_from_task_content,
        task_entry_is_ready,
    },
};
use chrono::{DateTime, Local, Utc};
use std::time::Duration;

pub(crate) struct RetryStatus {
    deadline: u64,
    failed_at: u64,
    reason: String,
    daemon_active: bool,
    pub label: String,
    pub message: String,
}

impl RetryStatus {
    pub fn new(
        project: &agent::AgentProject,
        run: Option<&agent::AgentRunRecord>,
        backoff: Duration,
        now: u64,
        daemon_active: bool,
    ) -> Option<Self> {
        if project.failure_count <= 0 {
            return None;
        }
        let failed_at = project.last_failure_at.as_deref()?.parse::<u64>().ok()?;
        // An acknowledgement or a new running row must not erase the cooldown.
        // Equally, never attribute an older failure's cause to this retry.
        let reason = run
            .filter(|run| {
                matches!(run.status.as_str(), "failure" | "timeout")
                    && run.finished_at.as_deref().and_then(|at| at.parse().ok()) == Some(failed_at)
            })
            .and_then(|run| run.summary.as_deref())
            .map(|summary| {
                summary
                    .strip_prefix("Codex runner failed before completion: ")
                    .unwrap_or(summary)
            })
            .unwrap_or("The last attempt failed; its failure details are unavailable.")
            .to_string();
        let mut retry = Self {
            deadline: failed_at.saturating_add(backoff.as_secs()),
            failed_at,
            reason,
            daemon_active,
            label: String::new(),
            message: String::new(),
        };
        retry.tick(now, project.enabled);
        Some(retry)
    }

    pub fn tick(&mut self, now: u64, enabled: bool) {
        let remaining = self.deadline.saturating_sub(now);
        let guidance;
        self.label = if !enabled {
            guidance = "The project is paused. Enable it to allow automatic retry.";
            "Retry paused".into()
        } else if !self.daemon_active {
            guidance =
                "The scheduler is not checking in. Start or restore it to allow automatic retry.";
            "Retry paused".into()
        } else if remaining > 0 {
            guidance =
                "Automatic retry is scheduled. No action is needed while the countdown runs.";
            format!("Retry in {}:{:02}", remaining / 60, remaining % 60)
        } else {
            guidance =
                "Waiting for the scheduler to resume work when its ownership and task checks pass.";
            "Retry ready".into()
        };
        let failed_at = DateTime::<Utc>::from_timestamp(self.failed_at as i64, 0)
            .map(|at| {
                at.with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M:%S %Z")
                    .to_string()
            })
            .unwrap_or_else(|| "unknown time".into());
        self.message = format!(
            "{} — {guidance}\nLast failure: {failed_at}\n{}\nPress l for saved output; its header identifies that attempt.",
            self.label, self.reason
        );
    }
}

/// Apply only to work that can actually be retried. Live owners, explicit
/// stops, supervisor decisions and sealed Git work keep their own explanations.
pub(super) fn apply(
    project: &mut TuiAgentProject,
    controls: &[AgentSessionControlRecord],
    sealed: bool,
    work: bool,
) {
    let recovery_required = project
        .failure_problem
        .as_deref()
        .is_some_and(failure_has_git_recovery);
    let owned = controls.iter().any(|control| {
        control.child_pid.is_some()
            || control.interactive_holder.is_some()
            || control.interactive_launch_token.is_some()
    });
    if project.retry_status.is_some()
        && work
        && !owned
        && !sealed
        && !recovery_required
        && project.daemon_scan_problem.is_none()
        && project
            .supervisor_notice
            .as_ref()
            .is_none_or(|notice| notice.runtime_state == TuiAgentRuntimeState::RetryApproved)
        && matches!(
            project.runtime_state,
            TuiAgentRuntimeState::Idle
                | TuiAgentRuntimeState::Fenced
                | TuiAgentRuntimeState::Waiting
                | TuiAgentRuntimeState::Error
                | TuiAgentRuntimeState::RetryApproved
        )
    {
        project.runtime_state = TuiAgentRuntimeState::RetryScheduled;
    }
}

pub(super) fn has_retryable_tasks(
    project: &TuiAgentProject,
    controls: &[AgentSessionControlRecord],
) -> bool {
    [TaskStatus::Todo, TaskStatus::Doing]
        .into_iter()
        .any(|status| {
            if status == TaskStatus::Todo && project.scan.available_todo_count() == 0 {
                return false;
            }
            crate::task::read_task_entries(&get_tasks_dir(&project.project.path), status)
                .unwrap_or_default()
                .iter()
                .any(|entry| {
                    task_entry_is_ready(entry)
                        && recoverable_codex_session_id_from_task_content(&entry.content)
                            .and_then(|session| {
                                controls
                                    .iter()
                                    .find(|control| control.codex_session_id == session)
                            })
                            .is_none_or(|control| {
                                !matches!(
                                    control.state,
                                    AgentSessionControlState::Stopped
                                        | AgentSessionControlState::ReadyInteractive
                                        | AgentSessionControlState::Interactive
                                )
                            })
                })
        })
}

pub(super) fn task_badge(app: &TuiApp, status: TaskStatus, entry: &TaskEntry) -> Option<String> {
    if !status.is_active()
        || crate::task::task_content_is_manual(&entry.content)
        || !task_entry_is_ready(entry)
        || task_has_stopped_agent_flag(status, entry, &app.task_agent_session_states)
    {
        return None;
    }
    if app
        .coordination
        .tasks
        .iter()
        .any(|task| task.entry.source == entry.source && task.entry.content == entry.content)
        && !app.coordination.ready(entry)
    {
        return None;
    }
    let session = recoverable_codex_session_id_from_task_content(&entry.content);
    let state = session.and_then(|id| app.task_agent_session_states.get(id));
    if matches!(
        state,
        Some(
            AgentSessionControlState::Running
                | AgentSessionControlState::StopRequested
                | AgentSessionControlState::Interactive
                | AgentSessionControlState::ReadyInteractive
                | AgentSessionControlState::InterruptRequested
        )
    ) {
        return None;
    }
    let project = app
        .agent_panel
        .projects
        .iter()
        .filter(|project| app.active_root.starts_with(&project.project.path))
        .max_by_key(|project| project.project.path.components().count())?;
    if let Some(retry) = project.displayed_retry() {
        return Some(format!("[{}] ", retry.label));
    }
    if let Some(activity) = &project.activity
        && session.is_some()
        && activity.session_id.as_deref() == session
        && activity.phase == WorkerActivityPhase::Resuming
    {
        return Some("[RESUMING] ".into());
    }
    None
}

#[cfg(test)]
mod tests;
