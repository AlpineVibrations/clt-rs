//! Immediate setting previews with serialized, coalesced durable saves.
use std::{
    collections::VecDeque,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use crate::{
    agent::{AGENT_CODEX_REASONING_EFFORTS, AgentGitMode, TursoAgentStore, open_agent_store},
    supervisor::SupervisorSettings,
    tui::{
        TuiAgentPanel, TuiApp, edit_supervisor_settings, next_agent_codex_setting,
        tui_toggles_models,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Enabled(i64, bool),
    Git(i64, AgentGitMode),
    Codex {
        project_id: i64,
        provider: Option<String>,
        model: Option<String>,
        thinking: Option<String>,
        fast: bool,
    },
    Supervisor(SupervisorSettings),
}

impl Value {
    fn same_setting(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Enabled(a, _), Self::Enabled(b, _)) | (Self::Git(a, _), Self::Git(b, _)) => {
                a == b
            }
            (Self::Codex { project_id: a, .. }, Self::Codex { project_id: b, .. }) => a == b,
            (Self::Supervisor(_), Self::Supervisor(_)) => true,
            _ => false,
        }
    }

    fn apply(&self, panel: &mut TuiAgentPanel) {
        let project_id = match self {
            Self::Enabled(id, _) | Self::Git(id, _) | Self::Codex { project_id: id, .. } => *id,
            Self::Supervisor(settings) => {
                panel.supervisor_settings = settings.clone();
                return;
            }
        };
        let Some(entry) = panel
            .projects
            .iter_mut()
            .find(|entry| entry.project.id == project_id)
        else {
            return;
        };
        match self {
            Self::Enabled(_, enabled) => entry.project.enabled = *enabled,
            Self::Git(_, mode) => entry.project.git_mode = *mode,
            Self::Codex {
                provider,
                model,
                thinking,
                fast,
                ..
            } => {
                entry.project.codex_provider = provider.clone();
                entry.project.codex_model = model.clone();
                entry.project.codex_reasoning_effort = thinking.clone();
                entry.project.codex_fast_enabled = *fast;
            }
            Self::Supervisor(_) => unreachable!(),
        }
    }

    fn save(&self, store: &TursoAgentStore) -> Result<()> {
        let changed = match self {
            Self::Enabled(id, enabled) => store.set_project_enabled_blocking(*id, *enabled)?,
            Self::Git(id, mode) => store.set_project_git_mode_blocking(*id, *mode)?,
            Self::Codex {
                project_id,
                provider,
                model,
                thinking,
                fast,
            } => store.set_project_codex_settings_blocking(
                *project_id,
                provider.as_deref(),
                model.as_deref(),
                thinking.as_deref(),
                *fast,
            )?,
            Self::Supervisor(settings) => {
                store.set_supervisor_settings_blocking(settings)?;
                true
            }
        };
        anyhow::ensure!(changed, "Project is no longer registered");
        Ok(())
    }
}

struct Change {
    before: Value,
    after: Value,
    label: String,
}

struct Saving {
    change: Change,
    result: Receiver<Result<()>>,
}

#[derive(Default)]
pub(super) struct Settings {
    queued: VecDeque<Change>,
    saving: Option<Saving>,
    // Both edits and completions invalidate refreshes already in flight.
    pub(super) revision: u64,
    pub(super) error: Option<String>,
    failed_setting: Option<Value>,
    pub(super) quit_requested: bool,
}

impl Settings {
    pub(super) fn is_pending(&self) -> bool {
        self.saving.is_some() || !self.queued.is_empty()
    }

    pub(super) fn accepts_refresh(&self, revision: u64) -> bool {
        !self.is_pending() && self.revision == revision
    }

    fn queue(&mut self, change: Change, panel: &mut TuiAgentPanel) {
        self.clear_error_for(&change.after);
        change.after.apply(panel);
        if let Some(queued) = self
            .queued
            .iter_mut()
            .find(|queued| queued.after.same_setting(&change.after))
        {
            // Keep the last confirmed value for rollback, not an intermediate preview.
            queued.after = change.after;
            queued.label = change.label;
        } else {
            self.queued.push_back(change);
        }
        self.revision += 1;
    }

    fn clear_error_for(&mut self, value: &Value) {
        if self
            .failed_setting
            .as_ref()
            .is_some_and(|failed| failed.same_setting(value))
        {
            self.failed_setting = None;
            self.error = None;
        }
    }

    pub(super) fn apply_pending(&self, panel: &mut TuiAgentPanel) {
        if let Some(saving) = &self.saving {
            saving.change.after.apply(panel);
        }
        for change in &self.queued {
            change.after.apply(panel);
        }
    }

    fn start_with(&mut self, save: impl FnOnce(Value) -> Result<()> + Send + 'static) {
        if self.saving.is_some() {
            return;
        }
        let Some(change) = self.queued.pop_front() else {
            return;
        };
        let value = change.after.clone();
        let (sender, result) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(save(value));
        });
        self.saving = Some(Saving { change, result });
    }

    fn poll(&mut self, panel: &mut TuiAgentPanel) -> Option<String> {
        let result = match self.saving.as_ref()?.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("Settings writer disconnected")),
        };
        let change = self.saving.take()?.change;
        self.revision += 1;
        let confirmed = if result.is_ok() {
            &change.after
        } else {
            &change.before
        };
        confirmed.apply(panel);
        if let Some(queued) = self
            .queued
            .iter_mut()
            .find(|queued| queued.after.same_setting(&change.after))
        {
            queued.before = confirmed.clone();
        }
        self.apply_pending(panel);
        match result {
            Ok(()) => {
                self.clear_error_for(&change.after);
                Some(format!("Saved: {}", change.label))
            }
            Err(error) => {
                let message = format!("Unable to save {}: {error:#}", change.label);
                self.error = Some(message.clone());
                self.failed_setting = Some(change.after);
                self.quit_requested = false;
                Some(message)
            }
        }
    }
}

