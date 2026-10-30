use super::*;
use crate::tui::{Mode, TuiApp, TuiPane, render_tui, update_tui_pane};
use crossterm::event::{KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::path::Path;

fn key(app: &mut TuiApp, code: KeyCode) {
    assert_eq!(
        update_tui_pane(app, KeyEvent::new(code, KeyModifiers::NONE)),
        Some(vec![]),
        "help must consume {code:?} without triggering effects"
    );
}

#[test]
fn help_opens_from_each_screen_and_keeps_commands_modal() {
    for pane in [TuiPane::Tasks, TuiPane::AgentProjects, TuiPane::Models] {
        for opener in ['h', 'H', '?'] {
            let mut app = TuiApp::new(Path::new("/tmp/clt-help-test"), true);
            app.current_pane = pane;
            app.selected_board = 2;
            app.board_states[2].select(Some(3));
            key(&mut app, KeyCode::Char(opener));
            assert_eq!(app.current_mode, Mode::Help);
            app.prepare_render(Rect::new(0, 0, 80, 24));
            key(&mut app, KeyCode::End);
            for code in [
                KeyCode::Char(' '),
                KeyCode::Char('q'),
                KeyCode::Char('S'),
                KeyCode::Char('t'),
                KeyCode::Char('u'),
                KeyCode::Char('M'),
                KeyCode::Char('c'),
                KeyCode::Delete,
                KeyCode::Tab,
                KeyCode::BackTab,
                KeyCode::Left,
                KeyCode::Right,
            ] {
                key(&mut app, code);
                assert_eq!(app.current_mode, Mode::Help);
                assert_eq!(app.current_pane, pane);
                assert_eq!(app.selected_board, 2);
                assert_eq!(app.board_states[2].selected(), Some(3));
            }
            for closer in [
                KeyCode::Esc,
                KeyCode::Enter,
                KeyCode::Char('h'),
                KeyCode::Char('H'),
                KeyCode::Char('?'),
            ] {
                key(&mut app, closer);
                assert_eq!(app.current_mode, Mode::View);
                assert_eq!(app.current_pane, pane);
                key(&mut app, KeyCode::Char(opener));
                assert_eq!(app.help.scroll, 0, "reopening starts at the help controls");
            }
        }
    }

    let mut app = TuiApp::new(Path::new("/tmp/clt-help-test"), true);
    app.archive_view = true;
    key(&mut app, KeyCode::Char('h'));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    app.prepare_render(Rect::new(0, 0, 80, 24));
    terminal.draw(|frame| render_tui(frame, &app)).unwrap();
    let rendered: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("HELP NAVIGATION"));
    assert!(rendered.contains("Home/End"));
    key(&mut app, KeyCode::Esc);
    assert!(app.archive_view);
}

#[test]
fn help_scrolls_by_line_and_page_and_clamps_after_resize() {
    let mut help = Help::default();
    help.prepare(Rect::new(0, 0, 40, 10));
    help.key(KeyCode::PageDown);
    assert_eq!(help.scroll, 8);
    help.key(KeyCode::Down);
    assert_eq!(help.scroll, 9);
    help.key(KeyCode::Up);
    assert_eq!(help.scroll, 8);
    help.key(KeyCode::PageUp);
    assert_eq!(help.scroll, 0);
    help.key(KeyCode::Up);
    help.key(KeyCode::PageUp);
    assert_eq!(help.scroll, 0);
    help.key(KeyCode::End);
    let bottom = help.scroll;
    help.key(KeyCode::Down);
    help.key(KeyCode::PageDown);
    assert_eq!(help.scroll, bottom);
    help.prepare(Rect::new(0, 0, 100, 50));
    assert!(help.scroll < bottom);
    assert_eq!(help.scroll, help.max_scroll());
    help.key(KeyCode::Up);
    assert_eq!(help.scroll, help.max_scroll() - 1);
    help.key(KeyCode::Home);
    assert_eq!(help.scroll, 0);
}

#[test]
fn help_reference_is_fully_readable_at_different_terminal_sizes() {
    let without_spaces = |text: &str| {
        text.chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>()
    };
    for (width, height) in [(120, 32), (80, 24), (40, 10), (24, 6)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut help = Help::default();
        let size = Rect::new(0, 0, width, height);
        help.prepare(size);
        let (area, _) = layout(size);
        let mut read = String::new();
        for offset in 0..=help.max_scroll() {
            terminal
                .draw(|frame| {
                    frame.render_widget(Paragraph::new("x".repeat(10_000)), size);
                    help.render(frame);
                })
                .unwrap();
            let last_row = if offset == help.max_scroll() {
                area.bottom() - 1
            } else {
                area.y + 2
            };
            for row in area.y + 1..last_row {
                for col in area.x + 1..area.right() - 1 {
                    read.push_str(terminal.backend().buffer()[(col, row)].symbol());
                }
            }
            help.key(KeyCode::Down);
        }
        assert_eq!(without_spaces(&read), without_spaces(COMMANDS));
    }
}

#[test]
fn help_handles_tiny_terminals_and_a_nonzero_viewport_origin() {
    for (width, height) in [(1, 1), (2, 2), (10, 3)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut help = Help::default();
        help.prepare(Rect::new(0, 0, width, height));
        help.key(KeyCode::End);
        terminal.draw(|frame| help.render(frame)).unwrap();
    }
    let (area, _) = layout(Rect::new(10, 20, 120, 24));
    assert_eq!(area, Rect::new(20, 20, 100, 24));
}
