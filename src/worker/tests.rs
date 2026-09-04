use crate::runner::tests::FakeAgentRunner;
use crate::test_support::prelude::*;
use crate::test_support::*;

pub(crate) fn reserve_test_worker(
    store: &agent::TursoAgentStore,
    project_id: i64,
    worker_token: &str,
    expected_lease_holder: &str,
    created_at: &str,
    max_active_workers: usize,
) -> bool {
    let service_label = format!("clt-agent-worker-{worker_token}.service");
    let command_arguments = serde_json::to_string(&vec![
        "--local",
        "agent",
        "worker",
        "--worker-token",
        worker_token,
    ])
    .unwrap();
    store
        .reserve_worker_blocking(agent::AgentWorkerReservation {
            project_id,
            worker_token,
            expected_lease_holder,
            max_active_workers,
            protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
            service_label: &service_label,
            binary_path: Path::new("/tmp/test-worker-clt"),
            command_arguments: &command_arguments,
            path_env: OsStr::new("/usr/bin:/bin"),
            codex_path: None,
            task_selection: "next_todo",
            resume_session_id: None,
            created_at,
        })
        .unwrap()
}

pub(crate) fn reserve_test_inline_worker(
    store: &agent::TursoAgentStore,
    project_id: i64,
    worker_token: &str,
    expected_lease_holder: &str,
    worker_pid: u32,
    observed_at: &str,
) -> bool {
    let service_label = format!("{AGENT_INLINE_WORKER_SERVICE_LABEL_PREFIX}{worker_token}");
    store
        .reserve_and_claim_worker_blocking(
            agent::AgentWorkerReservation {
                project_id,
                worker_token,
                expected_lease_holder,
                max_active_workers: 12,
                protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                service_label: &service_label,
                binary_path: Path::new("/tmp/test-inline-clt"),
                command_arguments: "[]",
                path_env: OsStr::new("/usr/bin:/bin"),
                codex_path: None,
                task_selection: "next_todo",
                resume_session_id: None,
                created_at: observed_at,
            },
            worker_pid,
            observed_at,
        )
        .unwrap()
}

struct InspectInlineOwnershipRunner {
    state_dir: PathBuf,
    break_lease_after_observation: bool,
    observed_run_token: Mutex<Option<String>>,
}

impl AgentRunner for InspectInlineOwnershipRunner {
    fn run_project(&self, request: AgentRunRequest<'_>) -> Result<AgentRunResult> {
        let run_token = request
            .run_token
            .context("inline run did not receive its worker token")?;
        assert_eq!(request.lease_holder, agent_worker_lease_holder(run_token));
        assert!(inline_agent_worker_generation_is_registered(run_token));
        let store = agent::TursoAgentStore::open_blocking(&self.state_dir)?;
        let worker = store
            .list_active_workers_blocking()?
            .into_iter()
            .find(|worker| worker.worker_token == run_token)
            .context("inline worker was not durably visible to its runner")?;
        assert_eq!(worker.state, AGENT_WORKER_STATE_RUNNING);
        assert_eq!(worker.worker_pid, Some(std::process::id()));
        assert!(is_inline_agent_worker(&worker));
        *self.observed_run_token.lock().unwrap() = Some(run_token.to_string());
        if self.break_lease_after_observation {
            assert!(store.release_lease_blocking(request.project.id, request.lease_holder)?);
        }
        Ok(AgentRunResult {
            status: "idle",
            exit_code: Some(0),
            log_dir: self.state_dir.join("runs/inline"),
            stdout_path: self.state_dir.join("runs/inline/run.out"),
            stderr_path: self.state_dir.join("runs/inline/run.err"),
            summary: "inline ownership observed".to_string(),
            codex_session_id: None,
            session_run_token: None,
            control_action: None,
        })
    }
}

