use super::*;
use crate::task::{TaskSource, task_entry_from_text};
use crate::tui::tests::tui_agent_project_for_test;
use crate::tui::*;

fn retry_project() -> TuiAgentProject {
    let mut project = tui_agent_project_for_test(1, "retry-demo");
    project.scan = AgentProjectScan::pending(1);
    project.project.failure_count = 1;
    project.project.last_failure_at = Some("100".into());
    project.retry_status =
        RetryStatus::new(&project.project, None, Duration::from_secs(300), 128, true);
    project.runtime_state = TuiAgentRuntimeState::Fenced;
    apply(&mut project, &[], false, true);
    project
}

fn task(content: &str) -> TaskEntry {
    task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 1 },
        "Retry demo",
        content,
        false,
    )
}

#[test]
fn cooldown_survives_missing_or_nonfailure_latest_record_and_ticks_to_ready() {
    let mut project = retry_project();
    assert_eq!(project.runtime_state, TuiAgentRuntimeState::RetryScheduled);
    assert_eq!(project.runtime_label(), "RETRY 4:32");
    assert!(
        project
            .displayed_retry()
            .unwrap()
            .message
            .contains("failure details are unavailable")
    );
    let run = agent::AgentRunRecord {
        id: 5,
        project_id: 1,
        project_name: "demo".into(),
        project_path: "/tmp/demo".into(),
        status: "success".into(),
        started_at: "101".into(),
        finished_at: Some("102".into()),
        exit_code: Some(0),
        stdout_path: None,
        stderr_path: None,
        summary: Some("Recovery acknowledged".into()),
        codex_session_id: Some("session".into()),
    };
    let mut retry = RetryStatus::new(
        &project.project,
        Some(&run),
        Duration::from_secs(300),
        399,
        true,
    )
    .unwrap();
    assert_eq!(retry.label, "Retry in 0:01");
    assert!(!retry.message.contains("Recovery acknowledged"));
    retry.tick(400, true);
    assert_eq!(retry.label, "Retry ready");
    retry.tick(401, false);
    assert_eq!(retry.label, "Retry paused");
    assert!(retry.message.contains("project is paused"));
    project.project.failure_count = 0;
    assert!(
        RetryStatus::new(&project.project, None, Duration::from_secs(300), 128, true).is_none()
    );
}

#[test]
fn retry_does_not_promise_execution_without_daemon_or_override_owners_and_holds() {
    let mut project = retry_project();
    project.retry_status =
        RetryStatus::new(&project.project, None, Duration::from_secs(300), 128, false);
    assert_eq!(project.displayed_retry().unwrap().label, "Retry paused");
    assert!(
        project
            .displayed_retry()
            .unwrap()
            .message
            .contains("not checking in")
    );
    for state in [
        TuiAgentRuntimeState::Running,
        TuiAgentRuntimeState::Resuming,
        TuiAgentRuntimeState::Interactive,
        TuiAgentRuntimeState::Stale,
        TuiAgentRuntimeState::Repair,
        TuiAgentRuntimeState::PushPending,
    ] {
        project.runtime_state = state;
        apply(&mut project, &[], false, true);
        assert_eq!(project.runtime_state, state);
        assert!(project.displayed_retry().is_none());
    }
    project.runtime_state = TuiAgentRuntimeState::Waiting;
    apply(&mut project, &[], true, true);
    assert_eq!(project.runtime_state, TuiAgentRuntimeState::Waiting);
    project.daemon_scan_problem = Some("Drive missing".into());
    apply(&mut project, &[], false, true);
    assert_eq!(project.runtime_state, TuiAgentRuntimeState::Waiting);
}

#[test]
fn task_and_project_show_countdown_then_exact_session_resume_and_running() {
    let mut app = TuiApp::new(Path::new("/tmp/retry-demo"), true);
    app.agent_panel.projects = vec![retry_project()];
    app.agent_panel.last_error = None;
    app.agent_panel.state.select(Some(0));
    let task = task("Retry demo codex:retry-session");
    app.task_snapshot.board_entries[0] = vec![task.clone()];
    app.task_agent_session_states.insert(
        "retry-session".into(),
        AgentSessionControlState::ResumeRequested,
    );
    assert!(
        app.task_display(&task, TaskStatus::Todo, true)
            .starts_with("[Retry in 4:32]")
    );
    for pane in [TuiPane::Tasks, TuiPane::AgentProjects] {
        app.current_pane = pane;
        for (width, height) in [(120, 30), (80, 24), (40, 12)] {
            app.prepare_render(Rect::new(0, 0, width, height));
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| render_tui(frame, &app)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("Retry in 4:32"), "{text}");
        }
    }
    app.agent_panel.projects[0].runtime_state = TuiAgentRuntimeState::Resuming;
    app.agent_panel.projects[0].activity = Some(WorkerActivity {
        phase: WorkerActivityPhase::Resuming,
        summary: "Resuming Retry demo".into(),
        session_id: Some("retry-session".into()),
    });
    assert!(
        app.task_display(&task, TaskStatus::Todo, true)
            .starts_with("[RESUMING]")
    );
    assert!(
        task_badge(
            &app,
            TaskStatus::Todo,
            &self::task("Other codex:other-session")
        )
        .is_none()
    );
    app.task_agent_session_states
        .insert("retry-session".into(), AgentSessionControlState::Running);
    assert!(
        app.task_display(&task, TaskStatus::Doing, true)
            .starts_with("[RUNNING]")
    );
}

