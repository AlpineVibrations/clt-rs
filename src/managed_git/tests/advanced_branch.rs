use super::*;

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
