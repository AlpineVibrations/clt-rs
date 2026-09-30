use crate::application::git_recovery::{plan_git_recovery, recover_git_task};
use crate::test_support::prelude::*;
use crate::test_support::*;

const SESSION: &str = "01900000-0000-7000-8000-000000000001";
const ERROR: &str = "Known Codex session has no frozen Git start journal; CLT will not reconstruct the task boundary from a later checkout";

fn fixture(folders: bool, status: TaskStatus) -> (PathBuf, TursoAgentStore, AgentProject) {
    let root = temp_root("missing-journal-recovery");
    let project_root = root.join("project");
    init_tasks(&project_root, folders).unwrap();
    TaskBoard::new(get_tasks_dir(&project_root))
        .insert_content(
            status,
            None,
            &format!("Finish feature. BLOCKED 2026-09-20: interrupted codex:{SESSION}"),
        )
        .unwrap();
    fs::write(project_root.join("feature.txt"), "before\n").unwrap();
    initialize_test_git_repository(&project_root);
    fs::write(project_root.join("feature.txt"), "partially implemented\n").unwrap();
    run_test_git(&project_root, &["add", "feature.txt"]);
    fs::write(
        project_root.join("feature.txt"),
        "additional user changes\n",
    )
    .unwrap();
    fs::write(project_root.join("untracked.txt"), "preserve this\n").unwrap();
    let store = TursoAgentStore::open_blocking(&root.join("state")).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .set_project_git_mode_blocking(project.id, AgentGitMode::Commit)
        .unwrap();
    let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    store
        .record_git_launch_state_blocking(
            project.id,
            "old-run",
            AgentGitMode::Commit,
            &start,
            &agent_timestamp(),
        )
        .unwrap();
    store
        .mark_session_running_with_git_mode_blocking(
            project.id,
            SESSION,
            12345,
            "old-run",
            &root.join("old.out"),
            &root.join("old.err"),
            AgentGitMode::Commit,
        )
        .unwrap();
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                SESSION,
                0,
                GitFinalizationState::Cancelled,
                None,
                None,
                Some("Simulate a removed historical journal"),
                &agent_timestamp(),
            )
            .unwrap()
    );
    assert!(
        store
            .delete_terminal_git_finalization_blocking(project.id, SESSION)
            .unwrap()
    );
    store
        .set_session_control_recovery_token_blocking(project.id, SESSION, "old-run")
        .unwrap();
    store
        .record_run_outcome_blocking(AgentRunOutcome {
            project_id: project.id,
            status: "failure",
            started_at: "99",
            finished_at: Some("100"),
            exit_code: None,
            log_dir: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some(ERROR),
            codex_session_id: Some(SESSION),
        })
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    (root, store, project)
}