#[test]
fn stopped_manual_blocked_and_completed_tasks_never_get_retry_badges() {
    let mut app = TuiApp::new(Path::new("/tmp/retry-demo"), true);
    app.agent_panel.projects = vec![retry_project()];
    for content in [
        "Task codex:session clt:stopped",
        "Task clt:manual codex:session",
        "Task — BLOCKED 2026-11-10: Waiting for input codex:session",
    ] {
        assert!(
            task_badge(&app, TaskStatus::Todo, &task(content)).is_none(),
            "{content}"
        );
    }
    assert!(task_badge(&app, TaskStatus::Done, &task("Task codex:session")).is_none());
    assert!(task_badge(&app, TaskStatus::Backlog, &task("Task codex:session")).is_none());
    app.task_agent_session_states
        .insert("session".into(), AgentSessionControlState::Stopped);
    assert!(task_badge(&app, TaskStatus::Todo, &task("Task codex:session")).is_none());
}

#[test]
fn reason_is_bound_to_the_failure_timestamp() {
    let project = retry_project();
    let mut run = agent::AgentRunRecord {
        id: 1,
        project_id: 1,
        project_name: "demo".into(),
        project_path: "/tmp/demo".into(),
        status: "failure".into(),
        started_at: "90".into(),
        finished_at: Some("100".into()),
        exit_code: Some(1),
        stdout_path: None,
        stderr_path: None,
        summary: Some("Codex runner failed before completion: Connection interrupted".into()),
        codex_session_id: None,
    };
    let retry = RetryStatus::new(
        &project.project,
        Some(&run),
        Duration::from_secs(300),
        128,
        true,
    )
    .unwrap();
    assert!(retry.message.contains("Connection interrupted"));
    run.finished_at = Some("50".into());
    let retry = RetryStatus::new(
        &project.project,
        Some(&run),
        Duration::from_secs(300),
        128,
        true,
    )
    .unwrap();
    assert!(!retry.message.contains("Connection interrupted"));
}

#[test]
fn retry_eligibility_uses_board_controls_and_preserves_owned_reservations() {
    let root = tempfile::tempdir().unwrap();
    let board = root.path().join("tasks");
    fs::create_dir(&board).unwrap();
    fs::write(board.join("doing.md"), "# Doing\n").unwrap();
    let mut project = retry_project();
    project.project.path = root.path().into();
    let mut control = AgentSessionControlRecord {
        project_id: project.project.id,
        codex_session_id: "session".into(),
        state: AgentSessionControlState::Stopped,
        child_pid: None,
        run_token: Some("attempt".into()),
        interactive_holder: None,
        interactive_launch_token: None,
        stdout_path: None,
        stderr_path: None,
        updated_at: "100".into(),
    };
    fs::write(
        board.join("todo.md"),
        "# Todo\n- Retry task codex:session\n",
    )
    .unwrap();
    assert!(!has_retryable_tasks(&project, &[control.clone()]));
    control.state = AgentSessionControlState::ResumeRequested;
    assert!(has_retryable_tasks(&project, &[control.clone()]));
    fs::write(
        board.join("todo.md"),
        "# Todo\n- Retry task clt:manual codex:session\n",
    )
    .unwrap();
    assert!(!has_retryable_tasks(&project, &[control.clone()]));
    project.runtime_state = TuiAgentRuntimeState::Fenced;
    control.child_pid = Some(4242);
    apply(&mut project, &[control.clone()], false, true);
    assert_eq!(project.runtime_state, TuiAgentRuntimeState::Fenced);
    control.child_pid = None;
    control.interactive_launch_token = Some("handoff".into());
    apply(&mut project, &[control], false, true);
    assert_eq!(project.runtime_state, TuiAgentRuntimeState::Fenced);
    apply(&mut project, &[], false, false);
    assert_eq!(project.runtime_state, TuiAgentRuntimeState::Fenced);
}
