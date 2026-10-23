use super::*;

#[test]
fn active_git_run_keeps_its_mode_through_activation_commit_and_publication() {
    for (original, next) in [
        (AgentGitMode::Commit, AgentGitMode::Off),
        (AgentGitMode::Commit, AgentGitMode::CommitAndPush),
        (AgentGitMode::CommitAndPush, AgentGitMode::Off),
        (AgentGitMode::CommitAndPush, AgentGitMode::Commit),
    ] {
        let root = temp_root("active-git-mode-change");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Finish feature\n",
        )
        .unwrap();
        fs::write(project_root.join("feature.txt"), "before\n").unwrap();
        let initial_head = initialize_test_git_repository(&project_root);
        let remote = root.join("remote.git");
        fs::create_dir_all(&remote).unwrap();
        run_test_git(&remote, &["init", "--bare"]);
        run_test_git(
            &project_root,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
        let project_root = fs::canonicalize(project_root).unwrap();
        let state_dir = root.join("state");
        let store = TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .set_project_git_mode_blocking(project.id, original)
            .unwrap();
        let launch = capture_agent_git_start_state(&project_root, original).unwrap();
        store
            .record_git_launch_state_blocking(project.id, "run", original, &launch, "100")
            .unwrap();

        // Edit once before the child registers, then again after registration.
        store
            .set_project_git_mode_blocking(project.id, next)
            .unwrap();
        store
            .mark_session_running_with_git_mode_blocking(
                project.id,
                "session",
                123,
                "run",
                &root.join("out"),
                &root.join("err"),
                original,
            )
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, next)
            .unwrap();
        drop(store);
        let store = TursoAgentStore::open_blocking(&state_dir).unwrap();
        let mut active_project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(active_project.git_mode, next);
        assert_eq!(
            agent_git_mode_for_launch(&store, &active_project, None).unwrap(),
            next
        );
        assert_eq!(
            agent_git_mode_for_launch(&store, &active_project, Some("session")).unwrap(),
            original
        );
        active_project.git_mode =
            effective_agent_git_mode(&store, &active_project, Some("session")).unwrap();
        assert_eq!(active_project.git_mode, original);
        let context = AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run".into(),
        };
        move_task_to_doing_with_agent_git_journal(
            &project_root,
            "1",
            &context,
            &active_project,
            &store,
        )
        .unwrap();
        fs::write(project_root.join("feature.txt"), "after\n").unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Finish feature — COMPLETED 2026-10-23: verified codex:session\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "feature.txt", "tasks"]);
        assert!(
            move_task_to_done_with_agent_store(
                &project_root,
                TaskStatus::Doing,
                "1",
                &context,
                &store
            )
            .unwrap()
        );
        run_test_git(&project_root, &["add", "tasks"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Finish feature",
                "-m",
                "CLT-Task: codex:session",
            ],
        );
        let commit = run_test_git(&project_root, &["rev-parse", "HEAD"]);
        let pending = store
            .git_finalization_blocking(project.id, "session")
            .unwrap()
            .unwrap();
        assert_eq!(pending.git_mode, original);
        // Settings can still change while commit verification/publication waits.
        store
            .set_project_git_mode_blocking(project.id, next)
            .unwrap();
        let completed =
            reconcile_agent_git_finalization(&store, &project_root, pending, Some("run"), None)
                .unwrap();
        assert_eq!(completed.state, GitFinalizationState::Completed);
        assert_eq!(completed.git_mode, original);
        assert_eq!(completed.commit_oid.as_deref(), Some(commit.as_str()));
        let branch = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);
        assert_eq!(
            run_test_git(&remote, &["rev-parse", &branch]),
            if original == AgentGitMode::CommitAndPush {
                commit
            } else {
                initial_head
            }
        );
        assert_eq!(store.list_projects_blocking().unwrap()[0].git_mode, next);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
