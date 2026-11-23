use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use ratatui::style::Color;

use super::{
    GitRecoveryPlan, Mode, TuiAgentProject, TuiAgentRuntimeState, TuiApp, TuiPane,
    plan_git_recovery,
};
use crate::agent::{agent_state_dir, open_agent_store_at};

pub(super) struct Retry {
    project_id: i64,
    name: String,
    message: String,
    color: Color,
    result: Option<Receiver<Result<Option<GitRecoveryPlan>>>>,
    finished: Option<Instant>,
}

impl TuiApp {
    pub(super) fn console_project(&self) -> Option<&TuiAgentProject> {
        match self.current_pane {
            TuiPane::AgentProjects if !self.agent_panel.supervisor_selected => {
                self.agent_panel.selected_project()
            }
            TuiPane::Tasks if self.current_mode == Mode::View && !self.archive_view => self
                .agent_panel
                .projects
                .iter()
                .filter(|project| self.active_root.starts_with(&project.project.path))
                .max_by_key(|project| project.project.path.components().count()),
            _ => None,
        }
    }

    pub(super) fn retry_feedback(&self) -> Option<(&str, Color)> {
        let project_id = self.console_project()?.project.id;
        self.retries
            .iter()
            .find(|retry| retry.project_id == project_id)
            .filter(|_| self.agent_log_view.is_none())
            .map(|retry| (retry.message.as_str(), retry.color))
    }

    pub(super) fn retry_pending(&self) -> bool {
        self.retries.iter().any(|retry| retry.result.is_some())
    }

    pub(super) fn request_project_retry(&mut self) {
        let Some(project) = self.console_project() else {
            self.feedback_buffer = self
                .agent_panel
                .last_error
                .clone()
                .unwrap_or_else(|| "No registered project is available to retry.".into());
            return;
        };
        let recover = project
            .failure_problem
            .as_deref()
            .is_some_and(|problem| problem.starts_with("Git recovery available"));
        let project = project.project.clone();
        let state_dir = agent_state_dir().and_then(|dir| Ok(std::path::absolute(dir)?));
        self.start_project_retry(project.id, project.name.clone(), move || {
            let store = open_agent_store_at(&state_dir?)?;
            if recover {
                return plan_git_recovery(&store, &project, None).map(Some);
            }
            anyhow::ensure!(
                store.retry_registered_project_blocking(&project.path, Some(project.id))?,
                "The selected project is no longer registered"
            );
            Ok(None)
        });
    }

    pub(super) fn start_project_retry(
        &mut self,
        id: i64,
        name: String,
        retry: impl FnOnce() -> Result<Option<GitRecoveryPlan>> + Send + 'static,
    ) {
        self.agent_log_view = None;
        if self
            .retries
            .iter()
            .any(|r| r.project_id == id && r.result.is_some())
        {
            return;
        }
        self.retries.retain(|r| r.project_id != id);
        let (sender, result) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(retry());
        });
        self.retries.push(Retry {
            project_id: id,
            message: format!("Requesting retry for {name}…"),
            name,
            color: Color::LightYellow,
            result: Some(result),
            finished: None,
        });
    }

    pub(super) fn poll_project_retries(&mut self) -> bool {
        self.retries.retain(|r| {
            r.finished
                .is_none_or(|when| when.elapsed() < Duration::from_secs(8))
        });
        let mut completed = false;
        let selected = self.console_project().map(|entry| entry.project.id);
        for retry in &mut self.retries {
            let Some(result) = &retry.result else {
                continue;
            };
            let result = match result.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => {
                    Err(anyhow::anyhow!("Retry worker disconnected"))
                }
            };
            retry.result = None;
            retry.finished = Some(Instant::now());
            if matches!(result, Ok(None))
                && let Some(project) = self
                    .agent_panel
                    .projects
                    .iter_mut()
                    .find(|entry| entry.project.id == retry.project_id)
            {
                project.project.failure_count = 0;
                project.retry_status = None;
                if project.runtime_state == TuiAgentRuntimeState::RetryScheduled {
                    project.runtime_state = TuiAgentRuntimeState::Idle;
                }
            }
            (retry.message, retry.color) = match result {
                Ok(None) => (
                    format!(
                        "Retry requested for {}. The scheduler can pick it up on its next eligible pass.",
                        retry.name
                    ),
                    Color::Gray,
                ),
                Ok(Some(plan)) => {
                    if selected == Some(retry.project_id)
                        && self.current_mode == Mode::View
                        && self.pending_git_recovery.is_none()
                        && self.agent_log_view.is_none()
                        && self.task_summary.is_none()
                        && self.task_details.is_none()
                        && self.supervisor_details.is_none()
                        && self.project_removal.is_none()
                    {
                        self.pending_git_recovery = Some(plan);
                    }
                    (
                        format!(
                            "{} requires Git recovery. Press r on the project to review it.",
                            retry.name
                        ),
                        Color::LightYellow,
                    )
                }
                Err(error) => (
                    format!("Unable to retry {}. {error:#}", retry.name),
                    Color::LightRed,
                ),
            };
            if retry.color == Color::LightRed {
                self.agent_settings.quit_requested = false;
            }
            completed = true;
        }
        if completed {
            self.agent_settings.revision += 1;
        }
        completed
    }
}
