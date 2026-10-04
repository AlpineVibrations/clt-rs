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
    let TaskSource::Path { path, .. } = &plan.linked.as_ref().unwrap().task.source else {
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
            None,
            false,
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
                &plan.linked.as_ref().unwrap().task,
                &format!(
                    "{} clt:stopped",
                    plan.linked.as_ref().unwrap().task.content.trim_end()
                ),
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

fn branch_fixture(folders: bool, status: TaskStatus) -> (PathBuf, TursoAgentStore, AgentProject) {
    let (root, store, project) = fixture(folders, status);
    let start = capture_agent_git_start_state(&project.path, AgentGitMode::Commit).unwrap();
    let task = TaskBoard::new(get_tasks_dir(&project.path))
        .entries(status)
        .unwrap()
        .remove(0);
    assert!(
        store
            .create_git_finalization_blocking(NewGitFinalization {
                project_id: project.id,
                codex_session_id: SESSION,
                git_mode: AgentGitMode::CommitAndPush,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &start.worktree_baseline,
                task_identity: durable_task_identity(&task.content).as_deref(),
                owner_run_token: None,
                created_at: &agent_timestamp(),
            })
            .unwrap()
    );
    for (generation, state) in [
        (0, GitFinalizationState::Tracking),
        (1, GitFinalizationState::CommitPending),
    ] {
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    SESSION,
                    generation,
                    state,
                    None,
                    None,
                    None,
                    &agent_timestamp()
                )
                .unwrap()
        );
    }
    run_test_git(&project.path, &["switch", "-c", "auth"]);
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
            summary: Some("Task Git finalization remains FINALIZING"),
            codex_session_id: Some(SESSION),
        })
        .unwrap();
    (root, store, project)
}

