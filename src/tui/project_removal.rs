use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    Frame,
    style::Color,
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::{
    TuiAgentPanel, TuiApp, TuiEffect, TuiPane, current_project_registration,
    initialization_prompt_choice, wrap_input_text,
};
use crate::agent::{agent_state_dir, open_agent_store_at};

pub(super) struct Target {
    pub(super) path: PathBuf,
    pub(super) name: String,
}

impl Target {
    pub(super) fn selected(panel: &TuiAgentPanel) -> Option<Self> {
        let project = &panel.selected_project()?.project;
        Some(Self {
            path: project.path.clone(),
            name: project.name.clone(),
        })
    }

    pub(super) fn render_confirmation(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let prompt = format!(
            "Press y to confirm. Press n or Esc to cancel.\n\nRemove '{}' from Agent Projects and clear its saved agent state?",
            self.name
        );
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(wrap_input_text(
                &prompt,
                area.width.saturating_sub(2).max(1) as usize,
            ))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Remove agent project "),
            ),
            area,
        );
    }
}

pub(super) enum Removal {
    Confirm(Target),
    Running {
        target: Target,
        message: String,
        result: Receiver<Result<bool>>,
    },
    Finished(String, Color),
}

impl Removal {
    pub(super) fn feedback(&self) -> Option<(&str, Color)> {
        match self {
            Self::Confirm(_) => None,
            Self::Running { message, .. } => Some((message, Color::LightYellow)),
            Self::Finished(message, color) => Some((message, *color)),
        }
    }
}

impl TuiApp {
    pub(super) fn agent_panel_write_pending(&self) -> bool {
        self.agent_settings.is_pending()
            || self.retry_pending()
            || matches!(self.project_removal, Some(Removal::Running { .. }))
    }

    pub(super) fn update_project_removal(&mut self, key: KeyEvent) -> Option<Vec<TuiEffect>> {
        if let Some(Removal::Confirm(target)) = &self.project_removal {
            if key.kind == KeyEventKind::Press {
                match initialization_prompt_choice(&key) {
                    Some(true) => return Some(vec![TuiEffect::RemoveAgentProject]),
                    choice if choice == Some(false) || key.code == KeyCode::Esc => {
                        self.project_removal = Some(Removal::Finished(
                            format!("Kept agent project: {}", target.name),
                            Color::Gray,
                        ));
                    }
                    None | Some(false) => {}
                }
            }
            return Some(Vec::new());
        }

        if key.kind == KeyEventKind::Press
            && matches!(self.project_removal, Some(Removal::Finished(..)))
        {
            self.project_removal = None;
        }
        if self.current_pane != TuiPane::AgentProjects
            || self.agent_panel.supervisor_selected
            || key.code != KeyCode::Delete
        {
            return None;
        }
        if key.kind == KeyEventKind::Press && self.project_removal.is_none() {
            if let Some(target) = Target::selected(&self.agent_panel) {
                self.agent_log_view = None;
                self.project_removal = Some(Removal::Confirm(target));
            } else {
                self.feedback_buffer = "No registered project selected to remove".into();
            }
        }
        Some(Vec::new())
    }

    pub(super) fn confirm_project_removal(&mut self) {
        // A project switch can change the working directory while removal runs.
        let state_dir = agent_state_dir().and_then(|path| Ok(std::path::absolute(path)?));
        self.start_project_removal(move |path| {
            let state_dir = state_dir?;
            let store = open_agent_store_at(&state_dir)?;
            #[cfg(not(test))]
            crate::worker::cleanup_terminal_agent_worker_services(&state_dir, &store, Some(path))?;
            store.unregister_project_blocking(path)
        });
    }

    pub(super) fn start_project_removal(
        &mut self,
        unregister: impl FnOnce(&Path) -> Result<bool> + Send + 'static,
    ) {
        let Some(Removal::Confirm(target)) = self.project_removal.take() else {
            unreachable!("project removal requires confirmation");
        };
        let (sender, result) = mpsc::channel();
        let path = target.path.clone();
        thread::spawn(move || {
            let _ = sender.send(unregister(&path));
        });
        let message = format!("Removing agent project '{}'…", target.name);
        self.project_removal = Some(Removal::Running {
            target,
            message,
            result,
        });
    }

    pub(super) fn poll_project_removal(&mut self) -> bool {
        let Some(Removal::Running { target, result, .. }) = &self.project_removal else {
            return false;
        };
        let result = match result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("Removal worker disconnected")),
        };

        // Reject any snapshot begun before removal completed, including one
        // that still contains the unregistered project.
        self.agent_settings.revision += 1;
        self.project_removal = Some(match result {
            Ok(removed) => Removal::Finished(
                apply_unregistration(&mut self.agent_panel, &self.active_root, target, removed),
                Color::Gray,
            ),
            Err(error) => {
                self.agent_settings.quit_requested = false;
                Removal::Finished(
                    format!("Unable to remove '{}': {error:#}", target.name),
                    Color::LightRed,
                )
            }
        });
        true
    }
}

pub(super) fn apply_unregistration(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
    target: &Target,
    removed: bool,
) -> String {
    let selected = panel.selected_row_identity();
    let selected_idx = panel.state.selected().unwrap_or(0);
    let removed_selection = panel
        .selected_project()
        .is_some_and(|entry| entry.project.path == target.path);
    panel
        .projects
        .retain(|entry| entry.project.path != target.path);
    panel.current_project_registration = current_project_registration(active_root, &panel.projects);
    panel.last_error = None;
    if removed_selection {
        panel.select_nearest_row(selected_idx);
    } else {
        panel.restore_or_normalize_selection(selected);
    }
    if removed {
        format!("Removed agent project: {}", target.name)
    } else {
        format!("Project is no longer registered: {}", target.path.display())
    }
}
