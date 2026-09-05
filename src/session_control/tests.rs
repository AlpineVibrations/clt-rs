use crate::runner::tests::FakeAgentRunner;
use crate::test_support::prelude::*;
use crate::test_support::*;
use crate::tui::tests::tui_agent_project_for_test;

#[test]
fn interactive_codex_resume_command_is_always_writable() {
    let project_root = PathBuf::from("/tmp/project with spaces");
    let mut command = Command::new("codex");

    configure_interactive_codex_resume_command(&mut command, &project_root, "session-123");

    let args: Vec<OsString> = command.get_args().map(OsStr::to_os_string).collect();
    assert_eq!(
        args,
        vec![
            OsString::from("resume"),
            OsString::from("--include-non-interactive"),
            OsString::from("--sandbox"),
            OsString::from("workspace-write"),
            OsString::from("--ask-for-approval"),
            OsString::from("on-request"),
            OsString::from("-C"),
            project_root.as_os_str().to_os_string(),
            OsString::from("session-123"),
        ]
    );
    assert_eq!(command.get_current_dir(), Some(project_root.as_path()));
}

#[test]
fn legacy_read_only_guardian_holders_recover_as_shared_sessions() {
    assert_eq!(
        InteractiveGuardianDisposition::from_guardian_holder(
            "clt-readonly-interactive-worker-42-generation"
        ),
        Some(InteractiveGuardianDisposition::PreserveSharedSession)
    );
    assert_eq!(
        InteractiveGuardianDisposition::from_guardian_holder(
            "clt-stopped-readonly-interactive-worker-42-generation"
        ),
        Some(InteractiveGuardianDisposition::RestoreStoppedShared)
    );
    assert!(is_stopped_shared_interactive_holder(
        "clt-stopped-readonly-interactive-42-generation"
    ));
}

#[test]
fn automated_codex_commands_allow_registered_non_git_projects() {
    let project_root = PathBuf::from("/tmp/non-git-project");
    let mut project = tui_agent_project_for_test(1, "non-git-project").project;
    project.path = project_root.clone();

    let mut new_command = Command::new("codex");
    let new_session = configure_automated_codex_subcommand(
        &mut new_command,
        &project,
        AgentTaskSelection::NextTodo,
        None,
    )
    .unwrap();
    let new_args: Vec<_> = new_command.get_args().collect();
    assert_eq!(new_session, None);
    assert_eq!(new_args[0], OsStr::new("exec"));
    assert_eq!(new_args[1], OsStr::new("--skip-git-repo-check"));
    assert_eq!(new_args[2], OsStr::new("-C"));
    assert_eq!(new_args[3], project_root.as_os_str());

    let mut resumed_command = Command::new("codex");
    let resumed_session = configure_automated_codex_subcommand(
        &mut resumed_command,
        &project,
        AgentTaskSelection::ResumeSession,
        Some("session-123"),
    )
    .unwrap();
    let resumed_args: Vec<_> = resumed_command.get_args().collect();
    assert_eq!(resumed_session.as_deref(), Some("session-123"));
    assert_eq!(resumed_args[0], OsStr::new("exec"));
    assert_eq!(resumed_args[1], OsStr::new("resume"));
    assert_eq!(resumed_args[2], OsStr::new("--skip-git-repo-check"));
    assert_eq!(resumed_args[3], OsStr::new("session-123"));
}

