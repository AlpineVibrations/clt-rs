use crate::test_support::prelude::*;
use crate::test_support::*;

#[test]
fn tui_requests_unambiguous_reporting_for_every_key() {
    let flags = tui_keyboard_enhancement_flags();

    assert!(flags.contains(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    assert!(flags.contains(KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES));
    assert!(!flags.contains(KeyboardEnhancementFlags::REPORT_EVENT_TYPES));
}

#[test]
fn initialization_prompt_accepts_y_or_n_without_enter() {
    let key = |code, modifiers| crossterm::event::KeyEvent::new(code, modifiers);

    assert_eq!(
        initialization_prompt_choice(&key(KeyCode::Char('y'), KeyModifiers::NONE)),
        Some(true)
    );
    assert_eq!(
        initialization_prompt_choice(&key(KeyCode::Char('Y'), KeyModifiers::SHIFT)),
        Some(true)
    );
    assert_eq!(
        initialization_prompt_choice(&key(KeyCode::Char('n'), KeyModifiers::NONE)),
        Some(false)
    );
    assert_eq!(
        initialization_prompt_choice(&key(KeyCode::Char('N'), KeyModifiers::SHIFT)),
        Some(false)
    );
    assert_eq!(
        initialization_prompt_choice(&key(KeyCode::Enter, KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        initialization_prompt_choice(&key(KeyCode::Char('y'), KeyModifiers::CONTROL)),
        None
    );
}

#[test]
fn tui_reorder_shortcuts_support_shift_arrows_and_control_previous_next() {
    let key = |code, modifiers| crossterm::event::KeyEvent::new(code, modifiers);

    assert_eq!(
        tui_task_reorder_direction(&key(KeyCode::Up, KeyModifiers::SHIFT)),
        Some(TuiTaskReorderDirection::Up)
    );
    assert_eq!(
        tui_task_reorder_direction(&key(KeyCode::Down, KeyModifiers::SHIFT)),
        Some(TuiTaskReorderDirection::Down)
    );
    assert_eq!(
        tui_task_reorder_direction(&key(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        Some(TuiTaskReorderDirection::Up)
    );
    assert_eq!(
        tui_task_reorder_direction(&key(KeyCode::Char('n'), KeyModifiers::CONTROL)),
        Some(TuiTaskReorderDirection::Down)
    );
    assert_eq!(
        tui_task_reorder_direction(&key(KeyCode::Up, KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        tui_task_reorder_direction(&key(KeyCode::Char('p'), KeyModifiers::NONE)),
        None
    );
}

#[test]
fn tui_subtask_shortcuts_support_n_and_both_terminal_forms_of_plus() {
    let key = |code, modifiers| crossterm::event::KeyEvent::new(code, modifiers);

    assert!(tui_starts_subtask_input(&key(
        KeyCode::Char('n'),
        KeyModifiers::NONE
    )));
    assert!(tui_starts_subtask_input(&key(
        KeyCode::Char('+'),
        KeyModifiers::NONE
    )));
    assert!(tui_starts_subtask_input(&key(
        KeyCode::Char('+'),
        KeyModifiers::SHIFT
    )));
    assert!(!tui_starts_subtask_input(&key(
        KeyCode::Char('n'),
        KeyModifiers::CONTROL
    )));
    assert_eq!(
        tui_task_reorder_direction(&key(KeyCode::Char('n'), KeyModifiers::CONTROL)),
        Some(TuiTaskReorderDirection::Down)
    );
}

#[test]
fn tui_task_prompt_cancel_shortcuts_support_escape_and_control_c() {
    let key = |code, modifiers| crossterm::event::KeyEvent::new(code, modifiers);

    assert!(tui_cancels_task_prompt(&key(
        KeyCode::Esc,
        KeyModifiers::NONE
    )));
    assert!(tui_cancels_task_prompt(&key(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL
    )));
    assert!(tui_cancels_task_prompt(&key(
        KeyCode::Char('C'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
    assert!(!tui_cancels_task_prompt(&key(
        KeyCode::Char('c'),
        KeyModifiers::NONE
    )));
    assert!(!tui_cancels_task_prompt(&key(
        KeyCode::Char('c'),
        KeyModifiers::ALT
    )));
}

#[test]
fn tui_reorganize_prefix_and_arrows_are_unambiguous() {
    let key = |code, modifiers| crossterm::event::KeyEvent::new(code, modifiers);

    assert!(tui_toggles_reorganize_mode(&key(
        KeyCode::Char('r'),
        KeyModifiers::NONE
    )));
    assert!(tui_toggles_reorganize_mode(&key(
        KeyCode::Char('R'),
        KeyModifiers::SHIFT
    )));
    assert!(!tui_toggles_reorganize_mode(&key(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL
    )));

    for (code, direction) in [
        (KeyCode::Up, TuiTaskReorganizeDirection::Up),
        (KeyCode::Down, TuiTaskReorganizeDirection::Down),
        (KeyCode::Left, TuiTaskReorganizeDirection::Left),
        (KeyCode::Right, TuiTaskReorganizeDirection::Right),
    ] {
        assert_eq!(
            tui_task_reorganize_direction(&key(code, KeyModifiers::NONE)),
            Some(direction)
        );
    }
    assert_eq!(
        tui_task_reorganize_direction(&key(KeyCode::Esc, KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        tui_task_reorganize_direction(&key(KeyCode::Char('x'), KeyModifiers::NONE)),
        None
    );

    assert_eq!(
        tui_reorganize_input(&key(KeyCode::Char('r'), KeyModifiers::NONE)),
        TuiReorganizeInput::Exit
    );
    assert_eq!(
        tui_reorganize_input(&key(KeyCode::Esc, KeyModifiers::NONE)),
        TuiReorganizeInput::Exit
    );
    assert_eq!(
        tui_reorganize_input(&key(KeyCode::Down, KeyModifiers::NONE)),
        TuiReorganizeInput::Move(TuiTaskReorganizeDirection::Down)
    );
    assert_eq!(
        tui_reorganize_input(&key(KeyCode::Char('x'), KeyModifiers::NONE)),
        TuiReorganizeInput::Ignore
    );
}

#[test]
fn tui_reorganize_mode_has_a_distinct_title_and_border_color() {
    assert_eq!(
        tui_task_column_title("To Do", true, true),
        " REORGANIZE MODE: To Do [r/Esc exits] "
    );
    assert_eq!(
        tui_task_column_title("To Do", true, false),
        "To Do   <<<<<< * >>>>>>     "
    );
    assert_eq!(tui_task_column_title("Doing", false, true), "Doing");
    assert_eq!(
        tui_task_column_border_color(Color::Indexed(110), true),
        Color::Yellow
    );
    assert_eq!(
        tui_task_column_border_color(Color::Indexed(110), false),
        Color::Indexed(110)
    );
}

#[test]
fn tui_task_column_keeps_controls_on_title_line_without_reserving_a_row() {
    use ratatui::backend::TestBackend;

    let backend = TestBackend::new(36, 5);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|frame| {
            let task_area = render_tui_task_column_header(
                frame,
                frame.area(),
                "To Do",
                1,
                true,
                false,
                Color::Indexed(110),
            );
            frame.render_widget(Paragraph::new("1. task"), task_area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let rows = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect::<Vec<_>>();

    assert!(rows[0].contains("To Do"));
    assert!(rows[0].contains("<<<<<< * >>>>>>"));
    assert!(rows[1].contains("1. task"));
}

#[test]
fn tui_reorder_action_moves_the_selected_task_and_selection() {
    let root = temp_root("tui-reorder-action");
    add_task(&root, "alpha", None).unwrap();
    add_task(&root, "beta", None).unwrap();
    let board_dir = root.join("tasks");
    let mut state = ListState::default();
    state.select(Some(0));

    let message = reorder_selected_tui_task(
        &board_dir,
        TaskStatus::Todo,
        &mut state,
        TuiTaskReorderDirection::Down,
    );

    assert_eq!(message, "Moved task down to position 2");
    assert_eq!(state.selected(), Some(1));
    assert_eq!(
        read_tasks(&root, "todo").unwrap(),
        vec!["- beta", "- alpha"]
    );

    let message = reorder_selected_tui_task(
        &board_dir,
        TaskStatus::Todo,
        &mut state,
        TuiTaskReorderDirection::Up,
    );

    assert_eq!(message, "Moved task up to position 1");
    assert_eq!(state.selected(), Some(0));
    assert_eq!(
        read_tasks(&root, "todo").unwrap(),
        vec!["- alpha", "- beta"]
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tui_reorganize_action_moves_the_selected_task_between_boards() {
    let root = temp_root("tui-reorganize-horizontal-action");
    add_task(&root, "alpha", None).unwrap();
    let board_dir = root.join("tasks");
    let mut board_states = [
        ListState::default(),
        ListState::default(),
        ListState::default(),
        ListState::default(),
    ];
    let mut selected_board = TODO_BOARD_INDEX;
    board_states[selected_board].select(Some(0));

    let message = reorganize_selected_tui_task(
        &board_dir,
        &TASK_STATUSES,
        &mut board_states,
        &mut selected_board,
        false,
        TuiTaskReorganizeDirection::Right,
    );

    assert_eq!(message, "Moved task to doing");
    assert_eq!(selected_board, 1);
    assert_eq!(board_states[selected_board].selected(), Some(0));
    assert!(read_tasks(&root, "todo").unwrap().is_empty());
    assert_eq!(read_tasks(&root, "doing").unwrap(), vec!["- alpha"]);

    let message = reorganize_selected_tui_task(
        &board_dir,
        &TASK_STATUSES,
        &mut board_states,
        &mut selected_board,
        false,
        TuiTaskReorganizeDirection::Left,
    );

    assert_eq!(message, "Moved task to todo");
    assert_eq!(selected_board, TODO_BOARD_INDEX);
    assert_eq!(board_states[selected_board].selected(), Some(0));
    assert_eq!(read_tasks(&root, "todo").unwrap(), vec!["- alpha"]);
    assert!(read_tasks(&root, "doing").unwrap().is_empty());

    fs::remove_dir_all(root).unwrap();
}

pub(crate) fn tui_agent_project_for_test(id: i64, name: &str) -> TuiAgentProject {
    TuiAgentProject {
        project: agent::AgentProject {
            id,
            path: PathBuf::from(format!("/tmp/{name}")),
            name: name.to_string(),
            enabled: true,
            git_mode: AgentGitMode::Off,
            codex_provider: None,
            codex_model: None,
            codex_reasoning_effort: None,
            codex_fast_enabled: false,
            last_scan_at: None,
            last_daemon_scan_status: None,
            last_daemon_scan_error: None,
            last_run_at: None,
            last_success_at: None,
            last_failure_at: None,
            last_blocked_recovery_at: None,
            failure_count: 0,
        },
        scan: AgentProjectScan::empty(),
        runtime_state: TuiAgentRuntimeState::Idle,
        daemon_scan_problem: None,
        failure_problem: None,
    }
}

#[test]
fn tui_agent_panel_starts_in_loading_state_without_fetching_a_snapshot() {
    let active_root = PathBuf::from("/tmp/current");

    let panel = TuiAgentPanel::new(&active_root);

    assert!(panel.projects.is_empty());
    assert_eq!(panel.daemon_status, "loading");
    assert_eq!(
        panel
            .selected_current_project_registration()
            .map(|registration| registration.path.as_path()),
        Some(active_root.as_path())
    );
}

#[test]
fn tui_agent_panel_refresh_worker_does_not_block_the_caller() {
    let active_root = PathBuf::from("/tmp/current");
    let (started_sender, started_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let mut worker = TuiAgentPanelRefreshWorker::new();

    assert!(worker.request_with(&active_root, move |active_root| {
        started_sender.send(()).unwrap();
        release_receiver.recv().unwrap();
        TuiAgentPanelRefreshResult {
            active_root,
            panel_snapshot: Ok(TuiAgentPanelSnapshot {
                projects: Vec::new(),
                daemon_status: "running".to_string(),
            }),
            task_session_states: Ok(TaskAgentSessionStates::default()),
        }
    }));
    started_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(worker.try_result().is_none());
    assert!(!worker.request_with(&active_root, |_| unreachable!()));

    release_sender.send(()).unwrap();
    let started = Instant::now();
    let result = loop {
        if let Some(result) = worker.try_result() {
            break result;
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        thread::yield_now();
    };

    assert_eq!(result.active_root, active_root);
    assert_eq!(result.panel_snapshot.unwrap().daemon_status, "running");
}

#[test]
fn tui_agent_panel_restore_keeps_scroll_offset_when_selection_still_exists() {
    let mut panel = TuiAgentPanel {
        projects: vec![
            tui_agent_project_for_test(1, "alpha"),
            tui_agent_project_for_test(2, "beta"),
            tui_agent_project_for_test(3, "gamma"),
            tui_agent_project_for_test(4, "delta"),
        ],
        current_project_registration: None,
        daemon_status: "not-installed".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(2));

    panel.restore_or_normalize_selection(Some(TuiAgentPanelRowIdentity::Project(3)));

    assert_eq!(panel.state.selected(), Some(2));
    assert_eq!(panel.scroll_offset, 0);
}

#[test]
fn tui_agent_panel_selects_nearest_row_after_removal() {
    let mut panel = TuiAgentPanel {
        projects: vec![
            tui_agent_project_for_test(1, "alpha"),
            tui_agent_project_for_test(2, "beta"),
        ],
        current_project_registration: None,
        daemon_status: "not-installed".to_string(),
        state: ListState::default(),
        scroll_offset: 1,
        last_error: None,
    };

    panel.projects.pop();
    panel.select_nearest_row(1);

    assert_eq!(panel.state.selected(), Some(0));
    assert_eq!(panel.scroll_offset, 0);
}

#[test]
fn tui_agent_panel_refresh_selects_a_newly_registered_current_project() {
    let active_root = PathBuf::from("/tmp/beta");
    let mut panel = TuiAgentPanel {
        projects: vec![
            tui_agent_project_for_test(1, "alpha"),
            tui_agent_project_for_test(3, "gamma"),
        ],
        current_project_registration: Some(TuiCurrentProjectRegistration {
            path: active_root.clone(),
            name: "beta".to_string(),
        }),
        daemon_status: "not-installed".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));
    let selected_row = panel.selected_row_identity();
    let snapshot = TuiAgentPanelSnapshot {
        projects: vec![
            tui_agent_project_for_test(1, "alpha"),
            tui_agent_project_for_test(2, "beta"),
            tui_agent_project_for_test(3, "gamma"),
        ],
        daemon_status: "running".to_string(),
    };

    panel.apply_refresh_result(&active_root, selected_row, Ok(snapshot));

    assert_eq!(panel.state.selected(), Some(1));
    assert_eq!(panel.selected_project().unwrap().project.name, "beta");
    assert!(panel.current_project_registration.is_none());
}

#[test]
fn tui_agent_panel_refresh_error_preserves_the_last_snapshot() {
    let mut panel = TuiAgentPanel {
        projects: vec![
            tui_agent_project_for_test(1, "alpha"),
            tui_agent_project_for_test(2, "beta"),
        ],
        current_project_registration: None,
        daemon_status: "running".to_string(),
        state: ListState::default(),
        scroll_offset: 1,
        last_error: None,
    };
    panel.state.select(Some(1));
    let selected_row = panel.selected_row_identity();
    let refresh_error = std::io::Error::other("database locked").into();

    panel.apply_refresh_result(Path::new("/tmp/alpha"), selected_row, Err(refresh_error));

    assert_eq!(panel.projects.len(), 2);
    assert_eq!(panel.daemon_status, "running");
    assert_eq!(panel.state.selected(), Some(1));
    assert_eq!(panel.scroll_offset, 1);
    assert_eq!(
        panel.last_error.as_deref(),
        Some("Agent registry unavailable: database locked")
    );
}

#[test]
fn tui_agent_panel_refresh_error_uses_the_red_console() {
    let panel = TuiAgentPanel {
        projects: vec![tui_agent_project_for_test(1, "alpha")],
        current_project_registration: None,
        daemon_status: "running".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: Some("Agent registry unavailable: database locked".to_string()),
    };
    let log_view = TuiAgentLogView::message("alpha".to_string(), "latest log".to_string());

    let (content, color) =
        tui_console_content(true, &panel, Some(&log_view), "Agent pane instructions");

    assert_eq!(content, "Agent registry unavailable: database locked");
    assert_eq!(color, Color::Red);
}

#[test]
fn tui_kanban_console_displays_an_open_agent_log() {
    let panel = TuiAgentPanel {
        projects: Vec::new(),
        current_project_registration: None,
        daemon_status: "running".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    let log_view = TuiAgentLogView::message("alpha".to_string(), "live output".to_string());

    let (content, color) =
        tui_console_content(false, &panel, Some(&log_view), "Kanban instructions");

    assert_eq!(content, "live output");
    assert_eq!(color, Color::Gray);
}

#[test]
fn tui_agent_panel_selects_the_active_project_by_path() {
    let mut panel = TuiAgentPanel {
        projects: vec![
            tui_agent_project_for_test(1, "alpha"),
            tui_agent_project_for_test(2, "beta"),
            tui_agent_project_for_test(3, "gamma"),
        ],
        current_project_registration: None,
        daemon_status: "not-installed".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));
    let active_path = panel.projects[2].project.path.clone();

    panel.select_project_for_path(&active_path);

    assert_eq!(panel.state.selected(), Some(2));
    assert_eq!(panel.selected_project().unwrap().project.name, "gamma");
}

#[test]
fn tui_agent_panel_selects_the_current_project_registration_by_path() {
    let active_path = PathBuf::from("/tmp/current");
    let mut panel = TuiAgentPanel {
        projects: vec![tui_agent_project_for_test(1, "alpha")],
        current_project_registration: Some(TuiCurrentProjectRegistration {
            path: active_path.clone(),
            name: "current".to_string(),
        }),
        daemon_status: "not-installed".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(1));

    panel.select_project_for_path(&active_path);

    assert_eq!(panel.state.selected(), Some(0));
    assert!(panel.selected_current_project_registration().is_some());
}

#[test]
fn tui_agent_panel_uses_auto_refresh_without_command_panel_wording() {
    assert_eq!(
        tui_agent_panel_refresh_interval(),
        Duration::from_secs(TUI_AGENT_PANEL_REFRESH_SECONDS)
    );
    assert!(!tui_agent_panel_instructions().contains("Auto-refreshes"));
    assert!(!tui_agent_panel_instructions().contains("r refresh"));
    assert!(tui_agent_panel_instructions().contains("m cycles the selected target"));
    assert!(tui_agent_panel_instructions().contains("M opens Models"));
    assert!(tui_agent_panel_instructions().contains("f toggles fast"));
    assert!(tui_agent_panel_instructions().contains("t cycles thinking"));
    assert!(tui_agent_panel_instructions().contains("r retries after fixing an error"));
    assert!(tui_agent_panel_instructions().contains("l shows output"));
    assert!(tui_agent_panel_instructions().contains("g cycles Git off/commit/push"));
    assert!(tui_agent_panel_instructions().contains("Delete removes with confirmation"));
}

#[test]
fn tui_agent_project_removal_requires_confirmation_and_only_unregisters() {
    let root = temp_root("tui-agent-remove");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let mut panel = TuiAgentPanel {
        projects: vec![TuiAgentProject {
            project,
            scan: AgentProjectScan::empty(),
            runtime_state: TuiAgentRuntimeState::Idle,
            daemon_scan_problem: None,
            failure_problem: None,
        }],
        current_project_registration: None,
        daemon_status: "not-installed".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));

    let removal = selected_tui_agent_project_removal(&panel).unwrap();
    assert!(tui_agent_project_removal_prompt(&removal).contains("Press y to confirm"));
    let message = remove_tui_agent_project_with_store(
        &mut panel,
        &project_root,
        &removal,
        &store,
        &state_dir,
    )
    .unwrap();

    assert_eq!(message, "Removed agent project: project");
    assert!(store.list_projects_blocking().unwrap().is_empty());
    assert!(project_root.exists());
    assert!(panel.projects.is_empty());
    assert!(panel.selected_current_project_registration().is_some());
    assert_eq!(panel.state.selected(), Some(0));

    fs::remove_dir_all(root).unwrap();
}

struct TuiWorkingProjectRemovalFixture {
    root: PathBuf,
    state_dir: PathBuf,
    project_root: PathBuf,
    store: agent::TursoAgentStore,
    panel: TuiAgentPanel,
    journal: agent::GitFinalizationRecord,
    head: String,
}

fn tui_working_project_removal_fixture(protection: &str) -> TuiWorkingProjectRemovalFixture {
    let root = temp_root(&format!("tui-working-project-removal-{protection}"));
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    add_task(&project_root, "Keep the project files", None).unwrap();
    fs::write(project_root.join("work.txt"), "Keep completed code\n").unwrap();
    let session_id = "session-tui-project-removal";
    if protection == "marker" {
        fs::write(
            project_root.join("tasks/done.md"),
            format!("# Done Tasks\n- Prior task codex:{session_id}\n"),
        )
        .unwrap();
    }
    let head = initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    let mut baseline: serde_json::Value = serde_json::from_str(&start.worktree_baseline).unwrap();
    if protection == "sealed" {
        baseline["staged_index_tree"] =
            serde_json::json!(run_test_git(&project_root, &["rev-parse", "HEAD^{tree}"]));
        baseline["manifest_parent_head"] = serde_json::json!(head);
        baseline["staged_non_task_patch_ids"] = serde_json::json!({});
    }
    let task_identity =
        (protection == "bound").then(|| durable_task_identity("Keep the project files").unwrap());
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    store
        .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
        .unwrap();
    store
        .set_project_enabled_for_path_blocking(&project_root, false)
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: session_id,
                git_mode: AgentGitMode::Commit,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &serde_json::to_string(&baseline).unwrap(),
                task_identity: task_identity.as_deref(),
                owner_run_token: None,
                created_at: "100",
            })
            .unwrap()
    );
    store
        .set_session_control_recovery_token_blocking(
            project.id,
            session_id,
            "clt-git-finalization:0",
        )
        .unwrap();
    let journal = store
        .git_finalization_blocking(project.id, session_id)
        .unwrap()
        .unwrap();
    let mut panel = TuiAgentPanel {
        projects: vec![TuiAgentProject {
            project,
            scan: AgentProjectScan::empty(),
            runtime_state: TuiAgentRuntimeState::Finalizing,
            daemon_scan_problem: None,
            failure_problem: None,
        }],
        current_project_registration: None,
        daemon_status: "not-installed".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));
    TuiWorkingProjectRemovalFixture {
        root,
        state_dir,
        project_root,
        store,
        panel,
        journal,
        head,
    }
}

#[test]
fn tui_agent_project_removal_retires_idle_orphan_journal_and_preserves_files() {
    let mut fixture = tui_working_project_removal_fixture("orphan");
    let todo_before = fs::read(fixture.project_root.join("tasks/todo.md")).unwrap();
    let removal = selected_tui_agent_project_removal(&fixture.panel).unwrap();

    let message = remove_tui_agent_project_with_store(
        &mut fixture.panel,
        &fixture.project_root,
        &removal,
        &fixture.store,
        &fixture.state_dir,
    )
    .unwrap();

    assert_eq!(message, "Removed agent project: project");
    assert!(fixture.store.list_projects_blocking().unwrap().is_empty());
    assert!(
        fixture
            .store
            .git_finalization_blocking(
                fixture.journal.project_id,
                &fixture.journal.codex_session_id,
            )
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .store
            .session_control_blocking(
                fixture.journal.project_id,
                &fixture.journal.codex_session_id,
            )
            .unwrap()
            .is_none()
    );
    assert!(fixture.panel.projects.is_empty());
    assert!(
        fixture
            .panel
            .selected_current_project_registration()
            .is_some()
    );
    assert_eq!(
        fs::read(fixture.project_root.join("tasks/todo.md")).unwrap(),
        todo_before
    );
    assert_eq!(
        fs::read(fixture.project_root.join("work.txt")).unwrap(),
        b"Keep completed code\n"
    );
    assert_eq!(
        run_test_git(&fixture.project_root, &["rev-parse", "HEAD"]),
        fixture.head
    );
    assert!(run_test_git(&fixture.project_root, &["status", "--porcelain"]).is_empty());
    drop(fixture.store);
    fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn tui_agent_project_removal_preserves_bound_sealed_and_marker_linked_journals() {
    for protection in ["bound", "sealed", "marker"] {
        let mut fixture = tui_working_project_removal_fixture(protection);
        let removal = selected_tui_agent_project_removal(&fixture.panel).unwrap();

        let result = remove_tui_agent_project_with_store(
            &mut fixture.panel,
            &fixture.project_root,
            &removal,
            &fixture.store,
            &fixture.state_dir,
        );

        assert!(result.is_err(), "protection={protection}");
        let projects = fixture.store.list_projects_blocking().unwrap();
        assert_eq!(projects.len(), 1, "protection={protection}");
        assert_eq!(projects[0].id, fixture.journal.project_id);
        assert_eq!(
            fixture
                .store
                .git_finalization_blocking(
                    fixture.journal.project_id,
                    &fixture.journal.codex_session_id,
                )
                .unwrap()
                .unwrap(),
            fixture.journal,
            "protection={protection}"
        );
        assert!(
            fixture
                .store
                .session_control_blocking(
                    fixture.journal.project_id,
                    &fixture.journal.codex_session_id,
                )
                .unwrap()
                .is_some()
        );
        assert_eq!(fixture.panel.projects.len(), 1);
        assert_eq!(fixture.panel.state.selected(), Some(0));
        assert_eq!(
            fixture.panel.projects[0].project.id,
            fixture.journal.project_id
        );
        assert_eq!(
            fs::read(fixture.project_root.join("work.txt")).unwrap(),
            b"Keep completed code\n"
        );
        assert_eq!(
            run_test_git(&fixture.project_root, &["rev-parse", "HEAD"]),
            fixture.head
        );
        assert!(run_test_git(&fixture.project_root, &["status", "--porcelain"]).is_empty());
        drop(fixture.store);
        fs::remove_dir_all(fixture.root).unwrap();
    }
}

#[test]
fn agent_log_console_expands_and_scrolls_to_latest_output() {
    assert_eq!(tui_feedback_console_height(40, 80, "short", false), 3);
    assert_eq!(
        tui_feedback_console_height(40, 12, "a message that wraps", false),
        4
    );
    assert_eq!(
        tui_feedback_console_height(40, 80, "one\ntwo\nthree", false),
        5
    );
    assert_eq!(
        tui_feedback_console_height(40, 12, &"x".repeat(1_000), false),
        20
    );
    assert_eq!(tui_feedback_console_height(40, 80, "short", true), 20);
    assert_eq!(tui_log_scroll_offset("one\ntwo\nthree\nfour", 2), 2);
}

#[test]
fn latest_agent_log_path_uses_newest_file_with_requested_extension() {
    let root = temp_root("agent-latest-log");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("100-000-p1-1.out"), "older").unwrap();
    fs::write(root.join("200-000-p1-1.out"), "newer").unwrap();
    fs::write(root.join("300-000-p1-1.err"), "latest progress").unwrap();

    assert_eq!(
        latest_agent_log_path(&root, "out").unwrap(),
        Some(root.join("200-000-p1-1.out"))
    );
    assert_eq!(
        latest_agent_log_path(&root, "err").unwrap(),
        Some(root.join("300-000-p1-1.err"))
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recorded_agent_output_falls_back_to_stderr_when_stdout_is_empty() {
    let root = temp_root("agent-recorded-output-fallback");
    fs::create_dir_all(&root).unwrap();
    let stdout_path = root.join("run.out");
    let stderr_path = root.join("run.err");
    fs::write(&stdout_path, "").unwrap();
    fs::write(&stderr_path, "agent progress").unwrap();
    let run = agent::AgentRunRecord {
        id: 1,
        project_id: 1,
        project_name: "alpha".to_string(),
        project_path: root.clone(),
        status: "success".to_string(),
        started_at: "100".to_string(),
        finished_at: Some("101".to_string()),
        exit_code: Some(0),
        stdout_path: Some(stdout_path.display().to_string()),
        stderr_path: Some(stderr_path.display().to_string()),
        summary: Some("completed".to_string()),
        codex_session_id: Some("session-recorded".to_string()),
    };

    assert_eq!(
        preferred_recorded_agent_output_path(&run),
        Some(stderr_path)
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn running_agent_log_view_streams_the_current_output_file() {
    let root = temp_root("agent-live-output");
    let state_dir = root.join("state/clt");
    let mut project = tui_agent_project_for_test(1, "alpha");
    project.runtime_state = TuiAgentRuntimeState::Running;
    project.project.codex_model = Some("new-project-default".to_string());
    project.project.codex_reasoning_effort = Some("low".to_string());
    let log_dir = agent_project_run_log_dir(&state_dir, &project.project).unwrap();
    fs::create_dir_all(&log_dir).unwrap();
    let stdout_path = log_dir.join("200-000-p1-1.out");
    let stderr_path = log_dir.join("200-000-p1-1.err");
    fs::write(&stdout_path, "").unwrap();
    let header = "Reading additional input from stdin...\nOpenAI Codex v0.153.3\n--------\nmodel: gpt-6-astra\nreasoning effort: xhigh\nsession id: session-live\n";
    fs::write(&stderr_path, header).unwrap();

    let mut panel = TuiAgentPanel {
        projects: vec![project],
        current_project_registration: None,
        daemon_status: "running".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));

    let mut log_view = selected_tui_agent_log_view_at(&panel, &state_dir)
        .unwrap()
        .unwrap();
    assert!(log_view.is_live);
    assert!(tui_agent_log_title(&log_view).contains("[LIVE]"));
    assert!(tui_agent_log_title(&log_view).contains("s/i/c controls"));
    assert_eq!(log_view.content, header);
    assert_eq!(
        log_view.settings_label(),
        " Model: unknown | Thinking: unknown "
    );
    assert_eq!(
        viewed_tui_codex_session_target(Some(&log_view)).unwrap(),
        TuiCodexSessionTarget {
            project_id: 1,
            project_path: panel.projects[0].project.path.clone(),
            session_id: "session-live".to_string(),
        }
    );

    append_agent_log_line(&stderr_path, "--------\nstarted\nstill working").unwrap();
    log_view.refresh().unwrap();
    assert!(log_view.content.contains("still working"));
    assert_eq!(
        log_view.settings_label(),
        " Model: gpt-6-astra | Thinking: xhigh "
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn fenced_agent_log_view_keeps_the_orphaned_session_controllable() {
    let root = temp_root("agent-fenced-output");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "fenced-project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let stdout_path = root.join("fenced.out");
    let stderr_path = root.join("fenced.err");
    fs::write(&stdout_path, "").unwrap();
    fs::write(
        &stderr_path,
        "session id: session-fenced\nwork survived its supervisor\n",
    )
    .unwrap();
    store
        .mark_session_running_blocking(
            project.id,
            "session-fenced",
            4242,
            "orphaned-run-token",
            &stdout_path,
            &stderr_path,
        )
        .unwrap();

    assert_eq!(
        store.suspending_session_project_ids_blocking().unwrap(),
        HashSet::from([project.id])
    );
    let mut panel = TuiAgentPanel {
        projects: vec![TuiAgentProject {
            project,
            scan: AgentProjectScan::empty(),
            runtime_state: TuiAgentRuntimeState::Fenced,
            daemon_scan_problem: None,
            failure_problem: None,
        }],
        current_project_registration: None,
        daemon_status: "running".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));

    let log_view = selected_tui_agent_log_view_at(&panel, &state_dir)
        .unwrap()
        .unwrap();
    assert!(log_view.is_live);
    assert!(log_view.content.contains("work survived its supervisor"));
    let target = viewed_tui_codex_session_target(Some(&log_view)).unwrap();
    assert_eq!(target.session_id, "session-fenced");
    assert!(tui_agent_log_title(&log_view).contains("s/i/c controls"));

    let message =
        toggle_tui_codex_session_stop_at(&state_dir, target.project_id, &target.session_id)
            .unwrap();
    assert!(message.starts_with("Stopping this Codex task session"));
    assert_eq!(
        store
            .session_control_blocking(target.project_id, &target.session_id)
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::StopRequested
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn kanban_agent_log_view_uses_the_active_project_for_selected_doing_task() {
    let root = temp_root("kanban-agent-log");
    let state_dir = root.join("state/clt");
    let mut alpha = tui_agent_project_for_test(1, "alpha");
    alpha.runtime_state = TuiAgentRuntimeState::Running;
    let active_path = alpha.project.path.clone();
    let log_dir = agent_project_run_log_dir(&state_dir, &alpha.project).unwrap();
    fs::create_dir_all(&log_dir).unwrap();
    fs::write(
        log_dir.join("200-000-p1-1.err"),
        "session id: session-live\nalpha is working\n",
    )
    .unwrap();

    let mut panel = TuiAgentPanel {
        projects: vec![alpha, tui_agent_project_for_test(2, "beta")],
        current_project_registration: None,
        daemon_status: "running".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(1));
    let task = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 1 },
        "Current task",
        "Current task codex:session-live",
        false,
    );

    let log_view = selected_tui_task_log_view_for_path_at(
        &mut panel,
        &active_path,
        TaskStatus::Doing,
        &task,
        &state_dir,
    )
    .unwrap()
    .unwrap();

    assert_eq!(panel.selected_project().unwrap().project.name, "alpha");
    assert_eq!(log_view.project_name, "alpha");
    assert!(log_view.content.contains("alpha is working"));
    assert!(log_view.is_live);

    panel.select_next();
    let project_log_view = selected_tui_task_or_project_log_view_for_path_at(
        &mut panel,
        &active_path,
        TaskStatus::Doing,
        None,
        &state_dir,
    )
    .unwrap()
    .unwrap();
    assert_eq!(panel.selected_project().unwrap().project.name, "alpha");
    assert!(project_log_view.content.contains("alpha is working"));
    assert!(project_log_view.is_live);

    let completed_view = selected_tui_task_log_view_for_path_at(
        &mut panel,
        &active_path,
        TaskStatus::Done,
        &task,
        &state_dir,
    )
    .unwrap()
    .unwrap();
    assert!(completed_view.is_live);
    assert_eq!(
        tui_codex_session_availability_for_path_at(
            &mut panel,
            &active_path,
            "session-live",
            &state_dir,
        )
        .unwrap(),
        TuiCodexSessionAvailability::SelectedSessionBusy
    );
    assert_eq!(
        tui_codex_session_availability_for_path_at(
            &mut panel,
            &active_path,
            "different-session",
            &state_dir,
        )
        .unwrap(),
        TuiCodexSessionAvailability::ProjectBusy
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn open_kanban_agent_log_follows_the_selected_task() {
    let root = temp_root("kanban-agent-log-follows-task");
    let state_dir = root.join("state/clt");
    let project_root = root.join("alpha");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "alpha")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);

    let first_stdout = root.join("first.out");
    let second_stdout = root.join("second.out");
    let first_stderr = root.join("first-metadata.log");
    let second_stderr = root.join("second-metadata.log");
    fs::write(&first_stdout, "first task output").unwrap();
    fs::write(&second_stdout, "second task output").unwrap();
    fs::write(
        &first_stderr,
        "OpenAI Codex v0.153.3\n--------\nmodel: gpt-6-astra\nreasoning effort: high\n--------\n",
    )
    .unwrap();
    fs::write(
        &second_stderr,
        "OpenAI Codex v0.153.3\n--------\nmodel: gpt-5.6-sol\nreasoning effort: medium\n--------\n",
    )
    .unwrap();
    for (started_at, session_id, stdout_path, stderr_path) in [
        ("100", "session-one", &first_stdout, &first_stderr),
        ("200", "session-two", &second_stdout, &second_stderr),
    ] {
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
                project_id: project.id,
                status: "success",
                started_at,
                finished_at: Some(started_at),
                exit_code: Some(0),
                log_dir: Some(root.to_str().unwrap()),
                stdout_path: Some(stdout_path.to_str().unwrap()),
                stderr_path: Some(stderr_path.to_str().unwrap()),
                summary: Some("completed"),
                codex_session_id: Some(session_id),
            })
            .unwrap();
    }

    let mut panel = TuiAgentPanel {
        projects: vec![TuiAgentProject {
            project,
            scan: AgentProjectScan::empty(),
            runtime_state: TuiAgentRuntimeState::Idle,
            daemon_scan_problem: None,
            failure_problem: None,
        }],
        current_project_registration: None,
        daemon_status: "not-installed".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));
    let first_task = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 1 },
        "First task",
        "First task codex:session-one",
        false,
    );
    let second_task = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 2 },
        "Second task",
        "Second task codex:session-two",
        false,
    );

    let mut log_view = selected_tui_task_log_view_for_path_at(
        &mut panel,
        &project_root,
        TaskStatus::Done,
        &first_task,
        &state_dir,
    )
    .unwrap();
    assert_eq!(log_view.as_ref().unwrap().content, "first task output");
    assert_eq!(
        log_view.as_ref().unwrap().settings_label(),
        " Model: gpt-6-astra | Thinking: high "
    );

    sync_open_tui_task_log_view_at(
        &mut panel,
        &project_root,
        TaskStatus::Done,
        Some(&second_task),
        &mut log_view,
        &state_dir,
    );

    assert_eq!(log_view.as_ref().unwrap().content, "second task output");
    assert_eq!(
        log_view.as_ref().unwrap().settings_label(),
        " Model: gpt-5.6-sol | Thinking: medium "
    );

    sync_open_tui_task_log_view_at(
        &mut panel,
        &project_root,
        TaskStatus::Done,
        None,
        &mut log_view,
        &state_dir,
    );

    let project_log_view = log_view.unwrap();
    assert_eq!(project_log_view.content, "second task output");
    assert_eq!(
        project_log_view.settings_label(),
        " Model: gpt-5.6-sol | Thinking: medium "
    );
    assert!(!project_log_view.is_live);
    assert_eq!(
        project_log_view
            .session_target
            .as_ref()
            .map(|target| target.session_id.as_str()),
        Some("session-two")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_log_settings_remain_visible_while_output_scrolls() {
    let root = temp_root("agent-log-settings-footer");
    fs::create_dir_all(&root).unwrap();
    let output_path = root.join("run.out");
    let settings_path = root.join("run.err");
    fs::write(
        &output_path,
        format!("{}last output line\n", "old output\n".repeat(100)),
    )
    .unwrap();
    fs::write(
        &settings_path,
        "OpenAI Codex v0.153.3\n--------\nmodel: gpt-6-astra\nreasoning effort: xhigh\n--------\n",
    )
    .unwrap();
    let mut app = TuiApp::new(&root, true);
    app.agent_log_view = Some(
        TuiAgentLogView::new(
            "alpha".to_string(),
            output_path,
            Some(settings_path.clone()),
            false,
            None,
        )
        .unwrap(),
    );
    let backend = ratatui::backend::TestBackend::new(60, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    for pane in [TuiPane::Tasks, TuiPane::AgentProjects] {
        app.current_pane = pane;
        terminal.draw(|frame| render_tui(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Model: gpt-6-astra | Thinking: xhigh"));
        assert!(rendered.contains("last output line"));
    }

    fs::remove_file(settings_path).unwrap();
    let view = app.agent_log_view.as_mut().unwrap();
    view.refresh().unwrap();
    assert_eq!(
        view.settings_label(),
        " Model: unknown | Thinking: unknown "
    );
    assert!(view.content.contains("last output line"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn open_agent_log_follows_the_highlighted_project() {
    let root = temp_root("agent-log-follows-selection");
    let state_dir = root.join("state/clt");
    let alpha_root = root.join("alpha");
    let beta_root = root.join("beta");
    init_tasks(&alpha_root, false).unwrap();
    init_tasks(&beta_root, false).unwrap();
    let alpha_root = fs::canonicalize(alpha_root).unwrap();
    let beta_root = fs::canonicalize(beta_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&alpha_root, "alpha")
        .unwrap();
    store.register_project_blocking(&beta_root, "beta").unwrap();

    let projects = store.list_projects_blocking().unwrap();
    for project in &projects {
        let stdout_path = root.join(format!("{}.out", project.name));
        fs::write(&stdout_path, format!("{} output", project.name)).unwrap();
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
                project_id: project.id,
                status: "success",
                started_at: "100",
                finished_at: Some("100"),
                exit_code: Some(0),
                log_dir: Some(root.to_str().unwrap()),
                stdout_path: Some(stdout_path.to_str().unwrap()),
                stderr_path: None,
                summary: Some("completed"),
                codex_session_id: None,
            })
            .unwrap();
    }

    let mut panel = TuiAgentPanel {
        projects: projects
            .into_iter()
            .map(|project| TuiAgentProject {
                project,
                scan: AgentProjectScan::empty(),
                runtime_state: TuiAgentRuntimeState::Idle,
                daemon_scan_problem: None,
                failure_problem: None,
            })
            .collect(),
        current_project_registration: None,
        daemon_status: "not-installed".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));
    let mut log_view = selected_tui_agent_log_view_at(&panel, &state_dir).unwrap();

    panel.select_next();
    sync_open_tui_agent_log_view_at(&panel, &mut log_view, &state_dir);

    let selected_name = &panel.selected_project().unwrap().project.name;
    let log_view = log_view.unwrap();
    assert_eq!(&log_view.project_name, selected_name);
    assert_eq!(log_view.content, format!("{selected_name} output"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tui_agent_panel_top_status_includes_time_and_daemon_status() {
    let status = format_tui_agent_panel_top_status_with_time("09:41", "running", 3, 2, 1);

    assert!(status.starts_with(" 09:41  daemon status: running"));
    assert!(status.contains("daemon status: running"));
    assert!(status.contains("3 projects"));
    assert!(status.contains("2 enabled"));
    assert!(status.contains("1 running"));
}

#[test]
fn tui_agent_runtime_state_distinguishes_running_from_doing_tasks() {
    let no_leases = Vec::new();
    assert_eq!(
        tui_agent_runtime_state(1, &no_leases),
        TuiAgentRuntimeState::Idle
    );

    let active_lease = agent::AgentLeaseRecord {
        project_id: 1,
        project_name: "alpha".to_string(),
        project_path: PathBuf::from("/tmp/alpha"),
        holder: agent_lease_holder(),
        acquired_at: "100".to_string(),
        expires_at: "200".to_string(),
    };
    assert_eq!(
        tui_agent_runtime_state(1, &[active_lease]),
        TuiAgentRuntimeState::Running
    );

    let interactive_lease = agent::AgentLeaseRecord {
        project_id: 1,
        project_name: "alpha".to_string(),
        project_path: PathBuf::from("/tmp/alpha"),
        holder: InteractiveAgentLease::holder_for_idle_session(),
        acquired_at: "100".to_string(),
        expires_at: "200".to_string(),
    };
    assert_eq!(
        tui_agent_runtime_state(1, &[interactive_lease]),
        TuiAgentRuntimeState::Fenced
    );

    let stale_interactive_lease = agent::AgentLeaseRecord {
        project_id: 1,
        project_name: "alpha".to_string(),
        project_path: PathBuf::from("/tmp/alpha"),
        holder: format!("clt-idle-interactive-worker-{}-1-1", u32::MAX),
        acquired_at: "100".to_string(),
        expires_at: "9999999999".to_string(),
    };
    assert_eq!(
        tui_agent_runtime_state(1, &[stale_interactive_lease]),
        TuiAgentRuntimeState::Stale
    );
}

#[test]
fn tui_agent_runtime_state_surfaces_failed_recovery_over_pending_finalization() {
    for finalization in [
        GitFinalizationState::Working,
        GitFinalizationState::Tracking,
        GitFinalizationState::CommitPending,
        GitFinalizationState::PushPending,
    ] {
        for runtime_state in [TuiAgentRuntimeState::Idle, TuiAgentRuntimeState::Fenced] {
            assert_eq!(
                resolve_tui_agent_runtime_state(
                    runtime_state,
                    false,
                    false,
                    Some(finalization),
                    true,
                ),
                TuiAgentRuntimeState::Error
            );
        }
    }
    assert_eq!(
        resolve_tui_agent_runtime_state(
            TuiAgentRuntimeState::Idle,
            false,
            true,
            Some(GitFinalizationState::Working),
            true,
        ),
        TuiAgentRuntimeState::Error
    );
    assert_eq!(
        resolve_tui_agent_runtime_state(
            TuiAgentRuntimeState::Idle,
            false,
            true,
            Some(GitFinalizationState::Working),
            false,
        ),
        TuiAgentRuntimeState::Finalizing
    );
    assert_eq!(
        resolve_tui_agent_runtime_state(
            TuiAgentRuntimeState::Idle,
            false,
            false,
            Some(GitFinalizationState::PushPending),
            false,
        ),
        TuiAgentRuntimeState::PushPending
    );
    assert_eq!(
        resolve_tui_agent_runtime_state(
            TuiAgentRuntimeState::Running,
            false,
            false,
            Some(GitFinalizationState::Working),
            true,
        ),
        TuiAgentRuntimeState::Running
    );
}

#[test]
fn agent_project_table_surfaces_external_daemon_scan_errors() {
    let mut item = tui_agent_project_for_test(1, "fishdome");
    item.project.path = PathBuf::from("/Volumes/External/FISHDOME");
    item.project.last_daemon_scan_status = Some("unavailable".to_string());
    item.project.last_daemon_scan_error = Some("Operation not permitted (os error 1)".to_string());
    item.daemon_scan_problem = tui_agent_daemon_scan_problem(&item.project);
    item.runtime_state = TuiAgentRuntimeState::Error;

    let codex_width = agent_codex_column_width(std::slice::from_ref(&item), false);
    let project_width =
        agent_project_column_width(std::slice::from_ref(&item), None, 160, codex_width);
    let row = format_agent_project_table_row(0, &item, 160, project_width, codex_width, false);
    let mut panel = TuiAgentPanel {
        projects: vec![item],
        current_project_registration: None,
        daemon_status: "service active".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));
    let (console, color) = tui_console_content(true, &panel, None, "instructions");

    assert!(row.contains("ERROR"));
    assert!(row.contains("External project scan failed"));
    assert!(console.contains("Full Disk Access"));
    assert!(console.contains("restart the agent"));
    assert_eq!(color, Color::LightRed);
}

#[test]
fn agent_project_table_surfaces_failed_run_reason_and_retry_guidance() {
    let mut item = tui_agent_project_for_test(13, "chitty");
    item.project.git_mode = AgentGitMode::CommitAndPush;
    item.project.last_failure_at = Some("100".to_string());
    item.project.failure_count = 1;
    item.scan = AgentProjectScan::pending(2);
    let latest_run = agent::AgentRunRecord {
            id: 2049,
            project_id: item.project.id,
            project_name: item.project.name.clone(),
            project_path: item.project.path.clone(),
            status: "failure".to_string(),
            started_at: "99".to_string(),
            finished_at: Some("100".to_string()),
            exit_code: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some(
                "Codex runner failed before completion: Todo candidate is not committed exactly once at the frozen task boundary"
                    .to_string(),
            ),
            codex_session_id: None,
        };
    item.failure_problem = tui_agent_failure_problem(
        &item.project,
        Some(&latest_run),
        250,
        Duration::from_secs(300),
    );
    item.runtime_state = resolve_tui_agent_runtime_state(
        TuiAgentRuntimeState::Idle,
        false,
        true,
        Some(GitFinalizationState::Working),
        item.failure_problem.is_some(),
    );

    let codex_width = agent_codex_column_width(std::slice::from_ref(&item), false);
    let project_width =
        agent_project_column_width(std::slice::from_ref(&item), None, 180, codex_width);
    let row = format_agent_project_table_row(0, &item, 180, project_width, codex_width, false);
    let mut panel = TuiAgentPanel {
        projects: vec![item],
        current_project_registration: None,
        daemon_status: "service active".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));
    let (console, color) = tui_console_content(true, &panel, None, "instructions");

    assert!(row.contains("ERROR"));
    assert!(row.contains("Last agent run failed"));
    assert!(console.contains("Automatic retry in 150s"));
    assert!(console.contains("checkpoints dirty Todo definitions automatically"));
    assert!(console.contains("Press r"));
    assert_eq!(color, Color::LightRed);
}

#[test]
fn current_project_registration_is_present_only_when_active_project_is_unregistered() {
    let active_root = PathBuf::from("/tmp/current");
    let other_project = tui_agent_project_for_test(1, "other");

    let registration = current_project_registration(&active_root, &[other_project]).unwrap();

    assert_eq!(registration.path, active_root);
    assert_eq!(registration.name, "current");

    let mut current_project = tui_agent_project_for_test(2, "current");
    current_project.project.path = registration.path.clone();

    assert!(current_project_registration(&registration.path, &[current_project]).is_none());
}

#[test]
fn tui_agent_panel_selects_current_project_registration_before_projects() {
    let mut panel = TuiAgentPanel {
        projects: vec![
            tui_agent_project_for_test(1, "alpha"),
            tui_agent_project_for_test(2, "beta"),
        ],
        current_project_registration: Some(TuiCurrentProjectRegistration {
            path: PathBuf::from("/tmp/current"),
            name: "current".to_string(),
        }),
        daemon_status: "running".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));

    assert!(panel.selected_current_project_registration().is_some());
    assert!(panel.selected_project().is_none());

    panel.select_next();
    assert_eq!(panel.selected_project().unwrap().project.name, "alpha");

    panel.select_previous();
    assert!(panel.selected_current_project_registration().is_some());
}

#[test]
fn current_project_registration_row_prompts_enter_or_space() {
    let registration = TuiCurrentProjectRegistration {
        path: PathBuf::from("/tmp/current"),
        name: "current".to_string(),
    };

    let project_width =
        agent_project_column_width(&[], Some(&registration), 100, "Enter/Space".len());
    let row = format_current_project_registration_row(
        &registration,
        100,
        project_width,
        "Enter/Space".len(),
    );

    assert!(row.contains("ADD"));
    assert!(row.contains("current"));
    assert!(row.contains("Enter/Space"));
}

#[test]
fn agent_project_table_shows_codex_settings() {
    let mut project = tui_agent_project_for_test(1, "alpha");
    project.scan = AgentProjectScan::pending_with_doing(12, 3);
    project.runtime_state = TuiAgentRuntimeState::Running;
    project.project.codex_model = Some("gpt-5.6-terra".to_string());
    project.project.codex_reasoning_effort = Some("high".to_string());
    project.project.codex_fast_enabled = true;

    let codex_width = agent_codex_column_width(std::slice::from_ref(&project), false);
    let compact_project_width =
        agent_project_column_width(std::slice::from_ref(&project), None, 100, codex_width);
    let wide_project_width =
        agent_project_column_width(std::slice::from_ref(&project), None, 160, codex_width);
    let compact_header = format_agent_project_table_header(100, compact_project_width, codex_width);
    let compact_row =
        format_agent_project_table_row(0, &project, 100, compact_project_width, codex_width, false);
    let active_compact_row =
        format_agent_project_table_row(0, &project, 100, compact_project_width, codex_width, true);
    let wide_header = format_agent_project_table_header(160, wide_project_width, codex_width);
    let wide_row =
        format_agent_project_table_row(0, &project, 160, wide_project_width, codex_width, false);

    for header in [&compact_header, &wide_header] {
        assert!(header.find("PROJECT").unwrap() < header.find("TODO").unwrap());
        assert!(header.find("TODO").unwrap() < header.find("DOING").unwrap());
        assert!(header.find("DOING").unwrap() < header.find("CODEX").unwrap());
        assert!(header.find("CODEX").unwrap() < header.find("LAST RUN").unwrap());
        assert!(header.find("LAST RUN").unwrap() < header.find("PATH").unwrap());
        assert!(!header.contains("FAST"));
        assert!(!header.contains("MODEL"));
        assert!(!header.contains("THINK"));
    }
    for row in [&compact_row, &wide_row] {
        assert!(row.contains("5.6-terra/high/fast"));
        assert!(!row.contains("gpt-"));
        assert!(row.contains("RUNNING"));
        assert!(row.contains("/tmp/alpha"));
    }
    assert!(active_compact_row.starts_with("*  1 "));
}

#[test]
fn agent_project_table_abbreviates_all_git_modes() {
    let mut project = tui_agent_project_for_test(1, "alpha");

    for (mode, expected) in [
        (AgentGitMode::Off, "OFF"),
        (AgentGitMode::Commit, "COM"),
        (AgentGitMode::CommitAndPush, "PUSH"),
    ] {
        project.project.git_mode = mode;
        let codex_width = agent_codex_column_width(std::slice::from_ref(&project), false);
        let project_width =
            agent_project_column_width(std::slice::from_ref(&project), None, 100, codex_width);
        let header = format_agent_project_table_header(100, project_width, codex_width);
        let row =
            format_agent_project_table_row(0, &project, 100, project_width, codex_width, false);
        let git_column = header.find("GIT").unwrap();

        assert_eq!(row[git_column..git_column + 4].trim(), expected);
    }
}

#[test]
fn agent_git_mode_cycles_off_commit_push() {
    assert_eq!(AgentGitMode::Off.next(), AgentGitMode::Commit);
    assert_eq!(AgentGitMode::Commit.next(), AgentGitMode::CommitAndPush);
    assert_eq!(AgentGitMode::CommitAndPush.next(), AgentGitMode::Off);
}

#[test]
fn compact_codex_settings_omit_disabled_overrides() {
    assert_eq!(
        compact_agent_codex_settings(None, None, None, false),
        "default"
    );
    assert_eq!(
        compact_agent_codex_settings(None, Some("gpt-5.6"), Some("high"), false),
        "5.6/high"
    );
    assert_eq!(
        compact_agent_codex_settings(None, None, Some("high"), false),
        "high"
    );
    assert_eq!(compact_agent_codex_settings(None, None, None, true), "fast");
    assert_eq!(
        compact_agent_codex_settings(
            Some("openrouter"),
            Some("anthropic/claude-sonnet-4"),
            None,
            false,
        ),
        "openrouter:anthropic/claude-sonnet-4"
    );
}

#[test]
fn codex_column_width_tracks_its_longest_value() {
    let default_project = tui_agent_project_for_test(1, "default");
    assert_eq!(agent_codex_column_width(&[default_project], false), 7);

    let mut configured_project = tui_agent_project_for_test(2, "configured");
    configured_project.project.codex_model = Some("gpt-5.6".to_string());
    configured_project.project.codex_reasoning_effort = Some("high".to_string());
    assert_eq!(agent_codex_column_width(&[configured_project], false), 8);
}

#[test]
fn project_column_prioritizes_the_full_name_over_the_path() {
    let project_name = "customer-facing-analytics-dashboard";
    let project = tui_agent_project_for_test(1, project_name);
    let full_path = project.project.path.display().to_string();
    let codex_width = agent_codex_column_width(std::slice::from_ref(&project), false);
    let project_width =
        agent_project_column_width(std::slice::from_ref(&project), None, 100, codex_width);

    let row = format_agent_project_table_row(0, &project, 100, project_width, codex_width, false);

    assert_eq!(project_width, project_name.chars().count());
    assert!(row.contains(project_name));
    assert!(!row.contains(&full_path));
}

#[test]
fn codex_reasoning_setting_cycles_return_to_project_default() {
    assert_eq!(
        AGENT_CODEX_REASONING_EFFORTS,
        ["", "low", "medium", "high", "xhigh", "max", "ultra"]
    );

    let mut reasoning = None;
    for _ in 0..AGENT_CODEX_REASONING_EFFORTS.len() {
        reasoning = next_agent_codex_setting(reasoning.as_deref(), &AGENT_CODEX_REASONING_EFFORTS);
    }
    assert_eq!(reasoning, None);
}

#[test]
fn tui_start_state_with_active_board_opens_task_pane() {
    let state = tui_start_state(true);

    assert!(state.active_board);
    assert_eq!(state.current_pane, TuiPane::Tasks);
    assert_eq!(state.feedback_buffer, tui_task_board_instructions());
}

#[test]
fn tui_task_board_instructions_only_describe_task_page_controls() {
    let instructions = tui_task_board_instructions();

    assert!(instructions.contains("Space creates a task"));
    assert!(instructions.contains("n or + creates a subtask under the selected task"));
    assert!(instructions.contains("e edits"));
    assert!(instructions.contains("Codex: s stops/resumes"));
    assert!(instructions.contains("i interrupts for interaction"));
    assert!(instructions.contains("c opens linked idle Doing, completed, or blocked sessions"));
    assert!(instructions.contains("l shows logs"));
    assert!(instructions.contains("Press r to reorganize"));
    assert!(instructions.contains("Tab opens Agent Projects"));
    assert!(!instructions.contains("toggles ON/OFF"));
    assert!(!instructions.contains("cycles Git"));
    assert!(!instructions.contains("cycles the selected target"));
    assert!(!instructions.contains("toggles fast"));
    assert!(!instructions.contains("cycles thinking"));
}

#[test]
fn tui_start_state_without_active_board_opens_agent_pane() {
    let state = tui_start_state(false);

    assert!(!state.active_board);
    assert_eq!(state.current_pane, TuiPane::AgentProjects);
    assert_eq!(state.feedback_buffer, TUI_NO_ACTIVE_BOARD_MESSAGE);
}

#[test]
fn tab_toggles_kanban_and_agent_projects_without_cycling_models() {
    assert_eq!(
        tui_pane_after_tab(TuiPane::Tasks, true),
        TuiPane::AgentProjects
    );
    assert_eq!(
        tui_pane_after_tab(TuiPane::AgentProjects, true),
        TuiPane::Tasks
    );
    assert_eq!(
        tui_pane_after_tab(TuiPane::AgentProjects, false),
        TuiPane::AgentProjects
    );
    assert_eq!(
        tui_pane_after_tab(TuiPane::Models, true),
        TuiPane::AgentProjects
    );
    assert_eq!(tui_models_return_pane(TuiPane::Tasks), TuiPane::Tasks);
    assert_eq!(
        tui_models_return_pane(TuiPane::AgentProjects),
        TuiPane::AgentProjects
    );
}

#[test]
fn tui_reducer_owns_pane_transitions_and_returns_effects() {
    let root = temp_root("tui-reducer-panes");
    let mut app = TuiApp::new(&root, true);

    let effects =
        update_tui_pane(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)).unwrap();
    assert_eq!(app.current_pane, TuiPane::AgentProjects);
    assert_eq!(
        effects,
        vec![TuiEffect::RefreshAgentPanel, TuiEffect::SyncAgentLog]
    );

    let effects =
        update_tui_pane(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)).unwrap();
    assert_eq!(app.current_pane, TuiPane::Tasks);
    assert_eq!(effects, vec![TuiEffect::SyncTaskLog]);

    let effects = update_tui_pane(
        &mut app,
        KeyEvent::new(KeyCode::Char('M'), KeyModifiers::SHIFT),
    )
    .unwrap();
    assert_eq!(app.current_pane, TuiPane::Models);
    assert_eq!(app.models_return_pane, TuiPane::Tasks);
    assert_eq!(effects, vec![TuiEffect::RefreshModels]);

    let effects =
        update_tui_pane(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).unwrap();
    assert_eq!(app.current_pane, TuiPane::Tasks);
    assert!(effects.is_empty());

    let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(
        update_tui_pane(&mut app, space).unwrap(),
        vec![TuiEffect::PaneKey(space)]
    );
}

#[test]
fn tui_models_shortcut_supports_terminal_shift_encodings() {
    let root = temp_root("tui-models-shortcut");
    for (pane, active_board) in [
        (TuiPane::Tasks, true),
        (TuiPane::AgentProjects, true),
        (TuiPane::AgentProjects, false),
    ] {
        for key in [
            KeyEvent::new(KeyCode::Char('M'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('M'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::SHIFT),
        ] {
            let mut app = TuiApp::new(&root, active_board);
            app.current_pane = pane;

            let effects = update_tui_pane(&mut app, key).unwrap();
            assert_eq!(app.current_pane, TuiPane::Models, "{pane:?}: {key:?}");
            assert_eq!(app.models_return_pane, pane);
            assert_eq!(effects, vec![TuiEffect::RefreshModels]);
            assert_eq!(app.feedback_buffer, tui_models_instructions());

            let effects = update_tui_pane(&mut app, key).unwrap();
            assert_eq!(app.current_pane, pane, "{pane:?}: {key:?}");
            assert!(effects.is_empty());
            assert_eq!(
                app.feedback_buffer,
                if pane == TuiPane::Tasks {
                    tui_task_board_instructions()
                } else {
                    tui_agent_panel_instructions()
                }
            );
        }
    }
}

#[test]
fn tui_models_shortcut_preserves_lowercase_m_and_task_input() {
    let root = temp_root("tui-models-shortcut-input");
    for pane in [TuiPane::Tasks, TuiPane::AgentProjects, TuiPane::Models] {
        let mut app = TuiApp::new(&root, true);
        app.current_pane = pane;
        let key = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE);

        assert_eq!(
            update_tui_pane(&mut app, key).unwrap(),
            vec![TuiEffect::PaneKey(key)]
        );
        assert_eq!(app.current_pane, pane);
    }

    for mode in [Mode::Input, Mode::Edit] {
        let mut app = TuiApp::new(&root, true);
        app.current_mode = mode;
        let key = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::SHIFT);

        assert_eq!(
            update_tui_pane(&mut app, key).unwrap(),
            vec![TuiEffect::PaneKey(key)]
        );
        assert_eq!(app.current_pane, TuiPane::Tasks);
        assert_eq!(app.current_mode, mode);
    }
}

#[test]
fn tui_task_reducer_navigates_cached_entries_without_storage_access() {
    let root = temp_root("tui-reducer-tasks");
    let mut app = TuiApp::new(&root, true);
    let entry = |summary: &str| TaskEntry {
        source: TaskSource::MarkdownLine { line_index: 0 },
        summary: summary.to_string(),
        content: summary.to_string(),
        metadata: None,
        has_subtasks: false,
    };
    app.task_snapshot.board_entries[TODO_BOARD_INDEX] = vec![entry("first"), entry("second")];
    app.task_snapshot.board_entries[1] = vec![entry("doing")];
    app.board_states[TODO_BOARD_INDEX].select(Some(0));

    let effects =
        update_tui_pane(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).unwrap();
    assert_eq!(app.board_states[TODO_BOARD_INDEX].selected(), Some(1));
    assert_eq!(effects, vec![TuiEffect::SyncTaskLog]);

    update_tui_pane(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).unwrap();
    assert_eq!(app.board_states[TODO_BOARD_INDEX].selected(), Some(0));

    update_tui_pane(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)).unwrap();
    assert_eq!(app.selected_board, 1);
    assert_eq!(app.board_states[1].selected(), Some(0));
}

#[test]
fn tui_render_uses_cached_state_without_mutating_the_app() {
    let root = temp_root("tui-pure-render");
    let mut app = TuiApp::new(&root, true);
    app.task_snapshot.board_title = "Cached Board".to_string();
    app.task_snapshot.board_entries[TODO_BOARD_INDEX] = vec![TaskEntry {
        source: TaskSource::MarkdownLine { line_index: 0 },
        summary: "cached task".to_string(),
        content: "cached task".to_string(),
        metadata: None,
        has_subtasks: false,
    }];
    app.board_states[TODO_BOARD_INDEX].select(Some(0));
    app.current_time = "12:34".to_string();
    let selected_before = app.board_states[TODO_BOARD_INDEX].selected();
    let scroll_before = app.board_scroll_offsets;

    let backend = ratatui::backend::TestBackend::new(100, 28);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| render_tui(frame, &app)).unwrap();

    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("cached task"));
    assert_eq!(
        app.board_states[TODO_BOARD_INDEX].selected(),
        selected_before
    );
    assert_eq!(app.board_scroll_offsets, scroll_before);
}

#[test]
fn tui_render_entry_point_contains_no_effectful_operations() {
    let source = include_str!("../tui.rs");
    let render = source
        .split_once("pub(super) fn render_tui(")
        .unwrap()
        .1
        .split_once("pub(super) fn execute_tui_key_effect(")
        .unwrap()
        .0;

    for forbidden in [
        "fs::",
        "read_task_entries",
        "read_archived_task_entries",
        "open_agent_store",
        "TursoAgentStore",
        "agent_service_",
        "Command::",
        "std::process",
        "std::env",
    ] {
        assert!(
            !render.contains(forbidden),
            "render_tui must not perform effects: found {forbidden}"
        );
    }
}

#[test]
fn tui_update_handlers_are_pure_and_effect_execution_is_separate() {
    let source = include_str!("../tui.rs");
    let updates = source
        .split_once("pub(super) fn update_tui_pane(")
        .unwrap()
        .1
        .split_once("pub(super) fn execute_tui_effect(")
        .unwrap()
        .0;

    for forbidden in [
        "fs::",
        "read_task_entries",
        "read_archived_task_entries",
        "open_agent_store",
        "TursoAgentStore",
        "agent_service_",
        "Command::",
        "std::process",
        "std::env",
        "Instant::now",
    ] {
        assert!(
            !updates.contains(forbidden),
            "TUI update handlers must return effects instead of executing them: found {forbidden}"
        );
    }
    assert!(source.contains("pub(super) fn execute_tui_effect("));
    assert!(source.contains("pub(super) fn execute_tui_key_effect("));
}

#[test]
fn tui_models_navigation_pages_and_searches_visible_models() {
    let mut panel = TuiModelsPanel {
        providers: Vec::new(),
        models: [
            ("alpha", "Alpha"),
            ("beta-v1", "Beta"),
            ("delta-v2", "Delta"),
            ("g-3", "Gamma Preview"),
        ]
        .into_iter()
        .map(|(model_id, label)| agent::AgentModelTarget {
            provider_id: "test".to_string(),
            model_id: model_id.to_string(),
            label: label.to_string(),
            enabled: true,
            favorite: false,
            reasoning_effort: None,
        })
        .collect(),
        defaults: agent::AgentModelDefaults::default(),
        codex_default: "not explicitly set".to_string(),
        codex_default_provider: None,
        codex_default_model: None,
        focus: TuiModelsFocus::Models,
        provider_state: ListState::default(),
        model_state: ListState::default().with_selected(Some(0)),
        model_search: String::new(),
        provider_viewport_height: 0,
        model_viewport_height: 2,
        last_error: None,
    };

    panel.select_page_down();
    assert_eq!(panel.model_state.selected(), Some(2));
    panel.select_page_down();
    assert_eq!(panel.model_state.selected(), Some(3));
    panel.select_page_up();
    assert_eq!(panel.model_state.selected(), Some(1));
    panel.select_first();
    assert_eq!(panel.model_state.selected(), Some(0));
    panel.select_last();
    assert_eq!(panel.model_state.selected(), Some(3));

    let mut search = TuiModelInput::search_models("TA".to_string());
    let message = submit_tui_model_input(&mut search, &mut panel)
        .unwrap()
        .unwrap();
    assert!(message.contains("2 matches"));
    assert_eq!(panel.visible_model_indices(), [1, 2]);
    assert_eq!(panel.model_state.selected(), Some(1));
    panel.select_next();
    assert_eq!(panel.model_state.selected(), Some(2));
    panel.select_next();
    assert_eq!(panel.model_state.selected(), Some(1));
    panel.select_previous();
    assert_eq!(panel.model_state.selected(), Some(2));
    panel.select_first();
    assert_eq!(panel.model_state.selected(), Some(1));
    panel.select_last();
    assert_eq!(panel.model_state.selected(), Some(2));

    assert_eq!(panel.set_model_search("PREVIEW".to_string()), 1);
    assert_eq!(panel.selected_model().unwrap().model_id, "g-3");
    assert_eq!(panel.set_model_search("missing".to_string()), 0);
    assert!(panel.selected_model().is_none());
    assert_eq!(panel.set_model_search(String::new()), 4);
    assert_eq!(panel.model_state.selected(), Some(0));
}

#[test]
fn tui_models_rows_have_labeled_columns_and_independent_defaults() {
    let provider = agent::AgentModelProvider {
        id: "openai".to_string(),
        name: "OpenAI".to_string(),
        base_url: None,
        env_key: None,
        built_in: true,
        enabled: true,
    };
    assert_eq!(
        tui_models_provider_header()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["USE", "TYPE", "PROVIDER", "(ID)"]
    );
    assert_eq!(
        tui_models_provider_row(&provider)
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["ON", "BUILTIN", "OpenAI", "(openai)"]
    );

    let model = agent::AgentModelTarget {
        provider_id: "openai".to_string(),
        model_id: "gpt-5.6".to_string(),
        label: "GPT-5.6".to_string(),
        enabled: true,
        favorite: true,
        reasoning_effort: None,
    };
    let defaults = agent::AgentModelDefaults {
        provider_id: Some("openai".to_string()),
        model_id: Some("gpt-5.6".to_string()),
    };
    assert!(tui_model_matches_clt_default(
        &defaults,
        None,
        Some("a-different-codex-model"),
        &model
    ));
    assert!(tui_model_matches_clt_default(
        &agent::AgentModelDefaults::default(),
        None,
        Some("gpt-5.6"),
        &model
    ));
    assert!(tui_model_matches_codex_default(
        None,
        Some("gpt-5.6"),
        &model
    ));
    assert_eq!(
        tui_models_model_header()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["USE", "FAV", "CLT", "CODEX", "THINK", "MODEL", "ID"]
    );
    let row = tui_models_model_row(&model, true, true);
    assert_eq!(
        row.split_whitespace().collect::<Vec<_>>(),
        ["ON", "YES", "YES", "YES", "system", "GPT-5.6", "gpt-5.6"]
    );
    assert!(!row.contains('★'));

    let same_id_on_openrouter = agent::AgentModelTarget {
        provider_id: "openrouter".to_string(),
        ..model
    };
    assert!(!tui_model_matches_codex_default(
        None,
        Some("gpt-5.6"),
        &same_id_on_openrouter
    ));
    assert!(tui_model_matches_codex_default(
        Some("openrouter"),
        Some("gpt-5.6"),
        &same_id_on_openrouter
    ));

    let root = temp_root("tui-import-codex-default-model");
    let store = agent::TursoAgentStore::open_blocking(&root).unwrap();
    let mut models = store.list_model_targets_blocking(Some("openai")).unwrap();
    include_codex_default_model_target(
        &store,
        "openai",
        None,
        Some("gpt-config-only"),
        &mut models,
    )
    .unwrap();
    include_codex_default_model_target(
        &store,
        "openai",
        None,
        Some("gpt-config-only"),
        &mut models,
    )
    .unwrap();
    assert_eq!(
        models
            .iter()
            .filter(|model| model.model_id == "gpt-config-only")
            .count(),
        1
    );
    assert!(
        store
            .list_model_targets_blocking(Some("openai"))
            .unwrap()
            .iter()
            .any(|model| model.model_id == "gpt-config-only" && model.enabled)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_model_endpoint_helpers_create_stable_ids_and_parse_openai_catalogs() {
    let providers = vec![agent::AgentModelProvider {
        id: "my-local-server".to_string(),
        name: "Existing".to_string(),
        base_url: None,
        env_key: None,
        built_in: false,
        enabled: true,
    }];
    assert_eq!(
        custom_provider_id("My Local Server", &providers),
        "my-local-server-2"
    );
    assert_eq!(custom_provider_id("🦙", &[]), "local");
    assert_eq!(
        openai_models_url("http://localhost:11434/v1/"),
        "http://localhost:11434/v1/models"
    );
    assert_eq!(
        openai_models_url("http://localhost:11434/v1/models"),
        "http://localhost:11434/v1/models"
    );
    assert_eq!(
        normalize_openai_api_base_url(" http://127.0.0.1:9090/v1/ ").unwrap(),
        "http://127.0.0.1:9090/v1"
    );
    assert_eq!(
        normalize_openai_api_base_url("http://127.0.0.1:9090").unwrap(),
        "http://127.0.0.1:9090"
    );
    for operation_url in [
        "http://localhost:9090/chat",
        "http://localhost:9090/v1/chat/completions",
        "http://localhost:9090/v1/models",
        "http://localhost:9090/v1/responses",
    ] {
        assert!(
            normalize_openai_api_base_url(operation_url).is_err(),
            "accepted operation URL {operation_url}"
        );
    }

    let model_ids = parse_openai_model_ids(&serde_json::json!({
        "object": "list",
        "data": [
            {"id": "zeta"},
            {"id": " alpha "},
            {"id": "zeta"},
            {"not_an_id": "ignored"}
        ]
    }))
    .unwrap();
    assert_eq!(model_ids, ["alpha", "zeta"]);
    assert!(parse_openai_model_ids(&serde_json::json!({"models": []})).is_err());
    assert_eq!(tui_models_add_provider_hint(), "[n] Add provider");
    assert!(tui_models_provider_choice_prompt().contains("[3] Ollama"));
    assert!(tui_models_provider_choice_prompt().contains("[5] Local/custom"));
    assert!(!tui_models_instructions().contains("1 OpenAI"));
    assert!(tui_models_instructions().contains("r refreshes"));
    assert!(tui_models_instructions().contains("/ searches"));
    assert!(tui_models_instructions().contains("PageUp/PageDown"));
    assert!(tui_models_instructions().contains("x/Delete to remove"));
    assert!(tui_models_instructions().contains("t cycles model reasoning"));
    let mut input = TuiModelInput::custom_provider();
    if let TuiModelInputKind::CustomProvider { step, .. } = &mut input.kind {
        *step = 1;
    }
    assert!(input.label().contains("usually .../v1"));
    assert!(input.guidance().contains("http://127.0.0.1:9090/v1"));
    assert!(input.guidance().contains("do not include /chat"));
}

#[test]
fn discovered_models_start_off_and_existing_choices_are_preserved() {
    let root = temp_root("discovered-model-choices");
    let store = agent::TursoAgentStore::open_blocking(&root).unwrap();
    let provider = agent::AgentModelProvider {
        id: "local-test".to_string(),
        name: "Local Test".to_string(),
        base_url: Some("http://localhost:8080/v1".to_string()),
        env_key: None,
        built_in: false,
        enabled: true,
    };
    store.upsert_model_provider_blocking(&provider).unwrap();
    store
        .upsert_model_target_blocking(&agent::AgentModelTarget {
            provider_id: provider.id.clone(),
            model_id: "already-selected".to_string(),
            label: "Already Selected".to_string(),
            enabled: true,
            favorite: true,
            reasoning_effort: Some("medium".to_string()),
        })
        .unwrap();

    assert_eq!(
        save_discovered_model_ids(
            &store,
            &provider.id,
            &["already-selected".to_string(), "new-model".to_string()]
        )
        .unwrap(),
        1
    );
    let models = store
        .list_model_targets_blocking(Some(&provider.id))
        .unwrap();
    let existing = models
        .iter()
        .find(|model| model.model_id == "already-selected")
        .unwrap();
    let discovered = models
        .iter()
        .find(|model| model.model_id == "new-model")
        .unwrap();
    assert!(existing.enabled && existing.favorite);
    assert_eq!(existing.reasoning_effort.as_deref(), Some("medium"));
    assert!(!discovered.enabled && !discovered.favorite);
    assert_eq!(discovered.reasoning_effort, None);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn model_discovery_queries_v1_models_with_bearer_auth() {
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(error) => panic!("failed to start model discovery test server: {error}"),
    };
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = [0_u8; 4096];
        let read = std::io::Read::read(&mut stream, &mut bytes).unwrap();
        let request = String::from_utf8_lossy(&bytes[..read]);
        assert!(request.starts_with("GET /v1/models HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key")
        );
        let body = r#"{"object":"list","data":[{"id":"llama3.2"},{"id":"qwen3"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
    });

    let model_ids =
        discover_openai_model_ids(&format!("http://{address}/v1"), Some("test-key")).unwrap();
    server.join().unwrap();
    assert_eq!(model_ids, ["llama3.2", "qwen3"]);
}

#[test]
fn codex_config_edits_preserve_existing_content_and_create_backup() {
    let root = temp_root("codex-config-model-provider");
    let config_path = root.join("codex/config.toml");
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::write(
            &config_path,
            "# keep this comment\napproval_policy = \"never\"\n\n[projects.\"/tmp/demo\"]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();

    assert_eq!(
        read_codex_default_config_at(&config_path).unwrap(),
        (None, None),
        "optional top-level defaults must not be indexed as required TOML keys"
    );

    upsert_codex_provider_config_at(
        &config_path,
        "openrouter",
        "OpenRouter",
        "https://openrouter.ai/api/v1",
        Some("OPENROUTER_API_KEY"),
    )
    .unwrap();
    set_codex_default_config_at(
        &config_path,
        "openrouter",
        "anthropic/claude-sonnet-4",
        Some("low"),
    )
    .unwrap();

    let updated = fs::read_to_string(&config_path).unwrap();
    let parsed = updated.parse::<DocumentMut>().unwrap();
    assert!(updated.contains("# keep this comment"));
    assert_eq!(parsed["approval_policy"].as_str(), Some("never"));
    assert_eq!(
        parsed["projects"]["/tmp/demo"]["trust_level"].as_str(),
        Some("trusted")
    );
    assert_eq!(
        parsed["model_providers"]["openrouter"]["wire_api"].as_str(),
        Some("responses")
    );
    assert_eq!(
        parsed["model_providers"]["openrouter"]["env_key"].as_str(),
        Some("OPENROUTER_API_KEY")
    );
    assert_eq!(parsed["model_provider"].as_str(), Some("openrouter"));
    assert_eq!(parsed["model"].as_str(), Some("anthropic/claude-sonnet-4"));
    assert_eq!(parsed["model_reasoning_effort"].as_str(), Some("low"));
    assert!(
        config_path
            .parent()
            .unwrap()
            .join("config.toml.clt.bak")
            .is_file()
    );

    assert!(
        !set_codex_model_reasoning_if_default_at(
            &config_path,
            "openrouter",
            "another-model",
            Some("high")
        )
        .unwrap()
    );
    assert!(
        set_codex_model_reasoning_if_default_at(
            &config_path,
            "openrouter",
            "anthropic/claude-sonnet-4",
            Some("high")
        )
        .unwrap()
    );
    let updated_reasoning = fs::read_to_string(&config_path)
        .unwrap()
        .parse::<DocumentMut>()
        .unwrap();
    assert_eq!(
        updated_reasoning["model_reasoning_effort"].as_str(),
        Some("high")
    );
    assert!(
        set_codex_model_reasoning_if_default_at(
            &config_path,
            "openrouter",
            "anthropic/claude-sonnet-4",
            None
        )
        .unwrap()
    );
    let system_reasoning = fs::read_to_string(&config_path)
        .unwrap()
        .parse::<DocumentMut>()
        .unwrap();
    assert!(system_reasoning.get("model_reasoning_effort").is_none());

    set_codex_default_config_at(
        &config_path,
        "openrouter",
        "anthropic/claude-sonnet-4",
        Some("low"),
    )
    .unwrap();

    assert!(remove_codex_provider_config_at(&config_path, "openrouter").unwrap());
    let removed = fs::read_to_string(&config_path)
        .unwrap()
        .parse::<DocumentMut>()
        .unwrap();
    assert_eq!(removed["approval_policy"].as_str(), Some("never"));
    assert_eq!(
        removed["projects"]["/tmp/demo"]["trust_level"].as_str(),
        Some("trusted")
    );
    assert!(removed.get("model_providers").is_none());
    assert!(removed.get("model_provider").is_none());
    assert!(removed.get("model").is_none());
    assert!(removed.get("model_reasoning_effort").is_none());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn codex_config_edit_rejects_invalid_toml_without_overwriting_it() {
    let root = temp_root("codex-config-invalid");
    let config_path = root.join("config.toml");
    let invalid = "model = [not valid";
    fs::create_dir_all(&root).unwrap();
    fs::write(&config_path, invalid).unwrap();

    assert!(set_codex_default_config_at(&config_path, "openai", "gpt-5.6", Some("low")).is_err());
    assert_eq!(fs::read_to_string(&config_path).unwrap(), invalid);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_display_name_uses_folder_name_with_root_fallback() {
    assert_eq!(
        project_display_name(Path::new("/Users/pro/code/lls/clt")),
        "clt"
    );
    assert_eq!(project_display_name(Path::new("/")), "/");
}

#[test]
fn app_title_includes_project_name() {
    assert_eq!(
        app_title(Path::new("/Users/pro/code/lls/example")),
        "clt | example"
    );
}

#[test]
fn tui_console_block_right_aligns_the_backlog_status() {
    use ratatui::{buffer::Buffer, widgets::Widget};

    let area = Rect::new(0, 0, 48, 3);
    let mut buffer = Buffer::empty(area);

    tui_console_block("clt Console", Some(" Backlog: 2 [B] ")).render(area, &mut buffer);

    let top_border = (0..area.width)
        .map(|x| buffer[(x, 0)].symbol())
        .collect::<Vec<_>>()
        .join("");
    assert!(top_border.starts_with("┌clt Console"));
    assert!(top_border.ends_with(" Backlog: 2 [B] ┐"));
}

#[test]
fn tui_codex_handoff_status_renders_while_the_event_handler_is_blocked() {
    use ratatui::backend::TestBackend;

    let backend = TestBackend::new(100, 9);
    let mut terminal = Terminal::new(backend).unwrap();

    draw_tui_codex_handoff_status(&mut terminal, TuiCodexHandoffStage::WaitingForAutomatedExit)
        .unwrap();

    let buffer = terminal.backend().buffer();
    let rendered = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Codex handoff"));
    assert!(rendered.contains("Requesting the automated Codex process to stop..."));
    assert!(rendered.contains("Waiting for the current run to exit safely."));
}

#[test]
fn tui_codex_handoff_status_is_printed_across_terminal_suspension() {
    let mut output = Vec::new();

    write_tui_codex_handoff_status(&mut output, TuiCodexHandoffStage::QueueingExecResume).unwrap();

    assert_eq!(
        String::from_utf8(output).unwrap(),
        "\nInteractive Codex exited.\nReturning the same session to automated exec mode...\n"
    );
}
#[test]
fn tui_backlog_visibility_preserves_existing_board_order() {
    assert_eq!(visible_tui_board_indices(false), &[0, 1, 2]);
    assert_eq!(visible_tui_board_indices(true), &[3, 0, 1, 2]);
    assert_eq!(adjacent_visible_tui_board(0, false, -1), None);
    assert_eq!(adjacent_visible_tui_board(0, true, -1), Some(3));
    assert_eq!(adjacent_visible_tui_board(3, true, 1), Some(0));
    assert_eq!(wrapped_visible_tui_board(0, false, -1), 2);
    assert_eq!(wrapped_visible_tui_board(0, true, -1), 3);
}

#[test]
fn tui_backlog_action_moves_the_selected_task_while_column_is_hidden() {
    let root = temp_root("tui-move-backlog-hidden");
    add_task(&root, "first", None).unwrap();
    add_task(&root, "move me", None).unwrap();
    let board_dir = root.join("tasks");
    let mut states: [ListState; 4] = std::array::from_fn(|_| ListState::default());
    states[TODO_BOARD_INDEX].select(Some(1));
    let mut selected_board = TODO_BOARD_INDEX;

    let message = move_selected_tui_task_to_backlog(
        &board_dir,
        &TASK_STATUSES,
        &mut states,
        &mut selected_board,
        false,
    )
    .unwrap();

    assert_eq!(message, "Moved task to backlog");
    assert_eq!(selected_board, TODO_BOARD_INDEX);
    assert_eq!(states[TODO_BOARD_INDEX].selected(), Some(0));
    assert_eq!(read_tasks(&root, "todo").unwrap(), vec!["- first"]);
    assert_eq!(read_tasks(&root, "backlog").unwrap(), vec!["- move me"]);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tui_archive_action_moves_the_selected_task() {
    let root = temp_root("tui-move-archive");
    add_task(&root, "keep this active", None).unwrap();
    add_task(&root, "archive me", None).unwrap();
    let board_dir = root.join("tasks");
    let mut states: [ListState; 4] = std::array::from_fn(|_| ListState::default());
    states[TODO_BOARD_INDEX].select(Some(1));

    let message = move_selected_tui_task_to_archive(
        &board_dir,
        &TASK_STATUSES,
        &mut states,
        TODO_BOARD_INDEX,
    )
    .unwrap();

    assert_eq!(message, "Moved task to archive");
    assert_eq!(states[TODO_BOARD_INDEX].selected(), Some(0));
    assert_eq!(
        read_tasks(&root, "todo").unwrap(),
        vec!["- keep this active"]
    );
    let archived = read_archived_task_entries(&board_dir).unwrap();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].summary, "archive me");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hiding_focused_backlog_returns_focus_to_todo() {
    let root = temp_root("tui-hide-backlog");
    add_task(&root, "todo task", None).unwrap();
    let board_dir = root.join("tasks");
    let mut states: [ListState; 4] = std::array::from_fn(|_| ListState::default());
    states[BACKLOG_BOARD_INDEX].select(Some(0));
    let mut selected_board = BACKLOG_BOARD_INDEX;
    let mut backlog_visible = true;

    let message = toggle_tui_backlog_column(
        &board_dir,
        &mut states,
        &mut selected_board,
        &mut backlog_visible,
    );

    assert_eq!(message, "Backlog column hidden. Press B to show it.");
    assert!(!backlog_visible);
    assert_eq!(selected_board, TODO_BOARD_INDEX);
    assert_eq!(states[TODO_BOARD_INDEX].selected(), Some(0));

    fs::remove_dir_all(root).unwrap();
}
#[test]
fn selected_tui_task_text_uses_full_task_content() {
    let entry = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 1 },
        "Write launch plan. This is hidden in summary.",
        "Write launch plan. This is hidden in summary.\n\n- Add rollout notes",
        false,
    );

    assert_eq!(task_tui_display_text(&entry, false), "Write launch plan.");
    assert_eq!(
        task_tui_display_text(&entry, true),
        "Write launch plan. This is hidden in summary. Add rollout notes"
    );
}

#[test]
fn selected_task_ignores_stale_selection() {
    let root = temp_root("stale-selection");
    ensure_task_store(&root).unwrap();

    let mut state = ListState::default();
    state.select(Some(0));

    assert_eq!(selected_task(&root, "todo", &state), None);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn selected_task_index_ignores_stale_selection() {
    let root = temp_root("stale-index");
    add_task(&root, "only task", None).unwrap();

    let mut state = ListState::default();
    state.select(Some(1));

    assert_eq!(selected_task_index(&root, "todo", &state), None);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tui_add_inserts_above_selected_markdown_task() {
    let root = temp_root("tui-add-above-markdown");
    add_task(&root, "first task", None).unwrap();
    add_task(&root, "selected task", None).unwrap();
    add_task(&root, "last task", None).unwrap();

    let mut state = ListState::default();
    state.select(Some(1));
    insert_task_at_selection_in_board(
        &root.join("tasks"),
        TaskStatus::Todo,
        &state,
        "new task",
        None,
    )
    .unwrap();

    assert_eq!(
        read_tasks(&root, "todo").unwrap(),
        vec![
            "- first task",
            "- new task",
            "- selected task",
            "- last task",
        ]
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tui_add_inserts_above_selected_folder_task() {
    let root = temp_root("tui-add-above-folder");
    init_tasks(&root, true).unwrap();
    add_task(&root, "first task", None).unwrap();
    add_task(&root, "selected task", None).unwrap();
    add_task(&root, "last task", None).unwrap();

    let mut state = ListState::default();
    state.select(Some(1));
    insert_task_at_selection_in_board(
        &root.join("tasks"),
        TaskStatus::Todo,
        &state,
        "new task",
        None,
    )
    .unwrap();

    assert_eq!(
        read_tasks(&root, "todo").unwrap(),
        vec![
            "- first task",
            "- new task",
            "- selected task",
            "- last task",
        ]
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn normalize_board_selection_clears_empty_board_selection() {
    let root = temp_root("normalize-empty");
    ensure_task_store(&root).unwrap();

    let mut state = ListState::default();
    state.select(Some(0));

    normalize_board_selection(&root, "todo", &mut state);

    assert_eq!(state.selected(), None);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn normalize_board_selection_clamps_out_of_range_selection() {
    let root = temp_root("normalize-range");
    add_task(&root, "only task", None).unwrap();

    let mut state = ListState::default();
    state.select(Some(4));

    normalize_board_selection(&root, "todo", &mut state);

    assert_eq!(state.selected(), Some(0));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn keep_selected_task_visible_scrolls_down_to_selection() {
    let tasks = vec![
        "- task one".to_string(),
        "- task two".to_string(),
        "- task three".to_string(),
        "- task four".to_string(),
    ];
    let mut scroll_offset = 0;

    keep_selected_task_visible(&tasks, Some(3), &mut scroll_offset, 3, 20);

    assert_eq!(scroll_offset, 1);
}

#[test]
fn keep_selected_task_visible_scrolls_up_to_selection() {
    let tasks = vec![
        "- task one".to_string(),
        "- task two".to_string(),
        "- task three".to_string(),
    ];
    let mut scroll_offset = 2;

    keep_selected_task_visible(&tasks, Some(0), &mut scroll_offset, 3, 20);

    assert_eq!(scroll_offset, 0);
}

#[test]
fn input_cursor_offset_tracks_cursor_inside_wrapped_text() {
    let text = " Add Task: hello world";

    assert_eq!(wrap_input_text(text, 10), " Add Task:\n hello wor\nld");
    assert_eq!(
        input_cursor_offset_at(text, 10, " Add Task: hello".len()),
        (6, 1)
    );
    assert_eq!(input_cursor_offset_at(text, 10, text.len()), (2, 2));
}

#[test]
fn input_cursor_helpers_preserve_utf8_boundaries() {
    let text = "aéb";
    let inside_e = 2;

    assert_eq!(clamp_to_char_boundary(text, inside_e), 1);
    assert_eq!(previous_char_boundary(text, text.len()), 3);
    assert_eq!(previous_char_boundary(text, 3), 1);
    assert_eq!(next_char_boundary(text, 1), 3);
    assert_eq!(next_char_boundary(text, 3), text.len());
}

#[test]
fn input_key_handler_moves_and_edits_by_words() {
    let mut input = Input::new("first second third".to_string());

    handle_input_key(
        &mut input,
        crossterm::event::KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
        " Add Task: ",
        80,
    );
    handle_input_key(
        &mut input,
        crossterm::event::KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
        " Add Task: ",
        80,
    );

    assert_eq!(input.value(), "first second Xthird");

    handle_input_key(
        &mut input,
        crossterm::event::KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
        " Add Task: ",
        80,
    );

    assert_eq!(input.value(), "first second third");
}

#[test]
fn input_key_handler_supports_alt_b_and_alt_f_word_jumps() {
    let mut input = Input::new("first second third".to_string());

    handle_input_key(
        &mut input,
        crossterm::event::KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
        " Add Task: ",
        80,
    );
    handle_input_key(
        &mut input,
        crossterm::event::KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
        " Add Task: ",
        80,
    );
    handle_input_key(
        &mut input,
        crossterm::event::KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
        " Add Task: ",
        80,
    );
    handle_input_key(
        &mut input,
        crossterm::event::KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        " Add Task: ",
        80,
    );

    assert_eq!(input.value(), "first second Xthird!");
}

#[test]
fn input_key_handler_moves_vertically_through_wrapped_input() {
    let mut input = Input::new("hello world".to_string());

    handle_input_key(
        &mut input,
        crossterm::event::KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        " Add Task: ",
        10,
    );

    assert_eq!(input.cursor(), 1);

    handle_input_key(
        &mut input,
        crossterm::event::KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        " Add Task: ",
        10,
    );

    assert_eq!(input.cursor(), input.value().chars().count());
}

#[test]
fn task_input_collapses_multiline_paste_until_submission() {
    let mut input = TaskInput::new("before ".to_string());
    input.insert_paste("first\r\nsecond\r\nthird".to_string());
    handle_input_key(
        &mut input.input,
        crossterm::event::KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        " Add Task: ",
        80,
    );

    assert_eq!(input.display_value(), "before [Pasted Content 3 lines]!");
    assert_eq!(input.submitted_value(), "before first\nsecond\nthird!");
    assert_eq!(
        input.display_cursor(),
        input.display_value().chars().count()
    );

    let lines = styled_task_input_lines(" Add Task: ", &input, 80);
    let blue_text: String = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.style.fg == Some(Color::Blue))
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(blue_text, "[Pasted Content 3 lines]");
}

#[test]
fn task_input_treats_a_paste_placeholder_as_one_editable_character() {
    let mut input = TaskInput::default();
    input.insert_paste("first\nsecond".to_string());

    handle_input_key(
        &mut input.input,
        crossterm::event::KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        " Add Task: ",
        80,
    );

    assert_eq!(input.display_value(), "");
    assert_eq!(input.submitted_value(), "");
}

#[test]
fn task_input_inserts_single_line_paste_directly() {
    let mut input = TaskInput::new("before ".to_string());
    input.insert_paste("one line".to_string());

    assert_eq!(input.display_value(), "before one line");
    assert_eq!(input.submitted_value(), "before one line");
    assert!(input.pasted_content.is_empty());
}
