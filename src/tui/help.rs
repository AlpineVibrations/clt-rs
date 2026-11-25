//! Contextual, searchable keyboard help. The modal consumes its own input.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Margin, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
};
use tui_input::Input;

mod reference;

use super::{TuiPane, byte_index_at_char, handle_input_key, tui_cancels_task_prompt, wrap_text};
use reference::Topic;

const ACCENT: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const KEY: Style = Style::new().add_modifier(Modifier::BOLD);
const SIDEBAR_WIDTH: u16 = 18;
const TWO_COLUMN_WIDTH: u16 = 44;

#[derive(Default)]
struct Search {
    query: Input,
    scroll: usize,
}

#[derive(Default)]
pub(super) struct Help {
    topic: Topic,
    topic_scroll: usize,
    search: Option<Search>,
    layout: Layout,
    rows: Vec<Line<'static>>,
}

#[derive(Default)]
struct Layout {
    area: Rect,
    bordered: bool,
    sidebar: Rect,
    header: Rect,
    body: Rect,
}

impl Layout {
    fn new(size: Rect, searching: bool) -> Self {
        let width = (size.width - if size.width >= 68 { 4 } else { 0 }).min(104);
        let height = (size.height - if size.height >= 14 { 2 } else { 0 }).min(28);
        let area = Rect::new(
            size.x + (size.width - width) / 2,
            size.y + (size.height - height) / 2,
            width,
            height,
        );
        let bordered = width > 2 && height > 2;
        let inner = if bordered {
            area.inner(Margin::new(1, 1))
        } else {
            area
        };

        let show_sidebar = !searching
            && inner.width >= SIDEBAR_WIDTH + TWO_COLUMN_WIDTH + 2
            && inner.height as usize >= Topic::ALL.len();
        let sidebar_width = if show_sidebar { SIDEBAR_WIDTH } else { 0 };
        let sidebar = Rect::new(inner.x, inner.y, sidebar_width, inner.height);

        let mut content = Rect::new(
            sidebar.right(),
            inner.y,
            inner.width - sidebar.width,
            inner.height,
        );
        if content.width >= 24 {
            content = content.inner(Margin::new(1, 0));
        }
        let header_height = u16::from(!show_sidebar && content.height > 1);
        let header = Rect::new(content.x, content.y, content.width, header_height);
        let body = Rect::new(
            content.x,
            header.bottom(),
            content.width,
            content.height - header.height,
        );

        Self {
            area,
            bordered,
            sidebar,
            header,
            body,
        }
    }
}

fn reference_rows(topic: Topic, query: Option<&str>, width: usize) -> Vec<Line<'static>> {
    let query = query.map(str::to_ascii_lowercase);
    let mut rows = Vec::new();

    for topic in Topic::ALL
        .into_iter()
        .filter(|candidate| query.is_some() || *candidate == topic)
    {
        for &(section, commands) in topic.sections() {
            let mut matches = commands
                .iter()
                .filter(|&&(keys, action)| {
                    query.as_deref().is_none_or(|query| {
                        query.split_whitespace().all(|term| {
                            keys.split(", ").any(|key| key.eq_ignore_ascii_case(term))
                                || (term.len() > 1
                                    && [keys, action, topic.title(), section]
                                        .iter()
                                        .any(|field| field.to_ascii_lowercase().contains(term)))
                        })
                    })
                })
                .peekable();
            if matches.peek().is_none() {
                continue;
            }

            if !rows.is_empty() {
                rows.push(Line::default());
            }
            let heading = if query.is_some() {
                format!("{} / {section}", topic.title())
            } else {
                section.to_owned()
            };
            rows.extend(
                wrap_text(&heading, width)
                    .lines()
                    .map(|line| Line::styled(line.to_owned(), ACCENT)),
            );
            for &(keys, action) in matches {
                append_shortcut(&mut rows, width, keys, action);
            }
        }
    }

    if rows.is_empty() {
        rows.extend(
            wrap_text("No matching shortcuts", width)
                .lines()
                .map(|line| Line::raw(line.to_owned())),
        );
    }
    rows
}

