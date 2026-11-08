//! Read-only task metadata from the cached board and coordination snapshots.
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::{TuiApp, task_full_display_text, wrap_input_text};

pub(super) struct Details {
    content: String,
    scroll: u16,
    page_height: u16,
    line_count: u16,
}

impl Details {
    fn new(content: String) -> Self {
        Self {
            content,
            scroll: 0,
            page_height: 1,
            line_count: 0,
        }
    }

    fn layout(&self, size: Rect) -> (Rect, String) {
        let width = size.width.min(100);
        let text = wrap_input_text(&self.content, width.saturating_sub(2).max(1) as usize);
        let height = size
            .height
            .min((text.lines().count().min(u16::MAX as usize) as u16).saturating_add(2));
        (
            Rect::new(
                size.x + size.width.saturating_sub(width) / 2,
                size.y + size.height.saturating_sub(height) / 2,
                width,
                height,
            ),
            text,
        )
    }

    pub(super) fn prepare(&mut self, size: Rect) {
        let (area, text) = self.layout(size);
        self.page_height = area.height.saturating_sub(2).max(1);
        self.line_count = text.lines().count().min(u16::MAX as usize) as u16;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn max_scroll(&self) -> u16 {
        self.line_count.saturating_sub(self.page_height)
    }

    pub(super) fn key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('v' | 'V') => return true,
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(self.page_height),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(self.page_height),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = self.max_scroll(),
            _ => {}
        }
        self.scroll = self.scroll.min(self.max_scroll());
        false
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>) {
        let (area, text) = self.layout(frame.area());
        let lines = text.lines().count().min(u16::MAX as usize) as u16;
        let scroll = self
            .scroll
            .min(lines.saturating_sub(area.height.saturating_sub(2)));
        let footer = if area.width >= 68 {
            " Up/Down | PgUp/PgDn | Home/End | v/Enter/Esc close "
        } else {
            " Up/Down scroll | Esc close "
        };
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(text).scroll((scroll, 0)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(format!(
                        " Task details {}/{} · read only ",
                        scroll.saturating_add(1),
                        lines
                    ))
                    .title_bottom(footer),
            ),
            area,
        );
    }
}

impl TuiApp {
    pub(super) fn open_task_details(&mut self) {
        let entry = if self.archive_view {
            self.archive_state
                .selected()
                .and_then(|index| self.task_snapshot.archived_entries.get(index))
        } else {
            self.board_states[self.selected_board]
                .selected()
                .and_then(|index| self.task_snapshot.board_entries[self.selected_board].get(index))
        };
        let Some(entry) = entry else {
            self.feedback_buffer = "No task selected".into();
            return;
        };
        self.task_details = Some(Details::new(format!(
            "{}\n\n{}",
            task_full_display_text(entry),
            self.coordination.details(entry)
        )));
    }
}

#[cfg(test)]
mod tests;
