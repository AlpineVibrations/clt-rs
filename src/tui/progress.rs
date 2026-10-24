//! Read-only progress dialog; loading never occupies the UI thread.
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::tui::{TuiApp, TuiEffect, wrap_input_text};
use crate::{agent::agent_state_dir, application::progress::task_progress_at, task::TaskEntry};

pub(super) struct TaskSummary {
    root: PathBuf,
    task: TaskEntry,
    content: String,
    pending: Option<Receiver<Result<String>>>,
    scroll: u16,
}

impl TaskSummary {
    fn refresh(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let root = self.root.clone();
        let task = self.task.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = agent_state_dir().and_then(|state| task_progress_at(&state, &root, &task));
            let _ = sender.send(result);
        });
        self.pending = Some(receiver);
    }

    fn poll(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!(
                "Progress reader disconnected; press s to retry"
            )),
        };
        self.pending = None;
        self.content =
            result.unwrap_or_else(|error| format!("Unable to read task progress: {error:#}"));
        self.scroll = 0;
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>) {
        let size = frame.area();
        let width = size.width.min(84);
        let content = wrap_input_text(&self.content, width.saturating_sub(2) as usize);
        let height = size
            .height
            .min((content.lines().count() as u16).saturating_add(4).max(7));
        let area = Rect::new(
            size.x + size.width.saturating_sub(width) / 2,
            size.y + size.height.saturating_sub(height) / 2,
            width,
            height,
        );
        let scroll = self
            .scroll
            .min((content.lines().count() as u16).saturating_sub(height.saturating_sub(2)));
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(content).scroll((scroll, 0)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(if self.pending.is_some() {
                        " Task progress · reading… "
                    } else {
                        " Task progress · read only "
                    })
                    .title_bottom(" s refresh · ↑↓ scroll · Esc/Enter close "),
            ),
            area,
        );
    }
}

impl TuiApp {
    pub(super) fn refresh_task_summary(&mut self) {
        if self.task_summary.is_none() {
            let Some(task) = self.board_states[self.selected_board]
                .selected()
                .and_then(|index| self.task_snapshot.board_entries[self.selected_board].get(index))
                .cloned()
            else {
                self.agent_log_view = None;
                self.feedback_buffer = "No task selected".into();
                return;
            };
            self.task_summary = Some(TaskSummary {
                root: self.active_root.clone(),
                task,
                content: "Reading the selected task's progress…".into(),
                pending: None,
                scroll: 0,
            });
        }
        if let Some(summary) = &mut self.task_summary {
            summary.refresh();
        }
    }

    pub(super) fn poll_task_summary(&mut self) {
        if let Some(summary) = &mut self.task_summary {
            summary.poll();
        }
    }

    pub(super) fn task_summary_key(&mut self, key: KeyEvent) -> Vec<TuiEffect> {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.task_summary = None,
            KeyCode::Char('s') => return vec![TuiEffect::RefreshTaskSummary],
            KeyCode::Char('q') => return vec![TuiEffect::Quit],
            KeyCode::Up | KeyCode::Down => {
                if let Some(summary) = &mut self.task_summary {
                    summary.scroll = if key.code == KeyCode::Up {
                        summary.scroll.saturating_sub(1)
                    } else {
                        summary.scroll.saturating_add(1)
                    };
                }
            }
            _ => {}
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests;
