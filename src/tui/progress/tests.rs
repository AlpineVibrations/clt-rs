use super::*;
use crate::{
    task::TaskSource,
    tui::{TuiPane, render_tui, update_tui_pane},
};

fn app() -> TuiApp {
    let mut app = TuiApp::new(PathBuf::from("/tmp/progress-test").as_path(), true);
    app.task_snapshot.board_entries[1].push(TaskEntry {
        summary: "Implement summary".into(),
        content: "Implement summary codex:session".into(),
        source: TaskSource::MarkdownLine { line_index: 0 },
        metadata: None,
        has_subtasks: false,
    });
    app.selected_board = 1;
    app.board_states[1].select(Some(0));
    app
}

#[test]
fn summary_key_is_read_only_and_stop_has_an_explicit_key() {
    use crossterm::event::KeyModifiers;
    let mut app = app();
    assert_eq!(
        update_tui_pane(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)
        ),
        Some(vec![TuiEffect::RefreshTaskSummary])
    );
    for stop in [
        KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::SHIFT),
    ] {
        assert_eq!(
            update_tui_pane(&mut app, stop),
            Some(vec![TuiEffect::ToggleTaskStop])
        );
    }
    app.current_pane = TuiPane::AgentProjects;
    let project_stop = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
    assert_eq!(
        update_tui_pane(&mut app, project_stop),
        Some(vec![TuiEffect::PaneKey(project_stop)])
    );
}

#[test]
fn summary_dialog_renders_refreshes_and_closes_without_mutations() {
    use crossterm::event::KeyModifiers;
    let mut app = app();
    let task = app.task_snapshot.board_entries[1][0].clone();
    let (sender, receiver) = mpsc::channel();
    app.task_summary = Some(TaskSummary {
        root: app.active_root.clone(),
        task,
        content: "Reading progress".into(),
        pending: Some(receiver),
        scroll: 0,
    });
    sender
        .send(Ok(
            "Agent: running\nLatest update: Running regression tests.".into(),
        ))
        .unwrap();
    app.poll_task_summary();
    assert!(app.task_summary.as_ref().unwrap().pending.is_none());
    for (width, height) in [(90, 24), (40, 10), (10, 3)] {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_tui(frame, &app)).unwrap();
        if width == 90 {
            let rendered: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(rendered.contains("Task progress"));
            assert!(rendered.contains("Running regression tests."));
        }
    }
    assert_eq!(
        update_tui_pane(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)
        ),
        Some(vec![TuiEffect::RefreshTaskSummary])
    );
    assert_eq!(
        update_tui_pane(
            &mut app,
            KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT)
        ),
        Some(vec![])
    );
    update_tui_pane(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.task_summary.is_none());
    assert_eq!(app.board_states[1].selected(), Some(0));
    assert_eq!(
        app.task_snapshot.board_entries[1][0].content,
        "Implement summary codex:session"
    );
}
