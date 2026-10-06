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
fn user_delete_refuses_active_owners_and_unconsumed_launch_boundaries() {
    for owner in ["session", "other-session", "lease", "worker", "launch"] {
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
            "other-session" => {
                store
                    .mark_session_running_blocking(
                        journal.project_id,
                        "other-session",
                        123,
                        "other-run",
                        &root.join("other.out"),
                        &root.join("other.err"),
                    )
                    .unwrap();
            }
            "lease" | "worker" => {
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
                if owner == "worker" {
                    assert!(reserve_test_worker(
                        &store,
                        journal.project_id,
                        "live-worker",
                        "live-owner",
                        "100",
                        1
                    ));
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
            "worker" => "active agent worker",
            "launch" => "unconsumed Git launch boundary",
            _ => "is still active",
        };
        assert!(format!("{error:#}").contains(expected), "{error:#}");
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
                journal.task_identity.as_deref().unwrap(),
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