fn prepare_inline_git_job(root: &Path) -> (PathBuf, AgentRunJob) {
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    add_task(&project_root, "inline Git task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let mut project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .set_project_git_mode_blocking(project.id, AgentGitMode::Commit)
            .unwrap()
    );
    project.git_mode = AgentGitMode::Commit;
    let holder = "foreground-inline-holder".to_string();
    assert!(
        store
            .try_acquire_lease_blocking(
                project.id,
                &holder,
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap()
    );
    let job = AgentRunJob {
        state_dir: state_dir.clone(),
        project,
        holder,
        worker_token: None,
        max_global_jobs: 12,
        task_selection: AgentTaskSelection::NextTodo,
        resume_session_id: None,
        blocked_task_count_before: 0,
        done_task_contents_before: Vec::new(),
        blocked_task_snapshots_before: Vec::new(),
    };
    (state_dir, job)
}

fn assert_independent_worker_control_finalizes(action: AgentSessionControlAction) {
    let (suffix, expected_status, expected_control_state) = match action {
        AgentSessionControlAction::Stop => ("stop", "stopped", AgentSessionControlState::Stopped),
        AgentSessionControlAction::Interrupt => (
            "interrupt",
            "handoff",
            AgentSessionControlState::ReadyInteractive,
        ),
    };
    let root = temp_root(&format!("independent-worker-{suffix}"));
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let session_id = format!("session-{suffix}");
    fs::write(
        project_root.join("tasks/done.md"),
        format!("# Done Tasks\n- controlled task codex:{session_id}\n"),
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
    let project = store.list_projects_blocking().unwrap().remove(0);
    let worker_token = format!("control-{suffix}-token");
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "100", "9999999999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        project.id,
        &worker_token,
        "scheduler",
        "101",
        12,
    ));
    assert!(
        store
            .claim_worker_blocking(&worker_token, std::process::id(), "102")
            .unwrap()
    );
    store
        .mark_session_running_blocking(
            project.id,
            &session_id,
            4242,
            &worker_token,
            &root.join(format!("{suffix}.out")),
            &root.join(format!("{suffix}.err")),
        )
        .unwrap();
    let interactive_holder = format!("clt-interactive-{suffix}");
    match action {
        AgentSessionControlAction::Stop => assert!(
            store
                .request_session_stop_blocking(project.id, &session_id, 4242, &worker_token,)
                .unwrap()
        ),
        AgentSessionControlAction::Interrupt => assert!(
            store
                .request_session_interrupt_blocking(
                    project.id,
                    &session_id,
                    4242,
                    &worker_token,
                    &interactive_holder,
                )
                .unwrap()
        ),
    }

    let runner = FakeAgentRunner {
        result: AgentRunResult {
            status: expected_status,
            exit_code: Some(0),
            log_dir: root.join("runs/project"),
            stdout_path: root.join(format!("{suffix}.out")),
            stderr_path: root.join(format!("{suffix}.err")),
            summary: format!("controlled {suffix}"),
            codex_session_id: Some(session_id.clone()),
            session_run_token: Some(worker_token.clone()),
            control_action: Some(action),
        },
        ran_projects: Mutex::new(Vec::new()),
        delay: Duration::ZERO,
    };
    let worker_holder = agent::worker_lease_holder(&worker_token);
    let completion = run_agent_job(
        AgentRunJob {
            state_dir: state_dir.clone(),
            project: project.clone(),
            holder: worker_holder,
            worker_token: Some(worker_token.clone()),
            max_global_jobs: 12,
            task_selection: AgentTaskSelection::NextTodo,
            resume_session_id: None,
            blocked_task_count_before: 0,
            done_task_contents_before: completed_task_contents(&project_root).unwrap(),
            blocked_task_snapshots_before: Vec::new(),
        },
        &runner,
        &new_agent_shutdown_signal(),
    )
    .unwrap();

    assert_eq!(completion.status, expected_status);
    assert_eq!(
        store
            .session_control_blocking(project.id, &session_id)
            .unwrap()
            .unwrap()
            .state,
        expected_control_state
    );
    assert_eq!(
        store
            .latest_run_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .status,
        expected_status
    );
    let terminal = store.list_terminal_workers_blocking().unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].state, "completed");
    match action {
        AgentSessionControlAction::Stop => {
            assert!(
                store
                    .lease_for_project_blocking(project.id)
                    .unwrap()
                    .is_none()
            );
            drop(store);
            let scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
            assert_eq!(scheduled.jobs.len(), 1);
            assert_eq!(
                scheduled.jobs[0].task_selection,
                AgentTaskSelection::NextTodo
            );
            let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
            assert!(
                store
                    .release_lease_blocking(project.id, &scheduled.jobs[0].holder)
                    .unwrap()
            );
        }
        AgentSessionControlAction::Interrupt => {
            assert_eq!(
                store
                    .lease_for_project_blocking(project.id)
                    .unwrap()
                    .unwrap()
                    .holder,
                interactive_holder
            );
        }
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn independent_worker_stop_finalizes_after_supervisor_reap() {
    assert_independent_worker_control_finalizes(AgentSessionControlAction::Stop);
}

#[test]
fn independent_worker_handoff_finalizes_after_supervisor_reap() {
    assert_independent_worker_control_finalizes(AgentSessionControlAction::Interrupt);
}

#[test]
fn completed_agent_run_records_its_codex_session_for_the_done_task() {
    let root = temp_root("agent-task-codex-session");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    add_task(&project_root, "resumable task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(start.jobs.len(), 1);
    move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
    move_task(&project_root, TaskStatus::Doing, TaskStatus::Done, "1").unwrap();

    let mut runner = FakeAgentRunner::new(&state_dir, "success");
    runner.result.codex_session_id = Some("session-for-task".to_string());
    let shutdown = new_agent_shutdown_signal();
    run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();

    let done_task = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Done)
        .unwrap()
        .remove(0);
    assert_eq!(done_task.content, "resumable task codex:session-for-task");
    assert_eq!(task_display_text(&done_task), "resumable task");
    assert_eq!(
        codex_session_id_from_task_content(&done_task.content),
        Some("session-for-task")
    );
    assert_eq!(
        codex_session_for_task(&done_task).as_deref(),
        Some("session-for-task")
    );

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(
        store.latest_codex_session_id_blocking().unwrap().as_deref(),
        Some("session-for-task")
    );

    let mut markerless_task = done_task;
    markerless_task.content = "resumable task".to_string();
    assert_eq!(codex_session_for_task(&markerless_task), None);

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn completed_agent_run_reports_a_session_marker_write_failure() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("agent-task-codex-session-write-failure");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    add_task(&project_root, "resumable task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
    move_task(&project_root, TaskStatus::Doing, TaskStatus::Done, "1").unwrap();
    let done_path = project_root.join("tasks/done.md");
    let mut permissions = fs::metadata(&done_path).unwrap().permissions();
    permissions.set_mode(0o444);
    fs::set_permissions(&done_path, permissions).unwrap();

    let mut runner = FakeAgentRunner::new(&state_dir, "success");
    runner.result.codex_session_id = Some("session-for-task".to_string());
    let shutdown = new_agent_shutdown_signal();
    let completion = run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();

    let mut permissions = fs::metadata(&done_path).unwrap().permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&done_path, permissions).unwrap();
    assert_eq!(completion.status, "failure");
    assert!(
        completion
            .summary
            .contains("Failed to persist the Codex session marker on the task")
    );
    assert_eq!(
        fs::read_to_string(&done_path).unwrap(),
        "# Done Tasks\n- resumable task\n"
    );

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(
        store.latest_codex_session_id_blocking().unwrap().as_deref(),
        Some("session-for-task")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn blocked_agent_run_records_its_codex_session_for_the_blocked_task() {
    let root = temp_root("agent-blocked-task-codex-session");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    add_task(&project_root, "resumable blocker", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(start.jobs.len(), 1);
    move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
    let blocked_content = "resumable blocker — BLOCKED 2026-08-13: dependency unavailable";
    fs::write(
        project_root.join("tasks/doing.md"),
        format!("# Doing Tasks\n- {blocked_content}\n"),
    )
    .unwrap();

    let mut runner = FakeAgentRunner::new(&state_dir, "success");
    runner.result.codex_session_id = Some("session-for-blocked-task".to_string());
    let shutdown = new_agent_shutdown_signal();
    run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();

    let blocked_task = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing)
        .unwrap()
        .remove(0);
    assert_eq!(
        blocked_task.content,
        format!("{blocked_content} codex:session-for-blocked-task")
    );
    assert_eq!(task_display_text(&blocked_task), blocked_content);
    assert_eq!(
        codex_session_id_from_task_content(&blocked_task.content),
        Some("session-for-blocked-task")
    );

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(
        store.latest_codex_session_id_blocking().unwrap().as_deref(),
        Some("session-for-blocked-task")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn foreground_git_run_uses_one_durable_inline_generation() {
    let root = temp_root("foreground-inline-generation");
    let (state_dir, job) = prepare_inline_git_job(&root);
    let runner = InspectInlineOwnershipRunner {
        state_dir: state_dir.clone(),
        break_lease_after_observation: false,
        observed_run_token: Mutex::new(None),
    };

    let completion = run_agent_job(job, &runner, &new_agent_shutdown_signal()).unwrap();
    assert_eq!(completion.status, "idle");
    let worker_token = runner.observed_run_token.lock().unwrap().clone().unwrap();
    assert!(!inline_agent_worker_generation_is_registered(&worker_token));
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert!(store.list_active_workers_blocking().unwrap().is_empty());
    let worker = store
        .list_terminal_workers_blocking()
        .unwrap()
        .into_iter()
        .find(|worker| worker.worker_token == worker_token)
        .unwrap();
    assert_eq!(worker.state, "completed");
    assert_eq!(worker.run_id, Some(completion.run_id));
    assert!(
        store
            .lease_for_project_blocking(worker.project_id)
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn foreground_git_run_error_abandons_its_exact_inline_generation() {
    let root = temp_root("foreground-inline-error");
    let (state_dir, job) = prepare_inline_git_job(&root);
    let runner = InspectInlineOwnershipRunner {
        state_dir: state_dir.clone(),
        break_lease_after_observation: true,
        observed_run_token: Mutex::new(None),
    };

    let error = run_agent_job(job, &runner, &new_agent_shutdown_signal())
        .err()
        .expect("broken inline lease must fail the run");
    assert!(
        error
            .to_string()
            .contains("lost its durable ownership fence")
    );
    let worker_token = runner.observed_run_token.lock().unwrap().clone().unwrap();
    assert!(!inline_agent_worker_generation_is_registered(&worker_token));
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert!(store.list_active_workers_blocking().unwrap().is_empty());
    let worker = store
        .list_terminal_workers_blocking()
        .unwrap()
        .into_iter()
        .find(|worker| worker.worker_token == worker_token)
        .unwrap();
    assert_eq!(worker.state, "abandoned");
    assert!(
        worker
            .error
            .unwrap()
            .contains("lost its durable ownership fence")
    );
    assert_eq!(store.run_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn independent_worker_dispatch_survives_scheduler_state_and_consumes_capacity() {
    let root = temp_root("independent-worker-dispatch");
    let state_dir = root.join("state/clt");
    let first_root = root.join("a-project");
    let second_root = root.join("b-project");
    add_task(&first_root, "first task", None).unwrap();
    add_task(&second_root, "second task", None).unwrap();
    let first_root = fs::canonicalize(first_root).unwrap();
    let second_root = fs::canonicalize(second_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&first_root, "a-project")
        .unwrap();
    store
        .register_project_blocking(&second_root, "b-project")
        .unwrap();
    drop(store);

    let mut first =
        run_agent_scheduler_pass_with_max_global_jobs(&state_dir, false, &[], 1, None).unwrap();
    assert_eq!(first.jobs.len(), 1);
    assert_eq!(first.pass.deferred_projects, 1);
    let launched = Cell::new(false);
    let mut observed = None;
    dispatch_independent_agent_worker_with(
        &state_dir,
        Path::new("/tmp/pinned-clt-generation"),
        first.jobs.pop().unwrap(),
        AgentServiceEnvironment {
            codex_path_override: None,
            path: OsString::from("/usr/bin:/bin"),
        },
        |spec| {
            observed = Some(spec.clone());
            Ok(())
        },
        |_| {
            launched.set(true);
            Ok(())
        },
    )
    .unwrap();
    assert!(launched.get());
    let observed = observed.unwrap();

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let workers = store.list_active_workers_blocking().unwrap();
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].worker_token, observed.worker_token);
    assert_eq!(workers[0].state, AGENT_WORKER_STATE_DISPATCHING);
    assert_eq!(
        workers[0].binary_path,
        PathBuf::from("/tmp/pinned-clt-generation")
    );
    assert_eq!(
        store
            .lease_for_project_blocking(workers[0].project_id)
            .unwrap()
            .unwrap()
            .holder,
        agent_worker_lease_holder(&observed.worker_token)
    );
    drop(store);

    let restarted =
        run_agent_scheduler_pass_with_max_global_jobs(&state_dir, false, &[], 1, None).unwrap();
    assert_eq!(restarted.pass.active_agent_jobs, 1);
    assert_eq!(restarted.pass.runs_started, 0);
    assert_eq!(restarted.pass.deferred_projects, 1);
    assert!(restarted.jobs.is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_independent_worker_launch_releases_only_its_fenced_lease() {
    let root = temp_root("independent-worker-launch-failure");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    add_task(&project_root, "task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);
    let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    let error = dispatch_independent_agent_worker_with(
        &state_dir,
        Path::new("/tmp/pinned-clt-generation"),
        start.jobs.pop().unwrap(),
        AgentServiceEnvironment {
            codex_path_override: None,
            path: OsString::from("/usr/bin:/bin"),
        },
        |_| Ok(()),
        |_| anyhow::bail!("synthetic launch failure"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("independent agent worker"));

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert!(store.list_active_workers_blocking().unwrap().is_empty());
    let terminal = store.list_terminal_workers_blocking().unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].state, "abandoned");
    assert!(
        store
            .lease_for_project_blocking(terminal[0].project_id)
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scheduler_resumes_doing_task_after_independent_worker_dies() {
    let root = temp_root("independent-worker-crash-recovery");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
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
            .try_acquire_lease_blocking(project.id, "scheduler", "100", "9999999999")
            .unwrap()
    );
    assert!(
        store
            .reserve_worker_blocking(agent::AgentWorkerReservation {
                project_id: project.id,
                worker_token: "crashed-worker",
                expected_lease_holder: "scheduler",
                max_active_workers: 12,
                protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                command_arguments: "[]",
                path_env: OsStr::new("/usr/bin:/bin"),
                codex_path: None,
                service_label: "clt-worker-crashed-worker",
                binary_path: Path::new("/tmp/old-clt-generation"),
                task_selection: "next_todo",
                resume_session_id: None,
                created_at: "101",
            })
            .unwrap()
    );
    assert!(
        store
            .claim_worker_blocking("crashed-worker", u32::MAX, "102")
            .unwrap()
    );
    drop(store);

    let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::ResumeDoing
    );
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert!(store.list_active_workers_blocking().unwrap().is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_worker_reservation_claim_and_heartbeat_are_fenced() {
    let root = temp_root("agent-worker-lifecycle");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "100", "200")
            .unwrap()
    );

    assert!(
        store
            .reserve_worker_blocking(agent::AgentWorkerReservation {
                project_id: project.id,
                worker_token: "token-one",
                expected_lease_holder: "scheduler",
                max_active_workers: 12,
                protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                command_arguments: "[]",
                path_env: OsStr::new("/usr/bin:/bin"),
                codex_path: None,
                service_label: "clt-worker-token-one",
                binary_path: Path::new("/tmp/clt-generation-one"),
                task_selection: "next_todo",
                resume_session_id: None,
                created_at: "101",
            })
            .unwrap()
    );
    let lease = store
        .lease_for_project_blocking(project.id)
        .unwrap()
        .unwrap();
    assert_eq!(lease.holder, agent::worker_lease_holder("token-one"));
    let workers = store.list_active_workers_blocking().unwrap();
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].state, "dispatching");
    assert_eq!(workers[0].heartbeat_at.as_deref(), Some("101"));
    assert_eq!(workers[0].worker_pid, None);
    assert!(
        !store
            .try_acquire_lease_blocking(project.id, "new-scheduler", "201", "400")
            .unwrap()
    );
    assert_eq!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .holder,
        agent::worker_lease_holder("token-one")
    );

    assert!(
        store
            .claim_worker_blocking("token-one", 123, "102")
            .unwrap()
    );
    assert!(
        !store
            .claim_worker_blocking("token-one", 456, "102")
            .unwrap()
    );
    assert!(
        store
            .claim_worker_blocking("token-one", 123, "102")
            .unwrap()
    );
    assert!(
        !store
            .renew_worker_blocking("token-one", 456, "103", "300")
            .unwrap()
    );
    let unchanged = store.list_active_workers_blocking().unwrap().remove(0);
    assert_eq!(unchanged.heartbeat_at.as_deref(), Some("102"));
    assert_eq!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .expires_at,
        "200"
    );
    assert!(
        store
            .renew_worker_blocking("token-one", 123, "104", "300")
            .unwrap()
    );
    let renewed = store.list_active_workers_blocking().unwrap().remove(0);
    assert_eq!(renewed.state, "running");
    assert_eq!(renewed.worker_pid, Some(123));
    assert_eq!(renewed.started_at.as_deref(), Some("102"));
    assert_eq!(renewed.heartbeat_at.as_deref(), Some("104"));
    assert_eq!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .expires_at,
        "300"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn inline_worker_reservation_and_claim_commit_as_one_lease_transfer() {
    let root = temp_root("inline-worker-atomic-claim");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "100", "200")
            .unwrap()
    );
    let reservation = |expected_lease_holder| agent::AgentWorkerReservation {
        project_id: project.id,
        worker_token: "inline-token",
        expected_lease_holder,
        max_active_workers: 12,
        protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
        service_label: "clt-inline-worker-inline-token",
        binary_path: Path::new("/tmp/clt-inline-generation"),
        command_arguments: "[]",
        path_env: OsStr::new("/usr/bin:/bin"),
        codex_path: None,
        task_selection: "next_todo",
        resume_session_id: None,
        created_at: "101",
    };

    assert!(
        !store
            .reserve_and_claim_worker_blocking(
                reservation("wrong-holder"),
                std::process::id(),
                "102",
            )
            .unwrap()
    );
    assert!(store.list_active_workers_blocking().unwrap().is_empty());
    assert_eq!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .holder,
        "scheduler"
    );

    assert!(
        store
            .reserve_and_claim_worker_blocking(reservation("scheduler"), std::process::id(), "102",)
            .unwrap()
    );
    let worker = store.list_active_workers_blocking().unwrap().remove(0);
    assert_eq!(worker.state, AGENT_WORKER_STATE_RUNNING);
    assert_eq!(worker.worker_pid, Some(std::process::id()));
    assert_eq!(worker.started_at.as_deref(), Some("102"));
    assert_eq!(worker.heartbeat_at.as_deref(), Some("102"));
    assert_eq!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .holder,
        agent_worker_lease_holder("inline-token")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_worker_reservation_rolls_back_failed_lease_transfer() {
    let root = temp_root("agent-worker-reservation-rollback");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "100", "200")
            .unwrap()
    );

    let reserve = |expected_lease_holder| agent::AgentWorkerReservation {
        project_id: project.id,
        worker_token: "token-one",
        expected_lease_holder,
        max_active_workers: 12,
        protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
        command_arguments: "[]",
        path_env: OsStr::new("/usr/bin:/bin"),
        codex_path: None,
        service_label: "clt-worker-token-one",
        binary_path: Path::new("/tmp/clt-generation-one"),
        task_selection: "next_todo",
        resume_session_id: None,
        created_at: "101",
    };
    assert!(
        !store
            .reserve_worker_blocking(reserve("wrong-holder"))
            .unwrap()
    );
    assert!(store.list_active_workers_blocking().unwrap().is_empty());
    assert_eq!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .holder,
        "scheduler"
    );
    assert!(store.reserve_worker_blocking(reserve("scheduler")).unwrap());

    assert!(
        !store
            .reserve_worker_blocking(agent::AgentWorkerReservation {
                project_id: project.id,
                worker_token: "token-two",
                expected_lease_holder: &agent::worker_lease_holder("token-one"),
                max_active_workers: 12,
                protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                command_arguments: "[]",
                path_env: OsStr::new("/usr/bin:/bin"),
                codex_path: None,
                service_label: "clt-worker-token-two",
                binary_path: Path::new("/tmp/clt-generation-two"),
                task_selection: "next_todo",
                resume_session_id: None,
                created_at: "102",
            })
            .unwrap()
    );
    assert_eq!(store.list_active_workers_blocking().unwrap().len(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_worker_abandonment_is_observation_and_lease_fenced() {
    let root = temp_root("agent-worker-abandonment");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "100", "200")
            .unwrap()
    );
    assert!(
        store
            .reserve_worker_blocking(agent::AgentWorkerReservation {
                project_id: project.id,
                worker_token: "token-one",
                expected_lease_holder: "scheduler",
                max_active_workers: 12,
                protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                command_arguments: "[]",
                path_env: OsStr::new("/usr/bin:/bin"),
                codex_path: None,
                service_label: "clt-worker-token-one",
                binary_path: Path::new("/tmp/clt-generation-one"),
                task_selection: "next_todo",
                resume_session_id: None,
                created_at: "101",
            })
            .unwrap()
    );
    assert!(
        store
            .claim_worker_blocking("token-one", 123, "102")
            .unwrap()
    );
    assert!(
        !store
            .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: "token-one",
                expected_state: "running",
                expected_worker_pid: Some(123),
                expected_heartbeat_at: Some("stale-observation"),
                finished_at: "103",
                error: "worker disappeared",
                permitted_successor_holder: None,
            })
            .unwrap()
    );
    assert_eq!(store.list_active_workers_blocking().unwrap().len(), 1);
    assert!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .is_some()
    );

    let worker_holder = agent::worker_lease_holder("token-one");
    assert!(
        store
            .release_lease_blocking(project.id, &worker_holder)
            .unwrap()
    );
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "successor-holder", "103", "300")
            .unwrap()
    );
    assert!(
        !store
            .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: "token-one",
                expected_state: "running",
                expected_worker_pid: Some(123),
                expected_heartbeat_at: Some("102"),
                finished_at: "104",
                error: "worker disappeared",
                permitted_successor_holder: Some("different-successor"),
            })
            .unwrap()
    );
    assert_eq!(store.list_active_workers_blocking().unwrap().len(), 1);
    assert!(
        store
            .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: "token-one",
                expected_state: "running",
                expected_worker_pid: Some(123),
                expected_heartbeat_at: Some("102"),
                finished_at: "104",
                error: "worker disappeared",
                permitted_successor_holder: Some("successor-holder"),
            })
            .unwrap()
    );
    assert!(store.list_active_workers_blocking().unwrap().is_empty());
    assert_eq!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .holder,
        "successor-holder"
    );
    let terminal = store.list_terminal_workers_blocking().unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].state, "abandoned");
    assert_eq!(terminal[0].finished_at.as_deref(), Some("104"));
    assert_eq!(terminal[0].error.as_deref(), Some("worker disappeared"));
    assert!(terminal[0].run_id.is_some());
    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(
        store
            .list_projects_blocking()
            .unwrap()
            .remove(0)
            .failure_count,
        1
    );
    assert!(
        !store
            .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: "token-one",
                expected_state: "running",
                expected_worker_pid: Some(123),
                expected_heartbeat_at: Some("102"),
                finished_at: "106",
                error: "duplicate observation",
                permitted_successor_holder: None,
            })
            .unwrap()
    );
    assert_eq!(store.run_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_worker_finalization_is_idempotent_and_transactional() {
    let root = temp_root("agent-worker-finalization");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "100", "200")
            .unwrap()
    );
    assert!(
        store
            .reserve_worker_blocking(agent::AgentWorkerReservation {
                project_id: project.id,
                worker_token: "token-one",
                expected_lease_holder: "scheduler",
                max_active_workers: 12,
                protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                command_arguments: "[]",
                path_env: OsStr::new("/usr/bin:/bin"),
                codex_path: None,
                service_label: "clt-worker-token-one",
                binary_path: Path::new("/tmp/clt-generation-one"),
                task_selection: "next_todo",
                resume_session_id: None,
                created_at: "101",
            })
            .unwrap()
    );
    assert!(
        store
            .claim_worker_blocking("token-one", 123, "102")
            .unwrap()
    );
    let worker_holder = agent::worker_lease_holder("token-one");
    let finalize = |expected_worker_pid| agent::AgentWorkerFinalization {
        worker_token: "token-one",
        expected_worker_pid,
        expected_lease_holder: &worker_holder,
        status: "failure",
        finished_at: "110",
        exit_code: Some(1),
        log_dir: Some("/tmp/logs"),
        stdout_path: Some("/tmp/logs/run.out"),
        stderr_path: Some("/tmp/logs/run.err"),
        summary: Some("failed once"),
        codex_session_id: Some("session-one"),
        error: Some("worker reported failure"),
    };
    assert_eq!(
        store.finalize_worker_blocking(finalize(Some(456))).unwrap(),
        None
    );
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .is_some()
    );

    let run_id = store
        .finalize_worker_blocking(finalize(Some(123)))
        .unwrap()
        .unwrap();
    assert_eq!(
        store.finalize_worker_blocking(finalize(Some(123))).unwrap(),
        Some(run_id)
    );
    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert!(store.list_active_workers_blocking().unwrap().is_empty());
    let terminal = store.list_terminal_workers_blocking().unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].state, "completed");
    assert_eq!(terminal[0].run_id, Some(run_id));
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.failure_count, 1);
    assert_eq!(project.last_failure_at.as_deref(), Some("110"));
    let run = store
        .latest_run_for_project_blocking(project.id)
        .unwrap()
        .unwrap();
    assert_eq!(run.id, run_id);
    assert_eq!(run.status, "failure");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dispatching_worker_requires_a_verified_drain_after_its_startup_deadline() {
    let root = temp_root("agent-worker-dispatch-timeout");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "99", "999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        project.id,
        "dispatch-token",
        "scheduler",
        "100",
        12,
    ));

    let launch_count = Cell::new(0);
    let workers = reconcile_independent_agent_workers_with(
        &state_dir,
        &store,
        159,
        |spec| {
            launch_count.set(launch_count.get() + 1);
            assert_eq!(
                spec.command_arguments.as_ref().unwrap(),
                &vec![
                    OsString::from("--local"),
                    OsString::from("agent"),
                    OsString::from("worker"),
                    OsString::from("--worker-token"),
                    OsString::from("dispatch-token"),
                ]
            );
            assert_eq!(spec.service_env.path, OsString::from("/usr/bin:/bin"));
            Ok(())
        },
        |_| panic!("a fresh dispatch must not be drained"),
    )
    .unwrap();
    assert_eq!(workers.len(), 1);
    assert_eq!(launch_count.get(), 1);

    let workers = reconcile_independent_agent_workers_with(
        &state_dir,
        &store,
        160,
        |_| panic!("a timed-out dispatch must not be relaunched before draining"),
        |_| Ok(false),
    )
    .unwrap();
    assert_eq!(workers.len(), 1);
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .is_some()
    );

    let mut process_is_running = |_: u32| panic!("a dispatching worker has no process to inspect");
    let mut launch =
        |_: &AgentWorkerLaunchSpec| panic!("a timed-out dispatch must not be relaunched");
    let mut drain = |_: &agent::AgentWorkerRecord| Ok(true);
    let mut timestamp = || "161".to_string();
    let reconciliation = reconcile_agent_worker_effects_stage(
        AgentWorkerReconciliationRequest {
            state_dir: &state_dir,
            store: &store,
            now_seconds: 161,
        },
        AgentWorkerReconciliationEffects {
            process_is_running: &mut process_is_running,
            launch_dispatching: &mut launch,
            drain_worker: &mut drain,
            timestamp: &mut timestamp,
        },
    )
    .unwrap();
    assert!(reconciliation.active_workers.is_empty());
    assert!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(store.run_count_blocking().unwrap(), 1);
    let terminal = store.list_terminal_workers_blocking().unwrap();
    assert_eq!(terminal[0].state, "abandoned");
    assert!(terminal[0].run_id.is_some());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn newer_worker_protocol_is_opaque_to_an_older_scheduler() {
    let root = temp_root("agent-worker-newer-protocol");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "1", "999")
            .unwrap()
    );
    assert!(
        store
            .reserve_worker_blocking(agent::AgentWorkerReservation {
                project_id: project.id,
                worker_token: "future-token",
                expected_lease_holder: "scheduler",
                max_active_workers: 12,
                protocol_version: AGENT_WORKER_PROTOCOL_VERSION + 1,
                service_label: "future-service",
                binary_path: Path::new("/tmp/future-clt"),
                command_arguments: "[]",
                path_env: OsStr::new("/usr/bin:/bin"),
                codex_path: None,
                task_selection: "next_todo",
                resume_session_id: None,
                created_at: "2",
            })
            .unwrap()
    );

    let workers = reconcile_independent_agent_workers_with(
        &state_dir,
        &store,
        10_000,
        |_| panic!("an older scheduler must not launch a newer worker protocol"),
        |_| panic!("an older scheduler must not drain a newer worker protocol"),
    )
    .unwrap();

    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].worker_token, "future-token");
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .holder,
        agent::worker_lease_holder("future-token")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dead_outer_worker_keeps_its_fence_until_the_service_is_drained() {
    let root = temp_root("agent-worker-drain-fence");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
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
        "dead-token",
        "scheduler",
        "101",
        12,
    ));
    assert!(
        store
            .claim_worker_blocking("dead-token", u32::MAX, "102")
            .unwrap()
    );

    let still_fenced = reconcile_independent_agent_workers_with(
        &state_dir,
        &store,
        103,
        |_| Ok(()),
        |_| Ok(false),
    )
    .unwrap();
    assert_eq!(still_fenced.len(), 1);
    assert!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .is_some()
    );
    assert_eq!(store.run_count_blocking().unwrap(), 0);

    let recovered =
        reconcile_independent_agent_workers_with(&state_dir, &store, 104, |_| Ok(()), |_| Ok(true))
            .unwrap();
    assert!(recovered.is_empty());
    assert!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(store.run_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_heartbeat_is_recovered_even_when_the_pid_was_reused() {
    let root = temp_root("agent-worker-stale-heartbeat");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "99", "999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        project.id,
        "stale-token",
        "scheduler",
        "100",
        12,
    ));
    assert!(
        store
            .claim_worker_blocking("stale-token", std::process::id(), "100")
            .unwrap()
    );

    let drain_count = Cell::new(0);
    let workers = reconcile_independent_agent_workers_with(
        &state_dir,
        &store,
        160,
        |_| Ok(()),
        |_| {
            drain_count.set(drain_count.get() + 1);
            Ok(true)
        },
    )
    .unwrap();
    assert!(workers.is_empty());
    assert_eq!(drain_count.get(), 1);
    assert_eq!(store.run_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn inline_worker_liveness_uses_the_registered_generation_not_a_reused_pid() {
    let root = temp_root("inline-worker-generation-liveness");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "99", "999")
            .unwrap()
    );
    let generation = InlineAgentWorkerGeneration::register("inline-live-token");
    assert!(reserve_test_inline_worker(
        &store,
        project.id,
        "inline-live-token",
        "scheduler",
        std::process::id(),
        "100",
    ));

    let drain_count = Cell::new(0);
    let workers = reconcile_independent_agent_workers_with(
        &state_dir,
        &store,
        160,
        |_| panic!("a running inline generation must not be dispatched"),
        |_| {
            drain_count.set(drain_count.get() + 1);
            Ok(true)
        },
    )
    .unwrap();
    assert_eq!(workers.len(), 1);
    assert_eq!(drain_count.get(), 0);

    drop(generation);
    let workers = reconcile_independent_agent_workers_with(
        &state_dir,
        &store,
        161,
        |_| panic!("an inline generation must not be dispatched"),
        |_| {
            drain_count.set(drain_count.get() + 1);
            Ok(true)
        },
    )
    .unwrap();
    assert!(workers.is_empty());
    assert_eq!(drain_count.get(), 1);
    assert_eq!(store.run_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn inline_worker_recovery_waits_for_exact_launch_reap_fence() {
    let root = temp_root("inline-worker-launch-reap-fence");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "99", "999")
            .unwrap()
    );
    assert!(reserve_test_inline_worker(
        &store,
        project.id,
        "inline-reap-token",
        "scheduler",
        u32::MAX,
        "100",
    ));
    let git_start = AgentGitStartState {
        starting_head: "abc123".to_string(),
        branch_ref: Some("refs/heads/main".to_string()),
        upstream_ref: None,
        worktree_baseline: "[]".to_string(),
    };
    assert!(
        store
            .record_git_launch_state_blocking(
                project.id,
                "inline-reap-token",
                AgentGitMode::Commit,
                &git_start,
                "101",
            )
            .unwrap()
    );

    let drain_count = Cell::new(0);
    let workers = reconcile_independent_agent_workers_with(
        &state_dir,
        &store,
        160,
        |_| panic!("an inline worker must not be dispatched"),
        |_| {
            drain_count.set(drain_count.get() + 1);
            Ok(true)
        },
    )
    .unwrap();
    assert_eq!(workers.len(), 1);
    assert_eq!(drain_count.get(), 0);
    assert_eq!(store.run_count_blocking().unwrap(), 0);

    assert!(
        store
            .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: "inline-reap-token",
                expected_state: AGENT_WORKER_STATE_RUNNING,
                expected_worker_pid: Some(u32::MAX),
                expected_heartbeat_at: Some("100"),
                finished_at: "161",
                error: "simulated supervisor finalization after reap",
                permitted_successor_holder: None,
            })
            .unwrap()
    );
    assert!(
        store
            .reclaim_unchanged_git_launch_state_blocking(
                project.id,
                "inline-reap-token",
                AgentGitMode::Commit,
                &git_start,
            )
            .unwrap()
    );
    assert!(
        store
            .git_launch_state_blocking(project.id, "inline-reap-token")
            .unwrap()
            .is_none()
    );
    assert_eq!(drain_count.get(), 0);
    assert_eq!(store.run_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn worker_finalization_requires_the_exact_current_lease_holder() {
    let root = temp_root("agent-worker-finalization-fence");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
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
        "fenced-token",
        "scheduler",
        "101",
        12,
    ));
    assert!(
        store
            .claim_worker_blocking("fenced-token", 123, "102")
            .unwrap()
    );
    store
        .mark_session_running_blocking(
            project.id,
            "session-fenced",
            456,
            "fenced-token",
            &root.join("session.out"),
            &root.join("session.err"),
        )
        .unwrap();
    let worker_holder = agent::worker_lease_holder("fenced-token");
    assert!(
        store
            .release_lease_blocking(project.id, &worker_holder)
            .unwrap()
    );
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "unrelated-successor", "103", "999")
            .unwrap()
    );
    assert!(
        store
            .mark_session_running_blocking(
                project.id,
                "session-fenced",
                789,
                "fenced-token",
                &root.join("new-session.out"),
                &root.join("new-session.err"),
            )
            .is_err()
    );
    assert!(
        !store
            .clear_running_session_control_blocking(
                project.id,
                "session-fenced",
                Some("fenced-token"),
            )
            .unwrap()
    );
    assert_eq!(
        store
            .session_control_blocking(project.id, "session-fenced")
            .unwrap()
            .unwrap()
            .child_pid,
        Some(456)
    );

    assert_eq!(
        store
            .finalize_worker_blocking(agent::AgentWorkerFinalization {
                worker_token: "fenced-token",
                expected_worker_pid: Some(123),
                expected_lease_holder: &worker_holder,
                status: "success",
                finished_at: "104",
                exit_code: Some(0),
                log_dir: None,
                stdout_path: None,
                stderr_path: None,
                summary: Some("must not commit"),
                codex_session_id: None,
                error: None,
            })
            .unwrap(),
        None
    );
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(store.list_active_workers_blocking().unwrap().len(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn abandoned_worker_preserves_a_generation_verified_interactive_successor() {
    let root = temp_root("agent-worker-interactive-successor");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "100", "9999999999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        project.id,
        "handoff-token",
        "scheduler",
        "101",
        12,
    ));
    assert!(
        store
            .claim_worker_blocking("handoff-token", 123, "102")
            .unwrap()
    );
    store
        .mark_session_running_blocking(
            project.id,
            "session-handoff",
            456,
            "handoff-token",
            &root.join("session.out"),
            &root.join("session.err"),
        )
        .unwrap();
    let interactive_holder = "clt-interactive-999";
    assert!(
        store
            .request_session_interrupt_blocking(
                project.id,
                "session-handoff",
                456,
                "handoff-token",
                interactive_holder,
            )
            .unwrap()
    );
    let worker_holder = agent::worker_lease_holder("handoff-token");
    assert_eq!(
        store
            .complete_session_interrupt_handoff_blocking(
                project.id,
                "session-handoff",
                "handoff-token",
                &worker_holder,
                60,
            )
            .unwrap()
            .as_deref(),
        Some(interactive_holder)
    );
    assert!(
        store
            .transition_session_control_state_blocking(
                project.id,
                "session-handoff",
                AgentSessionControlState::ReadyInteractive,
                AgentSessionControlState::Interactive,
            )
            .unwrap()
    );

    assert!(
        store
            .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: "handoff-token",
                expected_state: "running",
                expected_worker_pid: Some(123),
                expected_heartbeat_at: Some("102"),
                finished_at: "104",
                error: "outer worker exited after handoff",
                permitted_successor_holder: Some(interactive_holder),
            })
            .unwrap()
    );
    assert!(store.list_active_workers_blocking().unwrap().is_empty());
    assert_eq!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap()
            .holder,
        interactive_holder
    );
    assert_eq!(store.run_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn worker_reservation_serializes_the_global_capacity_limit() {
    let root = temp_root("agent-worker-global-capacity");
    let state_dir = root.join("state/clt");
    let first_root = root.join("first");
    let second_root = root.join("second");
    fs::create_dir_all(&first_root).unwrap();
    fs::create_dir_all(&second_root).unwrap();
    let first_root = fs::canonicalize(first_root).unwrap();
    let second_root = fs::canonicalize(second_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&first_root, "first")
        .unwrap();
    store
        .register_project_blocking(&second_root, "second")
        .unwrap();
    let projects = store.list_projects_blocking().unwrap();
    let first_id = projects
        .iter()
        .find(|project| project.name == "first")
        .unwrap()
        .id;
    let second_id = projects
        .iter()
        .find(|project| project.name == "second")
        .unwrap()
        .id;
    assert!(
        store
            .try_acquire_lease_blocking(first_id, "scheduler-one", "100", "999")
            .unwrap()
    );
    assert!(
        store
            .try_acquire_lease_blocking(second_id, "scheduler-two", "100", "999")
            .unwrap()
    );
    drop(store);

    let barrier = Arc::new(Barrier::new(3));
    let handles = [
        (first_id, "capacity-one", "scheduler-one"),
        (second_id, "capacity-two", "scheduler-two"),
    ]
    .into_iter()
    .map(|(project_id, token, holder)| {
        let state_dir = state_dir.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
            barrier.wait();
            reserve_test_worker(&store, project_id, token, holder, "101", 1)
        })
    })
    .collect::<Vec<_>>();
    barrier.wait();
    let reserved = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .filter(|reserved| *reserved)
        .count();
    assert_eq!(reserved, 1);
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(store.list_active_workers_blocking().unwrap().len(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn successful_worker_reservation_atomically_supersedes_old_abandonment() {
    let root = temp_root("agent-worker-supersede-atomic");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler-old", "100", "999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        project.id,
        "old-token",
        "scheduler-old",
        "101",
        12,
    ));
    assert!(
        store
            .claim_worker_blocking("old-token", 123, "102")
            .unwrap()
    );
    assert!(
        store
            .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: "old-token",
                expected_state: "running",
                expected_worker_pid: Some(123),
                expected_heartbeat_at: Some("102"),
                finished_at: "103",
                error: "old worker exited",
                permitted_successor_holder: None,
            })
            .unwrap()
    );
    assert_eq!(
        store.list_terminal_workers_blocking().unwrap()[0].state,
        "abandoned"
    );
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler-new", "104", "999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        project.id,
        "new-token",
        "scheduler-new",
        "105",
        12,
    ));
    let terminal = store.list_terminal_workers_blocking().unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].worker_token, "old-token");
    assert_eq!(terminal[0].state, "superseded");
    assert_eq!(
        store.list_active_workers_blocking().unwrap()[0].worker_token,
        "new-token"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn incompatible_schema_migration_is_deferred_for_pinned_workers() {
    let future_migration_version = 18;
    let root = temp_root("agent-worker-migration-barrier");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
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
        "migration-token",
        "scheduler",
        "101",
        12,
    ));
    assert!(
        !store
            .worker_schema_migration_deferred_blocking(AGENT_WORKER_SHARED_SCHEMA_VERSION)
            .unwrap()
    );
    assert!(
        store
            .worker_schema_migration_deferred_blocking(future_migration_version)
            .unwrap()
    );
    store
        .mark_session_running_blocking(
            project.id,
            "migration-session",
            123,
            "migration-token",
            &root.join("migration.out"),
            &root.join("migration.err"),
        )
        .unwrap();
    let compatibility_store = agent::TursoAgentStore::open_blocking_with_test_migration(
        &state_dir,
        future_migration_version,
        "CREATE TABLE deferred_worker_migration_probe (id INTEGER PRIMARY KEY)",
    )
    .unwrap();
    assert_eq!(
        compatibility_store.pending_migration_version(),
        Some(future_migration_version)
    );
    assert!(
        !compatibility_store
            .table_exists_blocking("deferred_worker_migration_probe")
            .unwrap()
    );
    assert!(
        compatibility_store
            .request_session_stop_blocking(project.id, "migration-session", 123, "migration-token",)
            .unwrap()
    );
    assert!(
        store
            .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: "migration-token",
                expected_state: AGENT_WORKER_STATE_DISPATCHING,
                expected_worker_pid: None,
                expected_heartbeat_at: Some("101"),
                finished_at: "102",
                error: "test completion",
                permitted_successor_holder: None,
            })
            .unwrap()
    );
    assert!(
        !store
            .worker_schema_migration_deferred_blocking(future_migration_version)
            .unwrap()
    );
    let migrated_store = agent::TursoAgentStore::open_blocking_with_test_migration(
        &state_dir,
        future_migration_version,
        "CREATE TABLE deferred_worker_migration_probe (id INTEGER PRIMARY KEY)",
    )
    .unwrap();
    assert_eq!(migrated_store.pending_migration_version(), None);
    assert!(
        migrated_store
            .table_exists_blocking("deferred_worker_migration_probe")
            .unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_recovers_stale_leases() {
    let root = temp_root("agent-run-stale-lease");
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
            .try_acquire_lease_blocking(project.id, "old-holder", "100", "101")
            .unwrap()
    );
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "new-holder", "102", "200")
            .unwrap()
    );

    assert_eq!(store.lease_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_lists_active_leases() {
    let root = temp_root("agent-active-leases");
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
            .try_acquire_lease_blocking(project.id, "holder", "100", "200")
            .unwrap()
    );

    let leases = store.list_active_leases_blocking("150").unwrap();

    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].project_id, project.id);
    assert_eq!(leases[0].project_name, "project");
    assert_eq!(leases[0].project_path, project_root);
    assert_eq!(leases[0].holder, "holder");
    assert_eq!(leases[0].acquired_at, "100");
    assert_eq!(leases[0].expires_at, "200");
    assert!(store.list_active_leases_blocking("250").unwrap().is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_lists_recent_runs() {
    let root = temp_root("agent-recent-runs");
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
        .record_run_outcome_blocking(agent::AgentRunOutcome {
            project_id: project.id,
            status: "success",
            started_at: "100",
            finished_at: Some("101"),
            exit_code: Some(0),
            log_dir: Some("/tmp/logs"),
            stdout_path: Some("/tmp/logs/run.out"),
            stderr_path: Some("/tmp/logs/run.err"),
            summary: Some("completed"),
            codex_session_id: Some("session-123"),
        })
        .unwrap();

    let runs = store.list_recent_runs_blocking(5).unwrap();

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].project_id, project.id);
    assert_eq!(runs[0].project_name, "project");
    assert_eq!(runs[0].project_path, project_root);
    assert_eq!(runs[0].status, "success");
    assert_eq!(runs[0].started_at, "100");
    assert_eq!(runs[0].finished_at.as_deref(), Some("101"));
    assert_eq!(runs[0].exit_code, Some(0));
    assert_eq!(runs[0].stdout_path.as_deref(), Some("/tmp/logs/run.out"));
    assert_eq!(runs[0].stderr_path.as_deref(), Some("/tmp/logs/run.err"));
    assert_eq!(runs[0].summary.as_deref(), Some("completed"));
    assert_eq!(
        store.latest_codex_session_id_blocking().unwrap().as_deref(),
        Some("session-123")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_finds_latest_run_for_selected_project() {
    let root = temp_root("agent-latest-project-run");
    let state_dir = root.join("state/clt");
    let first_root = root.join("first");
    let second_root = root.join("second");
    init_tasks(&first_root, false).unwrap();
    init_tasks(&second_root, false).unwrap();
    let first_root = fs::canonicalize(first_root).unwrap();
    let second_root = fs::canonicalize(second_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&first_root, "first")
        .unwrap();
    store
        .register_project_blocking(&second_root, "second")
        .unwrap();
    let projects = store.list_projects_blocking().unwrap();
    let first = projects
        .iter()
        .find(|project| project.name == "first")
        .unwrap();
    let second = projects
        .iter()
        .find(|project| project.name == "second")
        .unwrap();

    for (project_id, started_at, stdout_path) in [
        (first.id, "100", "/tmp/first-old.out"),
        (first.id, "200", "/tmp/first-new.out"),
        (second.id, "300", "/tmp/second-newest.out"),
    ] {
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
                project_id,
                status: "success",
                started_at,
                finished_at: Some(started_at),
                exit_code: Some(0),
                log_dir: Some("/tmp"),
                stdout_path: Some(stdout_path),
                stderr_path: None,
                summary: Some("completed"),
                codex_session_id: None,
            })
            .unwrap();
    }

    let latest = store
        .latest_run_for_project_blocking(first.id)
        .unwrap()
        .unwrap();

    assert_eq!(latest.project_id, first.id);
    assert_eq!(latest.stdout_path.as_deref(), Some("/tmp/first-new.out"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn successful_agent_run_clears_previous_failure_timestamp() {
    let root = temp_root("agent-success-clears-failure");
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
        .record_run_outcome_blocking(agent::AgentRunOutcome {
            project_id: project.id,
            status: "failure",
            started_at: "100",
            finished_at: Some("101"),
            exit_code: None,
            log_dir: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some("failed"),
            codex_session_id: None,
        })
        .unwrap();
    store
        .record_run_outcome_blocking(agent::AgentRunOutcome {
            project_id: project.id,
            status: "blocked",
            started_at: "150",
            finished_at: Some("151"),
            exit_code: Some(0),
            log_dir: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some("still blocked"),
            codex_session_id: None,
        })
        .unwrap();
    store
        .record_run_outcome_blocking(agent::AgentRunOutcome {
            project_id: project.id,
            status: "success",
            started_at: "200",
            finished_at: Some("201"),
            exit_code: Some(0),
            log_dir: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some("completed"),
            codex_session_id: None,
        })
        .unwrap();

    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.failure_count, 0);
    assert_eq!(project.last_failure_at, None);
    assert_eq!(project.last_blocked_recovery_at, None);
    assert_eq!(project.last_success_at.as_deref(), Some("201"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_clean_resets_failures_and_removes_logs() {
    let root = temp_root("agent-clean-state");
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
        .record_run_outcome_blocking(agent::AgentRunOutcome {
            project_id: project.id,
            status: "failure",
            started_at: "100",
            finished_at: Some("101"),
            exit_code: None,
            log_dir: Some("/tmp/logs"),
            stdout_path: Some("/tmp/logs/run.out"),
            stderr_path: Some("/tmp/logs/run.err"),
            summary: Some("failed"),
            codex_session_id: None,
        })
        .unwrap();
    store
        .record_run_outcome_blocking(agent::AgentRunOutcome {
            project_id: project.id,
            status: "blocked",
            started_at: "102",
            finished_at: Some("103"),
            exit_code: Some(0),
            log_dir: Some("/tmp/logs"),
            stdout_path: Some("/tmp/logs/run.out"),
            stderr_path: Some("/tmp/logs/run.err"),
            summary: Some("still blocked"),
            codex_session_id: None,
        })
        .unwrap();
    store
        .record_daemon_checkin_blocking("stale-daemon", "service", "90", "95", "99")
        .unwrap();
    fs::create_dir_all(state_dir.join("runs/project/run-1")).unwrap();
    fs::write(state_dir.join("runs/project/run-1/stdout.log"), "old run").unwrap();
    fs::write(state_dir.join("agent-service.out"), "service out").unwrap();
    fs::write(state_dir.join("agent-service.err"), "service err").unwrap();

    assert!(
        store
            .try_acquire_lease_blocking(project.id, "concurrent-scheduler", "104", "999")
            .unwrap()
    );
    let error = store.clean_agent_history_blocking("105").unwrap_err();
    assert!(error.to_string().contains("project leases are active"));
    assert_eq!(store.run_count_blocking().unwrap(), 2);
    assert!(
        store
            .release_lease_blocking(project.id, "concurrent-scheduler")
            .unwrap()
    );
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "stale-scheduler", "90", "100")
            .unwrap()
    );

    clean_agent_state(&store, &state_dir).unwrap();

    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.failure_count, 0);
    assert_eq!(project.last_failure_at, None);
    assert_eq!(project.last_blocked_recovery_at, None);
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert!(store.list_daemon_checkins_blocking().unwrap().is_empty());
    assert_eq!(fs::read_dir(state_dir.join("runs")).unwrap().count(), 0);
    assert_eq!(
        fs::read_to_string(state_dir.join("agent-service.out")).unwrap(),
        ""
    );
    assert_eq!(
        fs::read_to_string(state_dir.join("agent-service.err")).unwrap(),
        ""
    );

    fs::remove_dir_all(root).unwrap();
}
