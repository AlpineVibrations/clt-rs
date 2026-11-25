use super::*;
use crate::tui::{Mode, TuiApp, render_tui, update_tui_pane};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::path::Path;

fn event(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn key(app: &mut TuiApp, code: KeyCode) {
    assert_eq!(
        update_tui_pane(app, event(code)),
        Some(vec![]),
        "help must consume {code:?} without triggering effects"
    );
}

fn row(buffer: &Buffer, area: Rect) -> String {
    (area.x..area.right())
        .map(|x| buffer[(x, area.y)].symbol())
        .collect()
}

fn rendered(buffer: &Buffer) -> String {
    buffer
        .area
        .rows()
        .map(|area| row(buffer, area) + "\n")
        .collect()
}

#[test]
fn help_opens_to_the_origin_and_keeps_commands_modal() {
    for (pane, archive, topic, heading) in [
        (TuiPane::Tasks, false, Topic::Tasks, "Navigate"),
        (
            TuiPane::Tasks,
            true,
            Topic::Archive,
            "Browse archived tasks",
        ),
        (
            TuiPane::AgentProjects,
            false,
            Topic::Projects,
            "Project list",
        ),
        (TuiPane::Models, false, Topic::Models, "Browse and search"),
    ] {
        for opener in ['h', 'H', '?'] {
            let mut app = TuiApp::new(Path::new("/tmp/clt-help-test"), true);
            app.current_pane = pane;
            app.archive_view = archive;
            app.selected_board = 2;
            app.board_states[2].select(Some(3));

            key(&mut app, KeyCode::Char(opener));
            assert_eq!(app.current_mode, Mode::Help);
            assert_eq!(app.help.topic, topic);
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            app.prepare_render(Rect::new(0, 0, 80, 24));
            terminal.draw(|frame| render_tui(frame, &app)).unwrap();
            assert!(rendered(terminal.backend().buffer()).contains(heading));

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
                assert_eq!(app.archive_view, archive);
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
                assert_eq!(app.help.topic, topic);
                assert_eq!(app.help.topic_scroll, 0);
            }
        }
    }
}

#[test]
fn topic_navigation_wraps_and_scroll_stays_bounded() {
    let mut help = Help::default();
    let size = Rect::new(0, 0, 40, 10);
    help.prepare(size);
    let page = help.page_height();

    for (code, offset) in [
        (KeyCode::PageDown, page),
        (KeyCode::Down, page + 1),
        (KeyCode::Up, page),
        (KeyCode::PageUp, 0),
        (KeyCode::Up, 0),
    ] {
        assert!(!help.key(event(code)));
        assert_eq!(help.topic_scroll, offset);
    }
    help.key(event(KeyCode::End));
    let bottom = help.max_scroll();
    help.key(event(KeyCode::PageDown));
    assert_eq!(help.topic_scroll, bottom);
    help.prepare(Rect::new(0, 0, 100, 50));
    assert!(help.topic_scroll < bottom);
    assert_eq!(help.topic_scroll, help.max_scroll());
    help.key(event(KeyCode::Home));
    assert_eq!(help.topic_scroll, 0);

    help.key(event(KeyCode::End));
    help.key(event(KeyCode::Left));
    assert_eq!(help.topic, Topic::General);
    assert_eq!(help.topic_scroll, 0);
    help.key(event(KeyCode::Tab));
    assert_eq!(help.topic, Topic::Tasks);
    help.key(event(KeyCode::Right));
    assert_eq!(help.topic, Topic::Projects);
    help.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
    assert_eq!(help.topic, Topic::Tasks);
}