fn append_shortcut(rows: &mut Vec<Line<'static>>, width: usize, keys: &'static str, action: &str) {
    const KEY_COLUMN: usize = 22;
    let two_columns = width >= TWO_COLUMN_WIDTH as usize;
    let indent = if two_columns {
        KEY_COLUMN
    } else if width >= 16 {
        2
    } else {
        0
    };
    let inline = two_columns && keys.len() + 2 <= KEY_COLUMN;
    if !inline {
        rows.extend(
            wrap_text(keys, width)
                .lines()
                .map(|line| Line::styled(line.to_owned(), KEY)),
        );
    }

    for (index, line) in wrap_text(action, width - indent).lines().enumerate() {
        let label = if inline && index == 0 { keys } else { "" };
        rows.push(Line::from(vec![
            Span::styled(label, KEY),
            Span::raw(" ".repeat(indent - label.len())),
            Span::raw(line.to_owned()),
        ]));
    }
}

impl Help {
    pub(super) fn new(pane: TuiPane, archive: bool) -> Self {
        let topic = match pane {
            TuiPane::Tasks if archive => Topic::Archive,
            TuiPane::Tasks => Topic::Tasks,
            TuiPane::AgentProjects => Topic::Projects,
            TuiPane::Models => Topic::Models,
        };
        Self {
            topic,
            ..Self::default()
        }
    }

    pub(super) fn is_searching(&self) -> bool {
        self.search.is_some()
    }

    pub(super) fn prepare(&mut self, size: Rect) {
        self.layout = Layout::new(size, self.is_searching());
        let query = self.search.as_ref().map(|search| search.query.value());
        self.rows = reference_rows(self.topic, query, self.layout.body.width.max(1) as usize);
        let max_scroll = self.max_scroll();
        let scroll = self.scroll_mut();
        *scroll = (*scroll).min(max_scroll);
    }

    fn page_height(&self) -> usize {
        self.layout.body.height.max(1) as usize
    }

    fn max_scroll(&self) -> usize {
        self.rows.len().saturating_sub(self.page_height())
    }

    fn scroll_mut(&mut self) -> &mut usize {
        self.search
            .as_mut()
            .map_or(&mut self.topic_scroll, |search| &mut search.scroll)
    }

    fn change_topic(&mut self, delta: isize) {
        self.topic = self.topic.adjacent(delta);
        self.topic_scroll = 0;
        self.search = None;
    }