impl TuiApp {
    pub(super) fn poll_agent_settings(&mut self) -> bool {
        let message = self.agent_settings.poll(&mut self.agent_panel);
        let completed = message.is_some();
        if let Some(message) = message
            && (!self.agent_settings.is_pending() || self.agent_settings.error.is_some())
        {
            self.feedback_buffer = message;
        }
        self.agent_settings
            .start_with(|value| value.save(&open_agent_store()?));
        self.agent_settings.apply_pending(&mut self.agent_panel);
        completed
    }

    /// Reduces input using cached values only; no registry open or project scan.
    pub(super) fn edit_agent_setting(&mut self, key: KeyEvent) -> bool {
        if tui_toggles_models(&key) {
            return false;
        }
        let supervisor = self.agent_panel.supervisor_selected;
        if !matches!(key.code, KeyCode::Char(' ' | 'm' | 't' | 'T' | 'f' | 'F'))
            && (supervisor || !matches!(key.code, KeyCode::Char('g' | 'G')))
        {
            return false;
        }
        if key.kind != KeyEventKind::Press {
            return true;
        }
        let change = if supervisor {
            let before = self.agent_panel.supervisor_settings.clone();
            let mut after = before.clone();
            edit_supervisor_settings(&mut after, &self.agent_panel.enabled_models, key);
            Change {
                before: Value::Supervisor(before),
                after: Value::Supervisor(after),
                label: "supervisor settings".into(),
            }
        } else {
            let Some(entry) = self.agent_panel.selected_project() else {
                // Space still registers the current project through its existing effect.
                if key.code == KeyCode::Char(' ')
                    && self
                        .agent_panel
                        .selected_current_project_registration()
                        .is_some()
                {
                    return false;
                }
                self.feedback_buffer =
                    "Select a registered project before changing its settings".into();
                return true;
            };
            let project = &entry.project;
            let id = project.id;
            let (before, after, label) = match key.code {
                KeyCode::Char(' ') => (
                    Value::Enabled(id, project.enabled),
                    Value::Enabled(id, !project.enabled),
                    format!(
                        "agent {} for {}",
                        if project.enabled { "OFF" } else { "ON" },
                        project.name
                    ),
                ),
                KeyCode::Char('g' | 'G') => (
                    Value::Git(id, project.git_mode),
                    Value::Git(id, project.git_mode.next()),
                    format!(
                        "Git mode for {}: {} for subsequent runs; active runs keep their current mode",
                        project.name,
                        project.git_mode.next().label()
                    ),
                ),
                _ => {
                    let before = Value::Codex {
                        project_id: id,
                        provider: project.codex_provider.clone(),
                        model: project.codex_model.clone(),
                        thinking: project.codex_reasoning_effort.clone(),
                        fast: project.codex_fast_enabled,
                    };
                    let mut after = before.clone();
                    let Value::Codex {
                        provider,
                        model,
                        thinking,
                        fast,
                        ..
                    } = &mut after
                    else {
                        unreachable!()
                    };
                    let label = match key.code {
                        KeyCode::Char('m') => {
                            let targets = &self.agent_panel.enabled_models;
                            let current = targets.iter().position(|target| {
                                Some(&target.model_id) == model.as_ref()
                                    && target.provider_id == provider.as_deref().unwrap_or("openai")
                            });
                            let next =
                                (current.map_or(0, |index| index + 1) + 1) % (targets.len() + 1);
                            *provider = next
                                .checked_sub(1)
                                .map(|index| targets[index].provider_id.clone());
                            *model = next
                                .checked_sub(1)
                                .map(|index| targets[index].model_id.clone());
                            format!(
                                "Codex model for {}: {}",
                                project.name,
                                model.as_deref().unwrap_or("CLT default")
                            )
                        }
                        KeyCode::Char('t' | 'T') => {
                            *thinking = next_agent_codex_setting(
                                thinking.as_deref(),
                                &AGENT_CODEX_REASONING_EFFORTS,
                            );
                            format!(
                                "Codex thinking for {}: {}",
                                project.name,
                                thinking.as_deref().unwrap_or("default")
                            )
                        }
                        KeyCode::Char('f' | 'F') => {
                            *fast = !*fast;
                            format!(
                                "Codex fast mode for {}: {}",
                                project.name,
                                if *fast { "ON" } else { "OFF" }
                            )
                        }
                        _ => unreachable!(),
                    };
                    (before, after, label)
                }
            };
            Change {
                before,
                after,
                label,
            }
        };
        self.feedback_buffer = format!("Saving: {}", change.label);
        self.agent_settings.queue(change, &mut self.agent_panel);
        true
    }
}

#[cfg(test)]
mod tests;
