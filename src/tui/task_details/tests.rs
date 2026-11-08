use super::*;
use crate::{
    agent::AgentSessionControlState,
    coordination::Snapshot,
    task::{
        TaskSource, TaskStatus,
        coordination::{self as metadata, Metadata, Review, Scope},
        init_tasks, insert_task_in_board, read_task_entries, task_entry_from_text,
    },
    tui::{Mode, TASK_STATUSES, TuiPane, render_tui, update_tui_pane},
};
use crossterm::event::{KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::path::Path;

const TASK_ID: &str = "00000000-0000-0000-0000-000000000001";
const OTHER_ID: &str = "00000000-0000-0000-0000-000000000002";
const SESSION: &str = "019abcde-1234-7890-abcd-0123456789ab";

fn content() -> String {
    metadata::write(
        &format!("Open map. Show feedback while loading. (BUG, HIGH) codex:{SESSION}"),
        &Metadata {
            id: TASK_ID.into(),
            scope: Scope {
                intent: "Make opening a map responsive".into(),
                files: (0..40).map(|i| format!("src/map_{i}.rs")).collect(),
                modules: vec!["map renderer".into()],
                interfaces: vec!["MapPreview".into()],
                resources: vec!["tile cache".into()],
                behavior: [("map.open".into(), "Show progress during capture".into())].into(),
            },
            ..Default::default()
        },
    )
    .unwrap()
}

fn press(app: &mut TuiApp, code: KeyCode) {
    assert_eq!(
        update_tui_pane(app, KeyEvent::new(code, KeyModifiers::NONE)),
        Some(vec![]),
        "details must consume {code:?} without effects"
    );
}

fn draw(app: &mut TuiApp, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    app.prepare_render(Rect::new(0, 0, width, height));
    terminal.draw(|frame| render_tui(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn selected_cards_keep_prose_and_status_without_expanding_generated_metadata() {
    for folders in [false, true] {
        let root = tempfile::tempdir().unwrap();
        init_tasks(root.path(), folders).unwrap();
        let board = root.path().join("tasks");
        let original = content();
        insert_task_in_board(&board, TaskStatus::Doing, None, &original, None).unwrap();
        insert_task_in_board(&board, TaskStatus::Doing, None, "Next task.", None).unwrap();
        let contents = || {
            read_task_entries(&board, TaskStatus::Doing)
                .unwrap()
                .into_iter()
                .map(|entry| (entry.source, entry.content))
                .collect::<Vec<_>>()
        };
        let before = contents();
        let mut app = TuiApp::new(root.path(), true);
        app.refresh_task_snapshot();
        app.coordination = Snapshot::load_at(&board, &root.path().join("state")).unwrap();
        app.selected_board = 1;
        app.task_agent_session_states
            .insert(SESSION.into(), AgentSessionControlState::Running);
        for selected in [Some(0), None] {
            app.board_states[1].select(selected);
            let rendered = draw(&mut app, 120, 18);
            assert!(rendered.contains("[RUNNING] Open map."), "{rendered}");
            assert!(rendered.contains("Next task."), "{rendered}");
            assert!(!rendered.contains(TASK_ID));
            assert!(!rendered.contains("Scope:"));
            assert!(!rendered.contains("src/map_"));
            assert!(!rendered.contains("clt-coordination"));
            if selected.is_some() {
                assert!(rendered.contains("Show feedback"), "{rendered}");
                assert!(rendered.contains("BUG, HIGH"), "{rendered}");
            }
        }
        app.board_states[1].select(Some(0));
        press(&mut app, KeyCode::Char('v'));
        let details = &app.task_details.as_ref().unwrap().content;
        for expected in [
            TASK_ID,
            SESSION,
            "Make opening a map responsive",
            "src/map_39.rs",
            "Modules:\n- map renderer",
            "Interfaces:\n- MapPreview",
            "Resources:\n- tile cache",
            "map.open: Show progress during capture",
        ] {
            assert!(details.contains(expected), "Missing {expected}: {details}");
        }
        assert!(draw(&mut app, 80, 12).contains("Task details"));
        press(&mut app, KeyCode::Esc);
        assert_eq!(contents(), before);
    }
}

#[test]
fn waiting_badge_stays_compact_and_explanations_and_reviews_are_in_details() {
    let root = tempfile::tempdir().unwrap();
    init_tasks(root.path(), false).unwrap();
    let board = root.path().join("tasks");
    let mut meta = metadata::read(&content()).unwrap();
    meta.depends_on = vec![OTHER_ID.into()];
    meta.reviews = vec![Review {
        other: OTHER_ID.into(),
        evidence: "saved review fingerprint".into(),
        compatible: true,
        reason: "Separate renderer functions".into(),
    }];
    let text = metadata::write("Open map. Dependencies: MAP-002.", &meta).unwrap();
    insert_task_in_board(&board, TaskStatus::Todo, None, &text, None).unwrap();
    let mut app = TuiApp::new(root.path(), true);
    app.refresh_task_snapshot();
    app.coordination = Snapshot::load_at(&board, &root.path().join("state")).unwrap();
    app.board_states[0].select(Some(0));
    let rendered = draw(&mut app, 120, 24);
    assert!(rendered.contains("[WAITING]"));
    assert!(!rendered.contains("Missing prerequisite"));
    assert!(!rendered.contains(TASK_ID));
    // CLI annotations retain their detailed diagnostics.
    let entry = &app.task_snapshot.board_entries[0][0];
    assert!(app.coordination.annotation(entry).contains(TASK_ID));
    assert!(app.coordination.annotation(entry).contains("Missing prerequisite"));
    press(&mut app, KeyCode::Char('v'));
    let details = &app.task_details.as_ref().unwrap().content;
    for expected in [
        "Dependencies:",
        OTHER_ID,
        "MAP-002",
        "Coordination waits:",
        "Missing prerequisite",
        "Overlap reviews:",
        "Compatible with",
        "Separate renderer functions",
        "saved review fingerprint",
    ] {
        assert!(details.contains(expected), "Missing {expected}: {details}");
    }
}

#[test]
fn details_use_cached_selected_tasks_and_consume_commands_without_changing_selection() {
    let mut app = TuiApp::new(Path::new("/missing-clt-details-board"), true);
    app.board_stack.push(app.active_root.join("tasks/todo/nested"));
    let text = content();
    let entry = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 0 },
        &text,
        &text,
        false,
    );
    for board in 0..TASK_STATUSES.len() {
        app.task_snapshot.board_entries[board] = vec![entry.clone()];
        app.selected_board = board;
        app.board_states[board].select(Some(0));
        for opener in ['v', 'V'] {
            press(&mut app, KeyCode::Char(opener));
            assert!(app.task_details.as_ref().unwrap().content.contains(TASK_ID));
            draw(&mut app, 40, 10);
            press(&mut app, KeyCode::End);
            assert!(app.task_details.as_ref().unwrap().scroll > 0);
            for code in [
                KeyCode::Delete,
                KeyCode::Char(' '),
                KeyCode::Char('e'),
                KeyCode::Char('S'),
                KeyCode::Char('s'),
                KeyCode::Char('c'),
                KeyCode::Char('t'),
                KeyCode::Char('q'),
                KeyCode::Char('M'),
                KeyCode::Char('h'),
                KeyCode::Tab,
                KeyCode::Left,
                KeyCode::Right,
            ] {
                press(&mut app, code);
                assert!(app.task_details.is_some());
                assert_eq!(app.current_mode, Mode::View);
                assert_eq!(app.current_pane, TuiPane::Tasks);
                assert_eq!(app.selected_board, board);
                assert_eq!(app.board_states[board].selected(), Some(0));
            }
            for closer in [KeyCode::Esc, KeyCode::Enter, KeyCode::Char('v'), KeyCode::Char('V')] {
                press(&mut app, closer);
                assert!(app.task_details.is_none());
                press(&mut app, KeyCode::Char(opener));
                assert_eq!(app.task_details.as_ref().unwrap().scroll, 0);
            }
            press(&mut app, KeyCode::Esc);
        }
    }
    app.archive_view = true;
    app.task_snapshot.archived_entries = vec![entry];
    app.archive_state.select(Some(0));
    press(&mut app, KeyCode::Char('V'));
    assert!(app.task_details.as_ref().unwrap().content.contains(TASK_ID));
    press(&mut app, KeyCode::Esc);
    assert!(app.archive_view);
    assert_eq!(app.archive_state.selected(), Some(0));
}

#[test]
fn details_handle_empty_stale_and_unannotated_selections_without_a_log() {
    let mut app = TuiApp::new(Path::new("/missing-clt-details-board"), true);
    for archive in [false, true] {
        app.archive_view = archive;
        for selected in [None, Some(3)] {
            app.board_states[0].select(selected);
            app.archive_state.select(selected);
            press(&mut app, KeyCode::Char('v'));
            assert!(app.task_details.is_none());
            assert_eq!(app.feedback_buffer, "No task selected");
        }
    }
    app.archive_view = false;
    let entry = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 0 },
        "Simple task.",
        "Simple task.",
        false,
    );
    app.task_snapshot.board_entries[0] = vec![entry];
    app.board_states[0].select(Some(0));
    press(&mut app, KeyCode::Char('v'));
    assert!(app.task_details.as_ref().unwrap().content.contains("No coordination metadata recorded."));
    assert!(app.agent_log_view.is_none());
}

