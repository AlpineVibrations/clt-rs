use crate::runner::tests::FakeAgentRunner;
use crate::test_support::prelude::*;
use crate::test_support::*;
use crate::worker::tests::reserve_test_worker;

#[test]
fn agent_store_git_finalization_crud_is_idempotent_and_generation_fenced() {
    let root = temp_root("agent-store-git-finalization");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .mark_session_running_blocking(
            project.id,
            "session-one",
            123,
            "run-one",
            &root.join("session-one.out"),
            &root.join("session-one.err"),
        )
        .unwrap();
    store
        .mark_session_running_blocking(
            project.id,
            "session-two",
            124,
            "run-two",
            &root.join("session-two.out"),
            &root.join("session-two.err"),
        )
        .unwrap();
    let new_finalization = |codex_session_id: &'static str, owner_run_token: &'static str| {
        agent::NewGitFinalization {
            project_id: project.id,
            codex_session_id,
            git_mode: AgentGitMode::Commit,
            starting_head: Some("1111111111111111111111111111111111111111"),
            branch_ref: Some("refs/heads/master"),
            upstream_ref: Some("refs/remotes/origin/master"),
            worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
            task_identity: None,
            owner_run_token: Some(owner_run_token),
            created_at: "100",
        }
    };

    assert!(
        !store
            .create_git_finalization_blocking(new_finalization("session-one", "stale-run",))
            .unwrap()
    );
    assert!(
        store
            .create_git_finalization_blocking(new_finalization("session-one", "run-one"))
            .unwrap()
    );
    assert!(
        !store
            .create_git_finalization_blocking(new_finalization("session-one", "run-one"))
            .unwrap()
    );
    assert!(
        store
            .create_git_finalization_blocking(new_finalization("session-two", "run-two"))
            .unwrap()
    );

    let working = store
        .git_finalization_blocking(project.id, "session-one")
        .unwrap()
        .unwrap();
    assert_eq!(working.state, GitFinalizationState::Working);
    assert_eq!(working.git_mode, AgentGitMode::Commit);
    assert_eq!(working.owner_run_token.as_deref(), Some("run-one"));
    assert_eq!(working.generation, 0);
    assert_eq!(working.created_at, "100");
    assert_eq!(working.updated_at, "100");
    assert!(working.completed_at.is_none());
    assert!(working.task_identity.is_none());
    let working_two = store
        .git_finalization_blocking(project.id, "session-two")
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .list_pending_git_finalizations_blocking(Some(project.id))
            .unwrap(),
        vec![working.clone(), working_two.clone()]
    );
    assert!(
        store
            .compare_and_set_git_finalization_with_identity_blocking(
                project.id,
                "session-one",
                0,
                GitFinalizationState::Tracking,
                "first task",
                Some("run-one"),
                "101",
            )
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_git_finalization_with_identity_blocking(
                project.id,
                "session-two",
                0,
                GitFinalizationState::Tracking,
                "second task",
                Some("run-two"),
                "101",
            )
            .is_err()
    );
    let tracking = store
        .git_finalization_blocking(project.id, "session-one")
        .unwrap()
        .unwrap();
    assert_eq!(tracking.state, GitFinalizationState::Tracking);
    assert_eq!(tracking.task_identity.as_deref(), Some("first task"));
    assert_eq!(
        store
            .list_pending_git_finalizations_blocking(Some(project.id))
            .unwrap(),
        vec![working_two.clone(), tracking]
    );

    assert!(
        !store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-one",
                9,
                GitFinalizationState::CommitPending,
                Some("run-one"),
                None,
                None,
                "101",
            )
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-one",
                1,
                GitFinalizationState::CommitPending,
                Some("run-one"),
                None,
                Some("commit not attempted yet"),
                "101",
            )
            .unwrap()
    );
    assert!(
        !store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-one",
                1,
                GitFinalizationState::CommitPending,
                Some("run-one"),
                None,
                None,
                "102",
            )
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-one",
                2,
                GitFinalizationState::CommitPending,
                Some("run-one"),
                Some("2222222222222222222222222222222222222222"),
                None,
                "102",
            )
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-one",
                3,
                GitFinalizationState::Completed,
                None,
                None,
                None,
                "103",
            )
            .unwrap()
    );

    let completed = store
        .git_finalization_blocking(project.id, "session-one")
        .unwrap()
        .unwrap();
    assert_eq!(completed.state, GitFinalizationState::Completed);
    assert_eq!(completed.generation, 4);
    assert_eq!(
        completed.commit_oid.as_deref(),
        Some("2222222222222222222222222222222222222222")
    );
    assert_eq!(completed.completed_at.as_deref(), Some("103"));
    assert_eq!(
        store
            .list_pending_git_finalizations_blocking(Some(project.id))
            .unwrap(),
        vec![working_two]
    );

    assert!(
        !store
            .create_git_finalization_blocking(new_finalization("session-two", "run-two"))
            .unwrap()
    );
    assert!(
        !store
            .delete_terminal_git_finalization_blocking(project.id, "session-two")
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-two",
                0,
                GitFinalizationState::Cancelled,
                None,
                None,
                None,
                "104",
            )
            .unwrap()
    );
    assert!(
        store
            .delete_terminal_git_finalization_blocking(project.id, "session-one")
            .unwrap()
    );
    assert!(
        store
            .delete_terminal_git_finalization_blocking(project.id, "session-two")
            .unwrap()
    );
    assert!(
        store
            .git_finalization_blocking(project.id, "session-one")
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn push_pending_commit_oid_cannot_be_replaced() {
    let root = temp_root("agent-store-push-pending-immutable-oid");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-immutable-push",
                    git_mode: AgentGitMode::CommitAndPush,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/master"),
                    upstream_ref: Some("refs/remotes/origin/master"),
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: Some("immutable push"),
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
    for (generation, state, commit_oid) in [
        (0, GitFinalizationState::Tracking, None),
        (1, GitFinalizationState::CommitPending, None),
        (
            2,
            GitFinalizationState::PushPending,
            Some("2222222222222222222222222222222222222222"),
        ),
    ] {
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-immutable-push",
                    generation,
                    state,
                    None,
                    commit_oid,
                    None,
                    "101",
                )
                .unwrap()
        );
    }

    let error = store
        .compare_and_set_git_finalization_blocking(
            project.id,
            "session-immutable-push",
            3,
            GitFinalizationState::PushPending,
            None,
            Some("3333333333333333333333333333333333333333"),
            Some("retry must retain the sealed commit"),
            "102",
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("commit OID cannot change once recorded"));
    let pending = store
        .git_finalization_blocking(project.id, "session-immutable-push")
        .unwrap()
        .unwrap();
    assert_eq!(pending.state, GitFinalizationState::PushPending);
    assert_eq!(pending.generation, 3);
    assert_eq!(
        pending.commit_oid.as_deref(),
        Some("2222222222222222222222222222222222222222")
    );
    assert!(pending.last_error.is_none());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn automated_done_is_provisional_until_the_exact_task_commit_is_proven() {
    let root = temp_root("automated-git-finalization-proof");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Ship feature — COMPLETED 2026-09-02: cargo test passed codex:session-proof\n",
        )
        .unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    assert!(
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit,)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .mark_session_running_blocking(
            project.id,
            "session-proof",
            123,
            "run-proof",
            &root.join("proof.out"),
            &root.join("proof.err"),
        )
        .unwrap();
    let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    ensure_agent_git_working_record(
        &store,
        &project,
        "session-proof",
        "run-proof",
        Some(&git_start),
    )
    .unwrap();
    assert!(
        bind_agent_git_working_task_identity(&store, &project, "session-proof", "run-proof",)
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
            run_token: "run-proof".to_string(),
        },
        &store,
    )
    .unwrap();

    assert!(read_tasks(&project_root, "doing").unwrap().is_empty());
    assert_eq!(read_tasks(&project_root, "done").unwrap().len(), 1);
    let pending = store
        .git_finalization_blocking(project.id, "session-proof")
        .unwrap()
        .unwrap();
    assert_eq!(pending.state, GitFinalizationState::CommitPending);
    assert_eq!(
        pending.starting_head.as_deref(),
        Some(starting_head.as_str())
    );

    run_test_git(&project_root, &["add", "--all"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Ship feature",
            "-m",
            "CLT-Task: codex:session-proof",
        ],
    );
    let committed_head = run_test_git(&project_root, &["rev-parse", "HEAD"]);
    let completed =
        reconcile_agent_git_finalization(&store, &project_root, pending, Some("run-proof"), None)
            .unwrap();
    assert_eq!(completed.state, GitFinalizationState::Completed);
    assert_eq!(
        completed.commit_oid.as_deref(),
        Some(committed_head.as_str())
    );

    let acknowledged_again =
        reconcile_agent_git_finalization(&store, &project_root, completed, Some("run-proof"), None)
            .unwrap();
    assert_eq!(acknowledged_again.state, GitFinalizationState::Completed);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_start_journal_is_not_reconstructed_from_completed_evidence() {
    let root = temp_root("automated-git-missing-start-journal");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Already finished — COMPLETED 2026-09-02: checked codex:session-no-journal\n",
        )
        .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
            "session-no-journal",
            123,
            "run-no-journal",
            &root.join("missing.out"),
            &root.join("missing.err"),
        )
        .unwrap();
    let late_snapshot = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();

    let error = ensure_agent_git_working_record(
        &store,
        &project,
        "session-no-journal",
        "run-no-journal",
        Some(&late_snapshot),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("cannot safely reconstruct"));
    assert!(
        store
            .git_finalization_blocking(project.id, "session-no-journal")
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_enabled_task_must_exist_in_the_frozen_commit() {
    let root = temp_root("automated-git-untracked-task");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    fs::write(project_root.join("README.md"), "tracked\n").unwrap();
    initialize_test_git_repository(&project_root);
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Untracked task\n",
    )
    .unwrap();
    let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();

    let error = require_agent_git_start_task_identity(
        &project_root,
        &start.starting_head,
        &durable_task_identity("Untracked task").unwrap(),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("committed exactly once"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn prelaunch_git_snapshot_rejects_work_before_task_activation() {
    let root = temp_root("automated-git-prelaunch-snapshot");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Start first\n",
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    fs::write(project_root.join("too-early.txt"), "implementation\n").unwrap();

    let error = verify_agent_git_start_state_unchanged(&project_root, AgentGitMode::Commit, &start)
        .unwrap_err();
    assert!(format!("{error:#}").contains("changed after CLT froze"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scheduler_owned_git_launch_state_is_required_and_consumed_at_activation() {
    let root = temp_root("automated-git-server-launch-state");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Start from durable launch state\n",
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
            "session-launch-state",
            123,
            "run-launch-state",
            &root.join("launch.out"),
            &root.join("launch.err"),
        )
        .unwrap();
    let context = AutomatedAgentChildContext {
        project_id: project.id,
        run_token: "run-launch-state".to_string(),
    };

    let error =
        move_task_to_doing_with_agent_git_journal(&project_root, "1", &context, &project, &store)
            .unwrap_err();
    assert!(format!("{error:#}").contains("no atomically registered Working Git journal"));
    assert_eq!(read_tasks(&project_root, "todo").unwrap().len(), 1);
    assert!(read_tasks(&project_root, "doing").unwrap().is_empty());
    assert!(
        store
            .git_finalization_blocking(project.id, "session-launch-state")
            .unwrap()
            .is_none()
    );

    let launch = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    store
        .record_git_launch_state_blocking(
            project.id,
            "run-launch-state",
            AgentGitMode::Commit,
            &launch,
            "100",
        )
        .unwrap();
    assert_eq!(
        store
            .git_launch_state_blocking(project.id, "run-launch-state")
            .unwrap(),
        Some((AgentGitMode::Commit, launch.clone()))
    );
    store
        .mark_session_running_with_git_finalization_blocking(
            project.id,
            "session-launch-state",
            123,
            "run-launch-state",
            &root.join("launch.out"),
            &root.join("launch.err"),
            AgentGitMode::Commit,
        )
        .unwrap();
    let unbound = store
        .git_finalization_blocking(project.id, "session-launch-state")
        .unwrap()
        .unwrap();
    assert_eq!(unbound.state, GitFinalizationState::Working);
    assert_eq!(unbound.task_identity, None);
    assert_eq!(
        unbound.starting_head.as_deref(),
        Some(launch.starting_head.as_str())
    );
    assert!(
        store
            .git_launch_state_blocking(project.id, "run-launch-state")
            .unwrap()
            .is_none()
    );

    move_task_to_doing_with_agent_git_journal(&project_root, "1", &context, &project, &store)
        .unwrap();
    assert!(read_tasks(&project_root, "todo").unwrap().is_empty());
    assert_eq!(read_tasks(&project_root, "doing").unwrap().len(), 1);
    assert!(
        store
            .git_launch_state_blocking(project.id, "run-launch-state")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .git_finalization_blocking(project.id, "session-launch-state")
            .unwrap()
            .unwrap()
            .state,
        GitFinalizationState::Working
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unconsumed_git_launch_boundary_cannot_be_overwritten_after_release() {
    let root = temp_root("automated-git-unconsumed-launch");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Preserve the first launch boundary\n",
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    store
        .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let first = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    assert!(
        store
            .record_git_launch_state_blocking(
                project.id,
                "released-run-a",
                AgentGitMode::Commit,
                &first,
                "100",
            )
            .unwrap()
    );

    fs::write(
        project_root.join("unregistered-work.txt"),
        "made after release\n",
    )
    .unwrap();
    run_test_git(&project_root, &["add", "unregistered-work.txt"]);
    run_test_git(&project_root, &["commit", "-m", "Work from ambiguous run"]);
    let later = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    assert_ne!(later.starting_head, first.starting_head);

    let error = prepare_agent_git_start_state_for_run(
        &store,
        &project,
        AgentTaskSelection::NextTodo,
        false,
        false,
        "new-run-b",
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("unconsumed launch boundary"));
    let record_error = store
        .record_git_launch_state_blocking(
            project.id,
            "new-run-b",
            AgentGitMode::Commit,
            &later,
            "101",
        )
        .unwrap_err();
    assert!(format!("{record_error:#}").contains("refusing to replace"));
    assert_eq!(
        store
            .git_launch_state_blocking(project.id, "released-run-a")
            .unwrap(),
        Some((AgentGitMode::Commit, first))
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unchanged_launch_boundary_is_reclaimed_only_after_its_worker_is_dead() {
    let root = temp_root("automated-git-unchanged-launch-reclaim");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Retry after a pre-registration crash\n",
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    store
        .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "100", "999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        project.id,
        "dead-launch-worker",
        "scheduler",
        "101",
        12,
    ));
    assert!(
        store
            .claim_worker_blocking("dead-launch-worker", 123, "102")
            .unwrap()
    );
    let launch = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    store
        .record_git_launch_state_blocking(
            project.id,
            "dead-launch-worker",
            AgentGitMode::Commit,
            &launch,
            "103",
        )
        .unwrap();
    assert!(
        store
            .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: "dead-launch-worker",
                expected_state: "running",
                expected_worker_pid: Some(123),
                expected_heartbeat_at: Some("102"),
                finished_at: "104",
                error: "simulated crash before session registration",
                permitted_successor_holder: None,
            })
            .unwrap()
    );

    let recovered = prepare_agent_git_start_state_for_run(
        &store,
        &project,
        AgentTaskSelection::NextTodo,
        false,
        false,
        "replacement-worker",
    )
    .unwrap()
    .unwrap();
    assert_eq!(recovered, launch);
    assert!(
        store
            .git_launch_state_blocking(project.id, "dead-launch-worker")
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn terminal_worker_launch_reclaim_waits_for_every_exact_session_row() {
    let root = temp_root("automated-git-launch-session-fence");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Preserve a session-bound launch\n",
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    store
        .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "100", "999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        project.id,
        "session-bound-launch-worker",
        "scheduler",
        "101",
        12,
    ));
    assert!(
        store
            .claim_worker_blocking("session-bound-launch-worker", 123, "102")
            .unwrap()
    );
    let launch = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    assert!(
        store
            .record_git_launch_state_blocking(
                project.id,
                "session-bound-launch-worker",
                AgentGitMode::Commit,
                &launch,
                "103",
            )
            .unwrap()
    );
    store
        .set_session_control_recovery_token_blocking(
            project.id,
            "session-survives-reap",
            "session-bound-launch-worker",
        )
        .unwrap();
    assert!(
        store
            .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: "session-bound-launch-worker",
                expected_state: "running",
                expected_worker_pid: Some(123),
                expected_heartbeat_at: Some("102"),
                finished_at: "104",
                error: "simulated reap with a durable session row",
                permitted_successor_holder: None,
            })
            .unwrap()
    );

    assert!(
        !store
            .reclaim_unchanged_git_launch_state_blocking(
                project.id,
                "session-bound-launch-worker",
                AgentGitMode::Commit,
                &launch,
            )
            .unwrap()
    );
    assert!(
        store
            .git_launch_state_blocking(project.id, "session-bound-launch-worker")
            .unwrap()
            .is_some()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn atomic_git_session_registration_rolls_back_without_a_launch_boundary() {
    let root = temp_root("automated-git-session-registration-rollback");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);

    let error = store
        .mark_session_running_with_git_finalization_blocking(
            project.id,
            "session-without-launch",
            123,
            "run-without-launch",
            &root.join("missing.out"),
            &root.join("missing.err"),
            AgentGitMode::Commit,
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("no compatible scheduler-owned Git launch state"));
    assert!(
        store
            .session_control_blocking(project.id, "session-without-launch")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .git_finalization_blocking(project.id, "session-without-launch")
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_start_capture_rejects_detached_head() {
    let root = temp_root("automated-git-detached-head");
    init_tasks(&root, false).unwrap();
    initialize_test_git_repository(&root);
    run_test_git(&root, &["checkout", "--detach"]);

    let error = capture_agent_git_start_state(&root, AgentGitMode::Commit).unwrap_err();
    assert!(format!("{error:#}").contains("attached branch"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_start_preflight_rejects_unsafe_mixed_board_storage() {
    let first = temp_root("automated-git-mixed-doing-done");
    init_tasks(&first, false).unwrap();
    fs::remove_file(first.join("tasks/doing.md")).unwrap();
    fs::create_dir(first.join("tasks/doing")).unwrap();
    let error = require_agent_git_board_storage_compatible(&first).unwrap_err();
    assert!(format!("{error:#}").contains("folder-backed Done"));

    let second = temp_root("automated-git-mixed-todo-doing");
    init_tasks(&second, true).unwrap();
    fs::remove_dir(second.join("tasks/doing")).unwrap();
    fs::write(second.join("tasks/doing.md"), "# Doing Tasks\n").unwrap();
    let error = require_agent_git_board_storage_compatible(&second).unwrap_err();
    assert!(format!("{error:#}").contains("folder-backed Doing"));

    fs::remove_dir_all(first).unwrap();
    fs::remove_dir_all(second).unwrap();
}

#[test]
fn git_start_preflight_checkpoints_uncommitted_todos_before_launch() {
    let root = temp_root("automated-git-uncommitted-todo");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(project_root.join("source.txt"), "before\n").unwrap();
    initialize_test_git_repository(&project_root);
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- This task exists only in the worktree\n",
    )
    .unwrap();
    fs::write(project_root.join("source.txt"), "after\n").unwrap();
    fs::write(project_root.join("unrelated.txt"), "preserve me\n").unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    store
        .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let previous_head = run_test_git(&project_root, &["rev-parse", "HEAD"]);

    let start = prepare_agent_git_start_state_for_run(
        &store,
        &project,
        AgentTaskSelection::NextTodo,
        false,
        false,
        "uncommitted-run",
    )
    .unwrap()
    .unwrap();
    assert_ne!(start.starting_head, previous_head);
    assert_eq!(
        run_test_git(&project_root, &["show", "-s", "--format=%s", "HEAD"]),
        "Record CLT task board"
    );
    assert!(git_commit_uses_agent_identity(&project_root, &start.starting_head).unwrap());
    require_agent_git_start_task_identity(
        &project_root,
        &start.starting_head,
        &durable_task_identity("This task exists only in the worktree").unwrap(),
    )
    .unwrap();
    assert!(run_test_git(&project_root, &["status", "--short", "--", "tasks"]).is_empty());
    assert_eq!(
        run_test_git(&project_root, &["status", "--short", "--", "unrelated.txt"]),
        "?? unrelated.txt"
    );
    assert_eq!(
        run_test_git(&project_root, &["status", "--short", "--", "source.txt"]),
        "M source.txt"
    );
    assert!(run_test_git(&project_root, &["diff", "--cached", "--name-only"]).is_empty());
    assert!(
        store
            .git_launch_state_blocking(project.id, "uncommitted-run")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .list_pending_git_finalizations_blocking(Some(project.id))
            .unwrap()
            .is_empty()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_git_sync_fast_forwards_before_the_launch_snapshot() {
    let root = temp_root("automated-git-startup-sync");
    let project_root = root.join("project");
    let peer_root = root.join("peer");
    let remote_root = root.join("remote.git");
    init_tasks(&project_root, false).unwrap();
    initialize_test_git_repository(&project_root);
    fs::create_dir_all(&remote_root).unwrap();
    run_test_git(&remote_root, &["init", "--bare"]);
    run_test_git(
        &project_root,
        &["remote", "add", "origin", remote_root.to_str().unwrap()],
    );
    run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
    run_test_git(
        &root,
        &[
            "clone",
            remote_root.to_str().unwrap(),
            peer_root.to_str().unwrap(),
        ],
    );
    run_test_git(&peer_root, &["config", "user.name", "CLT Peer"]);
    run_test_git(
        &peer_root,
        &["config", "user.email", "clt-peer@example.invalid"],
    );
    fs::write(peer_root.join("upstream.txt"), "upstream\n").unwrap();
    run_test_git(&peer_root, &["add", "upstream.txt"]);
    run_test_git(&peer_root, &["commit", "-m", "Advance upstream"]);
    run_test_git(&peer_root, &["push"]);
    let expected_head = run_test_git(&peer_root, &["rev-parse", "HEAD"]);
    fs::write(project_root.join("untracked-local.txt"), "preserve me\n").unwrap();

    synchronize_agent_git_checkout_before_launch(&project_root).unwrap();

    assert_eq!(
        run_test_git(&project_root, &["rev-parse", "HEAD"]),
        expected_head
    );
    assert_eq!(
        fs::read_to_string(project_root.join("untracked-local.txt")).unwrap(),
        "preserve me\n"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_git_sync_rechecks_board_storage_after_fast_forward() {
    let root = temp_root("automated-git-startup-storage-change");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    let peer_root = root.join("peer");
    let remote_root = root.join("remote.git");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Refuse an unsafe upstream board layout\n",
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    fs::create_dir_all(&remote_root).unwrap();
    run_test_git(&remote_root, &["init", "--bare"]);
    run_test_git(
        &project_root,
        &["remote", "add", "origin", remote_root.to_str().unwrap()],
    );
    run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
    run_test_git(
        &root,
        &[
            "clone",
            remote_root.to_str().unwrap(),
            peer_root.to_str().unwrap(),
        ],
    );
    run_test_git(&peer_root, &["config", "user.name", "CLT Peer"]);
    run_test_git(
        &peer_root,
        &["config", "user.email", "clt-peer@example.invalid"],
    );
    run_test_git(&peer_root, &["rm", "tasks/doing.md"]);
    fs::create_dir(peer_root.join("tasks/doing")).unwrap();
    fs::write(peer_root.join("tasks/doing/.gitkeep"), "").unwrap();
    run_test_git(&peer_root, &["add", "tasks/doing/.gitkeep"]);
    run_test_git(
        &peer_root,
        &["commit", "-m", "Make only Doing folder-backed"],
    );
    run_test_git(&peer_root, &["push"]);
    let upstream_head = run_test_git(&peer_root, &["rev-parse", "HEAD"]);

    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    store
        .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let error = prepare_agent_git_start_state_for_run(
        &store,
        &project,
        AgentTaskSelection::NextTodo,
        false,
        false,
        "storage-change-run",
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("folder-backed Done"));
    assert_eq!(
        run_test_git(&project_root, &["rev-parse", "HEAD"]),
        upstream_head
    );
    assert!(
        store
            .git_launch_state_blocking(project.id, "storage-change-run")
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_git_sync_rejects_divergence_without_rewriting_local_history() {
    let root = temp_root("automated-git-startup-sync-diverged");
    let project_root = root.join("project");
    let peer_root = root.join("peer");
    let remote_root = root.join("remote.git");
    init_tasks(&project_root, false).unwrap();
    initialize_test_git_repository(&project_root);
    fs::create_dir_all(&remote_root).unwrap();
    run_test_git(&remote_root, &["init", "--bare"]);
    run_test_git(
        &project_root,
        &["remote", "add", "origin", remote_root.to_str().unwrap()],
    );
    run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
    run_test_git(
        &root,
        &[
            "clone",
            remote_root.to_str().unwrap(),
            peer_root.to_str().unwrap(),
        ],
    );
    run_test_git(&peer_root, &["config", "user.name", "CLT Peer"]);
    run_test_git(
        &peer_root,
        &["config", "user.email", "clt-peer@example.invalid"],
    );
    fs::write(peer_root.join("upstream.txt"), "upstream\n").unwrap();
    run_test_git(&peer_root, &["add", "upstream.txt"]);
    run_test_git(&peer_root, &["commit", "-m", "Advance upstream"]);
    run_test_git(&peer_root, &["push"]);
    fs::write(project_root.join("local.txt"), "local\n").unwrap();
    run_test_git(&project_root, &["add", "local.txt"]);
    run_test_git(&project_root, &["commit", "-m", "Advance locally"]);
    let local_head = run_test_git(&project_root, &["rev-parse", "HEAD"]);

    let error = synchronize_agent_git_checkout_before_launch(&project_root).unwrap_err();

    assert!(format!("{error:#}").contains("Failed to fast-forward"));
    assert_eq!(
        run_test_git(&project_root, &["rev-parse", "HEAD"]),
        local_head
    );
    assert_eq!(
        fs::read_to_string(project_root.join("local.txt")).unwrap(),
        "local\n"
    );

    fs::remove_dir_all(root).unwrap();
}

fn working_git_link_race_fixture(
    name: &str,
    task: &str,
    session_id: &str,
    run_token: &str,
) -> (
    PathBuf,
    PathBuf,
    agent::TursoAgentStore,
    agent::GitFinalizationRecord,
) {
    let root = temp_root(name);
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        format!("# Todo Tasks\n- {task}\n"),
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .mark_session_running_blocking(
            project.id,
            session_id,
            123,
            run_token,
            &root.join("race.out"),
            &root.join("race.err"),
        )
        .unwrap();
    let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    let task_identity = durable_task_identity(task).unwrap();
    assert!(
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: session_id,
                git_mode: AgentGitMode::Commit,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &start.worktree_baseline,
                task_identity: Some(&task_identity),
                owner_run_token: Some(run_token),
                created_at: "100",
            })
            .unwrap()
    );
    let finalization = store
        .git_finalization_blocking(project.id, session_id)
        .unwrap()
        .unwrap();
    (root, project_root, store, finalization)
}

#[test]
fn working_cancellation_wins_before_link_repair_without_mutating_the_board() {
    let task = "Cancel before repairing the link";
    let session_id = "session-cancel-first";
    let run_token = "run-cancel-first";
    let (root, project_root, store, finalization) = working_git_link_race_fixture(
        "automated-git-working-cancel-first",
        task,
        session_id,
        run_token,
    );
    let state_dir = root.join("state/clt");
    let (cancel_holding_tx, cancel_holding_rx) = mpsc::channel();
    let (release_cancel_tx, release_cancel_rx) = mpsc::channel();
    let cancel_state_dir = state_dir.clone();
    let cancel_project_root = project_root.clone();
    let cancel_finalization = finalization.clone();
    let cancel_thread = thread::spawn(move || {
        let store = agent::TursoAgentStore::open_blocking(&cancel_state_dir).unwrap();
        cancel_unlinked_working_git_finalization_with_lock_callbacks(
            &store,
            &cancel_project_root,
            &cancel_finalization,
            run_token,
            || {},
            move || {
                cancel_holding_tx.send(()).unwrap();
                release_cancel_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("cancellation was not released");
            },
            || {},
        )
        .unwrap()
    });
    cancel_holding_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("cancellation did not acquire the board lock");

    let (repair_contended_tx, repair_contended_rx) = mpsc::channel();
    let repair_state_dir = state_dir.clone();
    let repair_project_root = project_root.clone();
    let repair_finalization = finalization.clone();
    let repair_thread = thread::spawn(move || {
        let store = agent::TursoAgentStore::open_blocking(&repair_state_dir).unwrap();
        repair_working_git_task_link_with_lock_callbacks(
            &store,
            &repair_project_root,
            &repair_finalization,
            || {},
            || {},
            move || repair_contended_tx.send(()).unwrap(),
        )
        .unwrap()
    });
    repair_contended_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("repair did not wait for cancellation's board lock");
    release_cancel_tx.send(()).unwrap();

    assert!(cancel_thread.join().unwrap());
    assert!(!repair_thread.join().unwrap());
    assert_eq!(
        read_tasks(&project_root, "todo").unwrap(),
        vec![format!("- {task}")]
    );
    assert!(read_tasks(&project_root, "doing").unwrap().is_empty());
    let current = store
        .git_finalization_blocking(finalization.project_id, session_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.state, GitFinalizationState::Cancelled);
    assert_eq!(current.generation, finalization.generation + 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn working_link_repair_wins_before_idle_cancellation() {
    let task = "Repair before cancelling the journal";
    let session_id = "session-repair-first";
    let run_token = "run-repair-first";
    let (root, project_root, store, finalization) = working_git_link_race_fixture(
        "automated-git-working-repair-first",
        task,
        session_id,
        run_token,
    );
    let state_dir = root.join("state/clt");
    let (repair_holding_tx, repair_holding_rx) = mpsc::channel();
    let (release_repair_tx, release_repair_rx) = mpsc::channel();
    let repair_state_dir = state_dir.clone();
    let repair_project_root = project_root.clone();
    let repair_finalization = finalization.clone();
    let repair_thread = thread::spawn(move || {
        let store = agent::TursoAgentStore::open_blocking(&repair_state_dir).unwrap();
        repair_working_git_task_link_with_lock_callbacks(
            &store,
            &repair_project_root,
            &repair_finalization,
            || {},
            move || {
                repair_holding_tx.send(()).unwrap();
                release_repair_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("repair was not released");
            },
            || {},
        )
        .unwrap()
    });
    repair_holding_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("repair did not acquire the board lock");

    let (cancel_contended_tx, cancel_contended_rx) = mpsc::channel();
    let cancel_state_dir = state_dir.clone();
    let cancel_project_root = project_root.clone();
    let cancel_finalization = finalization.clone();
    let cancel_thread = thread::spawn(move || {
        let store = agent::TursoAgentStore::open_blocking(&cancel_state_dir).unwrap();
        cancel_unlinked_working_git_finalization_with_lock_callbacks(
            &store,
            &cancel_project_root,
            &cancel_finalization,
            run_token,
            || {},
            || {},
            move || cancel_contended_tx.send(()).unwrap(),
        )
        .unwrap()
    });
    cancel_contended_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("cancellation did not wait for repair's board lock");
    release_repair_tx.send(()).unwrap();

    assert!(repair_thread.join().unwrap());
    assert!(!cancel_thread.join().unwrap());
    assert!(read_tasks(&project_root, "todo").unwrap().is_empty());
    let doing = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing).unwrap();
    assert_eq!(doing.len(), 1);
    assert_eq!(doing[0].content, format!("{task} codex:{session_id}"));
    assert_eq!(
        store
            .git_finalization_blocking(finalization.project_id, session_id)
            .unwrap()
            .unwrap(),
        finalization
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn working_link_repair_refuses_a_different_branch() {
    let root = temp_root("automated-git-working-repair-branch");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Repair only on frozen branch\n",
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
            "session-working-repair",
            123,
            "run-working-repair",
            &root.join("working.out"),
            &root.join("working.err"),
        )
        .unwrap();
    let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    let task_identity = durable_task_identity("Repair only on frozen branch").unwrap();
    assert!(
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: "session-working-repair",
                git_mode: AgentGitMode::Commit,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &start.worktree_baseline,
                task_identity: Some(&task_identity),
                owner_run_token: Some("run-working-repair"),
                created_at: "100",
            })
            .unwrap()
    );
    let finalization = store
        .git_finalization_blocking(project.id, "session-working-repair")
        .unwrap()
        .unwrap();
    run_test_git(&project_root, &["checkout", "-b", "wrong-repair-branch"]);

    assert!(!repair_working_git_task_link(&store, &project_root, &finalization).unwrap());
    let todo = read_tasks(&project_root, "todo").unwrap();
    assert_eq!(todo, vec!["- Repair only on frozen branch"]);
    assert!(read_tasks(&project_root, "doing").unwrap().is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn working_link_repair_rechecks_history_after_waiting_for_the_board_lock() {
    let root = temp_root("automated-git-working-repair-history-race");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Recheck repair history\n",
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .mark_session_running_blocking(
            project.id,
            "session-repair-race",
            123,
            "run-repair-race",
            &root.join("race.out"),
            &root.join("race.err"),
        )
        .unwrap();
    let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    let task_identity = durable_task_identity("Recheck repair history").unwrap();
    assert!(
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: "session-repair-race",
                git_mode: AgentGitMode::Commit,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &start.worktree_baseline,
                task_identity: Some(&task_identity),
                owner_run_token: Some("run-repair-race"),
                created_at: "100",
            })
            .unwrap()
    );
    let finalization = store
        .git_finalization_blocking(project.id, "session-repair-race")
        .unwrap()
        .unwrap();

    assert!(
        !repair_working_git_task_link_with_before_lock(
            &store,
            &project_root,
            &finalization,
            || {
                fs::write(project_root.join("racing-commit.txt"), "unproven\n").unwrap();
                run_test_git(&project_root, &["add", "racing-commit.txt"]);
                run_test_git(&project_root, &["commit", "-m", "Unproven racing commit"]);
            },
        )
        .unwrap()
    );
    assert_eq!(
        read_tasks(&project_root, "todo").unwrap(),
        vec!["- Recheck repair history"]
    );
    assert!(read_tasks(&project_root, "doing").unwrap().is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn working_link_repair_finishes_marker_and_duplicate_activation_crashes() {
    let root = temp_root("automated-git-working-activation-repair");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Repair activation crash\n",
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .mark_session_running_blocking(
            project.id,
            "session-activation-repair",
            123,
            "run-activation-repair",
            &root.join("activation.out"),
            &root.join("activation.err"),
        )
        .unwrap();
    let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    let task_identity = durable_task_identity("Repair activation crash").unwrap();
    assert!(
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: "session-activation-repair",
                git_mode: AgentGitMode::Commit,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &start.worktree_baseline,
                task_identity: Some(&task_identity),
                owner_run_token: Some("run-activation-repair"),
                created_at: "100",
            })
            .unwrap()
    );
    let linked = "Repair activation crash codex:session-activation-repair";
    fs::write(
        project_root.join("tasks/todo.md"),
        format!("# Todo Tasks\n- {linked}\n"),
    )
    .unwrap();
    let finalization = store
        .git_finalization_blocking(project.id, "session-activation-repair")
        .unwrap()
        .unwrap();

    assert!(repair_working_git_task_link(&store, &project_root, &finalization).unwrap());
    assert!(read_tasks(&project_root, "todo").unwrap().is_empty());
    let doing = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing).unwrap();
    assert_eq!(doing.len(), 1);
    assert_eq!(doing[0].content, linked);

    fs::write(
        project_root.join("tasks/todo.md"),
        format!("# Todo Tasks\n- {linked}\n"),
    )
    .unwrap();
    assert!(repair_working_git_task_link(&store, &project_root, &finalization).unwrap());
    assert!(read_tasks(&project_root, "todo").unwrap().is_empty());
    let doing = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing).unwrap();
    assert_eq!(doing.len(), 1);
    assert_eq!(doing[0].content, linked);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sealed_git_finalization_rejects_a_same_path_rewrite() {
    let root = temp_root("automated-git-finalization-same-path-rewrite");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Seal exact bytes — COMPLETED 2026-09-02: checked codex:session-exact-bytes\n",
        )
        .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
            "session-exact-bytes",
            123,
            "run-exact-bytes",
            &root.join("exact.out"),
            &root.join("exact.err"),
        )
        .unwrap();
    let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    ensure_agent_git_working_record(
        &store,
        &project,
        "session-exact-bytes",
        "run-exact-bytes",
        Some(&git_start),
    )
    .unwrap();
    bind_agent_git_working_task_identity(
        &store,
        &project,
        "session-exact-bytes",
        "run-exact-bytes",
    )
    .unwrap();
    fs::write(project_root.join("feature.txt"), "sealed\n").unwrap();
    run_test_git(&project_root, &["add", "feature.txt"]);
    move_task_to_done_with_agent_store(
        &project_root,
        TaskStatus::Doing,
        "1",
        &AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run-exact-bytes".to_string(),
        },
        &store,
    )
    .unwrap();

    fs::write(project_root.join("feature.txt"), "rewritten after seal\n").unwrap();
    run_test_git(&project_root, &["add", "--all"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Rewrite sealed bytes",
            "-m",
            "CLT-Task: codex:session-exact-bytes",
        ],
    );
    let pending = store
        .git_finalization_blocking(project.id, "session-exact-bytes")
        .unwrap()
        .unwrap();
    let unchanged = reconcile_agent_git_finalization(
        &store,
        &project_root,
        pending,
        Some("run-exact-bytes"),
        None,
    )
    .unwrap();
    assert_eq!(unchanged.state, GitFinalizationState::CommitPending);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn provisional_done_can_reseal_a_hook_mutation_before_commit() {
    let root = temp_root("automated-git-finalization-reseal");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Reseal hook output — COMPLETED 2026-09-02: checked codex:session-reseal\n",
        )
        .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
            "session-reseal",
            123,
            "run-reseal",
            &root.join("reseal.out"),
            &root.join("reseal.err"),
        )
        .unwrap();
    let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    ensure_agent_git_working_record(
        &store,
        &project,
        "session-reseal",
        "run-reseal",
        Some(&git_start),
    )
    .unwrap();
    bind_agent_git_working_task_identity(&store, &project, "session-reseal", "run-reseal").unwrap();
    fs::write(project_root.join("formatted.txt"), "before hook\n").unwrap();
    run_test_git(&project_root, &["add", "formatted.txt"]);
    move_task_to_done_with_agent_store(
        &project_root,
        TaskStatus::Doing,
        "1",
        &AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run-reseal".to_string(),
        },
        &store,
    )
    .unwrap();

    fs::write(project_root.join("formatted.txt"), "after hook\n").unwrap();
    run_test_git(&project_root, &["add", "--all"]);
    let pending = store
        .git_finalization_blocking(project.id, "session-reseal")
        .unwrap()
        .unwrap();
    let task_identity = pending.task_identity.as_deref().unwrap();
    let resealed_manifest = capture_agent_git_resealed_manifest(
        AgentGitProofContext {
            store: &store,
            project_id: project.id,
        },
        &project_root,
        &pending.worktree_baseline,
        "session-reseal",
        task_identity,
        pending.starting_head.as_deref().unwrap(),
        pending.branch_ref.as_deref(),
    )
    .unwrap();
    assert!(
        store
            .reseal_git_finalization_manifest_blocking(
                project.id,
                "session-reseal",
                pending.generation,
                task_identity,
                &resealed_manifest,
                "run-reseal",
                "200",
            )
            .unwrap()
    );
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Reseal hook output",
            "-m",
            "CLT-Task: codex:session-reseal",
        ],
    );
    let resealed = store
        .git_finalization_blocking(project.id, "session-reseal")
        .unwrap()
        .unwrap();
    let completed =
        reconcile_agent_git_finalization(&store, &project_root, resealed, Some("run-reseal"), None)
            .unwrap();
    assert_eq!(completed.state, GitFinalizationState::Completed);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_finalization_preserves_a_concurrent_unstaged_todo() {
    let root = temp_root("automated-git-finalization-concurrent-todo");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Finish feature — COMPLETED 2026-09-02: checked codex:session-concurrent-todo\n",
        )
        .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
            "session-concurrent-todo",
            123,
            "run-concurrent-todo",
            &root.join("concurrent.out"),
            &root.join("concurrent.err"),
        )
        .unwrap();
    let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    ensure_agent_git_working_record(
        &store,
        &project,
        "session-concurrent-todo",
        "run-concurrent-todo",
        Some(&git_start),
    )
    .unwrap();
    bind_agent_git_working_task_identity(
        &store,
        &project,
        "session-concurrent-todo",
        "run-concurrent-todo",
    )
    .unwrap();

    fs::write(project_root.join("feature.txt"), "implemented\n").unwrap();
    run_test_git(&project_root, &["add", "feature.txt"]);
    fs::write(
        project_root.join("tasks/todo.md"),
        "# To Do Tasks\n- Added by a person while the agent was working\n",
    )
    .unwrap();

    move_task_to_done_with_agent_store(
        &project_root,
        TaskStatus::Doing,
        "1",
        &AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run-concurrent-todo".to_string(),
        },
        &store,
    )
    .unwrap();
    run_test_git(&project_root, &["add", "tasks/doing.md", "tasks/done.md"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Finish feature",
            "-m",
            "CLT-Task: codex:session-concurrent-todo",
        ],
    );

    let pending = store
        .git_finalization_blocking(project.id, "session-concurrent-todo")
        .unwrap()
        .unwrap();
    let completed = reconcile_agent_git_finalization(
        &store,
        &project_root,
        pending,
        Some("run-concurrent-todo"),
        None,
    )
    .unwrap();
    assert_eq!(completed.state, GitFinalizationState::Completed);
    assert_eq!(
        fs::read_to_string(project_root.join("tasks/todo.md")).unwrap(),
        "# To Do Tasks\n- Added by a person while the agent was working\n"
    );
    assert_eq!(
        run_test_git(&project_root, &["show", "HEAD:tasks/todo.md"]),
        "# To Do Tasks"
    );
    assert_eq!(
        run_test_git(&project_root, &["status", "--short", "--", "tasks/todo.md"]),
        "M tasks/todo.md"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_finalization_rejects_raw_changes_to_another_board_scope() {
    let root = temp_root("automated-git-finalization-raw-board-scope");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Protect board scope — COMPLETED 2026-09-02: checked codex:session-board-scope\n",
        )
        .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
            "session-board-scope",
            123,
            "run-board-scope",
            &root.join("scope.out"),
            &root.join("scope.err"),
        )
        .unwrap();
    let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    ensure_agent_git_working_record(
        &store,
        &project,
        "session-board-scope",
        "run-board-scope",
        Some(&git_start),
    )
    .unwrap();
    bind_agent_git_working_task_identity(
        &store,
        &project,
        "session-board-scope",
        "run-board-scope",
    )
    .unwrap();
    fs::write(project_root.join("feature.txt"), "implemented\n").unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Unrelated header rewrite\n",
    )
    .unwrap();
    run_test_git(&project_root, &["add", "feature.txt", "tasks/todo.md"]);
    let error = move_task_to_done_with_agent_store(
        &project_root,
        TaskStatus::Doing,
        "1",
        &AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run-board-scope".to_string(),
        },
        &store,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("raw changes outside the selected task"));
    assert_eq!(read_tasks(&project_root, "doing").unwrap().len(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tracking_repair_deduplicates_an_interrupted_markdown_move() {
    let root = temp_root("automated-git-finalization-move-repair");
    init_tasks(&root, false).unwrap();
    let content = "Repair move — COMPLETED 2026-09-02: checked codex:session-move-repair";
    fs::write(
        root.join("tasks/doing.md"),
        format!("# Doing Tasks\n- {content}\n"),
    )
    .unwrap();
    fs::write(
        root.join("tasks/done.md"),
        format!("# Done Tasks\n- {content}\n"),
    )
    .unwrap();

    assert!(
        repair_tracking_agent_git_board(
            &root,
            "session-move-repair",
            &durable_task_identity(content).unwrap(),
        )
        .unwrap()
    );
    assert!(read_tasks(&root, "doing").unwrap().is_empty());
    let done = read_task_entries(&root.join("tasks"), TaskStatus::Done).unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].content, content);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tracking_repair_deduplicates_identical_markdown_and_plain_file_entries() {
    let root = temp_root("automated-git-finalization-mixed-move-repair");
    init_tasks(&root, false).unwrap();
    let content = "Repair mixed move — COMPLETED 2026-09-02: checked codex:session-mixed-repair";
    fs::write(
        root.join("tasks/doing.md"),
        format!("# Doing Tasks\n- {content}\n"),
    )
    .unwrap();
    fs::remove_file(root.join("tasks/done.md")).unwrap();
    fs::create_dir_all(root.join("tasks/done")).unwrap();
    fs::write(
        root.join("tasks/done/0001-repair-mixed-move.md"),
        format!("{content}\n"),
    )
    .unwrap();

    assert!(
        repair_tracking_agent_git_board(
            &root,
            "session-mixed-repair",
            &durable_task_identity(content).unwrap(),
        )
        .unwrap()
    );
    assert!(read_tasks(&root, "doing").unwrap().is_empty());
    let done = read_task_entries(&root.join("tasks"), TaskStatus::Done).unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].content.trim_end(), content);
    assert!(matches!(
        done[0].source,
        TaskSource::Path { is_dir: false, .. }
    ));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn managed_directory_move_crash_never_reorders_or_hides_unrelated_tasks() {
    let root = temp_root("automated-git-directory-move-crash");
    init_tasks(&root, false).unwrap();
    let content =
        "Publish atomically — COMPLETED 2026-09-02: checked codex:session-directory-crash";
    fs::write(
        root.join("tasks/doing.md"),
        format!("# Doing Tasks\n- {content}\n"),
    )
    .unwrap();
    fs::remove_file(root.join("tasks/done.md")).unwrap();
    fs::create_dir(root.join("tasks/done")).unwrap();
    let unrelated = root.join("tasks/done/0007-unrelated.md");
    fs::write(&unrelated, "Unrelated completed task\n").unwrap();

    let error = move_task_without_reordering_with_after_destination(
        &root.join("tasks"),
        TaskStatus::Doing,
        TaskStatus::Done,
        1,
        || anyhow::bail!("simulated crash after destination publication"),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("simulated crash"));
    assert!(unrelated.is_file());
    assert_eq!(read_tasks(&root, "doing").unwrap().len(), 1);
    assert_eq!(read_tasks(&root, "done").unwrap().len(), 2);
    assert!(
        fs::read_dir(root.join("tasks/done"))
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".clt-reorder-"))
    );

    assert!(
        repair_tracking_agent_git_board(
            &root,
            "session-directory-crash",
            &durable_task_identity(content).unwrap(),
        )
        .unwrap()
    );
    assert!(read_tasks(&root, "doing").unwrap().is_empty());
    assert_eq!(read_tasks(&root, "done").unwrap().len(), 2);
    assert!(unrelated.is_file());

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn managed_git_recovery_sweeps_only_exact_orphaned_atomic_temp_files() {
    let root = temp_root("automated-git-orphaned-board-temps");
    init_tasks(&root, false).unwrap();
    let board_dir = root.join("tasks");
    let todo_file = board_dir.join("todo.md");
    let original_todo = fs::read_to_string(&todo_file).unwrap();
    let markdown_crash = std::panic::catch_unwind(|| {
        replace_file_atomically_with_before_publish(
            &todo_file,
            b"# Todo Tasks\n- unpublished\n",
            |_| panic!("simulated power loss before Markdown rename"),
        )
        .unwrap();
    });
    assert!(markdown_crash.is_err());
    assert!(
        fs::read_dir(&board_dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| is_clt_atomic_task_temporary_name(&entry.file_name().to_string_lossy()))
    );

    fs::remove_file(board_dir.join("done.md")).unwrap();
    fs::create_dir(board_dir.join("done")).unwrap();
    let target = board_dir.join("done/0001-unpublished.md");
    let directory_crash = std::panic::catch_unwind(|| {
        write_new_task_file_atomically(&target, b"unpublished\n", |_| {
            panic!("simulated power loss before task-file rename")
        })
        .unwrap();
    });
    assert!(directory_crash.is_err());
    fs::write(board_dir.join("done/.keep-me"), "user file\n").unwrap();

    assert_eq!(cleanup_clt_atomic_task_temporaries(&board_dir).unwrap(), 2);
    assert_eq!(fs::read_to_string(&todo_file).unwrap(), original_todo);
    assert!(!target.exists());
    assert!(board_dir.join("done/.keep-me").is_file());
    let done_dir = board_dir.join("done");
    for directory in [&board_dir, &done_dir] {
        assert!(
            fs::read_dir(directory)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !is_clt_atomic_task_temporary_name(
                    &entry.file_name().to_string_lossy()
                ))
        );
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn push_mode_uses_the_frozen_push_remote_and_one_exact_refspec() {
    let root = temp_root("automated-git-exact-push-finalization");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    let origin_root = root.join("origin.git");
    let publish_root = root.join("publish.git");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Publish feature — COMPLETED 2026-09-02: cargo test passed codex:session-push\n",
        )
        .unwrap();
    let initial_head = initialize_test_git_repository(&project_root);
    fs::create_dir_all(&origin_root).unwrap();
    fs::create_dir_all(&publish_root).unwrap();
    run_test_git(&origin_root, &["init", "--bare"]);
    run_test_git(&publish_root, &["init", "--bare"]);
    run_test_git(
        &project_root,
        &["remote", "add", "origin", origin_root.to_str().unwrap()],
    );
    run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
    run_test_git(
        &project_root,
        &["remote", "add", "publish", publish_root.to_str().unwrap()],
    );
    let branch = run_test_git(&project_root, &["branch", "--show-current"]);
    let branch_ref = format!("refs/heads/{branch}");
    run_test_git(&project_root, &["branch", "side"]);
    run_test_git(
        &project_root,
        &["push", "publish", &format!("HEAD:{branch_ref}")],
    );
    run_test_git(&project_root, &["push", "publish", "side:refs/heads/side"]);
    run_test_git(
        &project_root,
        &["config", &format!("branch.{branch}.pushRemote"), "publish"],
    );
    run_test_git(
        &project_root,
        &["config", "remote.publish.push", "refs/heads/*:refs/heads/*"],
    );

    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    assert!(
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::CommitAndPush,)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .mark_session_running_blocking(
            project.id,
            "session-push",
            123,
            "run-push",
            &root.join("push.out"),
            &root.join("push.err"),
        )
        .unwrap();
    let git_start =
        capture_agent_git_start_state(&project_root, AgentGitMode::CommitAndPush).unwrap();
    ensure_agent_git_working_record(
        &store,
        &project,
        "session-push",
        "run-push",
        Some(&git_start),
    )
    .unwrap();
    assert!(
        bind_agent_git_working_task_identity(&store, &project, "session-push", "run-push",)
            .unwrap()
    );
    fs::write(project_root.join("publish.txt"), "implemented\n").unwrap();
    run_test_git(&project_root, &["add", "publish.txt"]);
    move_task_to_done_with_agent_store(
        &project_root,
        TaskStatus::Doing,
        "1",
        &AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run-push".to_string(),
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
            "Publish feature",
            "-m",
            "CLT-Task: codex:session-push",
        ],
    );
    let task_commit = run_test_git(&project_root, &["rev-parse", "HEAD"]);
    run_test_git(&project_root, &["branch", "-f", "side", &task_commit]);
    let pending = store
        .git_finalization_blocking(project.id, "session-push")
        .unwrap()
        .unwrap();
    let completed =
        reconcile_agent_git_finalization(&store, &project_root, pending, Some("run-push"), None)
            .unwrap();
    assert_eq!(completed.state, GitFinalizationState::Completed);
    assert_eq!(completed.commit_oid.as_deref(), Some(task_commit.as_str()));
    assert_eq!(
        run_test_git(&publish_root, &["rev-parse", &branch_ref]),
        task_commit
    );
    assert_eq!(
        run_test_git(&publish_root, &["rev-parse", "refs/heads/side"]),
        initial_head
    );
    assert_eq!(
        run_test_git(&origin_root, &["rev-parse", &branch_ref]),
        initial_head
    );

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn sealed_commit_push_runs_the_repository_pre_push_hook() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("automated-git-pre-push-hook");
    let project_root = root.join("project");
    let remote_root = root.join("remote.git");
    let hook_marker = root.join("pre-push-ran");
    init_tasks(&project_root, false).unwrap();
    let initial_head = initialize_test_git_repository(&project_root);
    fs::create_dir_all(&remote_root).unwrap();
    run_test_git(&remote_root, &["init", "--bare"]);
    run_test_git(
        &project_root,
        &["remote", "add", "origin", remote_root.to_str().unwrap()],
    );
    run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);

    let start = capture_agent_git_start_state(&project_root, AgentGitMode::CommitAndPush).unwrap();
    let baseline = AgentGitWorktreeBaseline::from_json(&start.worktree_baseline).unwrap();
    fs::write(project_root.join("hooked.txt"), "must pass policy\n").unwrap();
    run_test_git(&project_root, &["add", "hooked.txt"]);
    run_test_git(&project_root, &["commit", "-m", "Exercise pre-push hook"]);
    let commit_oid = run_test_git(&project_root, &["rev-parse", "HEAD"]);

    let hook = project_root.join(".git/hooks/pre-push");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 1\n",
            hook_marker.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&hook, permissions).unwrap();

    let error = push_agent_git_commit_to_frozen_destination(
        &project_root,
        start.branch_ref.as_deref(),
        start.upstream_ref.as_deref(),
        &baseline,
        &commit_oid,
        None,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("Failed to push sealed commit"));
    assert!(hook_marker.is_file());
    let branch_ref = start.branch_ref.unwrap();
    assert_eq!(
        run_test_git(&remote_root, &["rev-parse", &branch_ref]),
        initial_head
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn push_mode_keeps_a_non_fast_forward_publication_pending() {
    let root = temp_root("automated-git-non-fast-forward-push");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    let peer_root = root.join("peer");
    let remote_root = root.join("remote.git");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Preserve rejected publication — COMPLETED 2026-09-02: checked codex:session-push-reject\n",
        )
        .unwrap();
    initialize_test_git_repository(&project_root);
    fs::create_dir_all(&remote_root).unwrap();
    run_test_git(&remote_root, &["init", "--bare"]);
    run_test_git(
        &project_root,
        &["remote", "add", "origin", remote_root.to_str().unwrap()],
    );
    run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);

    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    store
        .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::CommitAndPush)
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .mark_session_running_blocking(
            project.id,
            "session-push-reject",
            123,
            "run-push-reject",
            &root.join("reject.out"),
            &root.join("reject.err"),
        )
        .unwrap();
    let git_start =
        capture_agent_git_start_state(&project_root, AgentGitMode::CommitAndPush).unwrap();
    ensure_agent_git_working_record(
        &store,
        &project,
        "session-push-reject",
        "run-push-reject",
        Some(&git_start),
    )
    .unwrap();
    bind_agent_git_working_task_identity(
        &store,
        &project,
        "session-push-reject",
        "run-push-reject",
    )
    .unwrap();

    run_test_git(
        &root,
        &[
            "clone",
            remote_root.to_str().unwrap(),
            peer_root.to_str().unwrap(),
        ],
    );
    run_test_git(&peer_root, &["config", "user.name", "CLT Peer"]);
    run_test_git(
        &peer_root,
        &["config", "user.email", "clt-peer@example.invalid"],
    );
    fs::write(peer_root.join("remote-only.txt"), "remote advance\n").unwrap();
    run_test_git(&peer_root, &["add", "remote-only.txt"]);
    run_test_git(&peer_root, &["commit", "-m", "Advance remote"]);
    run_test_git(&peer_root, &["push"]);
    let remote_tip = run_test_git(&peer_root, &["rev-parse", "HEAD"]);

    fs::write(project_root.join("local-only.txt"), "local task\n").unwrap();
    run_test_git(&project_root, &["add", "local-only.txt"]);
    move_task_to_done_with_agent_store(
        &project_root,
        TaskStatus::Doing,
        "1",
        &AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run-push-reject".to_string(),
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
            "Preserve rejected publication",
            "-m",
            "CLT-Task: codex:session-push-reject",
        ],
    );
    let pending = store
        .git_finalization_blocking(project.id, "session-push-reject")
        .unwrap()
        .unwrap();
    let error = reconcile_agent_git_finalization(
        &store,
        &project_root,
        pending,
        Some("run-push-reject"),
        None,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("Failed to push sealed commit"));
    assert_eq!(
        store
            .git_finalization_blocking(project.id, "session-push-reject")
            .unwrap()
            .unwrap()
            .state,
        GitFinalizationState::PushPending
    );
    let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);
    assert_eq!(
        run_test_git(&remote_root, &["rev-parse", &branch_ref]),
        remote_tip
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_commit_proof_rejects_a_duplicate_session_marker() {
    let root = temp_root("git-finalization-duplicate-marker");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    fs::write(
        project_root.join("tasks/done.md"),
        "# Done Tasks\n- Finished — COMPLETED 2026-09-02: checked codex:session-duplicate\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- Duplicate marker codex:session-duplicate\n",
    )
    .unwrap();
    run_test_git(&project_root, &["add", "--all"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Duplicate marker fixture",
            "-m",
            "CLT-Task: codex:session-duplicate",
        ],
    );
    let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);

    assert!(
        !git_ref_contains_completed_task(&project_root, &branch_ref, "session-duplicate").unwrap()
    );
    assert_eq!(
        find_agent_git_task_commit(
            &project_root,
            &starting_head,
            Some(&branch_ref),
            "session-duplicate",
            &durable_task_identity("Finished").unwrap(),
        )
        .unwrap(),
        None
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_commit_proof_rejects_a_partially_staged_board_move() {
    let root = temp_root("git-finalization-partial-board");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Move me\n",
    )
    .unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    fs::write(
        project_root.join("tasks/done.md"),
        "# Done Tasks\n- Move me — COMPLETED 2026-09-02: checked codex:session-partial\n",
    )
    .unwrap();
    run_test_git(&project_root, &["add", "tasks/done.md"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Stage only Done",
            "-m",
            "CLT-Task: codex:session-partial",
        ],
    );
    let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);

    assert_eq!(
        find_agent_git_task_commit(
            &project_root,
            &starting_head,
            Some(&branch_ref),
            "session-partial",
            &durable_task_identity("Move me").unwrap(),
        )
        .unwrap(),
        None
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_finalization_rejects_a_board_only_commit_with_source_left_uncommitted() {
    let root = temp_root("git-finalization-leftover-source");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- Implement source — COMPLETED 2026-09-02: checked codex:session-source\n",
    )
    .unwrap();
    fs::write(project_root.join("source.txt"), "before\n").unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
            "session-source",
            123,
            "run-source",
            &root.join("source.out"),
            &root.join("source.err"),
        )
        .unwrap();
    let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    ensure_agent_git_working_record(
        &store,
        &project,
        "session-source",
        "run-source",
        Some(&git_start),
    )
    .unwrap();
    bind_agent_git_working_task_identity(&store, &project, "session-source", "run-source").unwrap();
    fs::write(project_root.join("source.txt"), "after\n").unwrap();
    let error = move_task_to_done_with_agent_store(
        &project_root,
        TaskStatus::Doing,
        "1",
        &AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run-source".to_string(),
        },
        &store,
    )
    .unwrap_err();
    assert!(error.to_string().contains("remaining unstaged"));

    let pending = store
        .git_finalization_blocking(project.id, "session-source")
        .unwrap()
        .unwrap();
    assert_eq!(pending.state, GitFinalizationState::Working);
    assert_eq!(read_tasks(&project_root, "doing").unwrap().len(), 1);
    assert_eq!(
        fs::read_to_string(project_root.join("source.txt")).unwrap(),
        "after\n"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_commit_proof_rejects_an_extra_untrailed_agent_commit() {
    let root = temp_root("git-finalization-extra-agent-commit");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- One commit task\n",
    )
    .unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    fs::write(project_root.join("implementation.txt"), "implemented\n").unwrap();
    run_test_git(&project_root, &["add", "implementation.txt"]);
    run_test_agent_git(&project_root, &["commit", "-m", "Implement first"]);
    fs::write(project_root.join("tasks/todo.md"), "# Todo Tasks\n").unwrap();
    fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- One commit task — COMPLETED 2026-09-02: checked codex:session-one-commit\n",
        )
        .unwrap();
    run_test_git(&project_root, &["add", "--all", "--", "tasks"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Finish task",
            "-m",
            "CLT-Task: codex:session-one-commit",
        ],
    );
    let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);

    assert_eq!(
        find_agent_git_task_commit(
            &project_root,
            &starting_head,
            Some(&branch_ref),
            "session-one-commit",
            &durable_task_identity("One commit task").unwrap(),
        )
        .unwrap(),
        None
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn premanifest_audit_rejects_an_agent_commit_with_an_overridden_author() {
    let root = temp_root("git-finalization-overridden-author");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    fs::write(project_root.join("implementation.txt"), "premature\n").unwrap();
    run_test_git(&project_root, &["add", "implementation.txt"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "--author=Different Author <different@example.invalid>",
            "-m",
            "Premature implementation",
        ],
    );
    let manifest_parent = run_test_git(&project_root, &["rev-parse", "HEAD"]);
    let store = agent::TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();

    assert!(
        !agent_git_range_is_safe_before_manifest(
            AgentGitProofContext {
                store: &store,
                project_id: 1,
            },
            &project_root,
            &starting_head,
            &manifest_parent,
            "session-current",
        )
        .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn premanifest_audit_rejects_a_commit_with_no_agent_identity_or_journal() {
    let root = temp_root("git-finalization-unproven-non-agent");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    fs::write(project_root.join("premature.txt"), "premature\n").unwrap();
    run_test_git(&project_root, &["add", "premature.txt"]);
    run_test_git(&project_root, &["commit", "-m", "Unproven premature work"]);
    let manifest_parent = run_test_git(&project_root, &["rev-parse", "HEAD"]);
    let store = agent::TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();

    assert!(
        !agent_git_range_is_safe_before_manifest(
            AgentGitProofContext {
                store: &store,
                project_id: 1,
            },
            &project_root,
            &starting_head,
            &manifest_parent,
            "session-current",
        )
        .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn premanifest_audit_accepts_another_sessions_exact_completed_commit() {
    let root = temp_root("git-finalization-proven-other-task");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    let peer_root = root.join("peer");
    let remote_root = root.join("remote.git");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Task A — BLOCKED 2026-09-02: waiting codex:session-a\n- Task B — COMPLETED 2026-09-02: checked\n",
        )
        .unwrap();
    fs::write(project_root.join("tasks/doing.md"), "# Doing Tasks\n").unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    fs::create_dir_all(&remote_root).unwrap();
    run_test_git(&remote_root, &["init", "--bare"]);
    run_test_git(
        &project_root,
        &["remote", "add", "origin", remote_root.to_str().unwrap()],
    );
    run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
    run_test_git(
        &root,
        &[
            "clone",
            remote_root.to_str().unwrap(),
            peer_root.to_str().unwrap(),
        ],
    );
    run_test_git(&peer_root, &["config", "user.name", "CLT Peer"]);
    run_test_git(
        &peer_root,
        &["config", "user.email", "clt-peer@example.invalid"],
    );
    fs::write(peer_root.join("upstream.txt"), "upstream\n").unwrap();
    run_test_git(&peer_root, &["add", "upstream.txt"]);
    run_test_git(&peer_root, &["commit", "-m", "Advance upstream"]);
    run_test_git(&peer_root, &["push"]);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
            "session-a",
            122,
            "run-a",
            &root.join("a.out"),
            &root.join("a.err"),
        )
        .unwrap();
    let task_a_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    ensure_agent_git_working_record(&store, &project, "session-a", "run-a", Some(&task_a_start))
        .unwrap();
    assert!(bind_agent_git_working_task_identity(&store, &project, "session-a", "run-a").unwrap());
    store
        .set_session_control_state_blocking(
            project.id,
            "session-a",
            AgentSessionControlState::ResumeRequested,
        )
        .unwrap();
    let git_start = prepare_agent_git_start_state_for_run(
        &store,
        &project,
        AgentTaskSelection::NextTodo,
        false,
        false,
        "run-b",
    )
    .unwrap()
    .unwrap();
    assert_eq!(git_start.starting_head, starting_head);
    assert_eq!(
        run_test_git(&project_root, &["rev-parse", "HEAD"]),
        starting_head
    );
    move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "2").unwrap();
    let task_b = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing)
        .unwrap()
        .remove(0);
    attach_codex_session_to_task_after_lock(
        &project_root,
        TaskStatus::Doing,
        &task_b,
        "session-b",
        || {},
    )
    .unwrap();
    store
        .mark_session_running_blocking(
            project.id,
            "session-b",
            123,
            "run-b",
            &root.join("b.out"),
            &root.join("b.err"),
        )
        .unwrap();
    ensure_agent_git_working_record(&store, &project, "session-b", "run-b", Some(&git_start))
        .unwrap();
    assert!(bind_agent_git_working_task_identity(&store, &project, "session-b", "run-b").unwrap());
    fs::write(project_root.join("task-b.txt"), "implemented\n").unwrap();
    run_test_git(&project_root, &["add", "--all"]);
    move_task_to_done_with_agent_store(
        &project_root,
        TaskStatus::Doing,
        "1",
        &AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run-b".to_string(),
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
            "Finish task B",
            "-m",
            "CLT-Task: codex:session-b",
        ],
    );
    let task_b_commit = run_test_git(&project_root, &["rev-parse", "HEAD"]);
    let pending = store
        .git_finalization_blocking(project.id, "session-b")
        .unwrap()
        .unwrap();
    let completed =
        reconcile_agent_git_finalization(&store, &project_root, pending, Some("run-b"), None)
            .unwrap();
    assert_eq!(completed.state, GitFinalizationState::Completed);

    assert!(
        agent_git_range_is_safe_before_manifest(
            AgentGitProofContext {
                store: &store,
                project_id: project.id,
            },
            &project_root,
            &starting_head,
            &task_b_commit,
            "session-a",
        )
        .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn premanifest_audit_rejects_a_merge_with_extra_resolution_content() {
    let root = temp_root("git-finalization-mutated-merge");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    let primary_branch = run_test_git(&project_root, &["branch", "--show-current"]);
    run_test_git(&project_root, &["checkout", "-b", "upstream-fixture"]);
    fs::write(project_root.join("upstream.txt"), "upstream\n").unwrap();
    run_test_git(&project_root, &["add", "upstream.txt"]);
    run_test_git(&project_root, &["commit", "-m", "Upstream change"]);
    run_test_git(&project_root, &["checkout", &primary_branch]);
    fs::write(project_root.join("local.txt"), "local\n").unwrap();
    run_test_git(&project_root, &["add", "local.txt"]);
    run_test_git(&project_root, &["commit", "-m", "Local change"]);
    run_test_git(&project_root, &["merge", "--no-commit", "upstream-fixture"]);
    fs::write(project_root.join("smuggled.txt"), "implementation\n").unwrap();
    run_test_git(&project_root, &["add", "smuggled.txt"]);
    run_test_agent_git(&project_root, &["commit", "-m", "Synchronization merge"]);
    let manifest_parent = run_test_git(&project_root, &["rev-parse", "HEAD"]);
    let store = agent::TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();

    assert!(
        !agent_git_range_is_safe_before_manifest(
            AgentGitProofContext {
                store: &store,
                project_id: 1,
            },
            &project_root,
            &starting_head,
            &manifest_parent,
            "session-current",
        )
        .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn premanifest_audit_rejects_a_clean_merge_with_an_unproven_side_commit() {
    let root = temp_root("git-finalization-clean-sync-merge");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    let primary_branch = run_test_git(&project_root, &["branch", "--show-current"]);
    run_test_git(&project_root, &["checkout", "-b", "upstream-fixture"]);
    fs::write(project_root.join("upstream.txt"), "upstream\n").unwrap();
    run_test_git(&project_root, &["add", "upstream.txt"]);
    run_test_git(&project_root, &["commit", "-m", "Upstream change"]);
    run_test_git(&project_root, &["checkout", &primary_branch]);
    run_test_git(
        &project_root,
        &[
            "merge",
            "--no-ff",
            "upstream-fixture",
            "-m",
            "Synchronization merge",
        ],
    );
    let manifest_parent = run_test_git(&project_root, &["rev-parse", "HEAD"]);
    let store = agent::TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();

    assert!(
        !agent_git_range_is_safe_before_manifest(
            AgentGitProofContext {
                store: &store,
                project_id: 1,
            },
            &project_root,
            &starting_head,
            &manifest_parent,
            "session-current",
        )
        .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_commit_proof_rejects_a_divergent_starting_head() {
    let root = temp_root("git-finalization-divergent-head");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    fs::write(
        project_root.join("tasks/done.md"),
        "# Done Tasks\n- Divergent task — COMPLETED 2026-09-02: checked codex:session-divergent\n",
    )
    .unwrap();
    run_test_git(&project_root, &["add", "tasks/done.md"]);
    let tree = run_test_git(&project_root, &["write-tree"]);
    let divergent = run_test_agent_git(
        &project_root,
        &[
            "commit-tree",
            &tree,
            "-m",
            "Divergent task",
            "-m",
            "CLT-Task: codex:session-divergent",
        ],
    );
    let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);
    run_test_git(&project_root, &["update-ref", &branch_ref, &divergent]);

    assert_eq!(
        find_agent_git_task_commit(
            &project_root,
            &starting_head,
            Some(&branch_ref),
            "session-divergent",
            &durable_task_identity("Divergent task").unwrap(),
        )
        .unwrap(),
        None
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_commit_proof_rejects_a_distinct_agent_task_in_the_same_range() {
    let root = temp_root("git-finalization-distinct-agent-task");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Task A\n- Task B\n",
    )
    .unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Task A\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/done.md"),
        "# Done Tasks\n- Task B — COMPLETED 2026-09-02: checked codex:session-b\n",
    )
    .unwrap();
    run_test_git(&project_root, &["add", "--all", "--", "tasks"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Finish task B",
            "-m",
            "CLT-Task: codex:session-b",
        ],
    );
    fs::write(project_root.join("tasks/todo.md"), "# Todo Tasks\n").unwrap();
    fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Task A — COMPLETED 2026-09-02: checked codex:session-a\n- Task B — COMPLETED 2026-09-02: checked codex:session-b\n",
        )
        .unwrap();
    run_test_git(&project_root, &["add", "--all", "--", "tasks"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Finish task A",
            "-m",
            "CLT-Task: codex:session-a",
        ],
    );
    let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);

    assert_eq!(
        find_agent_git_task_commit(
            &project_root,
            &starting_head,
            Some(&branch_ref),
            "session-a",
            &durable_task_identity("Task A").unwrap(),
        )
        .unwrap(),
        None
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_commit_proof_rejects_two_commits_with_the_same_task_trailer() {
    let root = temp_root("git-finalization-two-task-trailers");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Trailer task\n",
    )
    .unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    fs::write(project_root.join("implementation.txt"), "implemented\n").unwrap();
    run_test_git(&project_root, &["add", "implementation.txt"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "First task commit",
            "-m",
            "CLT-Task: codex:session-two-trailers",
        ],
    );
    fs::write(project_root.join("tasks/todo.md"), "# Todo Tasks\n").unwrap();
    fs::write(
        project_root.join("tasks/done.md"),
        "# Done Tasks\n- Trailer task — COMPLETED 2026-09-02: checked codex:session-two-trailers\n",
    )
    .unwrap();
    run_test_git(&project_root, &["add", "--all", "--", "tasks"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Second task commit",
            "-m",
            "CLT-Task: codex:session-two-trailers",
        ],
    );
    let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);

    assert_eq!(
        find_agent_git_task_commit(
            &project_root,
            &starting_head,
            Some(&branch_ref),
            "session-two-trailers",
            &durable_task_identity("Trailer task").unwrap(),
        )
        .unwrap(),
        None
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn committed_task_parser_matches_folder_and_nested_board_grammar() {
    let root = temp_root("git-finalization-folder-parser");
    let project_root = root.join("project");
    fs::create_dir_all(project_root.join("tasks/doing/0001-parent/todo")).unwrap();
    fs::create_dir_all(project_root.join("tasks/doing/0002-real/assets/done")).unwrap();
    fs::write(
        project_root.join("tasks/doing/0001-parent/task.md"),
        "Parent task\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/doing/0001-parent/todo/0001-child.md"),
        "Nested child\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/doing/0002-real/task.md"),
        "Real task codex:session-real\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/doing/0002-real/assets/done/fake.md"),
        "Fake attachment codex:session-fake\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/doing/README.md"),
        "Direct README task\n",
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    let entries = git_ref_task_entries(&project_root, "HEAD").unwrap();
    let contents = entries
        .iter()
        .map(|entry| entry.content.trim().to_string())
        .collect::<Vec<_>>();

    assert!(contents.contains(&"Parent task".to_string()));
    assert!(contents.contains(&"Nested child".to_string()));
    assert!(contents.contains(&"Real task codex:session-real".to_string()));
    assert!(contents.contains(&"Direct README task".to_string()));
    assert!(
        !contents
            .iter()
            .any(|content| content.contains("Fake attachment"))
    );
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.content.contains("Nested child"))
            .map(|entry| entry.status.as_str()),
        Some("todo")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_finalizer_does_not_steal_an_expired_lease_from_a_running_session() {
    let root = temp_root("git-finalizer-live-control-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-live-control",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/master"),
                    upstream_ref: None,
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: None,
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
    store
        .mark_session_running_blocking(
            project.id,
            "session-live-control",
            std::process::id(),
            "run-live-control",
            &root.join("live.out"),
            &root.join("live.err"),
        )
        .unwrap();
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "live-session-holder", "100", "101")
            .unwrap()
    );

    assert!(
        !store
            .try_acquire_git_finalization_lease_blocking(
                project.id,
                "git-finalizer-contender",
                "200",
                "500",
                None,
            )
            .unwrap()
    );
    assert_eq!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .holder,
        "live-session-holder"
    );
    assert_eq!(
        store
            .session_control_blocking(project.id, "session-live-control")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Running
    );
    assert!(
        store
            .release_lease_blocking(project.id, "live-session-holder")
            .unwrap()
    );
    store
        .set_session_control_recovery_token_blocking(
            project.id,
            "session-live-control",
            "ordinary-resume-token",
        )
        .unwrap();
    assert!(
        !store
            .try_acquire_git_finalization_lease_blocking(
                project.id,
                "git-finalizer-contender",
                "200",
                "500",
                None,
            )
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project.id, "session-live-control")
            .unwrap()
            .unwrap()
            .run_token
            .as_deref(),
        Some("ordinary-resume-token")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_finalizer_gate_accepts_each_pending_sessions_exact_recovery_generation() {
    let root = temp_root("git-finalizer-multiple-recovery-controls");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    for (session, task_identity) in [
        ("session-working", None),
        ("session-finalizing", Some("finalizing task")),
    ] {
        assert!(
                store
                    .create_git_finalization_blocking(agent::NewGitFinalization {
                        project_id: project.id,
                        codex_session_id: session,
                        git_mode: AgentGitMode::Commit,
                        starting_head: Some("1111111111111111111111111111111111111111"),
                        branch_ref: Some("refs/heads/master"),
                        upstream_ref: None,
                        worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                        task_identity,
                        owner_run_token: None,
                        created_at: "100",
                    })
                    .unwrap()
            );
    }
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-finalizing",
                0,
                GitFinalizationState::Tracking,
                None,
                None,
                None,
                "101",
            )
            .unwrap()
    );
    store
        .set_session_control_recovery_token_blocking(
            project.id,
            "session-working",
            "clt-git-finalization:999",
        )
        .unwrap();
    assert!(
        store
            .ensure_pending_git_finalization_resume_requested_blocking(
                project.id,
                "session-finalizing",
            )
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project.id, "session-working")
            .unwrap()
            .unwrap()
            .run_token
            .as_deref(),
        Some("clt-git-finalization:999")
    );
    assert_eq!(
        store
            .session_control_blocking(project.id, "session-finalizing")
            .unwrap()
            .unwrap()
            .run_token
            .as_deref(),
        Some("clt-git-finalization:1")
    );
    assert!(
        store
            .try_acquire_git_finalization_lease_blocking(
                project.id,
                "multi-session-finalizer",
                &agent_timestamp(),
                &agent_timestamp_after(60),
                None,
            )
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project.id, "session-working")
            .unwrap()
            .unwrap()
            .run_token
            .as_deref(),
        Some("clt-git-finalization:0")
    );
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-finalizing",
                1,
                GitFinalizationState::CommitPending,
                None,
                None,
                None,
                "102",
            )
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project.id, "session-finalizing")
            .unwrap()
            .unwrap()
            .run_token
            .as_deref(),
        Some("clt-git-finalization:2")
    );
    assert!(
        store
            .git_finalization_lease_is_owned_blocking(
                project.id,
                "multi-session-finalizer",
                &agent_timestamp(),
            )
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-finalizing",
                2,
                GitFinalizationState::Completed,
                None,
                Some("2222222222222222222222222222222222222222"),
                None,
                "103",
            )
            .unwrap()
    );
    assert!(
        store
            .session_control_blocking(project.id, "session-finalizing")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .git_finalization_lease_is_owned_blocking(
                project.id,
                "multi-session-finalizer",
                &agent_timestamp(),
            )
            .unwrap()
    );
    assert!(
        store
            .release_lease_blocking(project.id, "multi-session-finalizer")
            .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_finalizer_heartbeat_renews_and_exact_holder_loss_fences_it() {
    let root = temp_root("git-finalizer-heartbeat-fence");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-heartbeat",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/master"),
                    upstream_ref: None,
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: None,
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
    drop(store);

    let lease = try_acquire_agent_git_finalization_lease_with_timeout(
        &state_dir,
        &project,
        false,
        Duration::from_secs(2),
    )
    .unwrap()
    .unwrap();
    thread::sleep(Duration::from_secs(3));
    lease.ensure_owned().unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert!(
        !store
            .try_acquire_git_finalization_lease_blocking(
                project.id,
                "competing-finalizer",
                &agent_timestamp(),
                &agent_timestamp_after(1),
                None,
            )
            .unwrap()
    );
    let exact_holder = lease.holder.clone();
    assert!(
        store
            .release_lease_blocking(project.id, &exact_holder)
            .unwrap()
    );
    let fence_error = lease.ensure_owned().unwrap_err();
    assert!(format!("{fence_error:#}").contains("lost its exact project lease"));
    drop(lease);

    assert!(
        store
            .try_acquire_git_finalization_lease_blocking(
                project.id,
                "replacement-finalizer",
                &agent_timestamp(),
                &agent_timestamp_after(1),
                None,
            )
            .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scheduler_prioritizes_pending_git_finalization_over_new_todo_work() {
    let root = temp_root("scheduler-git-finalization-priority");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Later task\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/done.md"),
        "# Done Tasks\n- Provisional — COMPLETED 2026-09-02: checked codex:session-pending\n",
    )
    .unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    assert!(
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit,)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-pending",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some(&starting_head),
                    branch_ref: Some(&branch_ref),
                    upstream_ref: None,
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: None,
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
    assert!(
        store
            .recover_git_finalization_intent_blocking(
                project.id,
                "session-pending",
                0,
                "provisional",
                None,
                "101",
            )
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-pending",
                1,
                GitFinalizationState::CommitPending,
                None,
                None,
                None,
                "102",
            )
            .unwrap()
    );
    store
        .set_session_control_state_blocking(
            project.id,
            "session-pending",
            AgentSessionControlState::ResumeRequested,
        )
        .unwrap();
    drop(store);

    let start =
        run_agent_scheduler_pass_with_max_global_jobs(&state_dir, true, &[], 1, None).unwrap();
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::ResumeSession
    );
    assert_eq!(
        start.jobs[0].resume_session_id.as_deref(),
        Some("session-pending")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scheduler_keeps_push_pending_autonomous_and_does_not_resume_codex() {
    let root = temp_root("scheduler-autonomous-push-pending");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Do not start while publication is pending\n",
    )
    .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-autonomous-push",
                    git_mode: AgentGitMode::CommitAndPush,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/master"),
                    upstream_ref: Some("refs/remotes/origin/master"),
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: Some("autonomous push"),
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
    for (generation, state, commit_oid) in [
        (0, GitFinalizationState::Tracking, None),
        (1, GitFinalizationState::CommitPending, None),
        (
            2,
            GitFinalizationState::PushPending,
            Some("2222222222222222222222222222222222222222"),
        ),
    ] {
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-autonomous-push",
                    generation,
                    state,
                    None,
                    commit_oid,
                    None,
                    "101",
                )
                .unwrap()
        );
    }
    store
        .set_session_control_state_blocking(
            project.id,
            "session-autonomous-push",
            AgentSessionControlState::ResumeRequested,
        )
        .unwrap();
    assert!(
        store
            .try_acquire_lease_blocking(
                project.id,
                "concurrent-finalizer",
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap()
    );
    drop(store);

    let fenced =
        run_agent_scheduler_pass_with_max_global_jobs(&state_dir, true, &[], 1, None).unwrap();
    assert!(fenced.jobs.is_empty());
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let untouched = store
        .git_finalization_blocking(project.id, "session-autonomous-push")
        .unwrap()
        .unwrap();
    assert_eq!(untouched.generation, 3);
    assert!(untouched.last_error.is_none());
    assert!(
        store
            .release_lease_blocking(project.id, "concurrent-finalizer")
            .unwrap()
    );
    drop(store);

    let pass =
        run_agent_scheduler_pass_with_max_global_jobs(&state_dir, true, &[], 1, None).unwrap();
    assert!(pass.jobs.is_empty());
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert!(
        store
            .session_control_blocking(project.id, "session-autonomous-push")
            .unwrap()
            .is_none()
    );
    let failed = store
        .git_finalization_blocking(project.id, "session-autonomous-push")
        .unwrap()
        .unwrap();
    assert_eq!(failed.state, GitFinalizationState::PushPending);
    assert_eq!(failed.generation, 4);
    assert!(failed.last_error.is_some());
    drop(store);

    let backed_off =
        run_agent_scheduler_pass_with_max_global_jobs(&state_dir, true, &[], 1, None).unwrap();
    assert!(backed_off.jobs.is_empty());
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let still_pending = store
        .git_finalization_blocking(project.id, "session-autonomous-push")
        .unwrap()
        .unwrap();
    assert_eq!(still_pending.state, GitFinalizationState::PushPending);
    assert_eq!(still_pending.generation, failed.generation);
    assert_eq!(still_pending.last_error, failed.last_error);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_job_preserves_pending_git_finalization_then_accepts_the_proven_commit() {
    let root = temp_root("agent-job-git-finalization-lifecycle");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Durable finish — COMPLETED 2026-09-02: cargo test passed codex:session-lifecycle\n",
        )
        .unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    assert!(
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit,)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .mark_session_running_blocking(
            project.id,
            "session-lifecycle",
            123,
            "run-before-commit",
            &root.join("before.out"),
            &root.join("before.err"),
        )
        .unwrap();
    let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    ensure_agent_git_working_record(
        &store,
        &project,
        "session-lifecycle",
        "run-before-commit",
        Some(&git_start),
    )
    .unwrap();
    assert!(
        bind_agent_git_working_task_identity(
            &store,
            &project,
            "session-lifecycle",
            "run-before-commit",
        )
        .unwrap()
    );
    fs::write(project_root.join("durable.txt"), "implemented\n").unwrap();
    run_test_git(&project_root, &["add", "durable.txt"]);
    move_task_to_done_with_agent_store(
        &project_root,
        TaskStatus::Doing,
        "1",
        &AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run-before-commit".to_string(),
        },
        &store,
    )
    .unwrap();

    let first_holder = "git-finalization-first-holder";
    assert!(
        store
            .try_acquire_lease_blocking(project.id, first_holder, "100", "9999999999")
            .unwrap()
    );
    let mut timed_out_runner = FakeAgentRunner::new(&state_dir, "timeout");
    timed_out_runner.result.codex_session_id = Some("session-lifecycle".to_string());
    timed_out_runner.result.session_run_token = Some("run-before-commit".to_string());
    timed_out_runner.result.exit_code = None;
    let pending_completion = run_agent_job(
        AgentRunJob {
            state_dir: state_dir.clone(),
            project: project.clone(),
            holder: first_holder.to_string(),
            worker_token: None,
            max_global_jobs: 12,
            task_selection: AgentTaskSelection::NextTodo,
            resume_session_id: None,
            blocked_task_count_before: 0,
            done_task_contents_before: Vec::new(),
            blocked_task_snapshots_before: Vec::new(),
        },
        &timed_out_runner,
        &new_agent_shutdown_signal(),
    )
    .unwrap();

    assert_eq!(pending_completion.status, "failure");
    assert!(pending_completion.summary.contains("FINALIZING"));
    assert_eq!(
        store
            .session_control_blocking(project.id, "session-lifecycle")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Running
    );
    assert_eq!(
        store
            .git_finalization_blocking(project.id, "session-lifecycle")
            .unwrap()
            .unwrap()
            .state,
        GitFinalizationState::CommitPending
    );

    run_test_git(&project_root, &["add", "--all"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Durable finish",
            "-m",
            "CLT-Task: codex:session-lifecycle",
        ],
    );
    store
        .mark_session_running_blocking(
            project.id,
            "session-lifecycle",
            124,
            "run-after-commit",
            &root.join("after.out"),
            &root.join("after.err"),
        )
        .unwrap();
    let second_holder = "git-finalization-second-holder";
    assert!(
        store
            .try_acquire_lease_blocking(project.id, second_holder, "200", "9999999999")
            .unwrap()
    );
    let mut resumed_runner = FakeAgentRunner::new(&state_dir, "timeout");
    resumed_runner.result.codex_session_id = Some("session-lifecycle".to_string());
    resumed_runner.result.session_run_token = Some("run-after-commit".to_string());
    resumed_runner.result.exit_code = None;
    let completed = run_agent_job(
        AgentRunJob {
            state_dir: state_dir.clone(),
            project: project.clone(),
            holder: second_holder.to_string(),
            worker_token: None,
            max_global_jobs: 12,
            task_selection: AgentTaskSelection::ResumeSession,
            resume_session_id: Some("session-lifecycle".to_string()),
            blocked_task_count_before: 0,
            done_task_contents_before: completed_task_contents(&project_root).unwrap(),
            blocked_task_snapshots_before: Vec::new(),
        },
        &resumed_runner,
        &new_agent_shutdown_signal(),
    )
    .unwrap();

    assert_eq!(completed.status, "success");
    assert!(completed.summary.contains("proved"));
    assert!(
        store
            .session_control_blocking(project.id, "session-lifecycle")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .git_finalization_blocking(project.id, "session-lifecycle")
            .unwrap()
            .unwrap()
            .state,
        GitFinalizationState::Completed
    );
    assert_eq!(
        store
            .latest_run_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .status,
        "success"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scheduler_rolls_forward_a_proven_commit_without_resuming_codex() {
    let root = temp_root("scheduler-git-finalization-roll-forward");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Later task\n",
    )
    .unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);
    fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Already committed — COMPLETED 2026-09-02: checked codex:session-roll-forward\n",
        )
        .unwrap();
    run_test_git(&project_root, &["add", "--all"]);
    run_test_agent_git(
        &project_root,
        &[
            "commit",
            "-m",
            "Already committed",
            "-m",
            "CLT-Task: codex:session-roll-forward",
        ],
    );

    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    assert!(
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit,)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-roll-forward",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some(&starting_head),
                    branch_ref: Some(&branch_ref),
                    upstream_ref: None,
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: None,
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
    assert!(
        store
            .recover_git_finalization_intent_blocking(
                project.id,
                "session-roll-forward",
                0,
                &durable_task_identity("Already committed").unwrap(),
                None,
                "101",
            )
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-roll-forward",
                1,
                GitFinalizationState::CommitPending,
                None,
                None,
                None,
                "102",
            )
            .unwrap()
    );
    store
        .set_session_control_state_blocking(
            project.id,
            "session-roll-forward",
            AgentSessionControlState::ResumeRequested,
        )
        .unwrap();
    drop(store);

    let mut start =
        run_agent_scheduler_pass_with_max_global_jobs(&state_dir, true, &[], 1, None).unwrap();
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(start.jobs[0].task_selection, AgentTaskSelection::NextTodo);

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(
        store
            .git_finalization_blocking(project.id, "session-roll-forward")
            .unwrap()
            .unwrap()
            .state,
        GitFinalizationState::Completed
    );
    assert!(
        store
            .session_control_blocking(project.id, "session-roll-forward")
            .unwrap()
            .is_none()
    );
    let job = start.jobs.pop().unwrap();
    assert!(
        store
            .release_lease_blocking(project.id, &job.holder)
            .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}
