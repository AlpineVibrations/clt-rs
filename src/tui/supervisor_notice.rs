//! Scrollable snapshot of a supervisor recommendation. It never applies a decision.
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::wrap_input_text;

pub(super) struct Details {
    content: String,
    scroll: u16,
    page_height: u16,
    line_count: u16,
}

impl Details {
    pub(super) fn new(content: String) -> Self {
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

    /// Consume every key, including mutation shortcuts, until dismissed.
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
                    .border_style(Style::default().fg(Color::LightYellow))
                    .title(format!(
                        " Supervisor details {}/{} · read only ",
                        scroll.saturating_add(1),
                        lines
                    ))
                    .title_bottom(footer),
            ),
            area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::Details;
    use crossterm::event::KeyCode;
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    #[test]
    fn supervisor_instructions_are_readable_at_small_terminal_sizes() {
        let content = "Action required: repair recommendation\nCLT is waiting for you.\nTask: TODO #3 — Apply elite and weak templates.\nNext: Accept existing completion only after verifying the implementation.\nReason: The original commit includes the implementation but lacks the task trailer.\nIn the project directory, run clt list todo, confirm the current index, then run clt done todo <index>.\nThe existing commit, conversation and journal history are preserved.";
        let without_spaces = |text: &str| {
            text.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };
        for (width, height) in [(100, 24), (80, 12), (40, 10), (24, 6)] {
            let mut details = Details::new(content.into());
            let size = Rect::new(0, 0, width, height);
            details.prepare(size);
            let (area, _) = details.layout(size);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut read = String::new();
            for offset in 0..=details.max_scroll() {
                terminal.draw(|frame| details.render(frame)).unwrap();
                let last_row = if offset == details.max_scroll() {
                    area.bottom() - 1
                } else {
                    area.y + 2
                };
                for row in area.y + 1..last_row {
                    for col in area.x + 1..area.right() - 1 {
                        read.push_str(terminal.backend().buffer()[(col, row)].symbol());
                    }
                }
                details.key(KeyCode::Down);
            }
            assert_eq!(without_spaces(&read), without_spaces(content));
            details.key(KeyCode::Home);
            assert_eq!(details.scroll, 0);
            details.key(KeyCode::PageDown);
            assert_eq!(
                details.scroll,
                details.page_height.min(details.max_scroll())
            );
            details.key(KeyCode::End);
            details.prepare(Rect::new(0, 0, 100, 50));
            assert_eq!(details.scroll, details.max_scroll());
        }
        for (width, height) in [(1, 1), (2, 2), (10, 3)] {
            let mut details = Details::new(content.into());
            details.prepare(Rect::new(0, 0, width, height));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| details.render(frame)).unwrap();
        }
    }
}
