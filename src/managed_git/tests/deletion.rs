use super::*;

fn deletion_fixture(
    name: &str,
) -> (
    PathBuf,
    PathBuf,
    agent::TursoAgentStore,
    GitFinalizationRecord,
) {
    let fixture = externally_completed_working_task_fixture(name, "session-deletion");
    let (_, project_root, store, journal) = &fixture;
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- Accidental task. BLOCKED 2026-10-06: Wrong repository. codex:session-deletion clt:stopped\n",
    )
    .unwrap();
    assert!(
        store
            .transition_session_control_state_blocking(
                journal.project_id,
                &journal.codex_session_id,
                AgentSessionControlState::Running,
                AgentSessionControlState::Stopped,
            )
            .unwrap()
    );
    fixture
}

#[test]
fn user_delete_cancels_idle_working_journal_and_preserves_other_work() {
    for folders in [false, true] {
        for status in [TaskStatus::Doing, TaskStatus::Todo] {
            let (root, project_root, store, journal) = deletion_fixture("user-delete-idle");
            let board_dir = get_tasks_dir(&project_root);
            let board = TaskBoard::new(&board_dir);
            if status == TaskStatus::Todo {
                board
                    .move_task_after_lock(TaskStatus::Doing, status, 1)
                    .unwrap();
            }
            board
                .insert_content(status, Some(0), "Keep earlier task")
                .unwrap();
            board
                .insert_content(status, None, "Keep later task")
                .unwrap();
            if folders {
                expand_status_for_command(&board_dir, status).unwrap();
            }
            let keep = board.entries(status).unwrap();
            let preserved = [keep[0].content.clone(), keep[2].content.clone()];
            fs::write(project_root.join("unrelated.txt"), "staged human work\n").unwrap();
            run_test_git(&project_root, &["add", "unrelated.txt"]);
            fs::write(project_root.join("untracked.txt"), "untracked human work\n").unwrap();
            let head = run_test_git(&project_root, &["rev-parse", "HEAD"]);
            let index = run_test_git(&project_root, &["write-tree"]);

            delete_task_in_board_with_store(&board_dir, status, "2", &store).unwrap();

            let remaining = board.entries(status).unwrap();
            assert_eq!(remaining.len(), 2);
            assert_eq!(remaining[0].content, preserved[0]);
            assert_eq!(remaining[1].content, preserved[1]);
            if folders {
                assert_eq!(remaining[0].source, keep[0].source);
                assert_eq!(remaining[1].source, keep[2].source);
            }
            let cancelled = store
                .git_finalization_blocking(journal.project_id, "session-deletion")
                .unwrap()
                .unwrap();
            assert_eq!(cancelled.state, GitFinalizationState::Cancelled);
            assert_eq!(cancelled.generation, journal.generation + 1);
            assert_eq!(
                cancelled.last_error.as_deref(),
                Some(AGENT_TASK_DELETION_REASON)
            );
            assert_eq!(cancelled.starting_head, journal.starting_head);
            assert_eq!(cancelled.task_identity, journal.task_identity);
            assert_eq!(cancelled.worktree_baseline, journal.worktree_baseline);
            assert!(cancelled.owner_run_token.is_none());
            assert_eq!(
                store
                    .session_control_blocking(journal.project_id, "session-deletion")
                    .unwrap()
                    .unwrap()
                    .state,
                AgentSessionControlState::Stopped
            );
            assert!(
                store
                    .lease_for_project_blocking(journal.project_id)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(run_test_git(&project_root, &["rev-parse", "HEAD"]), head);
            assert_eq!(run_test_git(&project_root, &["write-tree"]), index);
            assert_eq!(
                fs::read_to_string(project_root.join("untracked.txt")).unwrap(),
                "untracked human work\n"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn user_delete_stopped_task_preserves_another_running_task() {
    for folders in [false, true] {
        for worker in [false, true] {
            let (root, project_root, store, journal) = deletion_fixture("user-delete-other-task");
            let board_dir = get_tasks_dir(&project_root);
            let board = TaskBoard::new(&board_dir);
            board
                .move_task_after_lock(TaskStatus::Doing, TaskStatus::Todo, 1)
                .unwrap();
            board
                .insert_content(TaskStatus::Doing, None, "Other task codex:other-session")
                .unwrap();
            board
                .insert_content(TaskStatus::Todo, None, "Keep queued task")
                .unwrap();
            if folders {
                for status in [TaskStatus::Todo, TaskStatus::Doing] {
                    expand_status_for_command(&board_dir, status).unwrap();
                }
            }
            if worker {
                assert!(
                    store
                        .try_acquire_lease_blocking(
                            journal.project_id,
                            "scheduler",
                            "100",
                            "9999999999"
                        )
                        .unwrap()
                );
                assert!(reserve_test_worker(
                    &store,
                    journal.project_id,
                    "other-worker",
                    "scheduler",
                    "101",
                    1,
                ));
                assert!(
                    store
                        .claim_worker_blocking("other-worker", std::process::id(), "102")
                        .unwrap()
                );
            }
            store
                .mark_session_running_blocking(
                    journal.project_id,
                    "other-session",
                    std::process::id(),
                    "other-worker",
                    &root.join("other.out"),
                    &root.join("other.err"),
                )
                .unwrap();
            assert!(
                store
                    .create_git_finalization_blocking(agent::NewGitFinalization {
                        project_id: journal.project_id,
                        codex_session_id: "other-session",
                        git_mode: journal.git_mode,
                        starting_head: journal.starting_head.as_deref(),
                        branch_ref: journal.branch_ref.as_deref(),
                        upstream_ref: journal.upstream_ref.as_deref(),
                        worktree_baseline: &journal.worktree_baseline,
                        task_identity: Some("Other task"),
                        owner_run_token: Some("other-worker"),
                        created_at: "103",
                    })
                    .unwrap()
            );
            let other_task = board.entry(TaskStatus::Doing, 1).unwrap();
            let queued_task = board.entry(TaskStatus::Todo, 2).unwrap();
            let other_journal = store
                .git_finalization_blocking(journal.project_id, "other-session")
                .unwrap();
            let other_control = store
                .session_control_blocking(journal.project_id, "other-session")
                .unwrap();
            let workers = store.list_active_workers_blocking().unwrap();
            let lease = store
                .lease_for_project_blocking(journal.project_id)
                .unwrap();
            fs::write(project_root.join("human.txt"), "staged work\n").unwrap();
            run_test_git(&project_root, &["add", "human.txt"]);
            let head = run_test_git(&project_root, &["rev-parse", "HEAD"]);
            let index = run_test_git(&project_root, &["write-tree"]);

            delete_task_in_board_with_store(&board_dir, TaskStatus::Todo, "1", &store).unwrap();

            for (status, expected) in [
                (TaskStatus::Doing, other_task),
                (TaskStatus::Todo, queued_task),
            ] {
                let remaining = board.entries(status).unwrap();
                assert_eq!(remaining.len(), 1);
                assert_eq!(remaining[0].content, expected.content);
                if folders {
                    assert_eq!(remaining[0].source, expected.source);
                }
            }
            assert_eq!(store.list_active_workers_blocking().unwrap(), workers);
            assert_eq!(
                store
                    .git_finalization_blocking(journal.project_id, "other-session")
                    .unwrap(),
                other_journal
            );
            assert_eq!(
                store
                    .session_control_blocking(journal.project_id, "other-session")
                    .unwrap(),
                other_control
            );
            let after_lease = store
                .lease_for_project_blocking(journal.project_id)
                .unwrap();
            assert_eq!(
                after_lease.as_ref().map(|lease| (
                    &lease.holder,
                    &lease.acquired_at,
                    &lease.expires_at
                )),
                lease
                    .as_ref()
                    .map(|lease| (&lease.holder, &lease.acquired_at, &lease.expires_at)),
            );
            let cancelled = store
                .git_finalization_blocking(journal.project_id, "session-deletion")
                .unwrap()
                .unwrap();
            assert_eq!(cancelled.state, GitFinalizationState::Cancelled);
            assert_eq!(cancelled.generation, journal.generation + 1);
            assert_eq!(cancelled.starting_head, journal.starting_head);
            assert_eq!(cancelled.worktree_baseline, journal.worktree_baseline);
            let stopped = store
                .session_control_blocking(journal.project_id, "session-deletion")
                .unwrap()
                .unwrap();
            assert_eq!(stopped.state, AgentSessionControlState::Stopped);
            assert!(stopped.run_token.is_none());
            assert_eq!(run_test_git(&project_root, &["rev-parse", "HEAD"]), head);
            assert_eq!(run_test_git(&project_root, &["write-tree"]), index);
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn user_delete_reconciles_an_exited_worker_for_a_stopped_task() {
    let (root, project_root, store, journal) = deletion_fixture("user-delete-exited-worker");
    let token = journal.owner_run_token.as_deref().unwrap();
    assert!(
        store
            .try_acquire_lease_blocking(journal.project_id, "scheduler", "100", "9999999999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        journal.project_id,
        token,
        "scheduler",
        "101",
        1
    ));
    assert!(store.claim_worker_blocking(token, u32::MAX, "102").unwrap());

    delete_task_in_board_with_store(
        &get_tasks_dir(&project_root),
        TaskStatus::Doing,
        "1",
        &store,
    )
    .unwrap();

    assert!(read_tasks(&project_root, "doing").unwrap().is_empty());
    assert!(store.list_active_workers_blocking().unwrap().is_empty());
    assert!(
        store
            .lease_for_project_blocking(journal.project_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .git_finalization_blocking(journal.project_id, "session-deletion")
            .unwrap()
            .unwrap()
            .state,
        GitFinalizationState::Cancelled
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn user_delete_rechecks_the_selected_journal_generation_and_identity() {
    let (root, project_root, store, journal) = deletion_fixture("user-delete-changed-journal");
    for (generation, identity) in [
        (journal.generation + 1, journal.task_identity.as_deref()),
        (journal.generation, Some("another task")),
    ] {
        assert!(
            !store
                .cancel_idle_working_git_finalization_blocking(
                    journal.project_id,
                    &journal.codex_session_id,
                    generation,
                    identity,
                    "delete-test-fence",
                    "100",
                    "200",
                    GitTaskCancellation::Deletion,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .git_finalization_blocking(journal.project_id, "session-deletion")
                .unwrap()
                .unwrap(),
            journal
        );
        assert!(
            store
                .lease_for_project_blocking(journal.project_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(read_tasks(&project_root, "doing").unwrap().len(), 1);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn user_delete_refuses_active_owners_and_unconsumed_launch_boundaries() {
    for owner in ["session", "lease", "worker", "own-worker", "launch"] {
        let (root, project_root, store, journal) = deletion_fixture("user-delete-busy");
        match owner {
            "session" => {
                store
                    .set_session_control_state_blocking(
                        journal.project_id,
                        "session-deletion",
                        AgentSessionControlState::Running,
                    )
                    .unwrap();
            }
            "lease" | "worker" | "own-worker" => {
                assert!(
                    store
                        .try_acquire_lease_blocking(
                            journal.project_id,
                            "live-owner",
                            "100",
                            "9999999999"
                        )
                        .unwrap()
                );
                if owner != "lease" {
                    let token = if owner == "own-worker" {
                        journal.owner_run_token.as_deref().unwrap()
                    } else {
                        "live-worker"
                    };
                    assert!(reserve_test_worker(
                        &store,
                        journal.project_id,
                        token,
                        "live-owner",
                        "100",
                        1
                    ));
                    if owner == "own-worker" {
                        // A different control cannot disguise a worker that still
                        // owns the selected journal, even with a STOPPED marker.
                        store
                            .mark_session_running_blocking(
                                journal.project_id,
                                "other-session",
                                std::process::id(),
                                token,
                                &root.join("other.out"),
                                &root.join("other.err"),
                            )
                            .unwrap();
                    }
                }
            }
            "launch" => {
                let start =
                    capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
                assert!(
                    store
                        .record_git_launch_state_blocking(
                            journal.project_id,
                            "pending-launch",
                            AgentGitMode::Commit,
                            &start,
                            "100"
                        )
                        .unwrap()
                );
            }
            _ => unreachable!(),
        }
        let before = fs::read(project_root.join("tasks/doing.md")).unwrap();
        let error = delete_task_in_board_with_store(
            &get_tasks_dir(&project_root),
            TaskStatus::Doing,
            "1",
            &store,
        )
        .unwrap_err();
        let expected = match owner {
            "lease" => "active project lease",
            "worker" | "own-worker" => "active agent worker",
            "launch" => "unconsumed Git launch boundary",
            _ => "is still active",
        };
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        assert!(!format!("{error:#}").contains("Task session-deletion still has"));
        assert_eq!(
            fs::read(project_root.join("tasks/doing.md")).unwrap(),
            before
        );
        assert_eq!(
            store
                .git_finalization_blocking(journal.project_id, "session-deletion")
                .unwrap()
                .unwrap(),
            journal
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn user_delete_preserves_sealed_git_proof() {
    let (root, project_root, store, journal) = deletion_fixture("user-delete-sealed");
    for (generation, state) in [
        GitFinalizationState::Tracking,
        GitFinalizationState::CommitPending,
        GitFinalizationState::PushPending,
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    journal.project_id,
                    "session-deletion",
                    generation as i64,
                    state,
                    None,
                    (state != GitFinalizationState::Tracking)
                        .then_some("2222222222222222222222222222222222222222"),
                    None,
                    "100"
                )
                .unwrap()
        );
        let before = store
            .git_finalization_blocking(journal.project_id, "session-deletion")
            .unwrap()
            .unwrap();
        let error = delete_task_in_board_with_store(
            &get_tasks_dir(&project_root),
            TaskStatus::Doing,
            "1",
            &store,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("commit proof is already sealed"));
        assert_eq!(
            store
                .git_finalization_blocking(journal.project_id, "session-deletion")
                .unwrap()
                .unwrap(),
            before
        );
        assert_eq!(read_tasks(&project_root, "doing").unwrap().len(), 1);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interrupted_user_delete_stops_recovery_and_can_be_retried() {
    let (root, project_root, store, journal) = deletion_fixture("user-delete-interrupted");
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- Accidental task. codex:session-deletion\n",
    )
    .unwrap();
    assert!(
        store
            .transition_session_control_state_blocking(
                journal.project_id,
                "session-deletion",
                AgentSessionControlState::Stopped,
                AgentSessionControlState::ResumeRequested
            )
            .unwrap()
    );
    // Cancellation is durable before filesystem removal; simulate a crash there.
    assert!(
        store
            .cancel_idle_working_git_finalization_blocking(
                journal.project_id,
                "session-deletion",
                journal.generation,
                journal.task_identity.as_deref(),
                "delete-test-fence",
                "100",
                "101",
                GitTaskCancellation::Deletion
            )
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(!project_has_resumable_doing_task(&root.join("state/clt"), &project).unwrap());
    let pass =
        run_agent_scheduler_pass_with_max_global_jobs(&root.join("state/clt"), true, &[], 1, None)
            .unwrap();
    assert!(pass.jobs.is_empty());
    assert_eq!(
        store
            .session_control_blocking(journal.project_id, "session-deletion")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Stopped
    );
    delete_task_in_board_with_store(
        &get_tasks_dir(&project_root),
        TaskStatus::Doing,
        "1",
        &store,
    )
    .unwrap();
    assert!(read_tasks(&project_root, "doing").unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}
