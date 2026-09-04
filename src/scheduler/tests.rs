use crate::runner::tests::FakeAgentRunner;
use crate::test_support::prelude::*;
use crate::test_support::*;

#[test]
fn agent_scheduling_decision_stage_is_pure_and_prioritized() {
    let decide = |has_resume_session,
                  resume_interrupted_task,
                  has_blocked_task,
                  blocked_recovery_backoff_active,
                  has_pending_task| {
        decide_agent_scheduling_stage(AgentSchedulingDecisionRequest {
            has_resume_session,
            resume_interrupted_task,
            has_blocked_task,
            blocked_recovery_backoff_active,
            has_pending_task,
        })
        .task_selection
    };

    assert_eq!(
        decide(true, true, true, false, true),
        Some(AgentTaskSelection::ResumeSession)
    );
    assert_eq!(
        decide(false, true, true, false, true),
        Some(AgentTaskSelection::ResumeDoing)
    );
    assert_eq!(
        decide(false, false, true, false, true),
        Some(AgentTaskSelection::RecoverBlocked)
    );
    assert_eq!(
        decide(false, false, true, true, true),
        Some(AgentTaskSelection::NextTodo)
    );
    assert_eq!(decide(false, false, false, false, false), None);
}

#[test]
fn agent_supervision_outcome_stage_classifies_without_side_effects() {
    let success = Command::new("true").status().unwrap();
    let idle = classify_agent_supervision_stage(AgentSupervisionOutcomeRequest {
        wait_result: AgentProcessWait::Exited(success),
        requested_control: None,
        reported_no_tasks: true,
        timeout: Duration::from_secs(9),
    });
    assert_eq!(idle.status, "idle");
    assert_eq!(idle.summary, "Codex reported no available tasks.");
    assert_eq!(idle.stderr_note, None);

    let timeout = classify_agent_supervision_stage(AgentSupervisionOutcomeRequest {
        wait_result: AgentProcessWait::TimedOut(None),
        requested_control: None,
        reported_no_tasks: false,
        timeout: Duration::from_secs(9),
    });
    assert_eq!(timeout.status, "timeout");
    assert_eq!(
        timeout.stderr_note.as_deref(),
        Some("Codex timed out after 9 seconds.")
    );
}

