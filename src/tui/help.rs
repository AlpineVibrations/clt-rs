//! Complete keyboard reference and a scrollable, read-only help dialog.
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::wrap_text;

// Keep this reference aligned with the pane reducers, execute_tui_key_effect,
// handle_input_key and progress::task_summary_key when adding commands.
const COMMANDS: &str = "\
HELP NAVIGATION
Up/Down: Scroll one line.
PageUp/PageDown: Scroll one page. Home/End: First/last page.
h / H / ? / Enter / Esc: Close help and return to the same screen.

COMMON (outside text input and dialogs)
h / H / ?: Open this help.
Tab / Shift+Tab: Toggle Tasks and Agent Projects; return from Models.
M: Open Models from the task board or Agent Projects; return from Models.
q: Quit CLT.

TASK BOARD
Up/Down: Select previous/next task (wraps).
Left/Right: Focus previous/next visible column (wraps).
0 / 1 / 2 / 3: Focus Backlog / Todo / Doing / Done; 0 also shows Backlog.
Space: Create a task at the current selection.
Enter: Message a running Doing agent; otherwise open subtasks or edit. With no selection, create a task.
e: Edit the selected task.
n / +: Create a subtask under the selected task.
Backspace: Return to the parent board.
Esc: Close an open log; otherwise unselect the task.
t: Cycle the selected Todo's thinking override, including default.
s: Open a read-only task progress summary.
Shift+S: Stop/start the selected task.
i: Take over a linked live/stopped session interactively; resume automation on return.
c: Plan a Todo or open its linked session; take over an active run.
l / L: Toggle the selected task's output (project output if none selected).
a: Move the selected task to the archive.
A: Open the read-only archive.
b: Move the selected task to Backlog.
B: Show/hide the Backlog column.
Delete: Permanently delete the selected task.
r / R: Enter sticky Reorganize mode.
Shift+Up/Down or Ctrl+P/N: Reorder the selected task up/down.
Shift+Left/Right: Move the task to the previous/next visible column.
4-9: Move the selected task to that numbered position in its column.

REORGANIZE MODE
Up/Down: Reorder the selected task.
Left/Right: Move the task between visible columns.
r / R / Esc: Exit Reorganize mode.

ARCHIVE
Up/Down: Select previous/next archived task (wraps).
A: Return to the task board. The archive is read-only.

AGENT PROJECTS
Up/Down: Select a project.
Enter: Open its task board; register the current project on the add row.
Space: Toggle the selected project ON/OFF; register it on the add row.
Delete: Remove the selected project after confirmation.
g / G: Cycle project Git mode: off / commit / push.
m: Cycle the project's model target.
f / F: Toggle project Fast mode.
t / T: Cycle project thinking.
r / R: Retry a failed project, or confirm missing Git-record recovery.
s / S: Stop/resume the selected session; with output open, control that exact session.
l / L: Toggle the selected project's live/latest output.
i: With output open, take over its session and resume automation on return.
c: With output open, open its session interactively (take over if active).
Esc: Close an open log; otherwise return to the active task board.
u: Select/deselect global supervisor settings.

SUPERVISOR SETTINGS (after u in Agent Projects)
Space: Toggle the supervisor ON/OFF.
m: Cycle supervisor model. t / T: Cycle thinking. f / F: Toggle Fast.
u / Esc / Enter / Down: Return to the project list.

MODELS AND PROVIDERS
Left/Right: Focus providers/models. Up/Down: Select a row (wraps).
PageUp/PageDown: Move one page. Home/End: Select first/last row.
Space: Enable/disable the selected provider or model.
f / F: Toggle model favorite. t / T: Cycle model thinking.
d / D: Set the CLT default model.
c / C: Set the Codex default model and its thinking setting.
n / N: Open provider choices: 1 OpenAI, 2 OpenRouter, 3 Ollama, 4 LM Studio, 5 custom endpoint.
r / R: Discover/refresh the selected provider's models.
a / A: Manually add a model ID to the selected provider.
k / K: Store/replace the provider API key; submit empty input to clear it.
x / X / Delete: Remove the selected non-built-in provider and its key.
/: Search model names and IDs; submit empty input to clear the search.
M / Tab / Shift+Tab / Esc: Return to the previous pane (Esc closes a log first).

TASK PROGRESS DIALOG
s: Refresh the snapshot. Up/Down: Scroll.
Esc / Enter: Close the summary. q: Quit CLT.

TEXT INPUT (tasks, agent messages, models and providers)
Enter: Submit the task, message, search or current provider/model field.
Esc: Cancel input. Ctrl+C: Cancel a task or agent-message prompt.
Left/Right: Move by character. Up/Down: Move between wrapped rows.
Home / Ctrl+A: Start of input. End / Ctrl+E: End of input.
Ctrl/Alt+Left/Right or Alt+B/F: Move by word.
Backspace / Delete: Delete previous/next character.
Ctrl/Alt+Backspace or Ctrl+W: Delete previous word.
Ctrl/Alt+Delete: Delete next word.
Ctrl+U: Clear input. Ctrl+K: Delete to end of input.
Paste: Insert text; task/message prompts retain multiline pastes.

CONFIRMATION PROMPTS
y / n: Initialize a task board at startup / continue without one.
Ctrl+C: Cancel initialization and exit.
y / n / Esc: Confirm / cancel project removal or Git recovery.
1-4 / 5: Choose a provider preset / custom endpoint after n in Models.
Esc: Cancel provider selection.\n";

#[derive(Default)]
pub(super) struct Help {
    scroll: u16,
    page_height: u16,
    line_count: u16,
}

fn layout(size: Rect) -> (Rect, String) {
    let width = size.width.min(100);
    let text = COMMANDS
        .lines()
        .map(|line| wrap_text(line, width.saturating_sub(2).max(1) as usize))
        .collect::<Vec<_>>()
        .join("\n");
    let height = size.height.min(text.lines().count() as u16 + 2);
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

impl Help {
    pub(super) fn prepare(&mut self, size: Rect) {
        let (area, text) = layout(size);
        self.page_height = area.height.saturating_sub(2).max(1);
        self.line_count = text.lines().count() as u16;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn max_scroll(&self) -> u16 {
        self.line_count.saturating_sub(self.page_height)
    }

    /// Returns true only for help's dismissal keys; all other input is consumed.
    pub(super) fn key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('h' | 'H' | '?') => return true,
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
        let (area, text) = layout(frame.area());
        let lines = text.lines().count() as u16;
        let height = area.height.saturating_sub(2);
        let scroll = self.scroll.min(lines.saturating_sub(height));
        let title = format!(" Help {}/{} ", scroll.saturating_add(1), lines);
        let footer = if area.width >= 68 {
            " Up/Down scroll | PgUp/PgDn | Home/End | h/?/Enter/Esc close "
        } else {
            " Up/Down scroll | Esc close "
        };
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(text)
                .scroll((scroll, 0))
                .style(Style::default().fg(Color::White))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Yellow))
                        .title(title)
                        .title_bottom(footer),
                ),
            area,
        );
    }
}

#[cfg(test)]
mod tests;
