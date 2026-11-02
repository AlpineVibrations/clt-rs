use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};

use super::{Change, Saving, Value};
use crate::{
    agent::{AgentGitMode, AgentModelTarget, open_agent_store_at},
    test_support::temp_root,
    tui::{
        TuiAgentPanelRefreshWorker, TuiApp, TuiEffect, TuiPane, execute_tui_effect, render_tui,
        tests::tui_agent_project_for_test, update_tui_pane,
    },
};

fn app() -> TuiApp {
    let mut app = TuiApp::new(std::path::Path::new("/tmp/alpha"), false);
    app.agent_panel.projects = vec![
        tui_agent_project_for_test(1, "alpha"),
        tui_agent_project_for_test(2, "beta"),
    ];
    app.agent_panel.state.select(Some(0));
    app
}

fn key(app: &mut TuiApp, code: char) {
    assert_eq!(
        update_tui_pane(app, KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE)),
        Some(Vec::new())
    );
}

fn finish(app: &mut TuiApp) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(message) = app.agent_settings.poll(&mut app.agent_panel) {
            return message;
        }
        assert!(Instant::now() < deadline, "settings writer did not finish");
        thread::yield_now();
    }
}

#[test]
fn slow_save_keeps_settings_navigation_and_rendering_responsive() {
    let mut app = app();
    key(&mut app, 't');
    assert_eq!(
        app.agent_panel.projects[0]
            .project
            .codex_reasoning_effort
            .as_deref(),
        Some("low")
    );
    let (started, started_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    app.agent_settings.start_with(move |value| {
        assert!(matches!(value, Value::Codex { project_id: 1, .. }));
        started.send(()).unwrap();
        release_rx.recv().unwrap();
        Ok(())
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // The writer cannot finish until after these input and rendering assertions.
    for _ in 0..3 {
        key(&mut app, 't');
    }
    key(&mut app, 'f');
    key(&mut app, 'g');
    key(&mut app, ' ');
    let project = &app.agent_panel.projects[0].project;
    assert_eq!(project.codex_reasoning_effort.as_deref(), Some("xhigh"));
    assert!(project.codex_fast_enabled);
    assert_eq!(project.git_mode, AgentGitMode::Commit);
    assert!(!project.enabled);
    assert_eq!(
        app.agent_settings.queued.len(),
        3,
        "rapid Codex edits must coalesce"
    );
    assert!(app.agent_settings.poll(&mut app.agent_panel).is_none());
    app.agent_settings
        .start_with(|_| panic!("must not start a concurrent writer"));

    let effects =
        update_tui_pane(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).unwrap();
    assert_eq!(effects, vec![TuiEffect::SyncAgentLog]);
    assert_eq!(app.agent_panel.selected_project().unwrap().project.id, 2);
    key(&mut app, ' ');
    assert!(!app.agent_panel.projects[1].project.enabled);
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| render_tui(frame, &app)).unwrap();
    let rendered: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("Saving settings"));
    assert!(rendered.contains("xhigh"));

    release.send(()).unwrap();
    finish(&mut app);
    // An older completion cannot replace newer input or move the selection.
    assert_eq!(
        app.agent_panel.projects[0]
            .project
            .codex_reasoning_effort
            .as_deref(),
        Some("xhigh")
    );
    assert_eq!(app.agent_panel.selected_project().unwrap().project.id, 2);
    assert!(
        matches!(&app.agent_settings.queued[0].before, Value::Codex { thinking, .. } if thinking.as_deref() == Some("low"))
    );
}

#[test]
fn rapid_toggle_and_cycles_persist_the_latest_values_for_the_original_project() {
    let state = temp_root("tui-settings-persist");
    let store = open_agent_store_at(&state).unwrap();
    let mut app = app();
    for entry in &mut app.agent_panel.projects {
        store
            .register_project_blocking(&entry.project.path, &entry.project.name)
            .unwrap();
        entry.project = store
            .list_projects_blocking()
            .unwrap()
            .into_iter()
            .find(|project| project.path == entry.project.path)
            .unwrap();
    }
    let original_enabled = app.agent_panel.projects[0].project.enabled;
    key(&mut app, ' ');
    key(&mut app, ' ');
    key(&mut app, ' ');
    for _ in 0..3 {
        key(&mut app, 't');
    }
    key(&mut app, 'f');
    key(&mut app, 'g');
    key(&mut app, 'g');
    let project_id = app.agent_panel.projects[0].project.id;
    app.agent_panel.state.select(Some(1));
    while app.agent_settings.is_pending() {
        let state = state.clone();
        app.agent_settings
            .start_with(move |value| value.save(&open_agent_store_at(&state)?));
        assert!(finish(&mut app).starts_with("Saved:"));
    }
    let projects = store.list_projects_blocking().unwrap();
    let saved = projects.iter().find(|p| p.id == project_id).unwrap();
    assert_eq!(saved.enabled, !original_enabled);
    assert_eq!(saved.codex_reasoning_effort.as_deref(), Some("high"));
    assert!(saved.codex_fast_enabled);
    assert_eq!(saved.git_mode, AgentGitMode::CommitAndPush);
    let other = &app.agent_panel.projects[1].project;
    let saved_other = projects.iter().find(|p| p.id == other.id).unwrap();
    assert_eq!(saved_other.enabled, other.enabled);
    assert_eq!(saved_other.git_mode, other.git_mode);
    assert_eq!(
        saved_other.codex_reasoning_effort,
        other.codex_reasoning_effort
    );
    assert_eq!(saved_other.codex_fast_enabled, other.codex_fast_enabled);
    drop(store);
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn stale_refreshes_are_rejected_during_and_after_saving() {
    let mut app = app();
    let initial_revision = app.agent_settings.revision;
    key(&mut app, 'g');
    assert!(!app.agent_settings.accepts_refresh(initial_revision));
    let saving_revision = app.agent_settings.revision;
    assert!(!app.agent_settings.accepts_refresh(saving_revision));
    app.agent_settings.start_with(|_| Ok(()));
    finish(&mut app);
    assert!(!app.agent_settings.accepts_refresh(initial_revision));
    assert!(!app.agent_settings.accepts_refresh(saving_revision));
    assert!(
        app.agent_settings
            .accepts_refresh(app.agent_settings.revision)
    );
    assert_eq!(
        app.agent_panel.projects[0].project.git_mode,
        AgentGitMode::Commit
    );
}

#[test]
fn failed_save_rolls_back_without_losing_newer_input_and_cancels_quit() {
    let mut app = app();
    key(&mut app, 't');
    app.agent_settings
        .start_with(|_| anyhow::bail!("database locked"));
    key(&mut app, 't');
    app.agent_settings.quit_requested = true;
    assert!(finish(&mut app).contains("database locked"));
    assert!(!app.agent_settings.quit_requested);
    assert_eq!(
        app.agent_panel.projects[0]
            .project
            .codex_reasoning_effort
            .as_deref(),
        Some("medium")
    );
    app.agent_settings
        .start_with(|_| anyhow::bail!("still locked"));
    finish(&mut app);
    assert_eq!(
        app.agent_panel.projects[0].project.codex_reasoning_effort,
        None
    );
    assert!(!app.agent_settings.is_pending());
    assert!(
        app.agent_settings
            .error
            .as_deref()
            .unwrap()
            .contains("still locked")
    );
}

#[test]
fn quit_waits_for_pending_saves_and_disconnected_writers_report_failure() {
    let mut app = app();
    key(&mut app, ' ');
    let mut refresh = TuiAgentPanelRefreshWorker::new();
    let mut panel_time = Instant::now();
    let mut log_time = Instant::now();
    assert!(!execute_tui_effect(
        &mut app,
        TuiEffect::Quit,
        &mut refresh,
        &mut panel_time,
        &mut log_time
    ));
    assert!(app.agent_settings.quit_requested);
    app.agent_settings.start_with(|_| Ok(()));
    finish(&mut app);
    assert!(app.agent_settings.quit_requested && !app.agent_settings.is_pending());
    assert!(execute_tui_effect(
        &mut app,
        TuiEffect::Quit,
        &mut refresh,
        &mut panel_time,
        &mut log_time
    ));

    let (sender, result) = mpsc::channel();
    drop(sender);
    app.agent_settings.saving = Some(Saving {
        change: Change {
            before: Value::Enabled(1, false),
            after: Value::Enabled(1, true),
            label: "agent ON".into(),
        },
        result,
    });
    assert!(finish(&mut app).contains("disconnected"));
    assert!(!app.agent_settings.quit_requested);
}

#[test]
fn a_newer_success_clears_only_the_same_settings_failure() {
    let mut app = app();
    key(&mut app, 't');
    app.agent_settings
        .start_with(|_| anyhow::bail!("database locked"));
    key(&mut app, 't');
    finish(&mut app);
    key(&mut app, 'g');
    assert!(app.agent_settings.error.is_some());
    app.agent_settings.start_with(|_| Ok(()));
    finish(&mut app);
    assert!(app.agent_settings.error.is_none());
    assert_eq!(
        app.agent_panel.projects[0]
            .project
            .codex_reasoning_effort
            .as_deref(),
        Some("medium")
    );
    app.agent_settings
        .start_with(|_| anyhow::bail!("Git save failed"));
    finish(&mut app);
    key(&mut app, 't');
    app.agent_settings.start_with(|_| Ok(()));
    finish(&mut app);
    assert!(
        app.agent_settings
            .error
            .as_deref()
            .unwrap()
            .contains("Git save failed")
    );
}

#[test]
fn supervisor_and_project_model_edits_use_cached_targets_and_ignore_key_repeats() {
    let mut app = app();
    app.agent_panel.enabled_models = vec![AgentModelTarget {
        provider_id: "local".into(),
        model_id: "example".into(),
        label: "Example".into(),
        enabled: true,
        favorite: false,
        reasoning_effort: None,
    }];
    key(&mut app, 'm');
    assert_eq!(
        app.agent_panel.projects[0].project.codex_model.as_deref(),
        Some("example")
    );
    let mut repeat = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE);
    repeat.kind = KeyEventKind::Repeat;
    assert_eq!(update_tui_pane(&mut app, repeat), Some(Vec::new()));
    assert_eq!(
        app.agent_panel.projects[0].project.codex_model.as_deref(),
        Some("example")
    );
    key(&mut app, 'u');
    key(&mut app, ' ');
    key(&mut app, 't');
    key(&mut app, 'f');
    key(&mut app, 'm');
    let settings = &app.agent_panel.supervisor_settings;
    assert!(settings.enabled && settings.fast);
    assert_eq!(settings.model.as_deref(), Some("example"));
    assert_eq!(settings.thinking.as_deref(), Some("xhigh"));
    assert_eq!(app.agent_settings.queued.len(), 2);
    assert_eq!(
        app.agent_panel.projects[0].project.codex_reasoning_effort,
        None
    );
    key(&mut app, 'm');
    assert_eq!(app.agent_panel.supervisor_settings.model, None);
    let effects = update_tui_pane(
        &mut app,
        KeyEvent::new(KeyCode::Char('M'), KeyModifiers::SHIFT),
    )
    .unwrap();
    assert!(!effects.is_empty());
    assert_eq!(app.current_pane, TuiPane::AgentProjects); // Supervisor delegates M to the terminal effect.
}