    /// Returns true only on dismissal. Search and browsing consume all other keys.
    pub(super) fn key(&mut self, key: KeyEvent) -> bool {
        let searching = self.is_searching();
        if searching && tui_cancels_task_prompt(&key) {
            self.search = None;
            return false;
        }
        let page = self.page_height() as isize;
        let max_scroll = self.max_scroll();
        let delta = match key.code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('h' | 'H' | '?') if !searching => {
                return true;
            }
            KeyCode::Char('/') if !searching => {
                self.search = Some(Search::default());
                return false;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.change_topic(
                    if key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT) {
                        -1
                    } else {
                        1
                    },
                );
                return false;
            }
            KeyCode::Left if !searching => {
                self.change_topic(-1);
                return false;
            }
            KeyCode::Right if !searching => {
                self.change_topic(1);
                return false;
            }
            KeyCode::Up => -1,
            KeyCode::Down => 1,
            KeyCode::PageUp => -page,
            KeyCode::PageDown => page,
            KeyCode::Home if !searching => {
                self.topic_scroll = 0;
                return false;
            }
            KeyCode::End if !searching => {
                self.topic_scroll = max_scroll;
                return false;
            }
            _ => {
                if let Some(search) = &mut self.search {
                    let before = search.query.value().to_owned();
                    handle_input_key(&mut search.query, key, "", 1);
                    if search.query.value() != before {
                        search.scroll = 0;
                    }
                }
                return false;
            }
        };
        let scroll = self.scroll_mut();
        *scroll = scroll.saturating_add_signed(delta).min(max_scroll);
        false
    }

    pub(super) fn paste(&mut self, content: &str) {
        if let Some(search) = &mut self.search {
            let text: String = content
                .chars()
                .filter_map(|ch| {
                    if ch.is_whitespace() {
                        Some(' ')
                    } else if !ch.is_control() {
                        Some(ch)
                    } else {
                        None
                    }
                })
                .collect();
            let mut query = search.query.value().to_owned();
            let cursor = search.query.cursor();
            query.insert_str(byte_index_at_char(&query, cursor), &text);
            search.query = Input::new(query).with_cursor(cursor + text.chars().count());
            search.scroll = 0;
        }
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>) {
        let Layout {
            area,
            bordered,
            sidebar,
            header,
            body,
        } = self.layout;
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);
        if bordered {
            let title = if sidebar.is_empty() && header.is_empty() && !self.is_searching() {
                format!(" {} ", self.topic.title())
            } else {
                " Keyboard shortcuts ".to_owned()
            };
            frame.render_widget(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(Color::Cyan))
                    .title(Line::styled(title, ACCENT))
                    .title_bottom(footer(self.is_searching(), area.width.saturating_sub(2))),
                area,
            );
        }

        if !sidebar.is_empty() {
            self.render_topics(frame, sidebar);
        } else if !header.is_empty() {
            if let Some(search) = &self.search {
                render_search(frame, header, &search.query);
            } else {
                frame.render_widget(
                    Line::styled(format!("‹  {}  ›", self.topic.title()), ACCENT).centered(),
                    header,
                );
            }
        }

        let scroll = self
            .search
            .as_ref()
            .map_or(self.topic_scroll, |search| search.scroll);
        for (row, line) in body.rows().zip(self.rows.iter().skip(scroll)) {
            frame.render_widget(line, row);
        }

        if bordered && !body.is_empty() && self.max_scroll() > 0 {
            let mut state = ScrollbarState::new(self.max_scroll() + 1)
                .position(scroll)
                .viewport_content_length(self.page_height());
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_symbol(Some("│"))
                    .thumb_symbol("┃")
                    .track_style(Style::new().fg(Color::Cyan))
                    .thumb_style(Style::new().fg(Color::LightCyan)),
                Rect::new(area.right() - 1, body.y, 1, body.height),
                &mut state,
            );
        }
    }

    fn render_topics(&self, frame: &mut Frame<'_>, area: Rect) {
        let divider = Block::new()
            .borders(Borders::RIGHT)
            .border_style(Style::new().fg(Color::DarkGray));
        let inner = divider.inner(area);
        frame.render_widget(divider, area);
        for (row, topic) in inner.rows().zip(Topic::ALL) {
            let active = topic == self.topic;
            let marker = if active { "› " } else { "  " };
            let style = if active {
                ACCENT.add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            frame.render_widget(
                Line::styled(
                    format!(
                        "{marker}{:<width$}",
                        topic.title(),
                        width = inner.width as usize - 2
                    ),
                    style,
                ),
                row,
            );
        }
    }
}

fn render_search(frame: &mut Frame<'_>, area: Rect, query: &Input) {
    let mut field = area;
    if area.width >= 4 {
        frame.render_widget(Line::styled("/ ", ACCENT), Rect::new(area.x, area.y, 2, 1));
        field.x += 2;
        field.width -= 2;
    }
    let scroll = query.visual_scroll(field.width.saturating_sub(1) as usize);
    if query.value().is_empty() {
        frame.render_widget(
            Line::raw("Search by shortcut or action")
                .style(Style::new().add_modifier(Modifier::DIM)),
            field,
        );
    } else {
        frame.render_widget(
            Paragraph::new(query.value()).scroll((0, scroll.min(u16::MAX as usize) as u16)),
            field,
        );
    }
    let cursor = query
        .visual_cursor()
        .saturating_sub(scroll)
        .min(field.width.saturating_sub(1) as usize);
    frame.set_cursor_position(Position::new(field.x + cursor as u16, field.y));
}

fn footer(searching: bool, width: u16) -> Line<'static> {
    let options = if searching {
        [
            " ↑/↓ Results  PgUp/PgDn Page  Esc Back ",
            " ↑/↓ Results  Esc Back ",
            " Esc Back ",
            " Esc ",
        ]
    } else {
        [
            " ←/→ Topics  ↑/↓ Scroll  PgUp/PgDn Page  / Search  Esc Close ",
            " ←/→ Topics  ↑/↓ Scroll  / Search  Esc ",
            " ←/→ Topics  ↑/↓  / Search  Esc ",
            " ←/→  / Search  Esc ",
        ]
    };
    options
        .into_iter()
        .map(Line::raw)
        .find(|line| line.width() <= width as usize)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