#[test]
fn agent_scan_detects_markdown_backed_pending_tasks() {
    let root = temp_root("agent-scan-markdown");
    add_task(&root, "agent should run", None).unwrap();
    add_task(&root, "agent is running this", None).unwrap();
    move_task(&root, TaskStatus::Todo, TaskStatus::Doing, "2").unwrap();

    let scan = scan_agent_project(&root);

    assert_eq!(scan.status, AgentProjectScanStatus::Pending);
    assert_eq!(scan.todo_count, 1);
    assert_eq!(scan.doing_count, 1);
    assert!(has_pending_agent_task(&root));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_detects_folder_backed_pending_tasks() {
    let root = temp_root("agent-scan-folder");
    init_tasks(&root, true).unwrap();
    fs::write(
        root.join("tasks/todo/0010-write-agent-runner.md"),
        "Write agent runner. Include tests.\n",
    )
    .unwrap();

    let scan = scan_agent_project(&root);

    assert_eq!(scan.status, AgentProjectScanStatus::Pending);
    assert_eq!(scan.todo_count, 1);
    assert_eq!(scan.doing_count, 0);
    assert!(has_pending_agent_task(&root));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_only_reports_blocked_when_every_doing_task_has_a_blocked_note() {
    let root = temp_root("agent-scan-blocked-markdown");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/doing.md"),
        "# Doing Tasks\n- first task — BLOCKED 2026-08-09: waiting on a fixture\n- second task\n",
    )
    .unwrap();

    let partially_blocked = scan_agent_project(&root);

    assert_eq!(partially_blocked.status, AgentProjectScanStatus::Empty);
    assert_eq!(partially_blocked.doing_count, 2);
    assert_eq!(partially_blocked.blocked_doing_count, 1);
    assert!(!partially_blocked.all_actionable_tasks_blocked());

    fs::write(
            root.join("tasks/doing.md"),
            "# Doing Tasks\n- first task — BLOCKED 2026-08-09: waiting on a fixture\n- second task — blocked 2026-08-09: same fixture\n",
        )
        .unwrap();

    let all_blocked = scan_agent_project(&root);

    assert_eq!(all_blocked.status, AgentProjectScanStatus::Blocked);
    assert_eq!(all_blocked.blocked_doing_count, 2);
    assert!(all_blocked.all_actionable_tasks_blocked());
    assert!(all_blocked.has_schedulable_work());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_skips_blocked_todos_until_a_newer_note_unblocks_them() {
    let root = temp_root("agent-scan-blocked-todo");
    init_tasks(&root, false).unwrap();
    fs::write(
            root.join("tasks/todo.md"),
            "# Todo Tasks\n- first — BLOCKED 2026-08-09: dependency unavailable\n- second — BLOCKED 2026-08-09: fixture unavailable\n",
        )
        .unwrap();

    let all_blocked = scan_agent_project(&root);

    assert_eq!(all_blocked.status, AgentProjectScanStatus::Blocked);
    assert_eq!(all_blocked.todo_count, 2);
    assert_eq!(all_blocked.blocked_todo_count, 2);
    assert_eq!(all_blocked.available_todo_count(), 0);
    assert!(!all_blocked.has_pending_task());
    assert!(all_blocked.all_actionable_tasks_blocked());

    fs::write(
            root.join("tasks/todo.md"),
            "# Todo Tasks\n- first — BLOCKED 2026-08-09: dependency unavailable — UNBLOCKED 2026-08-09: dependency restored\n- second — BLOCKED 2026-08-09: fixture unavailable\n",
        )
        .unwrap();

    let one_available = scan_agent_project(&root);

    assert_eq!(one_available.status, AgentProjectScanStatus::Pending);
    assert_eq!(one_available.todo_count, 2);
    assert_eq!(one_available.blocked_todo_count, 1);
    assert_eq!(one_available.available_todo_count(), 1);
    assert!(one_available.has_pending_task());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_detects_folder_task_blocked_note_headings_without_matching_titles() {
    let root = temp_root("agent-scan-blocked-folder");
    init_tasks(&root, true).unwrap();
    fs::write(
        root.join("tasks/doing/0001-waiting.md"),
        "Waiting task.\n\nBlocked note:\n- BLOCKED 2026-08-09: dependency unavailable.\n",
    )
    .unwrap();

    let blocked = scan_agent_project(&root);

    assert_eq!(blocked.status, AgentProjectScanStatus::Blocked);
    assert_eq!(blocked.blocked_doing_count, 1);

    fs::write(
        root.join("tasks/doing/0001-waiting.md"),
        "Add blocked-task monitoring without a blocker note.\n",
    )
    .unwrap();

    let title_only = scan_agent_project(&root);

    assert_eq!(title_only.status, AgentProjectScanStatus::Empty);
    assert_eq!(title_only.blocked_doing_count, 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_does_not_treat_backlog_as_actionable_work() {
    let root = temp_root("agent-scan-backlog");
    add_task(&root, "not ready for an agent", None).unwrap();
    move_task(&root, TaskStatus::Todo, TaskStatus::Backlog, "1").unwrap();

    let scan = scan_agent_project(&root);

    assert_eq!(scan.status, AgentProjectScanStatus::Empty);
    assert_eq!(scan.todo_count, 0);
    assert_eq!(scan.doing_count, 0);
    assert!(!has_pending_agent_task(&root));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_reports_empty_missing_uninitialized_and_unavailable_projects() {
    let root = temp_root("agent-scan-states");
    let empty_project = root.join("empty");
    init_tasks(&empty_project, false).unwrap();

    let uninitialized_project = root.join("uninitialized");
    fs::create_dir_all(&uninitialized_project).unwrap();

    let unreadable_project = root.join("unavailable");
    fs::create_dir_all(unreadable_project.join("tasks")).unwrap();
    fs::write(unreadable_project.join("tasks/todo.md"), [0xff, 0xfe]).unwrap();
    fs::write(unreadable_project.join("tasks/doing.md"), "# Doing Tasks\n").unwrap();
    fs::write(unreadable_project.join("tasks/done.md"), "# Done Tasks\n").unwrap();

    assert_eq!(
        scan_agent_project(&empty_project),
        AgentProjectScan::empty()
    );
    assert_eq!(
        scan_agent_project(&root.join("missing")),
        AgentProjectScan::missing()
    );
    assert_eq!(
        scan_agent_project(&uninitialized_project),
        AgentProjectScan::uninitialized()
    );

    let scan = scan_agent_project(&unreadable_project);
    assert_eq!(scan.todo_count, 0);
    assert!(matches!(
        scan.status,
        AgentProjectScanStatus::Unavailable(_)
    ));
    assert!(!has_pending_agent_task(&unreadable_project));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_records_project_scan_timestamp() {
    let root = temp_root("agent-scan-store");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();

    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.last_scan_at, None);

    let scanned_at = store.record_project_scan_blocking(project.id).unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);

    assert_eq!(project.last_scan_at, Some(scanned_at));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_records_and_clears_daemon_scan_errors() {
    let root = temp_root("agent-daemon-scan-store");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap()[0].id;

    store
        .record_project_daemon_scan_blocking(
            project_id,
            "unavailable",
            Some("Operation not permitted (os error 1)"),
        )
        .unwrap();
    let failed = store.list_projects_blocking().unwrap().remove(0);

    assert_eq!(
        failed.last_daemon_scan_status.as_deref(),
        Some("unavailable")
    );
    assert_eq!(
        failed.last_daemon_scan_error.as_deref(),
        Some("Operation not permitted (os error 1)")
    );

    store
        .record_project_daemon_scan_blocking(project_id, "pending", None)
        .unwrap();
    let recovered = store.list_projects_blocking().unwrap().remove(0);

    assert_eq!(
        recovered.last_daemon_scan_status.as_deref(),
        Some("pending")
    );
    assert_eq!(recovered.last_daemon_scan_error, None);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scheduler_persists_unavailable_scan_for_the_ui() {
    let root = temp_root("agent-scheduler-scan-error");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    let tasks_dir = project_root.join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(tasks_dir.join("todo.md"), [0xff, 0xfe]).unwrap();
    fs::write(tasks_dir.join("doing.md"), "# Doing Tasks\n").unwrap();
    fs::write(tasks_dir.join("done.md"), "# Done Tasks\n").unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();

    let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);

    assert_eq!(start.pass.pending_projects, 0);
    assert_eq!(
        project.last_daemon_scan_status.as_deref(),
        Some("unavailable")
    );
    assert!(project.last_daemon_scan_error.is_some());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_records_and_clears_daemon_checkins() {
    let root = temp_root("agent-daemon-checkin-store");
    let state_dir = root.join("state/clt");
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    store
        .record_daemon_checkin_blocking("clt-agent-1", "cli", "100", "110", "155")
        .unwrap();
    let checkins = store.list_daemon_checkins_blocking().unwrap();

    assert_eq!(checkins.len(), 1);
    assert_eq!(checkins[0].holder, "clt-agent-1");
    assert_eq!(checkins[0].mode, "cli");
    assert_eq!(checkins[0].started_at, "100");
    assert_eq!(checkins[0].checked_in_at, "110");
    assert_eq!(checkins[0].expires_at, "155");

    assert!(store.clear_daemon_checkin_blocking("clt-agent-1").unwrap());
    assert!(store.list_daemon_checkins_blocking().unwrap().is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_runtime_status_prefers_fresh_checkins() {
    let fresh_cli = agent::AgentDaemonCheckin {
        holder: "clt-agent-1".to_string(),
        mode: "cli".to_string(),
        started_at: "100".to_string(),
        checked_in_at: "120".to_string(),
        expires_at: "200".to_string(),
    };
    let fresh_service = agent::AgentDaemonCheckin {
        holder: "clt-agent-2".to_string(),
        mode: "service".to_string(),
        started_at: "100".to_string(),
        checked_in_at: "120".to_string(),
        expires_at: "200".to_string(),
    };
    let stale_cli = agent::AgentDaemonCheckin {
        expires_at: "150".to_string(),
        ..fresh_cli.clone()
    };

    assert_eq!(
        format_agent_daemon_runtime_status("installed", std::slice::from_ref(&fresh_cli), 160,),
        "cli active"
    );
    assert_eq!(
        format_agent_daemon_runtime_status("running", &[fresh_cli], 160),
        "cli active; service no-check-in"
    );
    assert_eq!(
        format_agent_daemon_runtime_status("running", &[fresh_service], 160),
        "service active"
    );
    assert_eq!(
        format_agent_daemon_runtime_status("installed", &[stale_cli], 160),
        "cli stale"
    );
    assert_eq!(
        format_agent_daemon_runtime_status("installed", &[], 160),
        "service disabled"
    );
    assert_eq!(
        format_agent_daemon_runtime_status("not-installed", &[], 160),
        "disabled"
    );
}

#[test]
fn agent_service_restart_requires_running_service_with_only_stale_service_checkins() {
    let stale_service = agent::AgentDaemonCheckin {
        holder: "clt-agent-1".to_string(),
        mode: "service".to_string(),
        started_at: "100".to_string(),
        checked_in_at: "120".to_string(),
        expires_at: "150".to_string(),
    };
    let fresh_service = agent::AgentDaemonCheckin {
        holder: "clt-agent-2".to_string(),
        expires_at: "200".to_string(),
        ..stale_service.clone()
    };
    let stale_cli = agent::AgentDaemonCheckin {
        mode: "cli".to_string(),
        ..stale_service.clone()
    };

    assert!(agent_service_needs_restart(
        "running",
        std::slice::from_ref(&stale_service),
        160
    ));
    assert!(agent_service_needs_restart(
        "running",
        &[stale_service.clone(), stale_cli.clone()],
        160
    ));
    assert!(!agent_service_needs_restart(
        "installed",
        std::slice::from_ref(&stale_service),
        160
    ));
    assert!(!agent_service_needs_restart(
        "running",
        &[stale_service, fresh_service],
        160
    ));
    assert!(!agent_service_needs_restart("running", &[stale_cli], 160));
}

#[test]
fn agent_run_once_records_pending_projects_up_to_default_capacity() {
    let root = temp_root("agent-run-once");
    let state_dir = root.join("state/clt");
    let first_project = root.join("alpha");
    let second_project = root.join("beta");
    init_tasks(&first_project, false).unwrap();
    init_tasks(&second_project, false).unwrap();
    add_task(&first_project, "first task", None).unwrap();
    add_task(&second_project, "second task", None).unwrap();
    let first_project = fs::canonicalize(first_project).unwrap();
    let second_project = fs::canonicalize(second_project).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&first_project, "alpha")
        .unwrap();
    store
        .register_project_blocking(&second_project, "beta")
        .unwrap();
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        pass,
        AgentSchedulerPass {
            scanned_projects: 2,
            pending_projects: 2,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 2,
            runs_recorded: 2,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 2);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_loop_repeats_passes_and_respects_success_cooldown() {
    let root = temp_root("agent-daemon-loop");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "daemon task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let runner = Arc::new(FakeAgentRunner::new(&state_dir, "success"));
    drop(store);

    let daemon_runner: Arc<dyn AgentRunner> = runner.clone();
    run_agent_daemon_loop(&state_dir, daemon_runner, Duration::ZERO, Some(2)).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_dispatches_independent_worker_outside_async_runtime() {
    let root = temp_root("agent-daemon-independent-dispatch");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "independent daemon task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    drop(store);

    run_agent_daemon_loop_with_executor(
        &state_dir,
        AgentDaemonExecutor::Independent {
            executable: PathBuf::from("/tmp/pinned-clt-generation"),
            dispatch: dispatch_independent_agent_worker_without_service,
        },
        Duration::ZERO,
        Some(1),
        new_agent_shutdown_signal(),
    )
    .unwrap();

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let workers = store.list_active_workers_blocking().unwrap();
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].project_id, project_id);
    assert_eq!(workers[0].state, AGENT_WORKER_STATE_DISPATCHING);
    assert_eq!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap()
            .holder,
        workers[0].lease_holder
    );
    assert_eq!(store.run_count_blocking().unwrap(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_loop_polls_while_run_is_active_without_reclaiming_own_lease() {
    let root = temp_root("agent-daemon-active-poll");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "long daemon task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let runner = Arc::new(FakeAgentRunner::with_delay(
        &state_dir,
        "success",
        Duration::from_millis(75),
    ));
    drop(store);

    let daemon_runner: Arc<dyn AgentRunner> = runner.clone();
    run_agent_daemon_loop(&state_dir, daemon_runner, Duration::from_millis(5), Some(2)).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_scheduler_returns_acquired_jobs_before_recording_runs() {
    let root = temp_root("agent-daemon-start-job");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "async daemon task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let mut start = run_agent_daemon_scheduler_pass(&state_dir).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        start.pass,
        AgentSchedulerPass {
            scanned_projects: 1,
            pending_projects: 1,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 1,
            runs_recorded: 0,
        }
    );
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(store.lease_count_blocking().unwrap(), 1);
    drop(store);

    let blocked_by_lease = run_agent_daemon_scheduler_pass(&state_dir).unwrap();
    assert_eq!(blocked_by_lease.jobs.len(), 0);
    assert_eq!(blocked_by_lease.pass.skipped_active_lease, 1);

    let runner = FakeAgentRunner::new(&state_dir, "success");
    let shutdown = new_agent_shutdown_signal();
    let completion = run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(completion.status, "success");
    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_scheduler_records_checkin_with_registry_lookup() {
    let root = temp_root("agent-daemon-checkin");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "scheduled task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let checkin = AgentDaemonCheckinSource {
        holder: "clt-scheduler-test".to_string(),
        mode: "cli".to_string(),
        started_at: "100".to_string(),
    };
    let start = run_agent_daemon_scheduler_pass_with_active_and_checkin(
        &state_dir,
        Vec::new(),
        Some(&checkin),
    )
    .unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let checkins = store.list_daemon_checkins_blocking().unwrap();

    assert_eq!(start.pass.scanned_projects, 1);
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(start.jobs[0].holder, checkin.holder);
    assert_eq!(checkins.len(), 1);
    assert_eq!(checkins[0].mode, "cli");
    assert!(daemon_checkin_is_fresh(
        &checkins[0],
        agent_timestamp_seconds()
    ));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_scheduler_defers_pending_projects_when_active_jobs_fill_capacity() {
    let root = temp_root("agent-daemon-active-capacity");
    let state_dir = root.join("state/clt");
    let first_project = root.join("alpha");
    let second_project = root.join("beta");
    init_tasks(&first_project, false).unwrap();
    init_tasks(&second_project, false).unwrap();
    add_task(&first_project, "active task", None).unwrap();
    add_task(&second_project, "deferred task", None).unwrap();
    let first_project = fs::canonicalize(first_project).unwrap();
    let second_project = fs::canonicalize(second_project).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&first_project, "alpha")
        .unwrap();
    store
        .register_project_blocking(&second_project, "beta")
        .unwrap();
    let projects = store.list_projects_blocking().unwrap();
    let active_project = projects
        .iter()
        .find(|project| project.name == "alpha")
        .unwrap();
    assert!(
        store
            .try_acquire_lease_blocking(active_project.id, "active-daemon-run", "100", "9999999999")
            .unwrap()
    );
    let active_project_id = active_project.id;
    drop(store);

    let start = run_agent_scheduler_pass_with_max_global_jobs(
        &state_dir,
        false,
        &[active_project_id],
        1,
        None,
    )
    .unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(start.jobs.len(), 0);
    assert_eq!(
        start.pass,
        AgentSchedulerPass {
            scanned_projects: 2,
            pending_projects: 1,
            active_agent_jobs: 1,
            skipped_active_lease: 0,
            deferred_projects: 1,
            runs_started: 0,
            runs_recorded: 0,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(store.lease_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_run_once_skips_disabled_projects_but_reconciles_abandoned_handoffs() {
    let root = temp_root("agent-run-disabled");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "disabled task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .set_session_control_state_blocking(
            project.id,
            "session-123",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    assert!(
        store
            .begin_stopped_session_interactive_blocking(
                project.id,
                "session-123",
                "clt-interactive-4294967295",
                None,
            )
            .unwrap()
    );
    store
        .set_project_enabled_blocking(project.id, false)
        .unwrap();
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        pass,
        AgentSchedulerPass {
            scanned_projects: 0,
            pending_projects: 0,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 0,
            runs_recorded: 0,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 0);
    assert_eq!(
        store
            .session_control_blocking(project.id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::ResumeRequested
    );

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn agent_run_once_reclaims_dead_lease_for_disabled_project() {
    let root = temp_root("agent-run-disabled-dead-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "disabled task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "clt-agent-4294967295", "100", "9999999999",)
            .unwrap()
    );
    store
        .set_project_enabled_blocking(project.id, false)
        .unwrap();
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(pass.scanned_projects, 0);
    assert_eq!(pass.runs_started, 0);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn agent_run_once_reclaims_dead_orphaned_interactive_lease_before_expiry() {
    let root = temp_root("agent-run-disabled-dead-interactive-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "disabled task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let holder = format!("clt-idle-interactive-worker-{}-1-1", u32::MAX);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, &holder, "100", "9999999999",)
            .unwrap()
    );
    store
        .set_project_enabled_blocking(project.id, false)
        .unwrap();
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(pass.scanned_projects, 0);
    assert_eq!(pass.runs_started, 0);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_run_once_skips_projects_with_active_lease() {
    let root = temp_root("agent-run-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "leased task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "test-holder", "100", "9999999999")
            .unwrap()
    );
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        pass,
        AgentSchedulerPass {
            scanned_projects: 1,
            pending_projects: 1,
            active_agent_jobs: 0,
            skipped_active_lease: 1,
            deferred_projects: 0,
            runs_started: 0,
            runs_recorded: 0,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(store.lease_count_blocking().unwrap(), 1);
    assert_eq!(runner.ran_project_count(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_run_once_ignores_legacy_local_lock_directory() {
    let root = temp_root("agent-run-local-lock");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "locally locked task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    fs::create_dir(project_root.join(".codex-task-loop.lock")).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        pass,
        AgentSchedulerPass {
            scanned_projects: 1,
            pending_projects: 1,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 1,
            runs_recorded: 1,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn agent_run_once_reclaims_dead_local_process_lease() {
    let root = temp_root("agent-run-dead-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "dead leased task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "clt-agent-4294967295", "100", "9999999999")
            .unwrap()
    );
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        pass,
        AgentSchedulerPass {
            scanned_projects: 1,
            pending_projects: 1,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 1,
            runs_recorded: 1,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn agent_scheduler_resumes_doing_task_after_crashed_process() {
    let root = temp_root("agent-resume-doing-dead-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "interrupted task", None).unwrap();
    move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "clt-agent-4294967295", "100", "9999999999")
            .unwrap()
    );
    drop(store);

    let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    assert_eq!(start.pass.pending_projects, 1);
    assert_eq!(start.pass.runs_started, 1);
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::ResumeDoing
    );

    let runner = FakeAgentRunner::new(&state_dir, "success");
    let shutdown = new_agent_shutdown_signal();
    run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(runner.ran_project_count(), 1);
    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scheduler_resumes_doing_task_after_lease_expiry() {
    let root = temp_root("agent-resume-doing-expired-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "expired interrupted task", None).unwrap();
    move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "old-holder", "100", "101")
            .unwrap()
    );
    drop(store);

    let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    assert_eq!(start.pass.pending_projects, 1);
    assert_eq!(start.pass.runs_started, 1);
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::ResumeDoing
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scheduler_monitors_when_all_todo_and_doing_tasks_are_blocked() {
    let root = temp_root("agent-recover-blocked");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- queued — BLOCKED 2026-08-09: credentials unavailable\n",
    )
    .unwrap();
    fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- first — BLOCKED 2026-08-09: dependency unavailable\n- second — BLOCKED 2026-08-09: tests cannot start\n",
        )
        .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    assert_eq!(start.pass.pending_projects, 1);
    assert_eq!(start.pass.runs_started, 1);
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::RecoverBlocked
    );
    assert_eq!(start.jobs[0].blocked_task_count_before, 3);

    let runner = FakeAgentRunner::new(&state_dir, "success");
    let shutdown = new_agent_shutdown_signal();
    let completion = run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();

    assert_eq!(completion.status, "blocked");
    assert!(
        completion
            .summary
            .contains("left 3 blocked task(s) unresolved across todo and doing")
    );

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(project.last_blocked_recovery_at.is_some());
    assert_eq!(project.failure_count, 0);
    drop(store);

    let backed_off = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert!(backed_off.jobs.is_empty());
    assert_eq!(backed_off.pass.runs_started, 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scheduler_rechecks_blocked_work_before_todo_then_uses_backoff() {
    let root = temp_root("agent-blocked-before-todo");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- queued blocker — BLOCKED 2026-08-09: dependency unavailable\n- ready task\n",
        )
        .unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- waiting task — BLOCKED 2026-08-09: dependency unavailable\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    assert_eq!(start.jobs.len(), 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::RecoverBlocked
    );
    assert_eq!(start.jobs[0].blocked_task_count_before, 2);

    let runner = FakeAgentRunner::new(&state_dir, "success");
    let shutdown = new_agent_shutdown_signal();
    let completion = run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();
    assert_eq!(completion.status, "blocked");
    assert!(
        completion
            .summary
            .contains("left 2 blocked task(s) unresolved")
    );

    let during_backoff = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(during_backoff.jobs.len(), 1);
    assert_eq!(
        during_backoff.jobs[0].task_selection,
        AgentTaskSelection::NextTodo
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scheduler_recovers_blocked_work_without_taking_unblocked_doing() {
    let root = temp_root("agent-unblocked-doing");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- queued blocker — BLOCKED 2026-08-09: dependency unavailable\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- manually active task\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    assert_eq!(start.jobs.len(), 1);
    assert_eq!(start.pass.pending_projects, 1);
    assert_eq!(start.pass.runs_started, 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::RecoverBlocked
    );

    fs::remove_dir_all(root).unwrap();
}
#[test]
fn agent_lease_holder_liveness_detects_current_process_holder() {
    let holder = agent_lease_holder();

    assert_eq!(agent_lease_holder_pid(&holder), Some(std::process::id()));
    assert_eq!(
        agent_lease_holder_liveness(&holder),
        AgentLeaseHolderLiveness::CurrentProcess
    );
    let interactive_holder = format!("clt-interactive-{}", std::process::id());
    assert_eq!(
        agent_lease_holder_pid(&interactive_holder),
        Some(std::process::id())
    );
    assert_eq!(
        agent_lease_holder_liveness(&agent_scheduler_lease_holder()),
        AgentLeaseHolderLiveness::CurrentProcess
    );
    assert_eq!(agent_lease_holder_pid("external-agent"), None);
}
#[test]
fn generated_interactive_holders_are_reclaimed_by_full_token_ttl_not_pid() {
    for holder in [
        InteractiveAgentLease::holder_for_current_process(),
        InteractiveAgentLease::holder_for_idle_session(),
        InteractiveAgentLease::holder_for_stopped_session(),
        interactive_guardian_holder(InteractiveGuardianDisposition::ResumeExec),
        interactive_guardian_holder(InteractiveGuardianDisposition::PreserveIdleSession),
        interactive_guardian_holder(InteractiveGuardianDisposition::RestoreStopped),
    ] {
        assert_eq!(agent_lease_holder_pid(&holder), None);
        assert_eq!(
            interactive_lease_holder_pid(&holder),
            Some(std::process::id())
        );
        assert_eq!(
            interactive_lease_holder_liveness(&holder),
            Some(AgentLeaseHolderLiveness::CurrentProcess)
        );
        let lease = agent::AgentLeaseRecord {
            project_id: 1,
            project_name: "project".to_string(),
            project_path: PathBuf::from("/tmp/project"),
            holder,
            acquired_at: "100".to_string(),
            expires_at: "200".to_string(),
        };

        assert!(!agent_lease_is_reclaimable(&lease, true, 199));
        assert!(agent_lease_is_reclaimable(&lease, false, 200));
    }
}

#[test]
fn agent_backoff_duration_requires_positive_integer() {
    assert_eq!(
        parse_agent_timeout_duration(AGENT_FAILURE_BACKOFF_SECONDS_ENV, "300").unwrap(),
        300
    );
    assert_eq!(
        parse_agent_timeout_duration(AGENT_SUCCESS_COOLDOWN_SECONDS_ENV, "5").unwrap(),
        5
    );
    assert!(parse_agent_timeout_duration(AGENT_FAILURE_BACKOFF_SECONDS_ENV, "0").is_err());
    assert!(parse_agent_timeout_duration(AGENT_SUCCESS_COOLDOWN_SECONDS_ENV, "soon").is_err());
}

#[test]
fn agent_project_cooldown_reason_reports_success_and_failure_delays() {
    let mut project = agent::AgentProject {
        id: 1,
        path: PathBuf::from("/tmp/project"),
        name: "project".to_string(),
        enabled: true,
        git_mode: AgentGitMode::Off,
        codex_provider: None,
        codex_model: None,
        codex_reasoning_effort: None,
        codex_fast_enabled: false,
        last_scan_at: None,
        last_daemon_scan_status: None,
        last_daemon_scan_error: None,
        last_run_at: None,
        last_success_at: Some("100".to_string()),
        last_failure_at: None,
        last_blocked_recovery_at: None,
        failure_count: 0,
    };

    assert_eq!(
        agent_project_cooldown_reason(
            &project,
            102,
            Duration::from_secs(5),
            Duration::from_secs(300)
        ),
        Some("success cooldown active for 3s".to_string())
    );

    project.last_failure_at = Some("100".to_string());
    project.failure_count = 1;

    assert_eq!(
        agent_project_cooldown_reason(
            &project,
            250,
            Duration::from_secs(5),
            Duration::from_secs(300)
        ),
        Some("failure backoff active for 150s".to_string())
    );
}

#[test]
fn blocked_task_recovery_uses_failure_backoff_without_delaying_todo_work() {
    let project = agent::AgentProject {
        id: 1,
        path: PathBuf::from("/tmp/project"),
        name: "project".to_string(),
        enabled: true,
        git_mode: AgentGitMode::Off,
        codex_provider: None,
        codex_model: None,
        codex_reasoning_effort: None,
        codex_fast_enabled: false,
        last_scan_at: None,
        last_daemon_scan_status: None,
        last_daemon_scan_error: None,
        last_run_at: Some("100".to_string()),
        last_success_at: None,
        last_failure_at: None,
        last_blocked_recovery_at: Some("100".to_string()),
        failure_count: 0,
    };

    assert_eq!(
        agent_task_cooldown_reason(
            &project,
            AgentTaskSelection::RecoverBlocked,
            250,
            Duration::from_secs(5),
            Duration::from_secs(300),
        ),
        Some("blocked-task recovery backoff active for 150s".to_string())
    );
    assert_eq!(
        agent_task_cooldown_reason(
            &project,
            AgentTaskSelection::NextTodo,
            250,
            Duration::from_secs(5),
            Duration::from_secs(300),
        ),
        None
    );
    assert_eq!(
        agent_task_cooldown_reason(
            &project,
            AgentTaskSelection::ResumeDoing,
            101,
            Duration::from_secs(5),
            Duration::from_secs(300),
        ),
        None
    );
}