#[test]
fn all_metadata_remains_readable_on_small_terminals_and_after_resizing() {
    let text = content();
    let entry = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 0 },
        &text,
        &text,
        false,
    );
    let content = format!("Café map details\n{}", Snapshot::default().details(&entry));
    let without_spaces = |text: &str| text.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    for (width, height) in [(100, 24), (80, 12), (40, 10), (24, 6)] {
        let mut details = Details::new(content.clone());
        let size = Rect::new(0, 0, width, height);
        details.prepare(size);
        let (area, _) = details.layout(size);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut read = String::new();
        for offset in 0..=details.max_scroll() {
            terminal.draw(|frame| details.render(frame)).unwrap();
            let last_row = if offset == details.max_scroll() { area.bottom() - 1 } else { area.y + 2 };
            for row in area.y + 1..last_row {
                for col in area.x + 1..area.right() - 1 {
                    read.push_str(terminal.backend().buffer()[(col, row)].symbol());
                }
            }
            details.key(KeyCode::Down);
        }
        assert_eq!(without_spaces(&read), without_spaces(&content));
        details.key(KeyCode::Home);
        details.key(KeyCode::PageDown);
        assert_eq!(details.scroll, details.page_height.min(details.max_scroll()));
        details.key(KeyCode::PageUp);
        assert_eq!(details.scroll, 0);
        details.key(KeyCode::End);
        details.prepare(Rect::new(0, 0, 100, 60));
        assert_eq!(details.scroll, details.max_scroll());
    }
    for (width, height) in [(1, 1), (2, 2), (10, 3)] {
        let mut details = Details::new(content.clone());
        details.prepare(Rect::new(0, 0, width, height));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| details.render(frame)).unwrap();
    }
}