#[test]
fn branch_recovery_retires_pending_proof_and_requeues_even_provisional_done() {
    for folders in [false, true] {
        for status in [TaskStatus::Doing, TaskStatus::Done] {
            let (root, store, project) = branch_fixture(folders, status);
            let before = store
                .git_finalization_blocking(project.id, SESSION)
                .unwrap()
                .unwrap();
            let branch = run_test_git(&project.path, &["symbolic-ref", "HEAD"]);
            let head = run_test_git(&project.path, &["rev-parse", "HEAD"]);
            let index = run_test_git(&project.path, &["write-tree"]);
            let plan = plan_git_recovery(&store, &project, None).unwrap();
            assert!(plan.prompt().contains("refs/heads/auth"));
            assert!(plan.prompt().contains("provisional Done"));
            recover_git_task(&store, &plan).unwrap();
            let after = store
                .git_finalization_blocking(project.id, SESSION)
                .unwrap()
                .unwrap();
            assert_eq!(after.state, GitFinalizationState::Cancelled);
            assert_eq!(after.branch_ref, before.branch_ref);
            assert_eq!(after.starting_head, before.starting_head);
            assert_eq!(after.worktree_baseline, before.worktree_baseline);
            assert_eq!(after.task_identity, before.task_identity);
            assert!(after.commit_oid.is_none());
            assert_eq!(
                run_test_git(&project.path, &["symbolic-ref", "HEAD"]),
                branch
            );
            assert_eq!(run_test_git(&project.path, &["rev-parse", "HEAD"]), head);
            assert_eq!(run_test_git(&project.path, &["write-tree"]), index);
            assert_eq!(
                fs::read_to_string(project.path.join("feature.txt")).unwrap(),
                "additional user changes\n"
            );
            assert_eq!(
                fs::read_to_string(project.path.join("untracked.txt")).unwrap(),
                "preserve this\n"
            );
            let board = TaskBoard::new(get_tasks_dir(&project.path));
            assert!(board.entries(status).unwrap().is_empty());
            let todo = board.entries(TaskStatus::Todo).unwrap().remove(0);
            assert!(task_entry_is_ready(&todo));
            assert!(recoverable_codex_session_id_from_task_content(&todo.content).is_none());
            assert!(todo.content.contains("previous Git attempt was retired"));
            assert!(recover_git_task(&store, &plan).is_err());
            let fresh = prepare_agent_git_start_state_for_run(
                &store,
                &project,
                AgentTaskSelection::NextTodo,
                false,
                false,
                "fresh-branch-run",
            )
            .unwrap()
            .unwrap();
            assert_eq!(fresh.branch_ref.as_deref(), Some("refs/heads/auth"));
            drop(store);
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn branch_recovery_rechecks_checkout_journal_and_live_ownership() {
    for change in [
        "branch",
        "journal",
        "commit",
        "lease",
        "launch",
        "session",
        "other-journal",
    ] {
        let (root, store, project) = branch_fixture(false, TaskStatus::Done);
        let plan = plan_git_recovery(&store, &project, None).unwrap();
        match change {
            "branch" => {
                run_test_git(&project.path, &["switch", "-c", "another"]);
            }
            "journal" | "commit" => {
                assert!(
                    store
                        .compare_and_set_git_finalization_blocking(
                            project.id,
                            SESSION,
                            2,
                            if change == "commit" {
                                GitFinalizationState::PushPending
                            } else {
                                GitFinalizationState::CommitPending
                            },
                            None,
                            if change == "commit" {
                                Some("verified-commit")
                            } else {
                                None
                            },
                            Some("changed"),
                            &agent_timestamp()
                        )
                        .unwrap()
                );
            }
            "lease" => {
                assert!(
                    store
                        .try_acquire_lease_blocking(
                            project.id,
                            "someone",
                            &agent_timestamp(),
                            &agent_timestamp_after(60)
                        )
                        .unwrap()
                );
            }
            "launch" => {
                let start =
                    capture_agent_git_start_state(&project.path, AgentGitMode::Commit).unwrap();
                store
                    .record_git_launch_state_blocking(
                        project.id,
                        "other-launch",
                        AgentGitMode::Commit,
                        &start,
                        &agent_timestamp(),
                    )
                    .unwrap();
            }
            "session" => {
                store
                    .mark_session_running_with_git_mode_blocking(
                        project.id,
                        "other-session",
                        12345,
                        "live",
                        &root.join("out"),
                        &root.join("err"),
                        AgentGitMode::Off,
                    )
                    .unwrap();
            }
            "other-journal" => {
                let start =
                    capture_agent_git_start_state(&project.path, AgentGitMode::Commit).unwrap();
                store
                    .create_git_finalization_blocking(NewGitFinalization {
                        project_id: project.id,
                        codex_session_id: "other-session",
                        git_mode: AgentGitMode::Commit,
                        starting_head: Some(&start.starting_head),
                        branch_ref: start.branch_ref.as_deref(),
                        upstream_ref: None,
                        worktree_baseline: &start.worktree_baseline,
                        task_identity: None,
                        owner_run_token: None,
                        created_at: &agent_timestamp(),
                    })
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let journal = store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap();
        let board = fs::read(project.path.join("tasks/done.md")).unwrap();
        assert!(recover_git_task(&store, &plan).is_err(), "{change}");
        assert_eq!(
            store
                .git_finalization_blocking(project.id, SESSION)
                .unwrap(),
            journal
        );
        assert_eq!(fs::read(project.path.join("tasks/done.md")).unwrap(), board);
        if change == "commit" {
            assert!(plan_git_recovery(&store, &project, None).is_err());
        }
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn branch_recovery_can_retry_after_retiring_journal_but_failing_board_write() {
    let (root, store, project) = branch_fixture(true, TaskStatus::Done);
    let plan = plan_git_recovery(&store, &project, None).unwrap();
    let TaskSource::Path { path, .. } = &plan.linked.as_ref().unwrap().task.source else {
        unreachable!()
    };
    let permissions = fs::metadata(path).unwrap().permissions();
    let mut readonly = permissions.clone();
    readonly.set_readonly(true);
    fs::set_permissions(path, readonly).unwrap();
    assert!(recover_git_task(&store, &plan).is_err());
    assert_eq!(
        store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap()
            .unwrap()
            .state,
        GitFinalizationState::Cancelled
    );
    fs::set_permissions(path, permissions).unwrap();
    let retry = plan_git_recovery(&store, &project, None).unwrap();
    recover_git_task(&store, &retry).unwrap();
    assert!(task_entry_is_ready(
        &TaskBoard::new(get_tasks_dir(&project.path))
            .entries(TaskStatus::Todo)
            .unwrap()[0]
    ));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn resume_branch_check_rejects_switched_and_detached_checkouts_without_changes() {
    let (root, store, project) = branch_fixture(false, TaskStatus::Done);
    let journal = store
        .git_finalization_blocking(project.id, SESSION)
        .unwrap()
        .unwrap();
    for detached in [false, true] {
        if detached {
            run_test_git(&project.path, &["checkout", "--detach"]);
        }
        let index = run_test_git(&project.path, &["write-tree"]);
        let error = crate::managed_git::verify_agent_git_resume_branch(&project.path, &journal)
            .unwrap_err();
        assert!(error.to_string().contains("Git task branch changed:"));
        assert_eq!(run_test_git(&project.path, &["write-tree"]), index);
    }
    run_test_git(
        &project.path,
        &[
            "switch",
            journal
                .branch_ref
                .as_deref()
                .unwrap()
                .strip_prefix("refs/heads/")
                .unwrap(),
        ],
    );
    crate::managed_git::verify_agent_git_resume_branch(&project.path, &journal).unwrap();
    assert!(plan_git_recovery(&store, &project, None).is_err());
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn runner_refuses_branch_mismatch_before_spawning_codex() {
    use std::os::unix::fs::PermissionsExt;

    let (root, store, project) = branch_fixture(false, TaskStatus::Done);
    let journal = store
        .git_finalization_blocking(project.id, SESSION)
        .unwrap();
    store
        .try_acquire_lease_blocking(
            project.id,
            "resume-holder",
            &agent_timestamp(),
            &agent_timestamp_after(60),
        )
        .unwrap();
    let fake_codex = root.join("fake-codex");
    fs::write(&fake_codex, "#!/bin/sh\ntouch child-was-launched\n").unwrap();
    fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o755)).unwrap();
    let runner =
        CodexAgentRunner::with_command(root.join("state"), Duration::from_secs(5), fake_codex);
    let error = runner
        .run_project(
            &project,
            AgentTaskSelection::ResumeSession,
            Some(SESSION),
            "resume-holder",
            None,
            &new_agent_shutdown_signal(),
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("Git task branch changed:"),
        "{error:#}"
    );
    assert!(!project.path.join("child-was-launched").exists());
    assert_eq!(
        store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap(),
        journal
    );
    let completion = run_agent_job(
        AgentRunJob {
            state_dir: root.join("state"),
            project: project.clone(),
            holder: "resume-holder".to_string(),
            worker_token: None,
            max_global_jobs: 1,
            task_selection: AgentTaskSelection::ResumeSession,
            resume_session_id: Some(SESSION.to_string()),
            blocked_task_count_before: 0,
            done_task_contents_before: Vec::new(),
            blocked_task_snapshots_before: Vec::new(),
        },
        &runner,
        &new_agent_shutdown_signal(),
    )
    .unwrap();
    assert_eq!(completion.status, "failure");
    assert!(completion.summary.contains("Git task branch changed:"));
    let saved = store
        .latest_run_for_project_blocking(project.id)
        .unwrap()
        .unwrap();
    assert!(saved.summary.unwrap().contains("Git task branch changed:"));
    assert!(!project.path.join("child-was-launched").exists());
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn branch_mismatch_diagnostic_offers_recovery_with_a_readable_confirmation() {
    let (root, store, project) = branch_fixture(false, TaskStatus::Done);
    let journal = store
        .git_finalization_blocking(project.id, SESSION)
        .unwrap()
        .unwrap();
    let summary = crate::managed_git::verify_agent_git_resume_branch(&project.path, &journal)
        .unwrap_err()
        .to_string();
    store
        .record_run_outcome_blocking(AgentRunOutcome {
            project_id: project.id,
            status: "failure",
            started_at: "103",
            finished_at: Some("104"),
            exit_code: None,
            log_dir: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some(&summary),
            codex_session_id: Some(SESSION),
        })
        .unwrap();
    let run = store
        .latest_run_for_project_blocking(project.id)
        .unwrap()
        .unwrap();
    let problem =
        tui_agent_failure_problem(&project, Some(&run), 105, Duration::from_secs(300)).unwrap();
    assert!(problem.starts_with("Git recovery available - press r"));
    assert!(problem.contains("refs/heads/auth"));
    assert!(problem.contains(journal.branch_ref.as_deref().unwrap()));
    assert!(!problem.contains("Automatic retry"));
    let plan = plan_git_recovery(&store, &project, None).unwrap();
    let mut app = TuiApp::new(&project.path, false);
    app.pending_git_recovery = Some(plan);
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
        assert!(rendered.contains("[y/n]"));
        assert!(rendered.contains("refs/heads/auth"));
    }
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn branch_recovery_retires_orphan_without_touching_replacement_task_or_git() {
    for folders in [false, true] {
        let (root, store, project) = branch_fixture(folders, TaskStatus::Done);
        let board = TaskBoard::new(get_tasks_dir(&project.path));
        let task = board.entries(TaskStatus::Done).unwrap().remove(0);
        let replacement = task.content.replace(SESSION, "replacement-session");
        board
            .write_entry_content(TaskStatus::Done, &task, &replacement)
            .unwrap();
        let head = run_test_git(&project.path, &["rev-parse", "HEAD"]);
        let index = run_test_git(&project.path, &["write-tree"]);
        let before = store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap()
            .unwrap();
        let plan = plan_git_recovery(&store, &project, None).unwrap();
        assert!(plan.prompt().contains("orphaned Git attempt"));
        assert!(
            recover_git_task(&store, &plan)
                .unwrap()
                .contains("Retired orphaned")
        );
        assert_eq!(
            board.entries(TaskStatus::Done).unwrap()[0].content,
            replacement
        );
        assert!(board.entries(TaskStatus::Todo).unwrap().is_empty());
        assert_eq!(run_test_git(&project.path, &["rev-parse", "HEAD"]), head);
        assert_eq!(run_test_git(&project.path, &["write-tree"]), index);
        assert_eq!(
            fs::read_to_string(project.path.join("feature.txt")).unwrap(),
            "additional user changes\n"
        );
        let after = store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap()
            .unwrap();
        assert_eq!(after.state, GitFinalizationState::Cancelled);
        assert_eq!(after.worktree_baseline, before.worktree_baseline);
        assert_eq!(after.starting_head, before.starting_head);
        assert_eq!(after.branch_ref, before.branch_ref);
        assert!(after.commit_oid.is_none());
        assert!(
            store
                .resume_requested_session_blocking(project.id)
                .unwrap()
                .is_none()
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn orphan_branch_recovery_rechecks_links_and_rejects_same_branch() {
    let (root, store, project) = branch_fixture(false, TaskStatus::Done);
    let board = TaskBoard::new(get_tasks_dir(&project.path));
    let original = fs::read(project.path.join("tasks/done.md")).unwrap();
    fs::write(project.path.join("tasks/done.md"), "# Done\n").unwrap();
    let plan = plan_git_recovery(&store, &project, None).unwrap();
    fs::write(project.path.join("tasks/done.md"), &original).unwrap();
    assert!(
        recover_git_task(&store, &plan)
            .unwrap_err()
            .to_string()
            .contains("links changed")
    );
    assert_eq!(board.entries(TaskStatus::Done).unwrap().len(), 1);
    fs::write(project.path.join("tasks/done.md"), "# Done\n").unwrap();
    let journal = store
        .git_finalization_blocking(project.id, SESSION)
        .unwrap()
        .unwrap();
    run_test_git(
        &project.path,
        &[
            "switch",
            journal
                .branch_ref
                .as_deref()
                .unwrap()
                .strip_prefix("refs/heads/")
                .unwrap(),
        ],
    );
    assert!(plan_git_recovery(&store, &project, None).is_err());
    assert_eq!(
        store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap()
            .unwrap(),
        journal
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scheduler_automatically_recovers_changed_branch_and_starts_fresh_work() {
    for orphan in [false, true] {
        let (root, store, project) = branch_fixture(false, TaskStatus::Done);
        if orphan {
            fs::write(project.path.join("tasks/done.md"), "# Done\n- Finished elsewhere. COMPLETED 2026-10-04: verified codex:replacement-session\n").unwrap();
            add_task(&project.path, "Next feature", None).unwrap();
        }
        let done_before = fs::read(project.path.join("tasks/done.md")).unwrap();
        let head = run_test_git(&project.path, &["rev-parse", "HEAD"]);
        let index = run_test_git(&project.path, &["write-tree"]);
        let state_dir = root.join("state");
        let pass =
            run_agent_scheduler_pass_with_max_global_jobs(&state_dir, false, &[], 1, None).unwrap();
        assert!(pass.jobs.is_empty(), "Old session must not be launched");
        let journal = store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap()
            .unwrap();
        assert_eq!(journal.state, GitFinalizationState::Cancelled);
        assert_eq!(store.list_projects_blocking().unwrap()[0].failure_count, 0);
        assert_eq!(run_test_git(&project.path, &["rev-parse", "HEAD"]), head);
        assert_eq!(run_test_git(&project.path, &["write-tree"]), index);
        if orphan {
            assert_eq!(
                fs::read(project.path.join("tasks/done.md")).unwrap(),
                done_before
            );
        }
        let count = store.run_count_blocking().unwrap();
        let next =
            run_agent_scheduler_pass_with_max_global_jobs(&state_dir, false, &[], 1, None).unwrap();
        assert_eq!(next.jobs.len(), 1);
        assert_eq!(next.jobs[0].task_selection, AgentTaskSelection::NextTodo);
        assert!(next.jobs[0].resume_session_id.is_none());
        assert_eq!(store.run_count_blocking().unwrap(), count);
        store
            .release_lease_blocking(project.id, &next.jobs[0].holder)
            .unwrap();
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn scheduler_branch_recovery_preserves_stopped_and_verified_attempts() {
    for verified in [false, true] {
        let (root, store, project) = branch_fixture(false, TaskStatus::Done);
        if verified {
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    SESSION,
                    2,
                    GitFinalizationState::PushPending,
                    None,
                    Some("verified-commit"),
                    None,
                    &agent_timestamp(),
                )
                .unwrap();
        } else {
            store
                .set_session_control_state_blocking(
                    project.id,
                    SESSION,
                    AgentSessionControlState::Stopped,
                )
                .unwrap();
        }
        let before = fs::read(project.path.join("tasks/done.md")).unwrap();
        let pass =
            run_agent_scheduler_pass_with_max_global_jobs(&root.join("state"), false, &[], 1, None)
                .unwrap();
        assert!(pass.jobs.is_empty());
        let journal = store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap()
            .unwrap();
        assert_eq!(
            journal.state,
            if verified {
                GitFinalizationState::PushPending
            } else {
                GitFinalizationState::CommitPending
            }
        );
        assert_eq!(
            fs::read(project.path.join("tasks/done.md")).unwrap(),
            before
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn scheduler_does_not_repeat_failed_branch_recovery_or_launch_on_detached_head() {
    let (root, store, project) = branch_fixture(false, TaskStatus::Done);
    run_test_git(&project.path, &["switch", "--detach"]);
    let board_before = fs::read(project.path.join("tasks/done.md")).unwrap();
    let journal_before = store
        .git_finalization_blocking(project.id, SESSION)
        .unwrap();
    let mut reported_run = None;
    for _ in 0..2 {
        let pass =
            run_agent_scheduler_pass_with_max_global_jobs(&root.join("state"), false, &[], 1, None)
                .unwrap();
        assert!(pass.jobs.is_empty());
        let run = store
            .latest_run_for_project_blocking(project.id)
            .unwrap()
            .unwrap();
        assert!(run.summary.as_deref().unwrap().contains("detached HEAD"));
        if let Some(previous) = reported_run {
            assert_eq!(run.id, previous);
        }
        reported_run = Some(run.id);
    }
    assert_eq!(
        store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap(),
        journal_before
    );
    assert_eq!(
        fs::read(project.path.join("tasks/done.md")).unwrap(),
        board_before
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn automatic_branch_recovery_preserves_task_stop_and_rechecks_session_stop() {
    for stopped_task in [false, true] {
        let (root, store, project) = branch_fixture(false, TaskStatus::Done);
        let plan = plan_git_recovery(&store, &project, None).unwrap();
        let journal = store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap();
        if stopped_task {
            let board = TaskBoard::new(get_tasks_dir(&project.path));
            let task = board.entries(TaskStatus::Done).unwrap().remove(0);
            board
                .write_entry_content(
                    TaskStatus::Done,
                    &task,
                    &format!("{} clt:stopped", task.content),
                )
                .unwrap();
            assert!(
                super::recover_changed_branch_automatically(&store, &project, SESSION)
                    .unwrap()
                    .is_none()
            );
        } else {
            // A stop after the preview must be checked in the same transaction
            // that would cancel the journal, not just at scheduler selection.
            store
                .set_session_control_state_blocking(
                    project.id,
                    SESSION,
                    AgentSessionControlState::Stopped,
                )
                .unwrap();
            assert!(
                super::execute_git_recovery(&store, &plan, true)
                    .unwrap_err()
                    .to_string()
                    .contains("session was stopped")
            );
            assert!(
                store
                    .lease_for_project_blocking(project.id)
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            store
                .git_finalization_blocking(project.id, SESSION)
                .unwrap(),
            journal
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
}
