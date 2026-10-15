use super::*;

#[test]
fn shared_completed_conversation_does_not_block_commit_or_publication() {
    for folders in [false, true] {
        for mode in [AgentGitMode::Commit, AgentGitMode::CommitAndPush] {
            for prior_journal in [false, true] {
                let root = temp_root("git-finalization-shared-completed");
                let state_dir = root.join("state/clt");
                let project_root = root.join("project");
                init_tasks(&project_root, folders).unwrap();
                add_task(
                    &project_root,
                    "Older task — COMPLETED 2026-10-14: verified codex:older-session",
                    None,
                )
                .unwrap();
                move_task(&project_root, TaskStatus::Todo, TaskStatus::Done, "1").unwrap();
                add_task(&project_root, "Active task codex:active-session", None).unwrap();
                move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
                add_task(&project_root, "Later queued task", None).unwrap();
                initialize_test_git_repository(&project_root);
                let branch = run_test_git(&project_root, &["branch", "--show-current"]);
                if mode == AgentGitMode::CommitAndPush {
                    let remote = root.join("remote.git");
                    run_test_git(&root, &["init", "--bare", remote.to_str().unwrap()]);
                    run_test_git(
                        &project_root,
                        &["remote", "add", "origin", remote.to_str().unwrap()],
                    );
                    run_test_git(
                        &project_root,
                        &["push", "--set-upstream", "origin", &branch],
                    );
                }
                let project_root = fs::canonicalize(project_root).unwrap();
                let board = TaskBoard::new(get_tasks_dir(&project_root));
                let older_task = board.entry(TaskStatus::Done, 1).unwrap();
                let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
                store
                    .register_project_blocking(&project_root, "project")
                    .unwrap();
                store
                    .set_project_git_mode_for_path_blocking(&project_root, mode)
                    .unwrap();
                let project = store.list_projects_blocking().unwrap().remove(0);
                store
                    .mark_session_running_blocking(
                        project.id,
                        "active-session",
                        std::process::id(),
                        "active-run",
                        &root.join("run.out"),
                        &root.join("run.err"),
                    )
                    .unwrap();
                assert!(
                    store
                        .try_acquire_lease_blocking(
                            project.id,
                            "active-owner",
                            &agent_timestamp(),
                            &agent_timestamp_after(60),
                        )
                        .unwrap()
                );
                let start = capture_agent_git_start_state(&project_root, mode).unwrap();
                ensure_agent_git_working_record(
                    &store,
                    &project,
                    "active-session",
                    "active-run",
                    Some(&start),
                )
                .unwrap();
                assert!(
                    bind_agent_git_working_task_identity(
                        &store,
                        &project,
                        "active-session",
                        "active-run",
                    )
                    .unwrap()
                );

                if prior_journal {
                    assert!(
                        store
                            .create_git_finalization_blocking(agent::NewGitFinalization {
                                project_id: project.id,
                                codex_session_id: "older-session",
                                git_mode: AgentGitMode::Commit,
                                starting_head: Some(&start.starting_head),
                                branch_ref: start.branch_ref.as_deref(),
                                upstream_ref: None,
                                worktree_baseline: &start.worktree_baseline,
                                task_identity: Some("Older task"),
                                owner_run_token: None,
                                created_at: "100",
                            })
                            .unwrap()
                    );
                    for (generation, state) in [
                        GitFinalizationState::Tracking,
                        GitFinalizationState::CommitPending,
                        GitFinalizationState::Completed,
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        assert!(
                            store
                                .compare_and_set_git_finalization_blocking(
                                    project.id,
                                    "older-session",
                                    generation as i64,
                                    state,
                                    None,
                                    Some(&start.starting_head),
                                    None,
                                    "101",
                                )
                                .unwrap()
                        );
                    }
                    assert!(
                        store
                            .acknowledge_completed_git_finalization_session_blocking(
                                project.id,
                                "older-session",
                            )
                            .unwrap()
                    );
                    store
                        .set_session_control_state_blocking(
                            project.id,
                            "older-session",
                            AgentSessionControlState::Stopped,
                        )
                        .unwrap();
                }
                let older_journal = store
                    .git_finalization_blocking(project.id, "older-session")
                    .unwrap();
                let requester = InteractiveAgentLease::holder_for_shared_session(prior_journal);
                assert!(
                    store
                        .reserve_shared_session_interactive_blocking(
                            project.id,
                            "older-session",
                            &requester,
                            None,
                            false,
                        )
                        .unwrap()
                );
                let disposition = InteractiveGuardianDisposition::from_handoff(
                    InteractiveCodexResumeMode::WritableShared,
                    &requester,
                );
                let guardian = interactive_guardian_holder(disposition);
                assert!(
                    store
                        .adopt_interactive_guardian_blocking(
                            project.id,
                            Some("older-session"),
                            &requester,
                            &guardian,
                            60,
                        )
                        .unwrap()
                );
                assert!(
                    store
                        .register_interactive_guardian_child_blocking(
                            project.id,
                            "older-session",
                            &guardian,
                            std::process::id(),
                            60,
                        )
                        .unwrap()
                );
                let older_control = store
                    .session_control_blocking(project.id, "older-session")
                    .unwrap();

                let active = board.entry(TaskStatus::Doing, 1).unwrap();
                board
                    .write_entry_content(
                        TaskStatus::Doing,
                        &active,
                        "Active task — COMPLETED 2026-10-15: verified codex:active-session\n",
                    )
                    .unwrap();
                fs::write(
                    project_root.join("feature.txt"),
                    "verified implementation\n",
                )
                .unwrap();
                run_test_git(&project_root, &["add", "--all"]);
                assert!(
                    move_task_to_done_with_agent_store(
                        &project_root,
                        TaskStatus::Doing,
                        "1",
                        &AutomatedAgentChildContext {
                            project_id: project.id,
                            run_token: "active-run".into()
                        },
                        &store,
                    )
                    .unwrap()
                );
                run_test_git(&project_root, &["add", "--all", "--", "tasks"]);
                run_test_agent_git(
                    &project_root,
                    &[
                        "commit",
                        "-m",
                        "Finish active task",
                        "-m",
                        "CLT-Task: codex:active-session",
                    ],
                );
                let task_commit = run_test_git(&project_root, &["rev-parse", "HEAD"]);
                // Reproduce acknowledgement/publication after the automated worker
                // exits, with the older interactive child still open.
                assert!(
                    store
                        .clear_running_session_control_blocking(
                            project.id,
                            "active-session",
                            Some("active-run"),
                        )
                        .unwrap()
                );
                assert!(
                    store
                        .release_lease_blocking(project.id, "active-owner")
                        .unwrap()
                );
                let pass =
                    run_agent_scheduler_pass_with_max_global_jobs(&state_dir, false, &[], 1, None)
                        .unwrap();
                assert!(pass.jobs.is_empty());
                let completed = store
                    .git_finalization_blocking(project.id, "active-session")
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    completed.state,
                    GitFinalizationState::Completed,
                    "folders={folders}, mode={mode:?}, prior_journal={prior_journal}: {:?}",
                    completed.last_error
                );
                assert_eq!(completed.commit_oid.as_deref(), Some(task_commit.as_str()));
                assert!(completed.acknowledged_at.is_some());
                assert!(board.entries(TaskStatus::Doing).unwrap().is_empty());
                let done = board.entries(TaskStatus::Done).unwrap();
                assert_eq!(done.len(), 2);
                assert_eq!(done[1].content, older_task.content);
                if folders {
                    assert_eq!(done[1].source, older_task.source);
                }
                assert_eq!(
                    store
                        .session_control_blocking(project.id, "older-session")
                        .unwrap(),
                    older_control
                );
                assert_eq!(
                    store
                        .git_finalization_blocking(project.id, "older-session")
                        .unwrap(),
                    older_journal
                );
                assert_eq!(
                    run_test_git(
                        &project_root,
                        &[
                            "rev-list",
                            "--count",
                            &format!("{}..HEAD", start.starting_head)
                        ]
                    ),
                    "1"
                );
                if mode == AgentGitMode::CommitAndPush {
                    assert_eq!(
                        run_test_git(&root.join("remote.git"), &["rev-parse", &branch]),
                        task_commit
                    );
                }
                assert!(
                    !finish_interactive_guardian_after_reap(
                        &store,
                        project.id,
                        "older-session",
                        &guardian,
                        Duration::from_secs(60),
                        disposition,
                    )
                    .unwrap()
                );
                drop(store);
                fs::remove_dir_all(root).unwrap();
            }
        }
    }
}
