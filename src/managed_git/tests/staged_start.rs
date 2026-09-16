use super::*;

#[test]
fn task_commit_preserves_unrelated_staging_with_a_private_index() {
    const CHILD_ROOT: &str = "CLT_TEST_STAGED_TASK_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        let project_root = fs::canonicalize(root.join("project")).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&root.join("state")).unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        run_test_git(&project_root, &["read-tree", "HEAD"]);
        fs::write(project_root.join("feature.txt"), "implemented\n").unwrap();
        run_test_git(&project_root, &["add", "feature.txt"]);
        move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "private-run".into(),
            },
            &store,
        )
        .unwrap();
        run_test_git(&project_root, &["add", "tasks"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Finish feature",
                "-m",
                "CLT-Task: codex:private-session",
            ],
        );
        return;
    }

    let root = temp_root("staged-start-private-index");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- Finish feature — COMPLETED 2026-09-16: checked codex:private-session\n",
    )
    .unwrap();
    fs::write(project_root.join("unrelated.txt"), "original\n").unwrap();
    initialize_test_git_repository(&project_root);
    fs::write(project_root.join("unrelated.txt"), "staged user work\n").unwrap();
    run_test_git(&project_root, &["add", "unrelated.txt"]);
    fs::write(
        project_root.join("unrelated.txt"),
        "staged user work\nunstaged user work\n",
    )
    .unwrap();
    let staged_entry = run_test_git(
        &project_root,
        &["ls-files", "--stage", "--", "unrelated.txt"],
    );
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&root.join("state")).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    store
        .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .mark_session_running_blocking(
            project.id,
            "private-session",
            123,
            "private-run",
            &root.join("out"),
            &root.join("err"),
        )
        .unwrap();
    let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    ensure_agent_git_working_record(
        &store,
        &project,
        "private-session",
        "private-run",
        Some(&start),
    )
    .unwrap();
    bind_agent_git_working_task_identity(&store, &project, "private-session", "private-run")
        .unwrap();
    drop(store);

    // All sealing and commit commands inherit the same private index, without
    // changing process-global environment in the parallel test harness.
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "managed_git::tests::staged_start::task_commit_preserves_unrelated_staging_with_a_private_index", "--nocapture"])
        .env(CHILD_ROOT, &root)
        .env("GIT_INDEX_FILE", root.join("task.index"))
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Reconcile only the task's index entries after its private-index commit.
    run_test_git(
        &project_root,
        &["reset", "--quiet", "HEAD", "--", "feature.txt", "tasks"],
    );
    assert_eq!(
        run_test_git(
            &project_root,
            &["ls-files", "--stage", "--", "unrelated.txt"]
        ),
        staged_entry
    );
    assert_eq!(
        run_test_git(&project_root, &["diff", "--cached", "--name-only"]),
        "unrelated.txt"
    );
    assert_eq!(
        run_test_git(&project_root, &["show", "HEAD:unrelated.txt"]),
        "original"
    );
    assert_eq!(
        fs::read_to_string(project_root.join("unrelated.txt")).unwrap(),
        "staged user work\nunstaged user work\n"
    );
    let store = agent::TursoAgentStore::open_blocking(&root.join("state")).unwrap();
    let pending = store
        .git_finalization_blocking(project.id, "private-session")
        .unwrap()
        .unwrap();
    let completed =
        reconcile_agent_git_finalization(&store, &project_root, pending, Some("private-run"), None)
            .unwrap();
    assert_eq!(completed.state, GitFinalizationState::Completed);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn staged_start_checkpoints_the_board_without_losing_existing_index_entries() {
    let root = temp_root("staged-start-checkpoint");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    for path in ["source.txt", "deleted.txt", "renamed.txt"] {
        fs::write(project_root.join(path), "original\n").unwrap();
    }
    initialize_test_git_repository(&project_root);
    fs::write(project_root.join("source.txt"), "staged\n").unwrap();
    fs::remove_file(project_root.join("deleted.txt")).unwrap();
    fs::rename(
        project_root.join("renamed.txt"),
        project_root.join("new name.txt"),
    )
    .unwrap();
    fs::write(project_root.join("binary.dat"), [0, 255, 1, 2]).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Staged definition\n",
    )
    .unwrap();
    run_test_git(&project_root, &["add", "-A"]);
    // Neither the partially staged source nor task definition may be discarded.
    fs::write(project_root.join("source.txt"), "staged\nunstaged\n").unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Worktree definition\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/backlog.md"),
        "# Backlog Tasks\n- Later\n",
    )
    .unwrap();
    let original_entries = run_test_git(
        &project_root,
        &[
            "ls-files",
            "--stage",
            "--",
            "source.txt",
            "deleted.txt",
            "renamed.txt",
            "new name.txt",
            "binary.dat",
            "tasks/todo.md",
        ],
    );
    // A dirty checkout must not contact even its configured upstream.
    run_test_git(
        &project_root,
        &[
            "remote",
            "add",
            "origin",
            "/nonexistent/clt-test-remote.git",
        ],
    );
    let branch = run_test_git(&project_root, &["branch", "--show-current"]);
    run_test_git(
        &project_root,
        &["config", &format!("branch.{branch}.remote"), "origin"],
    );
    run_test_git(
        &project_root,
        &[
            "config",
            &format!("branch.{branch}.merge"),
            "refs/heads/main",
        ],
    );
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&root.join("state")).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    store
        .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);

    ensure_agent_git_index_preflight(&project, false).unwrap();
    let start = prepare_agent_git_start_state_for_run(
        &store,
        &project,
        AgentTaskSelection::NextTodo,
        false,
        false,
        "staged-run",
    )
    .unwrap()
    .unwrap();
    verify_agent_git_start_state_unchanged(&project_root, AgentGitMode::Commit, &start).unwrap();

    assert_eq!(
        original_entries,
        run_test_git(
            &project_root,
            &[
                "ls-files",
                "--stage",
                "--",
                "source.txt",
                "deleted.txt",
                "renamed.txt",
                "new name.txt",
                "binary.dat",
                "tasks/todo.md",
            ]
        )
    );
    assert_eq!(
        run_test_git(&project_root, &["show", "HEAD:source.txt"]),
        "original"
    );
    assert_eq!(
        run_test_git(&project_root, &["show", "HEAD:deleted.txt"]),
        "original"
    );
    assert_eq!(
        run_test_git(&project_root, &["show", "HEAD:renamed.txt"]),
        "original"
    );
    assert_eq!(
        run_test_git(&project_root, &["diff", "HEAD^", "HEAD", "--name-only"]),
        "tasks/backlog.md\ntasks/todo.md"
    );
    assert_eq!(
        run_test_git(&project_root, &["show", "HEAD:tasks/todo.md"]),
        "# Todo Tasks\n- Worktree definition"
    );
    assert_eq!(
        fs::read_to_string(project_root.join("source.txt")).unwrap(),
        "staged\nunstaged\n"
    );
    assert_eq!(
        fs::read(project_root.join("binary.dat")).unwrap(),
        [0, 255, 1, 2]
    );
    assert!(
        run_test_git(
            &project_root,
            &["status", "--short", "--", "tasks/backlog.md"]
        )
        .is_empty()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn frozen_staged_index_is_checked_even_when_the_worktree_does_not_change() {
    let root = temp_root("staged-start-snapshot");
    init_tasks(&root, false).unwrap();
    fs::write(root.join("source.txt"), "original\n").unwrap();
    initialize_test_git_repository(&root);
    fs::write(root.join("source.txt"), "staged\n").unwrap();
    run_test_git(&root, &["add", "source.txt"]);
    let start = capture_agent_git_start_state(&root, AgentGitMode::Commit).unwrap();
    let baseline = AgentGitWorktreeBaseline::from_json(&start.worktree_baseline).unwrap();
    assert_eq!(
        baseline.initial_index_tree,
        Some(run_test_git(&root, &["write-tree"]))
    );
    verify_agent_git_start_state_unchanged(&root, AgentGitMode::Commit, &start).unwrap();

    run_test_git(&root, &["reset", "--quiet", "HEAD", "--", "source.txt"]);
    assert!(verify_agent_git_start_state_unchanged(&root, AgentGitMode::Commit, &start).is_err());
    assert_eq!(
        fs::read_to_string(root.join("source.txt")).unwrap(),
        "staged\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_launch_snapshots_still_require_the_original_clean_index() {
    let root = temp_root("staged-start-legacy");
    init_tasks(&root, false).unwrap();
    initialize_test_git_repository(&root);
    let mut start = capture_agent_git_start_state(&root, AgentGitMode::Commit).unwrap();
    let mut baseline: serde_json::Value = serde_json::from_str(&start.worktree_baseline).unwrap();
    baseline
        .as_object_mut()
        .unwrap()
        .remove("initial_index_tree");
    start.worktree_baseline = baseline.to_string();
    verify_agent_git_start_state_unchanged(&root, AgentGitMode::Commit, &start).unwrap();
    fs::write(root.join("new.txt"), "staged\n").unwrap();
    run_test_git(&root, &["add", "new.txt"]);
    assert!(verify_agent_git_start_state_unchanged(&root, AgentGitMode::Commit, &start).is_err());
    baseline["initial_index_tree"] = serde_json::json!(false);
    assert!(AgentGitWorktreeBaseline::from_json(&baseline.to_string()).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_sync_preserves_staged_unstaged_and_untracked_work_without_contacting_remote() {
    for dirty_kind in ["staged", "unstaged", "untracked"] {
        let root = temp_root(&format!("dirty-startup-{dirty_kind}"));
        init_tasks(&root, false).unwrap();
        fs::write(root.join("source.txt"), "original\n").unwrap();
        let head = initialize_test_git_repository(&root);
        run_test_git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "/nonexistent/clt-test-remote.git",
            ],
        );
        let branch = run_test_git(&root, &["branch", "--show-current"]);
        run_test_git(
            &root,
            &["config", &format!("branch.{branch}.remote"), "origin"],
        );
        run_test_git(
            &root,
            &[
                "config",
                &format!("branch.{branch}.merge"),
                "refs/heads/main",
            ],
        );
        let path = if dirty_kind == "untracked" {
            "new.txt"
        } else {
            "source.txt"
        };
        fs::write(root.join(path), "preserve me\n").unwrap();
        if dirty_kind == "staged" {
            run_test_git(&root, &["add", path]);
        }
        let index = run_test_git(&root, &["write-tree"]);
        let status = run_test_git(&root, &["status", "--porcelain"]);
        synchronize_agent_git_checkout_before_launch(&root).unwrap();
        assert_eq!(run_test_git(&root, &["rev-parse", "HEAD"]), head);
        assert_eq!(run_test_git(&root, &["write-tree"]), index);
        assert_eq!(run_test_git(&root, &["status", "--porcelain"]), status);
        assert_eq!(
            fs::read_to_string(root.join(path)).unwrap(),
            "preserve me\n"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