#[test]
fn live_search_finds_actions_and_keys_across_topics() {
    let mut app = TuiApp::new(Path::new("/tmp/clt-help-test"), true);
    let size = Rect::new(0, 0, 80, 24);
    let mut terminal = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();

    key(&mut app, KeyCode::Char('h'));
    app.prepare_render(size);
    key(&mut app, KeyCode::End);
    let original_scroll = app.help.topic_scroll;
    assert!(original_scroll > 0);
    key(&mut app, KeyCode::Char('/'));

    app.help.paste("API\nkez");
    key(&mut app, KeyCode::Backspace);
    key(&mut app, KeyCode::Char('y'));
    key(&mut app, KeyCode::Enter);
    app.prepare_render(size);
    terminal.draw(|frame| app.help.render(frame)).unwrap();
    let text = rendered(terminal.backend().buffer());
    assert!(text.contains("Models / Providers"));
    assert!(text.contains("k, K"));
    assert!(!text.contains("Monitor and control"));

    let clear = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(update_tui_pane(&mut app, clear), Some(vec![]));
    app.help.paste("ctrl+n");
    app.prepare_render(size);
    terminal.draw(|frame| app.help.render(frame)).unwrap();
    let text = rendered(terminal.backend().buffer());
    assert!(text.contains("Task board / Move and organize"));
    assert!(text.contains("Ctrl+P, Ctrl+N"));

    update_tui_pane(&mut app, clear);
    key(&mut app, KeyCode::Char('?'));
    app.prepare_render(size);
    terminal.draw(|frame| app.help.render(frame)).unwrap();
    assert!(rendered(terminal.backend().buffer()).contains("Open keyboard help"));

    update_tui_pane(&mut app, clear);
    app.help.paste("ä missing shortcut");
    app.prepare_render(size);
    terminal.draw(|frame| app.help.render(frame)).unwrap();
    assert!(rendered(terminal.backend().buffer()).contains("No matching shortcuts"));

    key(&mut app, KeyCode::Esc);
    assert_eq!(app.current_mode, Mode::Help);
    assert!(!app.help.is_searching());
    app.prepare_render(size);
    assert_eq!(app.help.topic, Topic::Tasks);
    assert_eq!(app.help.topic_scroll, original_scroll);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.current_mode, Mode::View);
}

#[test]
fn help_layout_aligns_keys_and_stacks_them_in_narrow_terminals() {
    for (width, height) in [(80, 24), (40, 10)] {
        let size = Rect::new(0, 0, width, height);
        let mut help = Help::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        help.prepare(size);
        terminal.draw(|frame| help.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let text = rendered(buffer);
        assert!(text.contains("/ Search"));
        assert!(text.contains("Task board"));
        let body = help.layout.body;
        assert_eq!(buffer[(help.layout.area.right() - 1, body.y)].symbol(), "┃");
        let mut rows = body.rows();
        rows.next();
        let first_shortcut = row(buffer, rows.next().unwrap());
        assert!(first_shortcut.starts_with("Up, Down"));
        if width == 80 {
            assert!(text.contains("Agent Projects"));
            assert!(first_shortcut.contains("Select the previous"));
        } else {
            assert!(!first_shortcut.contains("Select the previous"));
            assert!(row(buffer, rows.next().unwrap()).starts_with("  Select the previous"));
        }
    }
}

#[test]
fn every_topic_is_readable_at_different_terminal_sizes() {
    let without_spaces = |text: &str| {
        text.chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>()
    };
    for (width, height) in [(120, 32), (80, 24), (40, 10), (24, 6)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let size = Rect::new(0, 0, width, height);
        for topic in Topic::ALL {
            let mut help = Help {
                topic,
                ..Help::default()
            };
            help.prepare(size);
            let mut read = String::new();
            for offset in 0..=help.max_scroll() {
                terminal.draw(|frame| help.render(frame)).unwrap();
                let visible_rows = if offset == help.max_scroll() {
                    help.layout.body.height
                } else {
                    1
                };
                for area in help.layout.body.rows().take(visible_rows as usize) {
                    read.push_str(&row(terminal.backend().buffer(), area));
                }
                help.key(event(KeyCode::Down));
            }
            let mut expected = String::new();
            for &(heading, shortcuts) in topic.sections() {
                expected.push_str(heading);
                for &(keys, action) in shortcuts {
                    expected.push_str(keys);
                    expected.push_str(action);
                }
            }
            assert_eq!(
                without_spaces(&read),
                without_spaces(&expected),
                "{topic:?} at {width}x{height}"
            );
        }
    }
}

#[test]
fn help_handles_tiny_terminals_and_a_nonzero_viewport_origin() {
    for (width, height) in [(1, 1), (2, 2), (10, 3)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut help = Help::default();
        let size = Rect::new(0, 0, width, height);
        help.prepare(size);
        help.key(event(KeyCode::End));
        terminal.draw(|frame| help.render(frame)).unwrap();
        help.key(event(KeyCode::Char('/')));
        help.paste("wide text 日本語");
        help.prepare(size);
        terminal.draw(|frame| help.render(frame)).unwrap();
    }
    let layout = Layout::new(Rect::new(10, 20, 120, 24), false);
    assert_eq!(layout.area, Rect::new(18, 21, 104, 22));
}