#[test]
fn recovery_queues_fresh_work_and_preserves_git_files_history_and_old_session() {
    for folders in [false, true] {
        let (root, store, project) = fixture(folders, TaskStatus::Doing);
        let before_head = run_test_git(&project.path, &["rev-parse", "HEAD"]);
        let before_index = run_test_git(&project.path, &["write-tree"]);
        let plan = plan_git_recovery(&store, &project, None).unwrap();
        assert!(plan.prompt().contains("fresh Codex conversation"));
        let message = recover_git_task(&store, &plan).unwrap();
        assert!(message.contains("Recovered task"));
        assert_eq!(
            run_test_git(&project.path, &["rev-parse", "HEAD"]),
            before_head
        );
        assert_eq!(run_test_git(&project.path, &["write-tree"]), before_index);
        assert_eq!(
            fs::read_to_string(project.path.join("feature.txt")).unwrap(),
            "additional user changes\n"
        );
        assert_eq!(
            fs::read_to_string(project.path.join("untracked.txt")).unwrap(),
            "preserve this\n"
        );
        let board = TaskBoard::new(get_tasks_dir(&project.path));
        assert!(board.entries(TaskStatus::Doing).unwrap().is_empty());
        let entries = board.entries(TaskStatus::Todo).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(task_entry_is_ready(&entries[0]));
        assert!(
            entries[0]
                .content
                .contains(&format!("Previous Codex session: {SESSION}"))
        );
        assert!(entries[0].content.contains("implement only what remains"));
        assert!(recoverable_codex_session_id_from_task_content(&entries[0].content).is_none());
        assert!(
            automated_codex_session_to_resume(&project.path, AgentTaskSelection::NextTodo)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .session_control_blocking(project.id, SESSION)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Stopped
        );
        assert_eq!(
            store
                .session_git_mode_blocking(project.id, SESSION)
                .unwrap(),
            Some(AgentGitMode::Commit)
        );
        assert!(
            store
                .git_finalization_blocking(project.id, SESSION)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .is_none()
        );
        let recovered = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(recovered.enabled, project.enabled);
        assert_eq!(recovered.git_mode, project.git_mode);
        assert_eq!(recovered.failure_count, 0);
        // A second confirmation cannot create a duplicate task.
        assert!(recover_git_task(&store, &plan).is_err());
        assert_eq!(board.entries(TaskStatus::Todo).unwrap().len(), 1);
        // The new attempt uses ordinary preparation; the old journal stays missing.
        assert!(
            prepare_agent_git_start_state_for_run(
                &store,
                &project,
                AgentTaskSelection::NextTodo,
                false,
                false,
                "new-run",
            )
            .unwrap()
            .is_some()
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn recovery_accepts_done_without_rewriting_the_task_or_starting_more_work() {
    let (root, store, project) = fixture(false, TaskStatus::Done);
    let before = fs::read(project.path.join("tasks/done.md")).unwrap();
    let plan = plan_git_recovery(&store, &project, None).unwrap();
    assert!(plan.prompt().contains("already in Done"));
    assert!(
        recover_git_task(&store, &plan)
            .unwrap()
            .contains("Accepted completed task")
    );
    assert_eq!(
        fs::read(project.path.join("tasks/done.md")).unwrap(),
        before
    );
    assert!(
        TaskBoard::new(get_tasks_dir(&project.path))
            .entries(TaskStatus::Todo)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .resume_requested_session_blocking(project.id)
            .unwrap()
            .is_none()
    );
    recover_git_task(&store, &plan).unwrap();
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recovery_rechecks_the_task_and_run_after_confirmation() {
    for changed_task in [true, false] {
        let (root, store, project) = fixture(false, TaskStatus::Doing);
        assert!(
            plan_git_recovery(&store, &project, Some("wrong-session"))
                .err()
                .unwrap()
                .to_string()
                .contains("does not match the failed run")
        );
        let plan = plan_git_recovery(&store, &project, None).unwrap();
        if changed_task {
            fs::write(
                project.path.join("tasks/doing.md"),
                format!("# Doing\n- Changed task codex:{SESSION}\n"),
            )
            .unwrap();
        } else {
            store
                .record_run_outcome_blocking(AgentRunOutcome {
                    project_id: project.id,
                    status: "success",
                    started_at: "101",
                    finished_at: Some("102"),
                    exit_code: Some(0),
                    log_dir: None,
                    stdout_path: None,
                    stderr_path: None,
                    summary: Some("Finished"),
                    codex_session_id: Some(SESSION),
                })
                .unwrap();
        }
        let before = fs::read(project.path.join("tasks/doing.md")).unwrap();
        assert!(recover_git_task(&store, &plan).is_err());
        assert_eq!(
            fs::read(project.path.join("tasks/doing.md")).unwrap(),
            before
        );
        assert!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .is_none()
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn recovery_refuses_live_owners_and_surviving_launch_or_session_journals() {
    for fence in ["lease", "live-session", "launch", "journal"] {
        let (root, store, project) = fixture(false, TaskStatus::Doing);
        let plan = plan_git_recovery(&store, &project, None).unwrap();
        match fence {
            "lease" => {
                store
                    .try_acquire_lease_blocking(
                        project.id,
                        "other",
                        &agent_timestamp(),
                        &agent_timestamp_after(60),
                    )
                    .unwrap();
            }
            "live-session" => {
                store
                    .mark_session_running_with_git_mode_blocking(
                        project.id,
                        "another-session",
                        12345,
                        "active",
                        &root.join("out"),
                        &root.join("err"),
                        AgentGitMode::Off,
                    )
                    .unwrap();
            }
            "launch" => {
                let start =
                    capture_agent_git_start_state(&project.path, AgentGitMode::Commit).unwrap();
                store
                    .record_git_launch_state_blocking(
                        project.id,
                        "launch",
                        AgentGitMode::Commit,
                        &start,
                        &agent_timestamp(),
                    )
                    .unwrap();
            }
            "journal" => {
                let start =
                    capture_agent_git_start_state(&project.path, AgentGitMode::Commit).unwrap();
                assert!(
                    store
                        .create_git_finalization_blocking(NewGitFinalization {
                            project_id: project.id,
                            codex_session_id: SESSION,
                            git_mode: AgentGitMode::Commit,
                            starting_head: Some(&start.starting_head),
                            branch_ref: start.branch_ref.as_deref(),
                            upstream_ref: start.upstream_ref.as_deref(),
                            worktree_baseline: &start.worktree_baseline,
                            task_identity: None,
                            owner_run_token: None,
                            created_at: &agent_timestamp(),
                        })
                        .unwrap()
                );
            }
            _ => unreachable!(),
        }
        let before = fs::read(project.path.join("tasks/doing.md")).unwrap();
        assert!(
            recover_git_task(&store, &plan)
                .unwrap_err()
                .to_string()
                .contains("idle project"),
            "{fence}"
        );
        assert_eq!(
            fs::read(project.path.join("tasks/doing.md")).unwrap(),
            before
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn recovery_keeps_an_interrupted_publication_stopped_and_retryable() {
    let (root, store, project) = fixture(true, TaskStatus::Doing);
    let plan = plan_git_recovery(&store, &project, None).unwrap();
    let TaskSource::Path { path, .. } = &plan.task.source else {
        unreachable!()
    };
    let original_permissions = fs::metadata(path).unwrap().permissions();
    let mut readonly = original_permissions.clone();
    readonly.set_readonly(true);
    fs::set_permissions(path, readonly).unwrap();
    assert!(recover_git_task(&store, &plan).is_err());
    let todo = TaskBoard::new(get_tasks_dir(&project.path))
        .entries(TaskStatus::Doing)
        .unwrap()
        .remove(0);
    assert_eq!(
        recoverable_codex_session_id_from_task_content(&todo.content),
        Some(SESSION)
    );
    assert_eq!(
        store
            .session_control_blocking(project.id, SESSION)
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Stopped
    );
    assert!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .is_none()
    );
    let TaskSource::Path { path, .. } = &todo.source else {
        unreachable!()
    };
    fs::set_permissions(path, original_permissions).unwrap();
    let retry = plan_git_recovery(&store, &project, None).unwrap();
    recover_git_task(&store, &retry).unwrap();
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recovery_resumes_an_interrupted_move_even_when_the_failed_run_has_no_session_id() {
    let (root, store, project) = fixture(false, TaskStatus::Doing);
    store
        .record_run_outcome_blocking(AgentRunOutcome {
            project_id: project.id,
            status: "failure",
            started_at: "101",
            finished_at: Some("102"),
            exit_code: None,
            log_dir: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some(ERROR),
            codex_session_id: None,
        })
        .unwrap();
    let plan = plan_git_recovery(&store, &project, None).unwrap();
    // Simulate interruption after stopping and moving, before detaching the old link.
    store
        .begin_missing_git_recovery_blocking(
            project.id,
            &project.path,
            SESSION,
            plan.run_id,
            "recovery",
            &agent_timestamp(),
            &agent_timestamp_after(60),
        )
        .unwrap();
    let board = TaskBoard::new(get_tasks_dir(&project.path));
    {
        let _lock = acquire_board_mutation_lock(board.path()).unwrap();
        board
            .write_entry_content(
                TaskStatus::Doing,
                &plan.task,
                &format!("{} clt:stopped", plan.task.content.trim_end()),
            )
            .unwrap();
        move_task_without_reordering_after_lock(
            board.path(),
            TaskStatus::Doing,
            TaskStatus::Todo,
            1,
        )
        .unwrap();
    }
    store
        .release_lease_blocking(project.id, "recovery")
        .unwrap();
    assert!(task_entry_is_stopped(
        &board.entries(TaskStatus::Todo).unwrap()[0]
    ));
    let retry = plan_git_recovery(&store, &project, None).unwrap();
    recover_git_task(&store, &retry).unwrap();
    assert!(task_entry_is_ready(
        &board.entries(TaskStatus::Todo).unwrap()[0]
    ));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recovery_confirmation_is_visible_and_captures_navigation_keys() {
    let (root, store, project) = fixture(false, TaskStatus::Doing);
    let plan = plan_git_recovery(&store, &project, None).unwrap();
    let mut app = TuiApp::new(&project.path, false);
    app.agent_panel.last_error = Some(ERROR.to_string());
    app.pending_git_recovery = Some(plan);
    for code in [
        KeyCode::Char('y'),
        KeyCode::Char('n'),
        KeyCode::Esc,
        KeyCode::Tab,
    ] {
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            update_tui_pane(&mut app, key),
            Some(vec![TuiEffect::PaneKey(key)])
        );
    }
    for width in [80, 120] {
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| render_tui(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Recover project: Finish feature."));
        assert!(rendered.contains("[y/n]"));
        assert!(!rendered.contains("no frozen Git start journal"));
    }
    // Merely viewing the confirmation has no persistent effect.
    app.pending_git_recovery = None;
    assert_eq!(
        store
            .session_control_blocking(project.id, SESSION)
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::ResumeRequested
    );
    assert!(
        TaskBoard::new(get_tasks_dir(&project.path))
            .entries(TaskStatus::Todo)
            .unwrap()
            .is_empty()
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