#[test]
fn interactive_codex_handoff_holds_the_project_lease_until_exit() {
    let root = temp_root("interactive-codex-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let project_id = project.id;

    let lease = InteractiveAgentLease::try_acquire_at(&state_dir, project_id, 60)
        .unwrap()
        .unwrap();
    assert!(
        InteractiveAgentLease::try_acquire_at(&state_dir, project_id, 60)
            .unwrap()
            .is_none()
    );
    drop(lease);
    let reacquired = InteractiveAgentLease::try_acquire_at(&state_dir, project_id, 60)
        .unwrap()
        .unwrap();
    drop(reacquired);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stopped_session_interactive_prepare_uses_one_holder_for_control_and_lease() {
    let root = temp_root("stopped-interactive-holder");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            101,
            "stopped-generation",
            &root.join("stopped.out"),
            &root.join("stopped.err"),
        )
        .unwrap();
    assert!(
        store
            .request_session_stop_blocking(project_id, "session-123", 101, "stopped-generation",)
            .unwrap()
    );
    assert!(
        store
            .complete_session_stop_blocking(project_id, "session-123", "stopped-generation",)
            .unwrap()
    );

    let lease = prepare_tui_codex_session_interrupt_at(
        &state_dir,
        project_id,
        "session-123",
        60,
        Duration::from_millis(50),
    )
    .unwrap();
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    let recorded_lease = store
        .lease_for_project_blocking(project_id)
        .unwrap()
        .unwrap();

    assert_eq!(control.state, AgentSessionControlState::ReadyInteractive);
    assert_eq!(control.interactive_holder.as_deref(), Some(&*lease.holder));
    assert_eq!(recorded_lease.holder, lease.holder);
    lease.release().unwrap();

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_session_control_cas_rejects_stale_run_generation() {
    let root = temp_root("session-control-generation");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let stdout_path = root.join("session.out");
    let stderr_path = root.join("session.err");

    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            101,
            "run-one",
            &stdout_path,
            &stderr_path,
        )
        .unwrap();
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::Running);
    assert_eq!(control.child_pid, Some(101));
    assert_eq!(control.run_token.as_deref(), Some("run-one"));
    assert!(
        !store
            .request_session_stop_blocking(project_id, "session-123", 102, "run-one")
            .unwrap()
    );
    assert!(
        !store
            .request_session_stop_blocking(project_id, "session-123", 101, "run-two")
            .unwrap()
    );
    assert!(
        store
            .request_session_stop_blocking(project_id, "session-123", 101, "run-one")
            .unwrap()
    );

    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            101,
            "run-one",
            &stdout_path,
            &stderr_path,
        )
        .unwrap();
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::StopRequested
    );

    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            202,
            "run-two",
            &stdout_path,
            &stderr_path,
        )
        .unwrap();
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::Running);
    assert_eq!(control.child_pid, Some(202));
    assert_eq!(control.run_token.as_deref(), Some("run-two"));
    assert!(
        !store
            .complete_session_stop_blocking(project_id, "session-123", "run-one")
            .unwrap()
    );
    assert!(
        !store
            .request_session_stop_blocking(project_id, "session-123", 101, "run-one")
            .unwrap()
    );
    assert!(
        store
            .request_session_stop_blocking(project_id, "session-123", 202, "run-two")
            .unwrap()
    );
    assert!(
        !store
            .complete_session_stop_blocking(project_id, "session-123", "run-one")
            .unwrap()
    );
    assert!(
        store
            .complete_session_stop_blocking(project_id, "session-123", "run-two")
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Stopped
    );
    assert!(
        !store
            .request_stopped_session_resume_blocking(project_id, "session-123", Some("run-one"),)
            .unwrap()
    );
    assert!(
        store
            .request_stopped_session_resume_blocking(project_id, "session-123", Some("run-two"),)
            .unwrap()
    );

    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            303,
            "run-three",
            &stdout_path,
            &stderr_path,
        )
        .unwrap();
    assert!(
        store
            .request_session_stop_blocking(project_id, "session-123", 303, "run-three")
            .unwrap()
    );
    assert!(
        store
            .complete_session_stop_blocking(project_id, "session-123", "run-three")
            .unwrap()
    );
    assert!(
        !store
            .begin_stopped_session_interactive_blocking(
                project_id,
                "session-123",
                "clt-interactive-stale",
                Some("run-two"),
            )
            .unwrap()
    );
    assert!(
        store
            .begin_stopped_session_interactive_blocking(
                project_id,
                "session-123",
                "clt-interactive-current",
                Some("run-three"),
            )
            .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn newer_scheduler_controls_old_worker_without_overwriting_its_generation() {
    let root = temp_root("cross-generation-worker-control");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let stdout_path = root.join("session.out");
    let stderr_path = root.join("session.err");
    store
        .mark_session_running_blocking(
            project_id,
            "session-old",
            101,
            "old-generation",
            &stdout_path,
            &stderr_path,
        )
        .unwrap();
    assert!(
        store
            .try_acquire_lease_blocking(project_id, "scheduler", "100", "9999999999")
            .unwrap()
    );
    assert!(
        store
            .reserve_worker_blocking(agent::AgentWorkerReservation {
                project_id,
                worker_token: "new-generation",
                expected_lease_holder: "scheduler",
                max_active_workers: 12,
                protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                command_arguments: "[]",
                path_env: OsStr::new("/usr/bin:/bin"),
                codex_path: None,
                service_label: "clt-worker-new-generation",
                binary_path: Path::new("/tmp/new-clt-generation"),
                task_selection: "resume_session",
                resume_session_id: Some("session-old"),
                created_at: "101",
            })
            .unwrap()
    );
    assert!(
        store
            .claim_worker_blocking("new-generation", std::process::id(), "102")
            .unwrap()
    );

    assert!(
        store
            .mark_session_running_blocking(
                project_id,
                "session-old",
                202,
                "new-generation",
                &stdout_path,
                &stderr_path,
            )
            .is_err()
    );
    assert!(
        !store
            .clear_running_session_control_blocking(project_id, "session-old", None)
            .unwrap()
    );
    let old_control = store
        .session_control_blocking(project_id, "session-old")
        .unwrap()
        .unwrap();
    assert_eq!(old_control.child_pid, Some(101));
    assert_eq!(old_control.run_token.as_deref(), Some("old-generation"));
    assert!(
        store
            .request_session_stop_blocking(project_id, "session-old", 101, "old-generation")
            .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancelling_interrupt_preserves_a_live_runner_but_queues_after_handoff() {
    let root = temp_root("interrupt-cancel-state");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let daemon_holder = "clt-agent-123";
    let interactive_holder = "clt-interactive-456-1-1";
    assert!(
        store
            .try_acquire_lease_blocking(
                project_id,
                daemon_holder,
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap()
    );
    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            101,
            "run-one",
            &root.join("run.out"),
            &root.join("run.err"),
        )
        .unwrap();
    assert!(
        store
            .request_session_interrupt_blocking(
                project_id,
                "session-123",
                101,
                "run-one",
                interactive_holder,
            )
            .unwrap()
    );
    assert!(
        store
            .cancel_session_interrupt_handoff_blocking(
                project_id,
                "session-123",
                interactive_holder,
            )
            .unwrap()
    );
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::Running);
    assert_eq!(control.child_pid, Some(101));
    assert_eq!(control.run_token.as_deref(), Some("run-one"));
    assert!(control.interactive_holder.is_none());

    assert!(
        store
            .request_session_interrupt_blocking(
                project_id,
                "session-123",
                101,
                "run-one",
                interactive_holder,
            )
            .unwrap()
    );
    assert_eq!(
        store
            .complete_session_interrupt_handoff_blocking(
                project_id,
                "session-123",
                "run-one",
                daemon_holder,
                60,
            )
            .unwrap()
            .as_deref(),
        Some(interactive_holder)
    );
    assert!(
        store
            .cancel_session_interrupt_handoff_blocking(
                project_id,
                "session-123",
                interactive_holder,
            )
            .unwrap()
    );
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::ResumeRequested);
    assert!(control.child_pid.is_none());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tui_stop_key_toggles_running_session_to_stop_then_resume_requested() {
    let root = temp_root("tui-session-stop-toggle");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            101,
            "run-one",
            &root.join("session.out"),
            &root.join("session.err"),
        )
        .unwrap();

    let message = toggle_tui_codex_session_stop_at(&state_dir, project_id, "session-123").unwrap();
    assert!(message.starts_with("Stopping this Codex task session"));
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::StopRequested
    );
    assert!(
        store
            .complete_session_stop_blocking(project_id, "session-123", "run-one")
            .unwrap()
    );

    let message = toggle_tui_codex_session_stop_at(&state_dir, project_id, "session-123").unwrap();
    assert!(message.starts_with("Resuming this stopped Codex task session"));
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::ResumeRequested
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scheduler_runs_next_todo_while_stopped_session_waits_for_exact_resume() {
    let root = temp_root("scheduler-session-control");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- paused task codex:session-123\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- next task\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    drop(store);

    let scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(scheduled.jobs.len(), 1);
    assert_eq!(scheduled.pass.runs_started, 1);
    assert_eq!(
        scheduled.jobs[0].task_selection,
        AgentTaskSelection::NextTodo
    );

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Stopped
    );
    assert!(
        store
            .release_lease_blocking(project_id, &scheduled.jobs[0].holder)
            .unwrap()
    );
    assert!(
        store
            .transition_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Stopped,
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap()
    );
    drop(store);

    let resumed = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(resumed.jobs.len(), 1);
    assert_eq!(resumed.pass.runs_started, 1);
    let job = &resumed.jobs[0];
    assert_eq!(job.task_selection, AgentTaskSelection::ResumeSession);
    assert_eq!(job.resume_session_id.as_deref(), Some("session-123"));

    let mut command = Command::new("codex");
    let configured_session = configure_automated_codex_subcommand(
        &mut command,
        &job.project,
        job.task_selection,
        job.resume_session_id.as_deref(),
    )
    .unwrap();
    let args: Vec<_> = command.get_args().collect();
    assert_eq!(configured_session.as_deref(), Some("session-123"));
    assert_eq!(args[0], OsStr::new("exec"));
    assert_eq!(args[1], OsStr::new("resume"));
    assert_eq!(args[2], OsStr::new("--skip-git-repo-check"));
    assert_eq!(args[3], OsStr::new("session-123"));
    assert!(
        args[4]
            .to_string_lossy()
            .contains("Interactive handoff recovery:")
    );

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::ResumeRequested
    );
    assert!(
        store
            .release_lease_blocking(project_id, &job.holder)
            .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stopped_sessions_remain_persisted_without_stranding_markerless_projects() {
    let root = temp_root("stopped-session-done");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- paused task codex:session-123\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- next task\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    drop(store);

    move_task(&project_root, TaskStatus::Doing, TaskStatus::Done, "1").unwrap();
    let scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(scheduled.jobs.len(), 1);
    assert_eq!(
        scheduled.jobs[0].task_selection,
        AgentTaskSelection::NextTodo
    );
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Stopped
    );
    assert!(
        store
            .release_lease_blocking(project_id, &scheduled.jobs[0].holder)
            .unwrap()
    );
    drop(store);
    toggle_tui_codex_session_stop_at(&state_dir, project_id, "session-123").unwrap();
    let exact_resume = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(exact_resume.jobs.len(), 1);
    assert_eq!(
        exact_resume.jobs[0].task_selection,
        AgentTaskSelection::ResumeSession
    );
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .release_lease_blocking(project_id, &exact_resume.jobs[0].holder)
        .unwrap();
    drop(store);
    fs::remove_dir_all(root).unwrap();

    let root = temp_root("stopped-session-deleted");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- orphaned task codex:session-456\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- next task\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .set_session_control_state_blocking(
            project_id,
            "session-456",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    delete_task(&project_root, "doing", "1").unwrap();
    drop(store);

    let scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(scheduled.jobs.len(), 1);
    assert_eq!(
        scheduled.jobs[0].task_selection,
        AgentTaskSelection::NextTodo
    );
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-456")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Stopped
    );
    store
        .release_lease_blocking(project_id, &scheduled.jobs[0].holder)
        .unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_unstarted_resume_session_remains_requested_before_registration() {
    let root = temp_root("failed-claimed-session-resume");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- retry task codex:session-123\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::ResumeRequested,
        )
        .unwrap();
    drop(store);

    let mut scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(scheduled.jobs.len(), 1);
    assert_eq!(
        scheduled.jobs[0].task_selection,
        AgentTaskSelection::ResumeSession
    );
    assert_eq!(
        scheduled.jobs[0].resume_session_id.as_deref(),
        Some("session-123")
    );
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let unstarted = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(unstarted.state, AgentSessionControlState::ResumeRequested);
    assert_eq!(unstarted.child_pid, None);
    assert_eq!(unstarted.run_token, None);
    drop(store);

    let runner = FakeAgentRunner::new(&state_dir, "failure");
    let completion = run_agent_job(
        scheduled.jobs.pop().unwrap(),
        &runner,
        &new_agent_shutdown_signal(),
    )
    .unwrap();
    assert_eq!(completion.status, "failure");
    assert_eq!(runner.ran_project_count(), 1);

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let restored = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(restored.state, AgentSessionControlState::ResumeRequested);
    assert_eq!(restored.child_pid, None);
    assert_eq!(restored.run_token, None);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exact_resume_claim_registers_child_and_extends_the_live_lease_atomically() {
    let root = temp_root("atomic-exact-resume-child-claim");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::ResumeRequested,
        )
        .unwrap();

    assert!(
        store
            .try_acquire_lease_blocking(project_id, "expired-holder", "0", "1")
            .unwrap()
    );
    assert!(
        !store
            .register_known_session_with_child_blocking(agent::AgentKnownSessionRegistration {
                project_id,
                codex_session_id: "session-123",
                child_pid: 123,
                run_token: "expired-generation",
                stdout_path: &root.join("expired.out"),
                stderr_path: &root.join("expired.err"),
                lease_holder: "expired-holder",
                lease_timeout_seconds: 120,
                claim_requested_resume: true,
            },)
            .unwrap()
    );
    assert!(
        store
            .release_lease_blocking(project_id, "expired-holder")
            .unwrap()
    );

    let holder = "live-holder";
    let initial_expiry = agent_timestamp_after(5);
    assert!(
        store
            .try_acquire_lease_blocking(project_id, holder, &agent_timestamp(), &initial_expiry,)
            .unwrap()
    );
    assert!(
        !store
            .register_known_session_with_child_blocking(agent::AgentKnownSessionRegistration {
                project_id,
                codex_session_id: "session-123",
                child_pid: 456,
                run_token: "wrong-holder-generation",
                stdout_path: &root.join("wrong.out"),
                stderr_path: &root.join("wrong.err"),
                lease_holder: "wrong-holder",
                lease_timeout_seconds: 120,
                claim_requested_resume: true,
            },)
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::ResumeRequested
    );

    assert!(
        store
            .register_known_session_with_child_blocking(agent::AgentKnownSessionRegistration {
                project_id,
                codex_session_id: "session-123",
                child_pid: 789,
                run_token: "registered-generation",
                stdout_path: &root.join("registered.out"),
                stderr_path: &root.join("registered.err"),
                lease_holder: holder,
                lease_timeout_seconds: 120,
                claim_requested_resume: true,
            },)
            .unwrap()
    );
    let registered = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(registered.state, AgentSessionControlState::Running);
    assert_eq!(registered.child_pid, Some(789));
    assert_eq!(
        registered.run_token.as_deref(),
        Some("registered-generation")
    );
    let renewed_expiry = store
        .lease_for_project_blocking(project_id)
        .unwrap()
        .unwrap()
        .expires_at;
    assert!(renewed_expiry.parse::<u64>().unwrap() > initial_expiry.parse::<u64>().unwrap());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unproven_spawned_child_keeps_an_exact_resume_claim_fenced() {
    struct UnprovenTerminationRunner {
        state_dir: PathBuf,
    }

    impl AgentRunner for UnprovenTerminationRunner {
        fn run_project(&self, request: AgentRunRequest<'_>) -> Result<AgentRunResult> {
            let session_id = request.resume_session_id.unwrap();
            let store = agent::TursoAgentStore::open_blocking(&self.state_dir)?;
            assert!(store.register_known_session_with_child_blocking(
                agent::AgentKnownSessionRegistration {
                    project_id: request.project.id,
                    codex_session_id: session_id,
                    child_pid: std::process::id(),
                    run_token: "registered-unproven-generation",
                    stdout_path: &self.state_dir.join("unproven.out"),
                    stderr_path: &self.state_dir.join("unproven.err"),
                    lease_holder: request.lease_holder,
                    lease_timeout_seconds: 60,
                    claim_requested_resume: true,
                },
            )?);
            Err(anyhow::Error::new(AgentChildTerminationUnproven(
                "spawned process group may still be alive".to_string(),
            )))
        }
    }

    let root = temp_root("unproven-child-exact-resume");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- retry task codex:session-123\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::ResumeRequested,
        )
        .unwrap();
    drop(store);

    let mut scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    let completion = run_agent_job(
        scheduled.jobs.pop().unwrap(),
        &UnprovenTerminationRunner {
            state_dir: state_dir.clone(),
        },
        &new_agent_shutdown_signal(),
    )
    .unwrap();
    assert_eq!(completion.status, "failure");

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::Running);
    assert_eq!(control.child_pid, Some(std::process::id()));
    assert_eq!(
        control.run_token.as_deref(),
        Some("registered-unproven-generation")
    );
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    let next_pass = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert!(next_pass.jobs.is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interrupt_handoff_atomically_transfers_lease_then_schedules_exact_resume() {
    let root = temp_root("interrupt-handoff-transaction");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- interrupted task codex:session-123\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let daemon_holder = "clt-agent-111";
    let interactive_holder = "clt-interactive-222";
    assert!(
        store
            .try_acquire_lease_blocking(project_id, daemon_holder, "100", "9999999999",)
            .unwrap()
    );
    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            101,
            "run-one",
            &root.join("session.out"),
            &root.join("session.err"),
        )
        .unwrap();
    assert!(
        store
            .request_session_interrupt_blocking(
                project_id,
                "session-123",
                101,
                "run-one",
                interactive_holder,
            )
            .unwrap()
    );

    assert_eq!(
        store
            .complete_session_interrupt_handoff_blocking(
                project_id,
                "session-123",
                "stale-run",
                daemon_holder,
                60,
            )
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap()
            .holder,
        daemon_holder
    );
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::InterruptRequested
    );

    assert_eq!(
        store
            .complete_session_interrupt_handoff_blocking(
                project_id,
                "session-123",
                "run-one",
                daemon_holder,
                60,
            )
            .unwrap()
            .as_deref(),
        Some(interactive_holder)
    );
    let lease = store
        .lease_for_project_blocking(project_id)
        .unwrap()
        .unwrap();
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(lease.holder, interactive_holder);
    assert_eq!(control.state, AgentSessionControlState::ReadyInteractive);
    assert_eq!(
        control.interactive_holder.as_deref(),
        Some(interactive_holder)
    );
    assert_eq!(control.child_pid, None);

    assert!(
        store
            .transition_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::ReadyInteractive,
                AgentSessionControlState::Interactive,
            )
            .unwrap()
    );
    assert!(
        store
            .transition_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Interactive,
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap()
    );
    assert!(
        store
            .release_lease_blocking(project_id, interactive_holder)
            .unwrap()
    );
    drop(store);

    let resumed = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(resumed.jobs.len(), 1);
    assert_eq!(resumed.pass.runs_started, 1);
    let job = &resumed.jobs[0];
    assert_eq!(job.task_selection, AgentTaskSelection::ResumeSession);
    assert_eq!(job.resume_session_id.as_deref(), Some("session-123"));

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::ResumeRequested);
    assert_eq!(control.codex_session_id, "session-123");
    assert!(
        store
            .release_lease_blocking(project_id, &job.holder)
            .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scheduler_clears_orphaned_resume_session_and_runs_next_todo() {
    let root = temp_root("scheduler-orphaned-resume-session");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "next task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .set_session_control_state_blocking(
            project_id,
            "session-without-task-marker",
            AgentSessionControlState::ResumeRequested,
        )
        .unwrap();
    drop(store);

    let scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    assert_eq!(scheduled.jobs.len(), 1);
    assert_eq!(scheduled.pass.runs_started, 1);
    assert_eq!(
        scheduled.jobs[0].task_selection,
        AgentTaskSelection::NextTodo
    );
    assert!(scheduled.jobs[0].resume_session_id.is_none());
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert!(
        store
            .session_control_blocking(project_id, "session-without-task-marker")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .release_lease_blocking(project_id, &scheduled.jobs[0].holder)
            .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interactive_guardian_adoption_and_finish_are_atomic_full_holder_cas() {
    let root = temp_root("interactive-guardian-holder-cas");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let daemon_holder = "clt-agent-daemon-generation";
    let tui_holder = "clt-interactive-tui-generation";
    let guardian_holder = "clt-interactive-worker-guardian-generation";
    assert!(
        store
            .try_acquire_lease_blocking(project_id, daemon_holder, "100", "9999999999")
            .unwrap()
    );
    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            101,
            "run-one",
            &root.join("session.out"),
            &root.join("session.err"),
        )
        .unwrap();
    assert!(
        store
            .request_session_interrupt_blocking(
                project_id,
                "session-123",
                101,
                "run-one",
                tui_holder,
            )
            .unwrap()
    );
    assert_eq!(
        store
            .complete_session_interrupt_handoff_blocking(
                project_id,
                "session-123",
                "run-one",
                daemon_holder,
                60,
            )
            .unwrap()
            .as_deref(),
        Some(tui_holder)
    );

    assert!(
        !store
            .adopt_interactive_guardian_blocking(
                project_id,
                Some("session-123"),
                "clt-interactive-stale-generation",
                guardian_holder,
                60,
            )
            .unwrap()
    );
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::ReadyInteractive);
    assert_eq!(control.interactive_holder.as_deref(), Some(tui_holder));
    assert_eq!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap()
            .holder,
        tui_holder
    );

    assert!(
        store
            .adopt_interactive_guardian_blocking(
                project_id,
                Some("session-123"),
                tui_holder,
                guardian_holder,
                60,
            )
            .unwrap()
    );
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::Interactive);
    assert_eq!(control.interactive_holder.as_deref(), Some(guardian_holder));
    assert_eq!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap()
            .holder,
        guardian_holder
    );

    let disconnected = AtomicBool::new(false);
    let terminal_input = fs::File::create(root.join("test-terminal-input")).unwrap();
    assert_eq!(
        run_guarded_interactive_codex(
            &store,
            &store.list_projects_blocking().unwrap().remove(0),
            "session-123",
            guardian_holder,
            Duration::from_secs(60),
            terminal_input,
            &disconnected,
        )
        .unwrap(),
        None
    );
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Interactive
    );

    assert!(
        !store
            .finish_interactive_guardian_blocking(
                project_id,
                "session-123",
                "clt-interactive-worker-stale-generation",
                InteractiveGuardianDisposition::ResumeExec,
            )
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Interactive
    );
    assert_eq!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap()
            .holder,
        guardian_holder
    );

    assert!(
        store
            .finish_interactive_guardian_blocking(
                project_id,
                "session-123",
                guardian_holder,
                InteractiveGuardianDisposition::ResumeExec,
            )
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::ResumeRequested
    );
    assert!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stopping_interactive_takeover_keeps_the_exact_session_stopped() {
    let root = temp_root("interactive-guardian-explicit-stop");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let requester = InteractiveAgentLease::holder_for_current_process();
    assert!(
        store
            .try_acquire_lease_blocking(project_id, &requester, "100", "9999999999")
            .unwrap()
    );
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    assert!(
            store
                .begin_stopped_session_interactive_blocking(
                    project_id,
                    "session-123",
                    &requester,
                    None,
                )
                .unwrap()
        );
    let guardian = interactive_guardian_holder(InteractiveGuardianDisposition::ResumeExec);
    assert!(
        store
            .adopt_interactive_guardian_blocking(
                project_id,
                Some("session-123"),
                &requester,
                &guardian,
                60,
            )
            .unwrap()
    );
    assert!(
        store
            .register_interactive_guardian_child_blocking(
                project_id,
                "session-123",
                &guardian,
                std::process::id(),
                60,
            )
            .unwrap()
    );
    assert!(
        store
            .request_interactive_session_stop_blocking(
                project_id,
                "session-123",
                std::process::id(),
                &guardian,
            )
            .unwrap()
    );
    assert!(
        store
            .finish_interactive_guardian_blocking(
                project_id,
                "session-123",
                &guardian,
                InteractiveGuardianDisposition::ResumeExec,
            )
            .unwrap()
    );
    let stopped = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(stopped.state, AgentSessionControlState::Stopped);
    assert!(stopped.child_pid.is_none());
    assert!(stopped.interactive_holder.is_none());
    assert!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_task_session_can_be_opened_repeatedly() {
    let root = temp_root("interactive-completed-task-repeat");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/done.md"),
        "# Done Tasks\n- finished task codex:session-123\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;

    let first_requester = InteractiveAgentLease::holder_for_idle_session();
    assert!(
        store
            .try_acquire_lease_blocking(project_id, &first_requester, "100", "9999999999",)
            .unwrap()
    );
    assert!(
        store
            .reserve_idle_session_interactive_blocking(
                project_id,
                "session-123",
                &first_requester,
                None,
            )
            .unwrap()
    );
    let first_disposition = InteractiveGuardianDisposition::from_handoff(
        InteractiveCodexResumeMode::WritableIdle,
        &first_requester,
    );
    assert_eq!(
        first_disposition,
        InteractiveGuardianDisposition::PreserveIdleSession
    );
    let first_guardian = interactive_guardian_holder(first_disposition);
    assert!(
        store
            .adopt_interactive_guardian_blocking(
                project_id,
                Some("session-123"),
                &first_requester,
                &first_guardian,
                60,
            )
            .unwrap()
    );
    assert!(
        store
            .finish_interactive_guardian_blocking(
                project_id,
                "session-123",
                &first_guardian,
                first_disposition,
            )
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Stopped
    );
    assert!(codex_session_task_supports_interactive_resume(&project_root, "session-123").unwrap());

    let second_requester = InteractiveAgentLease::holder_for_stopped_session();
    assert!(
        store
            .try_acquire_lease_blocking(project_id, &second_requester, "100", "9999999999",)
            .unwrap()
    );
    assert!(
        store
            .reserve_idle_session_interactive_blocking(
                project_id,
                "session-123",
                &second_requester,
                None,
            )
            .unwrap()
    );
    let second_disposition = InteractiveGuardianDisposition::from_handoff(
        InteractiveCodexResumeMode::WritableIdle,
        &second_requester,
    );
    assert_eq!(
        second_disposition,
        InteractiveGuardianDisposition::RestoreStopped
    );
    let second_guardian = interactive_guardian_holder(second_disposition);
    assert!(
        store
            .adopt_interactive_guardian_blocking(
                project_id,
                Some("session-123"),
                &second_requester,
                &second_guardian,
                60,
            )
            .unwrap()
    );
    assert!(
        store
            .finish_interactive_guardian_blocking(
                project_id,
                "session-123",
                &second_guardian,
                second_disposition,
            )
            .unwrap()
    );
    let preserved = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(preserved.state, AgentSessionControlState::Stopped);
    assert!(preserved.interactive_holder.is_none());
    assert!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_idle_guardian_recovery_preserves_the_session_and_releases_the_lease() {
    let root = temp_root("interactive-guardian-no-resume");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let tui_holder = "clt-interactive-done-task-generation";
    let guardian_holder = "clt-interactive-worker-done-task-generation";
    assert!(
        store
            .try_acquire_lease_blocking(project_id, tui_holder, "100", "9999999999")
            .unwrap()
    );
    assert!(
        store
            .reserve_idle_session_interactive_blocking(project_id, "session-123", tui_holder, None,)
            .unwrap()
    );
    assert!(
        store
            .adopt_interactive_guardian_blocking(
                project_id,
                Some("session-123"),
                tui_holder,
                guardian_holder,
                60,
            )
            .unwrap()
    );
    assert_eq!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap()
            .holder,
        guardian_holder
    );
    assert!(
        store
            .recover_stale_interactive_guardian_blocking(
                project_id,
                "session-123",
                guardian_holder,
                None,
                InteractiveGuardianDisposition::PreserveIdleSession,
            )
            .unwrap()
    );
    assert!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .is_none()
    );
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::Stopped);
    assert!(control.interactive_holder.is_none());
    assert!(control.interactive_launch_token.is_none());

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn dead_interactive_holder_with_a_matching_session_is_not_blindly_reclaimed() {
    let root = temp_root("interactive-guardian-session-fence");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let requester = "clt-idle-interactive-requester";
    let guardian = format!("clt-idle-interactive-worker-{}-1-1", u32::MAX);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, requester, "100", "9999999999",)
            .unwrap()
    );
    assert!(
        store
            .reserve_idle_session_interactive_blocking(project.id, "session-123", requester, None,)
            .unwrap()
    );
    assert!(
        store
            .adopt_interactive_guardian_blocking(
                project.id,
                Some("session-123"),
                requester,
                &guardian,
                60,
            )
            .unwrap()
    );
    let lease = store
        .lease_for_project_blocking(project.id)
        .unwrap()
        .unwrap();
    drop(store);

    assert!(!try_reclaim_inactive_agent_lease(&state_dir, &project, None, &lease, false).unwrap());
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .holder,
        guardian
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reaped_guardian_releases_its_lease_when_the_session_row_disappeared() {
    let root = temp_root("interactive-guardian-missing-session");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let requester = InteractiveAgentLease::holder_for_idle_session();
    let guardian = interactive_guardian_holder(InteractiveGuardianDisposition::PreserveIdleSession);
    assert!(
        store
            .try_acquire_lease_blocking(project_id, &requester, "100", "9999999999",)
            .unwrap()
    );
    assert!(
        store
            .adopt_interactive_guardian_blocking(project_id, None, &requester, &guardian, 60,)
            .unwrap()
    );

    let error = finish_interactive_guardian_after_reap(
        &store,
        project_id,
        "missing-session",
        &guardian,
        Duration::from_secs(60),
        InteractiveGuardianDisposition::PreserveIdleSession,
    )
    .unwrap_err();

    assert!(format!("{error:#}").contains("released the orphaned project reservation"));
    assert!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reaped_guardian_exits_for_registry_recovery_and_releases_database_access() {
    for marker in ["recovery-required", "registry-dirty"] {
        let root = temp_root("interactive-guardian-recovery-required");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let requester = InteractiveAgentLease::holder_for_idle_session();
        let guardian =
            interactive_guardian_holder(InteractiveGuardianDisposition::PreserveIdleSession);
        assert!(
            store
                .try_acquire_lease_blocking(project_id, &requester, "100", "9999999999")
                .unwrap()
        );
        assert!(
            store
                .reserve_idle_session_interactive_blocking(
                    project_id,
                    "session-123",
                    &requester,
                    None
                )
                .unwrap()
        );
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    &requester,
                    &guardian,
                    60
                )
                .unwrap()
        );
        let snapshot = fs::read(state_dir.join("registry.json")).unwrap();
        fs::write(state_dir.join(marker), "shared WAL ownership failure").unwrap();
        let expected_guardian = guardian.clone();
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let result = finish_interactive_guardian_after_reap(
                &store,
                project_id,
                "session-123",
                &guardian,
                Duration::from_secs(60),
                InteractiveGuardianDisposition::PreserveIdleSession,
            );
            drop(store);
            tx.send(result).unwrap();
        });
        let error = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("guardian must exit instead of retrying recovery forever")
            .unwrap_err();
        handle.join().unwrap();
        assert!(format!("{error:#}").contains("recovery required"));
        assert_eq!(fs::read(state_dir.join("registry.json")).unwrap(), snapshot);
        // Exclusive repair now succeeds, proving the guardian dropped its store.
        agent::recovery::recover_registry(&state_dir).unwrap();
        let reopened = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let control = reopened
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::Interactive);
        assert_eq!(
            control.interactive_holder.as_deref(),
            Some(expected_guardian.as_str())
        );
        assert_eq!(
            reopened
                .lease_for_project_blocking(project_id)
                .unwrap()
                .unwrap()
                .holder,
            expected_guardian
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn stale_stopped_guardian_recovery_restores_the_stopped_generation() {
    let root = temp_root("interactive-guardian-restore-stopped");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let tui_holder = "clt-stopped-interactive-tui-generation";
    let guardian_holder = "clt-stopped-interactive-worker-guardian-generation";
    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            101,
            "stopped-generation",
            &root.join("stopped.out"),
            &root.join("stopped.err"),
        )
        .unwrap();
    assert!(
        store
            .request_session_stop_blocking(project_id, "session-123", 101, "stopped-generation",)
            .unwrap()
    );
    assert!(
        store
            .complete_session_stop_blocking(project_id, "session-123", "stopped-generation",)
            .unwrap()
    );
    assert!(
        store
            .try_acquire_lease_blocking(project_id, tui_holder, "100", "9999999999")
            .unwrap()
    );
    assert!(
        !store
            .reserve_idle_session_interactive_blocking(
                project_id,
                "session-123",
                tui_holder,
                Some("stale-generation"),
            )
            .unwrap()
    );
    assert!(
        store
            .reserve_idle_session_interactive_blocking(
                project_id,
                "session-123",
                tui_holder,
                Some("stopped-generation"),
            )
            .unwrap()
    );
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::ReadyInteractive);
    assert_eq!(control.interactive_holder.as_deref(), Some(tui_holder));
    assert!(
        store
            .cancel_idle_session_interactive_blocking(project_id, "session-123", tui_holder)
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Stopped
    );
    assert!(
        store
            .reserve_idle_session_interactive_blocking(
                project_id,
                "session-123",
                tui_holder,
                Some("stopped-generation"),
            )
            .unwrap()
    );

    assert!(
        store
            .adopt_interactive_guardian_blocking(
                project_id,
                Some("session-123"),
                tui_holder,
                guardian_holder,
                60,
            )
            .unwrap()
    );
    assert!(
        store
            .recover_stale_interactive_guardian_blocking(
                project_id,
                "session-123",
                guardian_holder,
                None,
                InteractiveGuardianDisposition::RestoreStopped,
            )
            .unwrap()
    );
    let control = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::Stopped);
    assert!(control.interactive_holder.is_none());
    assert!(control.interactive_launch_token.is_none());
    assert!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn interactive_guardian_stops_owned_group_descendants_without_touching_another_group() {
    let spawn_group_with_term_ignoring_descendant = || {
        let mut command = Command::new("/bin/sh");
        command
                .arg("-c")
                .arg(
                    "(trap '' TERM; count=0; while [ \"$count\" -lt 20 ]; do sleep 1; count=$((count + 1)); done) & wait",
                );
        configure_interactive_child_command(&mut command);
        command.spawn().unwrap()
    };
    let mut owned_child = spawn_group_with_term_ignoring_descendant();
    let mut unrelated_child = spawn_group_with_term_ignoring_descendant();
    thread::sleep(Duration::from_millis(50));

    assert_eq!(
        // SAFETY: the child PID is live and getpgid only inspects process metadata.
        unsafe { libc::getpgid(i32::try_from(owned_child.id()).unwrap()) },
        i32::try_from(owned_child.id()).unwrap()
    );

    let stop_result = stop_interactive_child_process(&mut owned_child);
    let owned_child_was_reaped = owned_child.try_wait().unwrap().is_some();
    let unrelated_child_survived = unrelated_child.try_wait().unwrap().is_none();
    let _ = stop_interactive_child_process(&mut unrelated_child);

    assert!(stop_result.unwrap().is_some());
    assert!(owned_child_was_reaped);
    assert!(unrelated_child_survived);
}

