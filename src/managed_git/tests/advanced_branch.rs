use super::*;

#[test]
fn concurrent_commits_cannot_bypass_task_proof() {
    for scenario in [
        "current task trailer",
        "other task trailer",
        "agent author",
        "agent committer email",
        "checkpoint with implementation",
        "checkpoint with completion",
        "user commit with completion",
    ] {
        let root = temp_root("git-concurrent-task-proof");
        init_tasks(&root, false).unwrap();
        let starting_head = initialize_test_git_repository(&root);
        let mut command = Command::new("git");
        command.current_dir(&root).args(["commit", "-m"]);
        let checkpoint = scenario.starts_with("checkpoint");
        command.arg(if checkpoint {
            "Record CLT task board"
        } else {
            "Concurrent work"
        });
        if checkpoint {
            configure_agent_git_identity(&mut command, AgentGitMode::Commit);
        }
        match scenario {
            "current task trailer" => {
                command.args(["-m", "CLT-Task: codex:session-current"]);
            }
            "other task trailer" => {
                command.args(["-m", "CLT-Task: codex:session-other"]);
            }
            "agent author" => {
                command.arg("--author=CLT Agent <clt-agent@localhost>");
            }
            "agent committer email" => {
                command.env("GIT_COMMITTER_EMAIL", AGENT_GIT_IDENTITY_EMAIL);
            }
            _ => {}
        }
        if scenario.ends_with("with completion") {
            fs::write(
                root.join("tasks/done.md"),
                "# Done Tasks\n- Finished — COMPLETED 2026-09-10: checked codex:session-current\n",
            )
            .unwrap();
        } else {
            fs::write(root.join("implementation.txt"), "premature\n").unwrap();
        }
        run_test_git(&root, &["add", "--all"]);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let parent = run_test_git(&root, &["rev-parse", "HEAD"]);
        let store = agent::TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();
        assert!(
            !agent_git_range_is_safe_before_manifest(
                AgentGitProofContext {
                    store: &store,
                    project_id: 1
                },
                &root,
                &starting_head,
                &parent,
                "session-current",
            )
            .unwrap(),
            "{scenario}"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn task_finalization_accepts_concurrent_user_commits_and_board_checkpoints() {
    for git_mode in [AgentGitMode::Commit, AgentGitMode::CommitAndPush] {
        for after_seal in [false, true] {
            let root = temp_root("git-finalization-concurrent-user");
            let project_root = root.join("project");
            let remote_root = root.join("remote.git");
            init_tasks(&project_root, false).unwrap();
            fs::write(
                project_root.join("tasks/doing.md"),
                "# Doing Tasks\n- Ship feature — COMPLETED 2026-09-10: checked codex:session-concurrent\n",
            )
            .unwrap();
            fs::write(project_root.join("version.txt"), "0.4.0\n").unwrap();
            fs::write(project_root.join("notes.txt"), "original\n").unwrap();
            initialize_test_git_repository(&project_root);
            if git_mode == AgentGitMode::CommitAndPush {
                fs::create_dir_all(&remote_root).unwrap();
                run_test_git(&remote_root, &["init", "--bare"]);
                run_test_git(
                    &project_root,
                    &["remote", "add", "origin", remote_root.to_str().unwrap()],
                );
                run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
            }
            let project_root = fs::canonicalize(project_root).unwrap();
            let store = agent::TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();
            store
                .register_project_blocking(&project_root, "project")
                .unwrap();
            store
                .set_project_git_mode_for_path_blocking(&project_root, git_mode)
                .unwrap();
            let project = store.list_projects_blocking().unwrap().remove(0);
            store
                .mark_session_running_blocking(
                    project.id,
                    "session-concurrent",
                    123,
                    "run-concurrent",
                    &root.join("run.out"),
                    &root.join("run.err"),
                )
                .unwrap();
            fs::write(project_root.join("notes.txt"), "user scratch work\n").unwrap();
            let start = capture_agent_git_start_state(&project_root, git_mode).unwrap();
            ensure_agent_git_working_record(
                &store,
                &project,
                "session-concurrent",
                "run-concurrent",
                Some(&start),
            )
            .unwrap();
            assert!(
                bind_agent_git_working_task_identity(
                    &store,
                    &project,
                    "session-concurrent",
                    "run-concurrent",
                )
                .unwrap()
            );
            fs::write(project_root.join("feature.txt"), "implemented\n").unwrap();
            let context = AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-concurrent".to_string(),
            };
            if after_seal {
                run_test_git(&project_root, &["add", "feature.txt", "tasks/doing.md"]);
                move_task_to_done_with_agent_store(
                    &project_root,
                    TaskStatus::Doing,
                    "1",
                    &context,
                    &store,
                )
                .unwrap();
            }

            // A person's path-only commit leaves the agent's payload and the
            // pre-existing unstaged work alone, even if sealing already happened.
            fs::write(project_root.join("version.txt"), "0.4.1\n").unwrap();
            run_test_git(&project_root, &["add", "version.txt"]);
            run_test_git(
                &project_root,
                &[
                    "commit",
                    "--only",
                    "-m",
                    "Bump patch version",
                    "--",
                    "version.txt",
                ],
            );
            let user_commit = run_test_git(&project_root, &["rev-parse", "HEAD"]);
            if !after_seal {
                // A later scheduler launch can checkpoint the board while this
                // older Working journal is waiting to resume.
                fs::write(
                    project_root.join("tasks/todo.md"),
                    "# Todo Tasks\n- Later task\n",
                )
                .unwrap();
                assert!(
                    checkpoint_agent_git_task_board_before_launch(&project_root)
                        .unwrap()
                        .is_some()
                );
                let working = store
                    .git_finalization_blocking(project.id, "session-concurrent")
                    .unwrap()
                    .unwrap();
                assert!(repair_working_git_task_link(&store, &project_root, &working).unwrap());
            }
            let parent = run_test_git(&project_root, &["rev-parse", "HEAD"]);
            run_test_git(&project_root, &["add", "feature.txt", "tasks"]);
            if after_seal {
                let pending = store
                    .git_finalization_blocking(project.id, "session-concurrent")
                    .unwrap()
                    .unwrap();
                let identity = pending.task_identity.as_deref().unwrap();
                let manifest = capture_agent_git_resealed_manifest(
                    AgentGitProofContext {
                        store: &store,
                        project_id: project.id,
                    },
                    &project_root,
                    &pending.worktree_baseline,
                    "session-concurrent",
                    identity,
                    &start.starting_head,
                    start.branch_ref.as_deref(),
                )
                .unwrap();
                assert!(
                    store
                        .reseal_git_finalization_manifest_blocking(
                            project.id,
                            "session-concurrent",
                            pending.generation,
                            identity,
                            &manifest,
                            "run-concurrent",
                            "200",
                        )
                        .unwrap()
                );
            } else {
                move_task_to_done_with_agent_store(
                    &project_root,
                    TaskStatus::Doing,
                    "1",
                    &context,
                    &store,
                )
                .unwrap();
            }
            let pending = store
                .git_finalization_blocking(project.id, "session-concurrent")
                .unwrap()
                .unwrap();
            assert_eq!(
                pending.starting_head.as_deref(),
                Some(start.starting_head.as_str())
            );
            let baseline = AgentGitWorktreeBaseline::from_json(&pending.worktree_baseline).unwrap();
            assert_eq!(
                baseline.manifest_parent_head.as_deref(),
                Some(parent.as_str())
            );
            run_test_git(&project_root, &["add", "tasks"]);
            run_test_agent_git(
                &project_root,
                &[
                    "commit",
                    "-m",
                    "Ship feature",
                    "-m",
                    "CLT-Task: codex:session-concurrent",
                ],
            );
            let task_commit = run_test_git(&project_root, &["rev-parse", "HEAD"]);
            assert_eq!(run_test_git(&project_root, &["rev-parse", "HEAD^"]), parent);
            assert_eq!(
                run_test_git(&project_root, &["show", "HEAD:version.txt"]),
                "0.4.1"
            );
            assert_eq!(
                run_test_git(
                    &project_root,
                    &["diff", "HEAD^", "HEAD", "--", "version.txt"]
                ),
                ""
            );
            assert!(git_commit_is_ancestor(&project_root, &user_commit, &task_commit).unwrap());
            let completed = reconcile_agent_git_finalization(
                &store,
                &project_root,
                pending,
                Some("run-concurrent"),
                None,
            )
            .unwrap();
            assert_eq!(completed.state, GitFinalizationState::Completed);
            assert_eq!(completed.commit_oid.as_deref(), Some(task_commit.as_str()));
            assert_eq!(
                fs::read_to_string(project_root.join("notes.txt")).unwrap(),
                "user scratch work\n"
            );
            assert_eq!(
                run_test_git(&project_root, &["status", "--porcelain"]),
                "M notes.txt"
            );
            if git_mode == AgentGitMode::CommitAndPush {
                assert_eq!(
                    run_test_git(
                        &remote_root,
                        &["rev-parse", start.branch_ref.as_deref().unwrap()]
                    ),
                    task_commit
                );
            }
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn sealed_task_finalization_accepts_later_commits() {
    for (git_mode, already_published) in [
        (AgentGitMode::Commit, false),
        (AgentGitMode::CommitAndPush, false),
        (AgentGitMode::CommitAndPush, true),
    ] {
        let root = temp_root("git-finalization-advanced-branch");
        let project_root = root.join("project");
        let remote_root = root.join("remote.git");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Ship feature — COMPLETED 2026-09-04: checked codex:session-advanced\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);
        if git_mode == AgentGitMode::CommitAndPush {
            fs::create_dir_all(&remote_root).unwrap();
            run_test_git(&remote_root, &["init", "--bare"]);
            run_test_git(
                &project_root,
                &["remote", "add", "origin", remote_root.to_str().unwrap()],
            );
            run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
        }
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, git_mode)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-advanced",
                123,
                "run-advanced",
                &root.join("run.out"),
                &root.join("run.err"),
            )
            .unwrap();
        let git_start = capture_agent_git_start_state(&project_root, git_mode).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-advanced",
            "run-advanced",
            Some(&git_start),
        )
        .unwrap();
        assert!(
            bind_agent_git_working_task_identity(
                &store,
                &project,
                "session-advanced",
                "run-advanced",
            )
            .unwrap()
        );
        fs::write(project_root.join("feature.txt"), "implemented\n").unwrap();
        run_test_git(&project_root, &["add", "feature.txt"]);
        move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-advanced".to_string(),
            },
            &store,
        )
        .unwrap();
        run_test_git(&project_root, &["add", "--all"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Ship feature",
                "-m",
                "CLT-Task: codex:session-advanced",
            ],
        );
        let task_commit = run_test_git(&project_root, &["rev-parse", "HEAD"]);
        let pending = store
            .git_finalization_blocking(project.id, "session-advanced")
            .unwrap()
            .unwrap();
        assert_eq!(pending.state, GitFinalizationState::CommitPending);

        // A user keeps working before CLT acknowledges the sealed task commit.
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Later task\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "tasks/todo.md"]);
        run_test_git(&project_root, &["commit", "-m", "Add a later task"]);
        fs::write(project_root.join("other.txt"), "another change\n").unwrap();
        run_test_git(&project_root, &["add", "other.txt"]);
        run_test_git(&project_root, &["commit", "-m", "Continue unrelated work"]);
        let later_tip = run_test_git(&project_root, &["rev-parse", "HEAD"]);
        if already_published {
            run_test_git(&project_root, &["push", "origin", "HEAD"]);
        }
        fs::write(project_root.join("other.txt"), "staged follow-up\n").unwrap();
        run_test_git(&project_root, &["add", "other.txt"]);
        fs::write(project_root.join("other.txt"), "unstaged follow-up\n").unwrap();
        let index_before = run_test_git(&project_root, &["write-tree"]);
        let diff_before = run_test_git(&project_root, &["diff", "HEAD"]);

        let completed = reconcile_agent_git_finalization(
            &store,
            &project_root,
            pending,
            Some("run-advanced"),
            None,
        )
        .unwrap();
        assert_eq!(completed.state, GitFinalizationState::Completed);
        assert_eq!(completed.commit_oid.as_deref(), Some(task_commit.as_str()));
        let completed_again = reconcile_agent_git_finalization(
            &store,
            &project_root,
            completed,
            Some("run-advanced"),
            None,
        )
        .unwrap();
        assert_eq!(completed_again.state, GitFinalizationState::Completed);
        assert_eq!(
            run_test_git(&project_root, &["rev-parse", "HEAD"]),
            later_tip
        );
        assert_eq!(run_test_git(&project_root, &["write-tree"]), index_before);
        assert_eq!(run_test_git(&project_root, &["diff", "HEAD"]), diff_before);
        if git_mode == AgentGitMode::CommitAndPush {
            assert_eq!(
                run_test_git(&remote_root, &["rev-parse", &branch_ref]),
                if already_published {
                    later_tip
                } else {
                    task_commit
                }
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn task_commit_proof_rejects_a_later_commit_claiming_the_same_session() {
    let root = temp_root("git-finalization-later-duplicate-trailer");
    init_tasks(&root, false).unwrap();
    let starting_head = initialize_test_git_repository(&root);
    fs::write(
        root.join("tasks/done.md"),
        "# Done Tasks\n- Finished — COMPLETED 2026-09-04: checked codex:session-later-duplicate\n",
    )
    .unwrap();
    run_test_git(&root, &["add", "tasks/done.md"]);
    run_test_agent_git(
        &root,
        &[
            "commit",
            "-m",
            "Finish task",
            "-m",
            "CLT-Task: codex:session-later-duplicate",
        ],
    );
    fs::write(root.join("other.txt"), "later work\n").unwrap();
    run_test_git(&root, &["add", "other.txt"]);
    // Even a non-agent commit must not make a second claim to this task.
    run_test_git(
        &root,
        &[
            "commit",
            "-m",
            "Another task claim",
            "-m",
            "CLT-Task: codex:session-later-duplicate",
        ],
    );
    let branch_ref = run_test_git(&root, &["symbolic-ref", "HEAD"]);
    assert_eq!(
        find_agent_git_task_commit(
            &root,
            &starting_head,
            Some(&branch_ref),
            "session-later-duplicate",
            &durable_task_identity("Finished").unwrap(),
        )
        .unwrap(),
        None
    );
    fs::remove_dir_all(root).unwrap();
}