#[test]
fn stale_interactive_session_without_lease_is_queued_for_exec_resume() {
    let root = temp_root("stale-interactive-session");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    assert!(
        store
            .begin_stopped_session_interactive_blocking(
                project_id,
                "session-123",
                "clt-interactive-4294967295",
                None,
            )
            .unwrap()
    );
    assert!(
        !store
            .recover_stale_interactive_session_control_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::ReadyInteractive,
                AgentSessionControlState::ResumeRequested,
                Some("clt-interactive-stale-generation"),
            )
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .interactive_holder
            .as_deref(),
        Some("clt-interactive-4294967295")
    );
    drop(store);

    reconcile_stale_agent_session_controls(
        &state_dir,
        project_id,
        None,
        false,
        agent_timestamp_seconds()
            .saturating_add(TUI_SESSION_HANDOFF_TIMEOUT_SECONDS)
            .saturating_add(10),
    )
    .unwrap();

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::ResumeRequested
    );

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn stale_gated_guardian_recovers_after_dying_before_child_registration() {
    let root = temp_root("stale-interactive-guardian");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let provisional_holder = "clt-interactive-1-2-3";
    let guardian_holder = format!("clt-interactive-worker-{}-5-6", u32::MAX);
    assert!(
        store
            .try_acquire_lease_blocking(
                project_id,
                provisional_holder,
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap()
    );
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    assert!(
        store
            .begin_stopped_session_interactive_blocking(
                project_id,
                "session-123",
                provisional_holder,
                None,
            )
            .unwrap()
    );
    assert!(
        store
            .adopt_interactive_guardian_blocking(
                project_id,
                Some("session-123"),
                provisional_holder,
                &guardian_holder,
                60,
            )
            .unwrap()
    );
    let adopted = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(adopted.state, AgentSessionControlState::Interactive);
    assert_eq!(adopted.child_pid, None);
    assert_eq!(
        adopted.interactive_holder.as_deref(),
        Some(guardian_holder.as_str())
    );
    assert_eq!(
        adopted.interactive_launch_token.as_deref(),
        Some(guardian_holder.as_str())
    );
    let orphaned_lease = store
        .lease_for_project_blocking(project_id)
        .unwrap()
        .unwrap();
    assert_eq!(orphaned_lease.holder, guardian_holder);
    drop(store);

    reconcile_stale_agent_session_controls(
        &state_dir,
        project_id,
        Some(&orphaned_lease),
        false,
        agent_timestamp_seconds(),
    )
    .unwrap();

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let recovered = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(recovered.state, AgentSessionControlState::ResumeRequested);
    assert_eq!(recovered.child_pid, None);
    assert!(recovered.interactive_holder.is_none());
    assert!(recovered.interactive_launch_token.is_none());
    assert!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_guardian_without_a_launch_token_remains_fail_closed() {
    let root = temp_root("legacy-interactive-guardian");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let provisional_holder = "clt-interactive-legacy-requester";
    let guardian_holder = "clt-interactive-worker-legacy-guardian";
    assert!(
        store
            .try_acquire_lease_blocking(
                project_id,
                provisional_holder,
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap()
    );
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    assert!(
        store
            .begin_stopped_session_interactive_blocking(
                project_id,
                "session-123",
                provisional_holder,
                None,
            )
            .unwrap()
    );
    assert!(
        store
            .adopt_interactive_guardian_blocking(
                project_id,
                Some("session-123"),
                provisional_holder,
                guardian_holder,
                60,
            )
            .unwrap()
    );
    assert!(
        store
            .release_lease_blocking(project_id, guardian_holder)
            .unwrap()
    );
    let db_path = store.db_path().to_path_buf();
    drop(store);

    // Simulate an Interactive row adopted by a pre-gate CLT binary. A NULL
    // PID in that generation is not proof that Codex was never launched.
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
            .experimental_multiprocess_wal(true)
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "UPDATE session_controls
                    SET interactive_launch_token = NULL
                  WHERE project_id = ?1 AND codex_session_id = ?2",
            turso::params![project_id, "session-123"],
        )
        .await
        .unwrap();
    });

    reconcile_stale_agent_session_controls(
        &state_dir,
        project_id,
        None,
        false,
        agent_timestamp_seconds()
            .saturating_add(TUI_SESSION_HANDOFF_TIMEOUT_SECONDS)
            .saturating_add(10),
    )
    .unwrap();

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let legacy = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(legacy.state, AgentSessionControlState::Interactive);
    assert_eq!(legacy.child_pid, None);
    assert_eq!(legacy.interactive_holder.as_deref(), Some(guardian_holder));
    assert!(legacy.interactive_launch_token.is_none());

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn stale_automated_session_recovers_only_after_its_recorded_child_is_gone() {
    let root = temp_root("stale-automated-child-fence");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            std::process::id(),
            "run-live",
            &root.join("live.out"),
            &root.join("live.err"),
        )
        .unwrap();
    assert!(
        !store
            .recover_stale_automated_session_control_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Running,
                AgentSessionControlState::ResumeRequested,
                std::process::id(),
                Some("stale-run-token"),
            )
            .unwrap()
    );

    reconcile_stale_agent_session_controls(
        &state_dir,
        project_id,
        None,
        false,
        agent_timestamp_seconds(),
    )
    .unwrap();
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Running
    );
    assert!(session_controls_suspend_project(
        &store
            .session_controls_for_project_blocking(project_id)
            .unwrap(),
    ));

    store
        .mark_session_running_blocking(
            project_id,
            "session-123",
            u32::MAX,
            "run-gone",
            &root.join("gone.out"),
            &root.join("gone.err"),
        )
        .unwrap();
    reconcile_stale_agent_session_controls(
        &state_dir,
        project_id,
        None,
        false,
        agent_timestamp_seconds(),
    )
    .unwrap();
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::ResumeRequested
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn automated_codex_recovery_resumes_the_only_linked_task_session() {
    let root = temp_root("automated-codex-session-resume");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/doing.md"),
        "# Doing Tasks\n- waiting task — BLOCKED 2026-08-25: needs input codex:session-123\n",
    )
    .unwrap();
    let mut project = tui_agent_project_for_test(1, "resume-project").project;
    project.path = root.clone();
    let mut command = Command::new("codex");

    let session_id = configure_automated_codex_subcommand(
        &mut command,
        &project,
        AgentTaskSelection::RecoverBlocked,
        None,
    )
    .unwrap();
    let args: Vec<_> = command.get_args().collect();

    assert_eq!(session_id.as_deref(), Some("session-123"));
    assert_eq!(args[0], OsStr::new("exec"));
    assert_eq!(args[1], OsStr::new("resume"));
    assert_eq!(args[2], OsStr::new("--skip-git-repo-check"));
    assert_eq!(args[3], OsStr::new("session-123"));
    assert!(args[4].to_string_lossy().contains("Blocked-task monitor:"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_codex_session_is_attached_when_a_todo_task_enters_doing() {
    let root = temp_root("live-codex-session-link");
    add_task(&root, "existing todo", None).unwrap();
    let before = task_contents_for_status(&root, TaskStatus::Doing).unwrap();
    move_task(&root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();

    assert!(
        attach_codex_session_to_active_task(
            &root,
            AgentTaskSelection::NextTodo,
            &before,
            &[],
            "session-live",
        )
        .unwrap()
    );
    let task = read_task_entries(&get_tasks_dir(&root), TaskStatus::Doing)
        .unwrap()
        .remove(0);
    assert_eq!(
        codex_session_for_task(&task).as_deref(),
        Some("session-live")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_codex_session_is_not_reattached_after_its_task_leaves_doing() {
    let root = temp_root("live-codex-session-already-moved");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/doing.md"),
        "# Doing Tasks\n- unrelated active task\n",
    )
    .unwrap();
    fs::write(
        root.join("tasks/done.md"),
        "# Done Tasks\n- handed-back task codex:session-live\n",
    )
    .unwrap();

    assert!(
        attach_codex_session_to_active_task(
            &root,
            AgentTaskSelection::ResumeSession,
            &[],
            &[],
            "session-live",
        )
        .unwrap()
    );
    let doing = read_task_entries(&get_tasks_dir(&root), TaskStatus::Doing).unwrap();
    assert_eq!(doing.len(), 1);
    assert!(codex_session_for_task(&doing[0]).is_none());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exact_session_completion_accepts_a_displaced_marker_and_leaves_todo_ready() {
    let root = temp_root("exact-session-displaced-completion-marker");
    init_tasks(&root, false).unwrap();
    let session_id = "01a03e61-21a8-7da1-956a-f50e4565123b";
    fs::write(
        root.join("tasks/done.md"),
        format!(
            "# Done Tasks\n- handed-back task codex:{session_id} \
— COMPLETED 2026-08-26: done\n"
        ),
    )
    .unwrap();
    fs::write(
        root.join("tasks/todo.md"),
        "# Todo Tasks\n- next automated task\n",
    )
    .unwrap();

    let mut project = tui_agent_project_for_test(1, "project").project;
    project.path = root.clone();
    let job = AgentRunJob {
        state_dir: root.join("state/clt"),
        project,
        holder: "holder".to_string(),
        worker_token: None,
        max_global_jobs: 12,
        task_selection: AgentTaskSelection::ResumeSession,
        resume_session_id: Some(session_id.to_string()),
        blocked_task_count_before: 0,
        done_task_contents_before: Vec::new(),
        blocked_task_snapshots_before: Vec::new(),
    };

    let done = read_task_entries(&get_tasks_dir(&root), TaskStatus::Done)
        .unwrap()
        .remove(0);
    assert_eq!(codex_session_for_task(&done).as_deref(), Some(session_id));
    attach_codex_session_after_run(&job, session_id, "success").unwrap();
    assert_eq!(
        task_status_for_codex_session(&root, session_id).unwrap(),
        Some(TaskStatus::Done)
    );
    let scan = scan_agent_project(&root);
    assert_eq!(scan.available_todo_count(), 1);
    assert!(scan.has_pending_task());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exact_session_without_its_marker_does_not_guess_an_unrelated_doing_task() {
    let root = temp_root("live-exact-session-missing-marker");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/doing.md"),
        "# Doing Tasks\n- unrelated active task\n",
    )
    .unwrap();

    assert!(
        !attach_codex_session_to_active_task(
            &root,
            AgentTaskSelection::ResumeSession,
            &[],
            &[],
            "session-live",
        )
        .unwrap()
    );
    let doing = read_task_entries(&get_tasks_dir(&root), TaskStatus::Doing).unwrap();
    assert!(codex_session_for_task(&doing[0]).is_none());

    let mut project = tui_agent_project_for_test(1, "project").project;
    project.path = root.clone();
    let job = AgentRunJob {
        state_dir: root.join("state/clt"),
        project,
        holder: "holder".to_string(),
        worker_token: None,
        max_global_jobs: 12,
        task_selection: AgentTaskSelection::ResumeSession,
        resume_session_id: Some("session-live".to_string()),
        blocked_task_count_before: 0,
        done_task_contents_before: Vec::new(),
        blocked_task_snapshots_before: Vec::new(),
    };
    assert!(attach_codex_session_after_run(&job, "session-live", "success").is_err());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn nested_session_marker_prevents_top_level_reattachment() {
    let root = temp_root("nested-live-session-marker");
    init_tasks(&root, true).unwrap();
    let epic = root.join("tasks/doing/0001-epic");
    fs::create_dir_all(&epic).unwrap();
    fs::write(epic.join("task.md"), "Parent epic.\n").unwrap();
    fs::write(
        epic.join("done.md"),
        "# Done Tasks\n- nested task codex:session-live\n",
    )
    .unwrap();
    fs::write(
        root.join("tasks/doing/0002-unrelated.md"),
        "Unrelated active task.\n",
    )
    .unwrap();

    assert_eq!(
        task_status_for_codex_session(&root, "session-live").unwrap(),
        Some(TaskStatus::Done)
    );
    assert!(
        attach_codex_session_to_active_task(
            &root,
            AgentTaskSelection::ResumeSession,
            &[],
            &[],
            "session-live",
        )
        .unwrap()
    );
    let unrelated = fs::read_to_string(root.join("tasks/doing/0002-unrelated.md")).unwrap();
    assert!(!unrelated.contains("codex:session-live"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn blocked_recovery_does_not_guess_among_multiple_pre_run_candidates() {
    let root = temp_root("blocked-live-session-ambiguous");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/doing.md"),
        "# Doing Tasks\n- blocked doing — BLOCKED 2026-08-25: waiting\n",
    )
    .unwrap();
    fs::write(
        root.join("tasks/todo.md"),
        "# Todo Tasks\n- blocked todo — BLOCKED 2026-08-25: waiting\n",
    )
    .unwrap();
    let doing_before = task_contents_for_status(&root, TaskStatus::Doing).unwrap();
    let blocked_before = blocked_task_snapshots(&root).unwrap();

    assert!(
        !attach_codex_session_to_active_task(
            &root,
            AgentTaskSelection::RecoverBlocked,
            &doing_before,
            &blocked_before,
            "session-live",
        )
        .unwrap()
    );
    let doing = read_task_entries(&get_tasks_dir(&root), TaskStatus::Doing).unwrap();
    assert!(codex_session_for_task(&doing[0]).is_none());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_session_attachment_does_not_guess_between_multiple_new_doing_tasks() {
    let root = temp_root("live-session-multiple-doing");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/doing.md"),
        "# Doing Tasks\n- first new task\n- second new task\n",
    )
    .unwrap();

    assert!(
        !attach_codex_session_to_active_task(
            &root,
            AgentTaskSelection::NextTodo,
            &[],
            &[],
            "session-live",
        )
        .unwrap()
    );
    assert!(
        !fs::read_to_string(root.join("tasks/doing.md"))
            .unwrap()
            .contains("codex:session-live")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interactive_codex_resume_accepts_doing_done_and_currently_blocked_todo_tasks() {
    let done = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 1 },
        "finished task",
        "finished task",
        false,
    );
    let blocked = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 1 },
        "waiting task",
        "waiting task — BLOCKED 2026-08-13: dependency unavailable",
        false,
    );
    let unblocked = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 1 },
        "ready again",
        "ready again — BLOCKED 2026-08-12: waiting — UNBLOCKED 2026-08-13: restored",
        false,
    );

    assert!(task_supports_interactive_codex_resume(
        TaskStatus::Done,
        &done
    ));
    assert!(task_supports_interactive_codex_resume(
        TaskStatus::Todo,
        &blocked
    ));
    assert!(task_supports_interactive_codex_resume(
        TaskStatus::Doing,
        &blocked
    ));
    assert!(task_supports_interactive_codex_resume(
        TaskStatus::Doing,
        &unblocked
    ));
    assert!(!task_supports_interactive_codex_resume(
        TaskStatus::Todo,
        &unblocked
    ));
    assert!(!task_supports_interactive_codex_resume(
        TaskStatus::Backlog,
        &blocked
    ));
}

#[test]
fn interactive_resume_revalidation_rejects_an_unblocked_or_duplicate_session_task() {
    let root = temp_root("interactive-session-task-revalidation");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/done.md"),
        "# Done Tasks\n- resumable task codex:session-123\n",
    )
    .unwrap();
    assert!(codex_session_task_supports_interactive_resume(&root, "session-123").unwrap());

    fs::write(root.join("tasks/done.md"), "# Done Tasks\n").unwrap();
    fs::write(
        root.join("tasks/doing.md"),
        "# Doing Tasks\n- interrupted task codex:session-123\n",
    )
    .unwrap();
    assert!(codex_session_task_supports_interactive_resume(&root, "session-123").unwrap());

    fs::write(root.join("tasks/doing.md"), "# Doing Tasks\n").unwrap();
    fs::write(
        root.join("tasks/todo.md"),
        "# Todo Tasks\n- now unblocked codex:session-123\n",
    )
    .unwrap();
    assert!(!codex_session_task_supports_interactive_resume(&root, "session-123").unwrap());

    fs::write(
        root.join("tasks/done.md"),
        "# Done Tasks\n- duplicate codex:session-123\n",
    )
    .unwrap();
    assert!(!codex_session_task_supports_interactive_resume(&root, "session-123").unwrap());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interactive_c_busy_check_honors_persisted_session_fences_without_a_live_log() {
    let root = temp_root("interactive-c-session-fence");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let project_id = project.id;
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::Interactive,
        )
        .unwrap();
    let mut panel = TuiAgentPanel {
        projects: vec![TuiAgentProject {
            project,
            scan: AgentProjectScan::empty(),
            runtime_state: TuiAgentRuntimeState::Idle,
            daemon_scan_problem: None,
            failure_problem: None,
        }],
        current_project_registration: None,
        daemon_status: "running".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));

    assert_eq!(
        tui_codex_session_availability_for_path_at(
            &mut panel,
            &project_root,
            "session-123",
            &state_dir,
        )
        .unwrap(),
        TuiCodexSessionAvailability::SelectedSessionBusy
    );
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    assert_eq!(
        tui_codex_session_availability_for_path_at(
            &mut panel,
            &project_root,
            "session-123",
            &state_dir,
        )
        .unwrap(),
        TuiCodexSessionAvailability::Idle
    );
    assert_eq!(
        tui_codex_session_availability_for_path_at(
            &mut panel,
            &project_root,
            "different-session",
            &state_dir,
        )
        .unwrap(),
        TuiCodexSessionAvailability::Idle
    );
    store
        .set_session_control_state_blocking(
            project_id,
            "session-other",
            AgentSessionControlState::Running,
        )
        .unwrap();
    assert_eq!(
        tui_codex_session_availability_for_path_at(
            &mut panel,
            &project_root,
            "session-123",
            &state_dir,
        )
        .unwrap(),
        TuiCodexSessionAvailability::ProjectBusy
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn writable_shared_interactive_reservation_coexists_with_another_active_session() {
    let root = temp_root("writable-shared-interactive-reservation");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let project_id = project.id;
    let active_holder = "clt-worker-active";
    assert!(
        store
            .try_acquire_lease_blocking(project_id, active_holder, "100", "9999999999")
            .unwrap()
    );
    store
        .set_session_control_state_blocking(
            project_id,
            "session-active",
            AgentSessionControlState::Running,
        )
        .unwrap();

    let requester = InteractiveAgentLease::holder_for_shared_session(false);
    assert!(
        store
            .reserve_shared_session_interactive_blocking(
                project_id,
                "session-blocked",
                &requester,
                None,
            )
            .unwrap()
    );
    assert_eq!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap()
            .holder,
        active_holder
    );

    let guardian =
        interactive_guardian_holder(InteractiveGuardianDisposition::PreserveSharedSession);
    assert!(
        store
            .adopt_interactive_guardian_blocking(
                project_id,
                Some("session-blocked"),
                &requester,
                &guardian,
                60,
            )
            .unwrap()
    );
    assert!(
        store
            .register_interactive_guardian_child_blocking(
                project_id,
                "session-blocked",
                &guardian,
                std::process::id(),
                60,
            )
            .unwrap()
    );
    let message =
        toggle_tui_codex_session_stop_at(&state_dir, project_id, "session-blocked").unwrap();
    assert!(message.starts_with("Stopping this interactive Codex session safely"));
    let stopping = store
        .session_control_blocking(project_id, "session-blocked")
        .unwrap()
        .unwrap();
    assert_eq!(stopping.state, AgentSessionControlState::StopRequested);
    assert!(interactive_guardian_stop_requested(
        &stopping,
        std::process::id()
    ));
    let mut panel = TuiAgentPanel {
        projects: vec![TuiAgentProject {
            project,
            scan: AgentProjectScan::empty(),
            runtime_state: TuiAgentRuntimeState::Interactive,
            daemon_scan_problem: None,
            failure_problem: None,
        }],
        current_project_registration: None,
        daemon_status: "running".to_string(),
        state: ListState::default(),
        scroll_offset: 0,
        last_error: None,
    };
    panel.state.select(Some(0));
    assert_eq!(
        selected_tui_agent_session_target_at(&panel, &state_dir)
            .unwrap()
            .session_id,
        "session-blocked"
    );
    assert!(
        store
            .finish_interactive_guardian_blocking(
                project_id,
                "session-blocked",
                &guardian,
                InteractiveGuardianDisposition::PreserveSharedSession,
            )
            .unwrap()
    );
    let preserved = store
        .session_control_blocking(project_id, "session-blocked")
        .unwrap()
        .unwrap();
    assert_eq!(preserved.state, AgentSessionControlState::Stopped);
    assert!(preserved.interactive_holder.is_none());
    assert_eq!(
        store
            .session_control_blocking(project_id, "session-active")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Running
    );
    assert_eq!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap()
            .holder,
        active_holder
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn writable_shared_interactive_reservation_rejects_durable_git_boundaries() {
    let root = temp_root("writable-shared-interactive-git-boundaries");
    let state_dir = root.join("state/clt");
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let requester = InteractiveAgentLease::holder_for_shared_session(false);

    let launch_root = root.join("launch-project");
    fs::create_dir_all(&launch_root).unwrap();
    let launch_root = fs::canonicalize(launch_root).unwrap();
    store
        .register_project_blocking(&launch_root, "launch-project")
        .unwrap();
    let launch_project = store
        .list_projects_blocking()
        .unwrap()
        .into_iter()
        .find(|project| project.path == launch_root)
        .unwrap();
    store
        .set_session_control_state_blocking(
            launch_project.id,
            "session-active-launch",
            AgentSessionControlState::Running,
        )
        .unwrap();
    assert!(
            store
                .record_git_launch_state_blocking(
                    launch_project.id,
                    "launch-boundary-token",
                    AgentGitMode::Commit,
                    &AgentGitStartState {
                        starting_head: "1111111111111111111111111111111111111111".to_string(),
                        branch_ref: Some("refs/heads/master".to_string()),
                        upstream_ref: None,
                        worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#.to_string(),
                    },
                    "100",
                )
                .unwrap()
        );
    assert!(
        !store
            .reserve_shared_session_interactive_blocking(
                launch_project.id,
                "session-shared-launch",
                &requester,
                None,
            )
            .unwrap()
    );

    let finalization_root = root.join("finalization-project");
    fs::create_dir_all(&finalization_root).unwrap();
    let finalization_root = fs::canonicalize(finalization_root).unwrap();
    store
        .register_project_blocking(&finalization_root, "finalization-project")
        .unwrap();
    let finalization_project = store
        .list_projects_blocking()
        .unwrap()
        .into_iter()
        .find(|project| project.path == finalization_root)
        .unwrap();
    store
        .set_session_control_state_blocking(
            finalization_project.id,
            "session-active-finalization",
            AgentSessionControlState::Running,
        )
        .unwrap();
    assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: finalization_project.id,
                    codex_session_id: "session-working-finalization",
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
    assert!(
        !store
            .reserve_shared_session_interactive_blocking(
                finalization_project.id,
                "session-shared-finalization",
                &requester,
                None,
            )
            .unwrap()
    );

    let lease_root = root.join("lease-project");
    fs::create_dir_all(&lease_root).unwrap();
    let lease_root = fs::canonicalize(lease_root).unwrap();
    store
        .register_project_blocking(&lease_root, "lease-project")
        .unwrap();
    assert!(
        store
            .set_project_git_mode_for_path_blocking(&lease_root, AgentGitMode::Commit)
            .unwrap()
    );
    let lease_project = store
        .list_projects_blocking()
        .unwrap()
        .into_iter()
        .find(|project| project.path == lease_root)
        .unwrap();
    assert!(
        store
            .try_acquire_lease_blocking(lease_project.id, "git-owner", "100", "999")
            .unwrap()
    );
    assert!(
        !store
            .reserve_shared_session_interactive_blocking(
                lease_project.id,
                "session-shared-lease",
                &requester,
                None,
            )
            .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancelling_shared_resume_restores_a_stopped_session() {
    let root = temp_root("shared-stopped-reservation");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .set_session_control_state_blocking(
            project_id,
            "session-active",
            AgentSessionControlState::Running,
        )
        .unwrap();
    store
        .set_session_control_state_blocking(
            project_id,
            "session-stopped",
            AgentSessionControlState::Stopped,
        )
        .unwrap();

    let requester = InteractiveAgentLease::holder_for_shared_session(true);
    assert!(
        store
            .reserve_shared_session_interactive_blocking(
                project_id,
                "session-stopped",
                &requester,
                None,
            )
            .unwrap()
    );
    assert!(
        store
            .cancel_idle_session_interactive_blocking(project_id, "session-stopped", &requester,)
            .unwrap()
    );
    let stopped = store
        .session_control_blocking(project_id, "session-stopped")
        .unwrap()
        .unwrap();
    assert_eq!(stopped.state, AgentSessionControlState::Stopped);
    assert!(stopped.interactive_holder.is_none());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn newly_added_task_is_not_guessed_when_duplicate_content_is_ambiguous() {
    let before = vec!["duplicate task".to_string(), "older task".to_string()];
    let after = vec![
        task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 1 },
            "duplicate task",
            "duplicate task",
            false,
        ),
        task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 2 },
            "duplicate task",
            "duplicate task",
            false,
        ),
        task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 3 },
            "older task",
            "older task",
            false,
        ),
    ];

    assert!(newly_added_task_entry(&before, &after).is_none());
}
