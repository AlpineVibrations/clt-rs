use crate::test_support::prelude::*;
use crate::test_support::*;
use crate::tui::tests::tui_agent_project_for_test;
use crate::worker::tests::reserve_test_inline_worker;

#[test]
fn agent_run_settings_only_read_the_complete_startup_header() {
    let root = temp_root("agent-run-settings");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("run.err");
    fs::write(
        &path,
        "Reading additional input from stdin...\r\nOpenAI Codex v0.153.3\r\n--------\r\nworkdir: /tmp/project\r\nmodel: local/model:latest  \r\nreasoning effort: xhigh\r\nsession id: session-one\r\n--------\r\nuser\r\nmodel: unrelated\r\nreasoning effort: low\r\n",
    )
    .unwrap();
    let settings = agent_run_settings_from_log(&path).unwrap();
    assert_eq!(settings.model.as_deref(), Some("local/model:latest"));
    assert_eq!(settings.reasoning_effort.as_deref(), Some("xhigh"));

    for content in [
        "",
        "model: task text\nreasoning effort: high\n",
        "OpenAI Codex v0.153.3\n--------\nmodel: partial",
        "OpenAI Codex v0.153.3\n--------\nmodel: \nreasoning effort: \n--------\nmodel: task text\n",
    ] {
        fs::write(&path, content).unwrap();
        assert_eq!(
            agent_run_settings_from_log(&path).unwrap(),
            AgentRunSettings::default()
        );
    }
    fs::write(
        &path,
        "OpenAI Codex v0.153.3\n--------\nmodel: older-model\n--------\n",
    )
    .unwrap();
    let settings = agent_run_settings_from_log(&path).unwrap();
    assert_eq!(settings.model.as_deref(), Some("older-model"));
    assert_eq!(settings.reasoning_effort, None);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn interactive_terminal_event_source_process_entry() {
    if std::env::var_os("CLT_TEST_INTERACTIVE_TERMINAL_SOURCE").is_none() {
        return;
    }
    // SAFETY: isatty only inspects the inherited standard-input fd.
    assert_eq!(unsafe { libc::isatty(libc::STDIN_FILENO) }, 1);
    event::poll(Duration::ZERO)
        .expect("crossterm must initialize from the inherited terminal descriptor");
}

#[cfg(unix)]
#[test]
fn interactive_exec_gate_process_entry() {
    if std::env::var_os(TEST_INTERACTIVE_EXEC_GATE_ENV).is_none() {
        return;
    }
    let argument_count = std::env::var("CLT_TEST_INTERACTIVE_GATE_ARGUMENT_COUNT")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let arguments = (0..argument_count)
        .map(|index| {
            std::env::var_os(format!("CLT_TEST_INTERACTIVE_GATE_ARGUMENT_{index}")).unwrap()
        })
        .collect::<Vec<_>>();
    let control_fd = std::env::var("CLT_TEST_INTERACTIVE_GATE_CONTROL_FD")
        .unwrap()
        .parse::<i32>()
        .unwrap();
    run_interactive_exec_gate(
        Some(control_fd),
        Path::new(&std::env::var_os("CLT_TEST_INTERACTIVE_GATE_PROGRAM").unwrap()),
        &arguments,
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn automated_session_supervisor_process_entry() {
    if std::env::var_os(TEST_AUTOMATED_SUPERVISOR_ENV).is_none() {
        return;
    }
    let argument_count = std::env::var("CLT_TEST_SUPERVISOR_ARGUMENT_COUNT")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let arguments = (0..argument_count)
        .map(|index| std::env::var_os(format!("CLT_TEST_SUPERVISOR_ARGUMENT_{index}")).unwrap())
        .collect::<Vec<_>>();
    let state_dir = PathBuf::from(std::env::var_os("CLT_TEST_SUPERVISOR_STATE_DIR").unwrap());
    let run_token = std::env::var("CLT_TEST_SUPERVISOR_RUN_TOKEN").unwrap();
    let lease_holder = std::env::var("CLT_TEST_SUPERVISOR_LEASE_HOLDER").unwrap();
    let stdout_path = PathBuf::from(std::env::var_os("CLT_TEST_SUPERVISOR_STDOUT_PATH").unwrap());
    let stderr_path = PathBuf::from(std::env::var_os("CLT_TEST_SUPERVISOR_STDERR_PATH").unwrap());
    let exit_code = run_automated_session_supervisor(
        AutomatedSupervisorSpec {
            state_dir: &state_dir,
            project_id: std::env::var("CLT_TEST_SUPERVISOR_PROJECT_ID")
                .unwrap()
                .parse()
                .unwrap(),
            run_token: &run_token,
            lease_holder: &lease_holder,
            stdout_path: &stdout_path,
            stderr_path: &stderr_path,
        },
        Path::new(&std::env::var_os("CLT_TEST_SUPERVISOR_PROGRAM").unwrap()),
        &arguments,
    )
    .unwrap();
    std::process::exit(exit_code);
}

#[cfg(unix)]
#[test]
fn automated_runner_owner_process_entry() {
    if std::env::var_os("CLT_TEST_AUTOMATED_RUNNER_OWNER").is_none() {
        return;
    }
    let state_dir = PathBuf::from(std::env::var_os("CLT_TEST_OWNER_STATE_DIR").unwrap());
    let project_id = std::env::var("CLT_TEST_OWNER_PROJECT_ID")
        .unwrap()
        .parse::<i64>()
        .unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let project = store
        .list_projects_blocking()
        .unwrap()
        .into_iter()
        .find(|project| project.id == project_id)
        .unwrap();
    let mut runner = CodexAgentRunner::with_command(
        state_dir,
        Duration::from_secs(30),
        PathBuf::from(std::env::var_os("CLT_TEST_OWNER_CODEX").unwrap()),
    );
    runner.heartbeat_interval = Duration::from_millis(50);
    runner
        .run_project(
            &project,
            AgentTaskSelection::ResumeSession,
            Some(&std::env::var("CLT_TEST_OWNER_SESSION_ID").unwrap()),
            &std::env::var("CLT_TEST_OWNER_LEASE_HOLDER").unwrap(),
            None,
            &new_agent_shutdown_signal(),
        )
        .unwrap();
}

#[cfg(unix)]
fn assert_crashed_automated_owner_control_is_reaped(action: AgentSessionControlAction) {
    use std::os::unix::fs::PermissionsExt;

    let suffix = match action {
        AgentSessionControlAction::Stop => "stop",
        AgentSessionControlAction::Interrupt => "interrupt",
    };
    let root = temp_root(&format!("automated-owner-crash-{suffix}"));
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "crash-owner-project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let session_id = format!("session-owner-crash-{suffix}");
    store
        .set_session_control_state_blocking(
            project.id,
            &session_id,
            AgentSessionControlState::ResumeRequested,
        )
        .unwrap();
    let lease_holder = format!("crash-owner-lease-{suffix}");
    assert!(
        store
            .try_acquire_lease_blocking(
                project.id,
                &lease_holder,
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap()
    );

    let launch_marker = root.join("codex-launches");
    let fake_codex = root.join("fake-codex");
    fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\nprintf 'launched\\n' >> '{}'\ntrap '' TERM HUP\nwhile :; do sleep 1; done\n",
                launch_marker.display()
            ),
        )
        .unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();

    let executable = std::env::current_exe().unwrap();
    let mut owner = Command::new(executable)
        .arg("--exact")
        .arg("runner::tests::automated_runner_owner_process_entry")
        .arg("--nocapture")
        .env("CLT_TEST_AUTOMATED_RUNNER_OWNER", "1")
        .env("CLT_TEST_OWNER_STATE_DIR", &state_dir)
        .env("CLT_TEST_OWNER_PROJECT_ID", project.id.to_string())
        .env("CLT_TEST_OWNER_CODEX", &fake_codex)
        .env("CLT_TEST_OWNER_SESSION_ID", &session_id)
        .env("CLT_TEST_OWNER_LEASE_HOLDER", &lease_holder)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let started = Instant::now();
    let running = loop {
        if let Some(control) = store
            .session_control_blocking(project.id, &session_id)
            .unwrap()
            && control.state == AgentSessionControlState::Running
            && control.child_pid.is_some()
            && launch_marker.exists()
        {
            break control;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "runner never registered and launched supervised Codex"
        );
        thread::sleep(Duration::from_millis(25));
    };
    let child_pid = running.child_pid.unwrap();
    let run_token = running.run_token.clone().unwrap();

    owner.kill().unwrap();
    owner.wait().unwrap();
    let interactive_lease = match action {
        AgentSessionControlAction::Stop => {
            let message =
                toggle_tui_codex_session_stop_at(&state_dir, project.id, &session_id).unwrap();
            assert!(message.starts_with("Stopping this Codex task session"));
            None
        }
        AgentSessionControlAction::Interrupt => Some(
            prepare_tui_codex_session_interrupt_at(
                &state_dir,
                project.id,
                &session_id,
                60,
                Duration::from_secs(10),
            )
            .unwrap(),
        ),
    };

    let expected_state = match action {
        AgentSessionControlAction::Stop => AgentSessionControlState::Stopped,
        AgentSessionControlAction::Interrupt => AgentSessionControlState::ReadyInteractive,
    };
    let started = Instant::now();
    let finalized = loop {
        let control = store
            .session_control_blocking(project.id, &session_id)
            .unwrap()
            .unwrap();
        if control.state == expected_state {
            break control;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "supervisor did not finalize crashed-owner {suffix} request; state={} ",
            control.state.database_value()
        );
        thread::sleep(Duration::from_millis(25));
    };
    assert!(finalized.child_pid.is_none());
    assert_eq!(finalized.run_token.as_deref(), Some(run_token.as_str()));
    assert_eq!(
        automated_agent_process_group_is_running(child_pid),
        Some(false)
    );
    assert_eq!(
        fs::read_to_string(&launch_marker).unwrap().lines().count(),
        1
    );
    let lease = store.lease_for_project_blocking(project.id).unwrap();
    match action {
        AgentSessionControlAction::Stop => assert!(lease.is_none()),
        AgentSessionControlAction::Interrupt => {
            let interactive_holder = &interactive_lease.as_ref().unwrap().holder;
            assert_eq!(
                finalized.interactive_holder.as_deref(),
                Some(interactive_holder.as_str())
            );
            assert_eq!(
                lease.as_ref().map(|lease| lease.holder.as_str()),
                Some(interactive_holder.as_str())
            );
        }
    }
    if let Some(lease) = interactive_lease {
        lease.release().unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn crashed_automated_owner_stop_is_completed_by_supervisor() {
    assert_crashed_automated_owner_control_is_reaped(AgentSessionControlAction::Stop);
}

#[cfg(unix)]
#[test]
fn crashed_automated_owner_interrupt_is_handed_to_interactive_holder() {
    assert_crashed_automated_owner_control_is_reaped(AgentSessionControlAction::Interrupt);
}

#[cfg(unix)]
#[test]
fn automated_supervisor_monitor_panic_still_reaps_its_codex_group() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("automated-supervisor-monitor-panic");
    fs::create_dir_all(&root).unwrap();
    let stdout_path = root.join("codex.out");
    let stderr_path = root.join("codex.err");
    let fake_codex = root.join("fake-codex");
    fs::write(
        &fake_codex,
        "#!/bin/sh\ntrap '' TERM HUP\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();

    let target = Command::new(&fake_codex);
    let supervisor_stderr = fs::File::create(&stderr_path).unwrap();
    let mut supervised = spawn_automated_session_supervisor(
        &target,
        AutomatedSupervisorSpec {
            state_dir: &root,
            project_id: 1,
            run_token: "panic-supervisor-after-launch-test",
            lease_holder: "test-holder",
            stdout_path: &stdout_path,
            stderr_path: &stderr_path,
        },
        supervisor_stderr,
    )
    .unwrap();
    supervised.control.write_all(b"x").unwrap();
    supervised.control.flush().unwrap();
    drop(supervised.control);

    let status =
        wait_for_automated_supervisor_reaped(&mut supervised.process, &mut supervised.proof)
            .unwrap();

    assert!(!status.success());
    assert_eq!(
        automated_agent_process_group_is_running(supervised.child_pid),
        Some(false)
    );
    assert!(
        fs::read_to_string(&stderr_path)
            .unwrap()
            .contains("monitor panicked; stopping its owned Codex process group")
    );

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
fn assert_disconnected_no_session_launch_recovery(mutate_checkout: bool) {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root(if mutate_checkout {
        "automated-supervisor-pre-session-mutated"
    } else {
        "automated-supervisor-pre-session-unchanged"
    });
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    add_task(&project_root, "committed task", None).unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    assert!(
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "scheduler", "99", "999")
            .unwrap()
    );
    let run_token = "inline-crash-before-session";
    assert!(reserve_test_inline_worker(
        &store,
        project.id,
        run_token,
        "scheduler",
        std::process::id(),
        "100",
    ));
    let lease_holder = agent_worker_lease_holder(run_token);
    let launch_marker = root.join("codex-launched");
    let fake_codex = root.join("fake-codex");
    fs::write(
        &fake_codex,
        format!(
            "#!/bin/sh\nprintf launched > '{}'\n",
            launch_marker.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();
    let stdout_path = root.join("codex.out");
    let stderr_path = root.join("codex.err");
    let target = Command::new(&fake_codex);
    let supervisor_stderr = fs::File::create(&stderr_path).unwrap();
    let mut supervised = spawn_automated_session_supervisor(
        &target,
        AutomatedSupervisorSpec {
            state_dir: &state_dir,
            project_id: project.id,
            run_token,
            lease_holder: &lease_holder,
            stdout_path: &stdout_path,
            stderr_path: &stderr_path,
        },
        supervisor_stderr,
    )
    .unwrap();
    let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    assert!(
        store
            .record_git_launch_state_blocking(
                project.id,
                run_token,
                AgentGitMode::Commit,
                &git_start,
                "101",
            )
            .unwrap()
    );
    assert!(
        store
            .git_launch_state_blocking(project.id, run_token)
            .unwrap()
            .is_some()
    );

    drop(supervised.control);
    wait_for_automated_supervisor_reaped(&mut supervised.process, &mut supervised.proof).unwrap();

    assert_eq!(
        automated_agent_process_group_is_running(supervised.child_pid),
        Some(false)
    );
    assert!(!launch_marker.exists());
    assert!(
        store
            .git_launch_state_blocking(project.id, run_token)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .is_none()
    );
    assert!(store.list_active_workers_blocking().unwrap().is_empty());
    let worker = store
        .list_terminal_workers_blocking()
        .unwrap()
        .into_iter()
        .find(|worker| worker.worker_token == run_token)
        .unwrap();
    assert_eq!(worker.state, "abandoned");
    assert!(
        worker
            .error
            .unwrap()
            .contains("supervised Codex process group was proven reaped")
    );
    if mutate_checkout {
        fs::write(project_root.join("unexpected.txt"), "changed after crash\n").unwrap();
        let error = prepare_agent_git_start_state_for_run(
            &store,
            &project,
            AgentTaskSelection::NextTodo,
            false,
            false,
            "replacement-run",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unconsumed launch boundary"));
        assert!(
            store
                .git_launch_state_blocking(project.id, run_token)
                .unwrap()
                .is_some()
        );
    } else {
        assert!(
            prepare_agent_git_start_state_for_run(
                &store,
                &project,
                AgentTaskSelection::NextTodo,
                false,
                false,
                "replacement-run",
            )
            .unwrap()
            .is_some()
        );
        assert!(
            store
                .git_launch_state_blocking(project.id, run_token)
                .unwrap()
                .is_none()
        );
    }

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn disconnected_no_session_reclaims_unchanged_launch_after_worker_abandonment() {
    assert_disconnected_no_session_launch_recovery(false);
}

#[cfg(unix)]
#[test]
fn disconnected_no_session_preserves_launch_after_checkout_mutation() {
    assert_disconnected_no_session_launch_recovery(true);
}

pub(crate) struct FakeAgentRunner {
    pub(crate) result: AgentRunResult,
    pub(crate) ran_projects: Mutex<Vec<PathBuf>>,
    pub(crate) delay: Duration,
}

impl FakeAgentRunner {
    pub(crate) fn new(log_root: &Path, status: &'static str) -> Self {
        Self::with_delay(log_root, status, Duration::ZERO)
    }

    pub(crate) fn with_delay(log_root: &Path, status: &'static str, delay: Duration) -> Self {
        Self {
            result: AgentRunResult {
                status,
                exit_code: Some(0),
                log_dir: log_root.join("runs/test-project"),
                stdout_path: log_root.join("runs/test-project/test.out"),
                stderr_path: log_root.join("runs/test-project/test.err"),
                summary: format!("fake {status} result"),
                codex_session_id: None,
                session_run_token: None,
                control_action: None,
            },
            ran_projects: Mutex::new(Vec::new()),
            delay,
        }
    }

    pub(crate) fn ran_project_count(&self) -> usize {
        self.ran_projects.lock().unwrap().len()
    }
}

impl AgentRunner for FakeAgentRunner {
    fn run_project(&self, request: AgentRunRequest<'_>) -> Result<AgentRunResult> {
        self.ran_projects
            .lock()
            .unwrap()
            .push(request.project.path.clone());
        thread::sleep(self.delay);
        Ok(self.result.clone())
    }
}

#[test]
fn tail_lines_returns_only_the_requested_suffix() {
    let content = "one\ntwo\nthree\nfour\n";

    assert_eq!(tail_lines(content, 2), vec!["three", "four"]);
    assert_eq!(tail_lines(content, 10), vec!["one", "two", "three", "four"]);
}

#[test]
fn agent_codex_prompt_follows_git_mode() {
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
        last_success_at: None,
        last_failure_at: None,
        last_blocked_recovery_at: None,
        failure_count: 0,
    };

    let base_prompt = build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, true, true);
    assert!(base_prompt.contains("Use the existing task-management CLI tooling: clt."));
    assert!(base_prompt.contains("Use the $clt-task-management skill"));
    assert!(base_prompt.contains("Pick the next available unblocked TODO"));
    assert!(base_prompt.contains("A dirty worktree is expected"));
    assert!(base_prompt.contains("it is not a blocker by itself"));
    assert!(base_prompt.contains("same file is not automatically a blocker"));
    assert!(base_prompt.contains("the first non-whitespace token is exactly `/goal`"));
    assert!(base_prompt.contains("without including `/goal` in the goal objective"));
    assert!(base_prompt.contains("do not create a goal when `/goal` appears anywhere except"));
    assert!(base_prompt.contains("the goal objective is missing"));
    assert!(base_prompt.contains("skip tasks whose latest dated state note is `BLOCKED"));
    assert!(!base_prompt.contains("Embedded skill fallback:"));
    assert!(!base_prompt.contains("Interrupted task recovery:"));
    assert!(!base_prompt.contains("$git-commit"));
    assert!(!base_prompt.contains("Git push:"));

    project.git_mode = AgentGitMode::Commit;
    let commit_prompt =
        build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, true, true);
    assert!(commit_prompt.contains("$git-commit"));
    assert!(commit_prompt.contains("CLT Agent <clt-agent@localhost>"));
    assert!(commit_prompt.contains("do not change Git configuration"));
    assert!(commit_prompt.contains("Pre-existing unstaged changes do not prevent a commit"));
    assert!(commit_prompt.contains("Do not require the worktree to be clean"));
    assert!(commit_prompt.contains("Do not commit when there are no tasks left"));
    assert!(commit_prompt.contains("CLT completed the scheduler-owned startup preparation"));
    assert!(commit_prompt.contains("Do not pull, fetch or otherwise synchronize"));
    assert!(!commit_prompt.contains("Git push:"));

    project.git_mode = AgentGitMode::CommitAndPush;
    let push_prompt = build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, true, true);
    assert!(push_prompt.contains("Git commit:"));
    assert!(push_prompt.contains("Git push:"));
    assert!(!push_prompt.contains("git pull --no-rebase --autostash"));
    assert!(push_prompt.contains("Do not run `git push`"));
    assert!(push_prompt.contains("CLT proves the sealed local commit"));
    assert!(push_prompt.contains("single intended push URL"));
    assert!(push_prompt.contains("remains PUSH-PENDING"));
    assert!(push_prompt.contains("Never force-push"));

    let recovery_prompt =
        build_agent_codex_prompt(&project, AgentTaskSelection::ResumeDoing, true, true);
    assert!(recovery_prompt.contains("Interrupted task recovery:"));
    assert!(recovery_prompt.contains("Resume and finish exactly one existing doing task."));
    assert!(recovery_prompt.contains("Do not pick or move a TODO task"));

    let exact_recovery_prompt =
        build_agent_codex_prompt(&project, AgentTaskSelection::ResumeSession, true, true);
    assert!(exact_recovery_prompt.contains("Interactive handoff recovery:"));
    assert!(exact_recovery_prompt.contains("next unfinished substantive step"));
    assert!(exact_recovery_prompt.contains("claimed completion is not proof"));
    assert!(exact_recovery_prompt.contains("durable changes actually exist"));
    assert!(exact_recovery_prompt.contains("response-only task"));
    assert!(exact_recovery_prompt.contains("any requested durable output"));
    assert!(exact_recovery_prompt.contains("Do not select another Todo or Backlog task"));

    let blocked_prompt =
        build_agent_codex_prompt(&project, AgentTaskSelection::RecoverBlocked, true, true);
    assert!(blocked_prompt.contains("Blocked-task monitor:"));
    assert!(blocked_prompt.contains("at least one blocked task in todo or doing"));
    assert!(blocked_prompt.contains("before starting fresh Todo work"));
    assert!(blocked_prompt.contains("whether the recorded blocking conditions still exist"));
    assert!(blocked_prompt.contains("blocked task from todo or doing"));
    assert!(blocked_prompt.contains("Update the existing task; do not create a replacement"));
    assert!(blocked_prompt.contains("`UNBLOCKED YYYY-MM-DD:` note"));
    assert!(blocked_prompt.contains("Stop after handling that one blocked task"));
    assert!(!blocked_prompt.contains("Interrupted task recovery:"));
}

#[test]
fn agent_git_identity_is_scoped_to_commit_modes() {
    let mut off_command = Command::new("codex");
    configure_agent_git_identity(&mut off_command, AgentGitMode::Off);
    assert!(off_command.get_envs().next().is_none());

    for mode in [AgentGitMode::Commit, AgentGitMode::CommitAndPush] {
        let mut command = Command::new("codex");
        configure_agent_git_identity(&mut command, mode);

        for (key, expected) in [
            ("GIT_AUTHOR_NAME", AGENT_GIT_IDENTITY_NAME),
            ("GIT_AUTHOR_EMAIL", AGENT_GIT_IDENTITY_EMAIL),
            ("GIT_COMMITTER_NAME", AGENT_GIT_IDENTITY_NAME),
            ("GIT_COMMITTER_EMAIL", AGENT_GIT_IDENTITY_EMAIL),
        ] {
            let value = command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new(key))
                .and_then(|(_, value)| value)
                .and_then(OsStr::to_str);
            assert_eq!(value, Some(expected), "unexpected value for {key}");
        }
    }
}

#[test]
fn agent_codex_prompt_embeds_only_missing_required_skills() {
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
        last_success_at: None,
        last_failure_at: None,
        last_blocked_recovery_at: None,
        failure_count: 0,
    };

    let base_prompt =
        build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, false, false);
    assert!(base_prompt.contains("<name>clt-task-management</name>"));
    assert!(base_prompt.contains("# Skills: Project Task Management with `clt`"));
    assert!(!base_prompt.contains("<name>git-commit</name>"));

    project.git_mode = AgentGitMode::Commit;
    let commit_prompt =
        build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, true, false);
    assert!(!commit_prompt.contains("<name>clt-task-management</name>"));
    assert!(commit_prompt.contains("<name>git-commit</name>"));
    assert!(commit_prompt.contains("# Git Commit Workflow"));
}

#[test]
fn agent_skill_lookup_uses_frontmatter_name() {
    let root = temp_root("agent-skill-lookup");
    let skills_root = root.join("skills");
    let skill_dir = skills_root.join("custom-folder-name");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: \"git-commit\"\ndescription: Test skill.\n---\n",
    )
    .unwrap();

    assert!(agent_skill_root_contains_name(&skills_root, "git-commit"));
    assert!(!agent_skill_root_contains_name(
        &skills_root,
        "clt-task-management"
    ));

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn wait_for_child_with_timeout_emits_heartbeats() {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("sleep 0.2")
        .spawn()
        .unwrap();
    let mut heartbeats = 0;

    let result = wait_for_child_with_timeout_and_heartbeat(
        &mut child,
        Duration::from_secs(2),
        Duration::from_millis(25),
        |_| {
            heartbeats += 1;
            Ok(())
        },
        || Ok(()),
        || false,
    )
    .unwrap();

    match result {
        AgentProcessWait::Exited(status) => assert!(status.success()),
        AgentProcessWait::TimedOut(_) => panic!("child should not time out"),
        AgentProcessWait::Interrupted(_) => panic!("child should not be interrupted"),
    }
    assert!(heartbeats > 0);
}

#[cfg(unix)]
#[test]
fn wait_for_child_with_timeout_stops_child_on_shutdown() {
    let mut command = Command::new("sh");
    command.arg("-c").arg("sleep 10");
    configure_agent_child_command(&mut command);
    let mut child = command.spawn().unwrap();
    let shutdown = new_agent_shutdown_signal();
    shutdown.store(true, Ordering::SeqCst);

    let result = wait_for_child_with_timeout_and_heartbeat(
        &mut child,
        Duration::from_secs(10),
        Duration::from_millis(25),
        |_| Ok(()),
        || Ok(()),
        || shutdown.load(Ordering::SeqCst),
    )
    .unwrap();

    match result {
        AgentProcessWait::Interrupted(_) => {}
        AgentProcessWait::Exited(_) => panic!("child should be interrupted"),
        AgentProcessWait::TimedOut(_) => panic!("child should not time out"),
    }
}

#[cfg(unix)]
#[test]
fn stop_agent_child_process_kills_term_resistant_group_descendant() {
    let root = temp_root("agent-process-group-shutdown");
    fs::create_dir_all(&root).unwrap();
    let descendant_pid_path = root.join("descendant.pid");
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(
            r#"
(
    trap '' TERM
    count=0
    while [ "$count" -lt 20 ]; do
        sleep 1
        count=$((count + 1))
    done
) &
printf '%s\n' "$!" > "$1"
wait
"#,
        )
        .arg("sh")
        .arg(&descendant_pid_path);
    configure_agent_child_command(&mut command);
    let mut child = command.spawn().unwrap();
    let process_group = i32::try_from(child.id()).unwrap();

    let started = Instant::now();
    let descendant_pid = loop {
        if let Ok(raw_pid) = fs::read_to_string(&descendant_pid_path)
            && let Ok(pid) = raw_pid.trim().parse::<libc::pid_t>()
        {
            break pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "TERM-resistant descendant did not start"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert_ne!(descendant_pid, process_group);

    let status = stop_agent_child_process(&mut child).unwrap();

    assert!(status.is_some());
    assert!(!agent_process_group_exists(process_group).unwrap());
    // SAFETY: signal zero only checks whether the recorded descendant PID exists.
    assert_eq!(unsafe { libc::kill(descendant_pid, 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));

    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn stop_agent_child_process_accepts_darwin_zombie_only_group_signal_rejection() {
    let mut command = Command::new("/usr/bin/true");
    configure_agent_child_command(&mut command);
    let mut child = command.spawn().unwrap();
    let process_group = i32::try_from(child.id()).unwrap();

    let started = Instant::now();
    while !interactive_child_exited_without_reaping(&child).unwrap() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "process-group leader did not exit"
        );
        thread::sleep(Duration::from_millis(10));
    }

    // Darwin reports EPERM for a group containing only its unreaped zombie
    // leader. This is the exact rejection that cleanup must disambiguate.
    // SAFETY: the retained direct child anchors this positive process-group ID.
    assert_eq!(unsafe { libc::kill(-process_group, libc::SIGTERM) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));

    let status = stop_agent_child_process(&mut child).unwrap();

    assert!(status.is_some_and(|status| status.success()));
    assert!(!agent_process_group_exists(process_group).unwrap());
}

#[cfg(unix)]
#[test]
fn rejected_group_signal_is_accepted_only_after_reap_and_absence_proof() {
    let mut command = Command::new("/usr/bin/true");
    configure_agent_child_command(&mut command);
    let mut child = command.spawn().unwrap();
    let process_group = i32::try_from(child.id()).unwrap();

    let started = Instant::now();
    while !interactive_child_exited_without_reaping(&child).unwrap() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "process-group leader did not exit"
        );
        thread::sleep(Duration::from_millis(10));
    }

    let status = recover_rejected_agent_process_group_signal(
        &mut child,
        process_group,
        anyhow::anyhow!("synthetic group signal rejection"),
        |_| Ok(false),
    )
    .unwrap();

    assert!(status.is_some_and(|status| status.success()));
}

#[cfg(unix)]
#[test]
fn rejected_group_signal_stays_fenced_while_leader_is_live() {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg("sleep 30");
    configure_agent_child_command(&mut command);
    let mut child = command.spawn().unwrap();
    let process_group = i32::try_from(child.id()).unwrap();
    let probe_called = std::cell::Cell::new(false);

    let result = recover_rejected_agent_process_group_signal(
        &mut child,
        process_group,
        anyhow::anyhow!("synthetic group signal rejection"),
        |_| {
            probe_called.set(true);
            Ok(false)
        },
    );

    assert!(result.is_err());
    assert!(!probe_called.get());
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    child.wait().unwrap();
}

#[cfg(unix)]
#[test]
fn rejected_group_signal_stays_fenced_when_group_is_live_or_unknown() {
    for group_probe in [Ok(true), Err(anyhow::anyhow!("synthetic probe failure"))] {
        let mut command = Command::new("/usr/bin/true");
        configure_agent_child_command(&mut command);
        let mut child = command.spawn().unwrap();
        let process_group = i32::try_from(child.id()).unwrap();

        let started = Instant::now();
        while !interactive_child_exited_without_reaping(&child).unwrap() {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "process-group leader did not exit"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let result = recover_rejected_agent_process_group_signal(
            &mut child,
            process_group,
            anyhow::anyhow!("synthetic group signal rejection"),
            |_| group_probe,
        );

        assert!(result.is_err());
        assert!(child.try_wait().unwrap().is_some());
    }
}

#[cfg(unix)]
#[test]
fn interactive_exit_observation_keeps_the_group_anchored_until_descendants_stop() {
    let root = temp_root("interactive-exit-group-drain");
    fs::create_dir_all(&root).unwrap();
    let descendant_pid_path = root.join("descendant.pid");
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(
            r#"
(
    trap '' TERM
    sleep 30
) &
printf '%s\n' "$!" > "$1"
exit 0
"#,
        )
        .arg("sh")
        .arg(&descendant_pid_path);
    configure_interactive_child_command(&mut command);
    let mut child = command.spawn().unwrap();
    let process_group = i32::try_from(child.id()).unwrap();

    let started = Instant::now();
    let descendant_pid = loop {
        if let Ok(raw_pid) = fs::read_to_string(&descendant_pid_path)
            && let Ok(pid) = raw_pid.trim().parse::<libc::pid_t>()
        {
            break pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "interactive descendant did not start"
        );
        thread::sleep(Duration::from_millis(10));
    };
    while !interactive_child_exited_without_reaping(&child).unwrap() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "interactive leader did not exit"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(agent_process_group_exists(process_group).unwrap());

    let status = stop_agent_child_process(&mut child).unwrap();

    assert!(status.is_some_and(|status| status.success()));
    assert!(!agent_process_group_exists(process_group).unwrap());
    // SAFETY: signal zero only checks whether the recorded descendant PID exists.
    assert_eq!(unsafe { libc::kill(descendant_pid, 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn interactive_exec_gate_preserves_inherited_terminal_for_crossterm() {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    // SAFETY: openpty initializes both integer descriptors; null termios,
    // winsize, and name pointers request platform defaults.
    let open_result = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(
        open_result,
        0,
        "openpty failed: {}",
        io::Error::last_os_error()
    );
    // SAFETY: successful openpty returned two newly owned descriptors.
    let master = unsafe { fs::File::from_raw_fd(master_fd) };
    // SAFETY: successful openpty returned two newly owned descriptors.
    let slave = unsafe { fs::File::from_raw_fd(slave_fd) };

    let executable = std::env::current_exe().unwrap();
    let mut target = Command::new(executable);
    target
        .arg("--exact")
        .arg("runner::tests::interactive_terminal_event_source_process_entry")
        .arg("--nocapture")
        .env("CLT_TEST_INTERACTIVE_TERMINAL_SOURCE", "1");
    let mut gate = interactive_exec_gate_command(&target).unwrap();
    gate.command_mut()
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    configure_interactive_child_command(gate.command_mut());
    let (mut child, mut launch_gate) = gate.spawn().unwrap();
    launch_gate.write_all(b"x").unwrap();
    launch_gate.flush().unwrap();
    drop(launch_gate);

    let status = child.wait().unwrap();
    drop(master);
    assert!(
        status.success(),
        "terminal source probe exited with {status}"
    );
}

#[cfg(unix)]
#[test]
fn interactive_exec_gate_parent_drop_prevents_target_exec() {
    let root = temp_root("interactive-exec-gate-parent-drop");
    let launched_marker = root.join("target-launched");
    fs::create_dir_all(&root).unwrap();

    let mut target = Command::new("/bin/sh");
    target
        .arg("-c")
        .arg("printf launched > \"$1\"")
        .arg("sh")
        .arg(&launched_marker);
    let mut gate = interactive_exec_gate_command(&target).unwrap();
    configure_interactive_child_command(gate.command_mut());
    let (mut child, launch_gate) = gate.spawn().unwrap();

    // A hard guardian death closes its only writer. The gate must exit
    // without ever replacing itself with the interactive Codex target.
    drop(launch_gate);
    assert!(child.wait().unwrap().success());
    assert!(!launched_marker.exists());

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn registered_interactive_gate_dying_before_release_recovers_exact_session() {
    let root = temp_root("interactive-gate-registered-before-release");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    let launched_marker = root.join("target-launched");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let provisional_holder = "clt-interactive-pre-release-requester";
    let guardian_holder = format!("clt-interactive-worker-{}-pre-release-guardian", u32::MAX);
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

    let mut target = Command::new("/bin/sh");
    target
        .arg("-c")
        .arg("printf launched > \"$1\"")
        .arg("sh")
        .arg(&launched_marker);
    let mut gate = interactive_exec_gate_command(&target).unwrap();
    configure_interactive_child_command(gate.command_mut());
    let (mut child, launch_gate) = gate.spawn().unwrap();
    let child_pid = child.id();
    assert!(
        store
            .register_interactive_guardian_child_blocking(
                project_id,
                "session-123",
                &guardian_holder,
                child_pid,
                60,
            )
            .unwrap()
    );
    let registered = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(registered.state, AgentSessionControlState::Interactive);
    assert_eq!(registered.child_pid, Some(child_pid));
    assert_eq!(
        registered.interactive_launch_token.as_deref(),
        Some(guardian_holder.as_str())
    );
    assert!(!launched_marker.exists());

    // Simulate SIGKILL after registration but before the one-byte release.
    let orphaned_lease = store
        .lease_for_project_blocking(project_id)
        .unwrap()
        .unwrap();
    assert_eq!(orphaned_lease.holder, guardian_holder);
    drop(launch_gate);
    assert!(child.wait().unwrap().success());
    assert!(!launched_marker.exists());
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

#[cfg(unix)]
#[test]
fn stale_guardian_keeps_a_live_registered_group_fenced_then_recovers_when_gone() {
    let root = temp_root("interactive-gate-live-child-fence");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    let launched_marker = root.join("target-launched");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    let provisional_holder = "clt-interactive-live-requester";
    let guardian_holder = format!("clt-interactive-worker-{}-live-guardian", u32::MAX);
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

    let mut target = Command::new("/bin/sh");
    target
        .arg("-c")
        .arg("printf launched > \"$1\"; sleep 30")
        .arg("sh")
        .arg(&launched_marker);
    let mut gate = interactive_exec_gate_command(&target).unwrap();
    configure_interactive_child_command(gate.command_mut());
    let (mut child, mut launch_gate) = gate.spawn().unwrap();
    let child_pid = child.id();
    assert!(
        store
            .register_interactive_guardian_child_blocking(
                project_id,
                "session-123",
                &guardian_holder,
                child_pid,
                60,
            )
            .unwrap()
    );
    assert!(
        !store
            .recover_stale_interactive_guardian_blocking(
                project_id,
                "session-123",
                &guardian_holder,
                Some(child_pid.checked_add(1).unwrap()),
                InteractiveGuardianDisposition::ResumeExec,
            )
            .unwrap()
    );
    launch_gate.write_all(b"x").unwrap();
    launch_gate.flush().unwrap();
    drop(launch_gate);
    let launch_deadline = Instant::now() + Duration::from_secs(2);
    while !launched_marker.exists() && Instant::now() < launch_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(launched_marker.exists());
    assert!(child.try_wait().unwrap().is_none());
    let orphaned_lease = store
        .lease_for_project_blocking(project_id)
        .unwrap()
        .unwrap();
    assert_eq!(orphaned_lease.holder, guardian_holder);
    drop(store);

    let stale_now = agent_timestamp_seconds();
    reconcile_stale_agent_session_controls(
        &state_dir,
        project_id,
        Some(&orphaned_lease),
        false,
        stale_now,
    )
    .unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let fenced = store
        .session_control_blocking(project_id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(fenced.state, AgentSessionControlState::Interactive);
    assert_eq!(fenced.child_pid, Some(child_pid));
    assert!(child.try_wait().unwrap().is_none());
    drop(store);

    assert!(
        stop_interactive_child_process(&mut child)
            .unwrap()
            .is_some()
    );
    reconcile_stale_agent_session_controls(
        &state_dir,
        project_id,
        Some(&orphaned_lease),
        false,
        stale_now,
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

#[cfg(unix)]
#[test]
fn automated_exec_gate_parent_drop_prevents_target_exec() {
    let root = temp_root("automated-exec-gate-parent-drop");
    let launched_marker = root.join("target-launched");
    fs::create_dir_all(&root).unwrap();

    let mut target = Command::new("/bin/sh");
    target
        .arg("-c")
        .arg("printf launched > \"$1\"")
        .arg("sh")
        .arg(&launched_marker);
    let mut gate = automated_exec_gate_command(&target).unwrap();
    let mut child = gate.spawn().unwrap();

    // Dropping the parent's only writer simulates its death before session
    // registration. The helper must observe EOF and never exec the target.
    drop(child.stdin.take().unwrap());
    assert!(child.wait().unwrap().success());
    assert!(!launched_marker.exists());

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn exact_resume_runner_releases_gate_only_after_live_lease_registration() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("exact-resume-runner-launch-gate");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
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
            AgentSessionControlState::ResumeRequested,
        )
        .unwrap();
    let lease_holder = "exact-resume-live-holder";
    assert!(
        store
            .try_acquire_lease_blocking(
                project.id,
                lease_holder,
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap()
    );

    let launched_marker = root.join("fake-codex-launched");
    let fake_codex = root.join("fake-codex");
    fs::write(
        &fake_codex,
        format!(
            "#!/bin/sh\nprintf launched > \"{}\"\nprintf 'NO_TASKS_LEFT\\n'\n",
            launched_marker.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();
    let runner =
        CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);

    let wrong_holder_result = runner.run_project(
        &project,
        AgentTaskSelection::ResumeSession,
        Some("session-123"),
        "wrong-holder",
        None,
        &new_agent_shutdown_signal(),
    );
    assert!(wrong_holder_result.is_err());
    assert!(!launched_marker.exists());
    let unclaimed = store
        .session_control_blocking(project.id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(unclaimed.state, AgentSessionControlState::ResumeRequested);
    assert!(unclaimed.child_pid.is_none());
    assert!(unclaimed.run_token.is_none());

    let result = runner
        .run_project(
            &project,
            AgentTaskSelection::ResumeSession,
            Some("session-123"),
            lease_holder,
            None,
            &new_agent_shutdown_signal(),
        )
        .unwrap();
    assert_eq!(result.status, "idle");
    assert!(launched_marker.exists());
    let registered = store
        .session_control_blocking(project.id, "session-123")
        .unwrap()
        .unwrap();
    assert_eq!(registered.state, AgentSessionControlState::Running);
    assert!(registered.child_pid.is_some());
    assert_eq!(
        registered.run_token.as_deref(),
        result.session_run_token.as_deref()
    );
    assert_eq!(
        registered.stdout_path.as_deref(),
        Some(result.stdout_path.to_string_lossy().as_ref())
    );
    assert_eq!(
        registered.stderr_path.as_deref(),
        Some(result.stderr_path.to_string_lossy().as_ref())
    );

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn codex_runner_writes_logs_and_treats_no_tasks_left_as_idle() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("agent-codex-runner");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .set_model_target_reasoning_blocking("openai", "gpt-5.6-terra", Some("low"))
        .unwrap();
    store
        .register_project_blocking(&project_root, "Project With Spaces")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;

    let fake_codex = root.join("fake-codex");
    fs::write(
            &fake_codex,
            "#!/bin/sh\nprintf 'session id: session-42\\n' >&2\nprintf 'arg=%s\\n' \"$@\" >&2\nprintf 'NO_TASKS_LEFT\\n'\n",
        )
        .unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();

    let project = agent::AgentProject {
        id: project_id,
        path: project_root.clone(),
        name: "Project With Spaces".to_string(),
        enabled: true,
        git_mode: AgentGitMode::Off,
        codex_provider: Some("openai".to_string()),
        codex_model: Some("gpt-5.6-terra".to_string()),
        codex_reasoning_effort: Some("high".to_string()),
        codex_fast_enabled: true,
        last_scan_at: None,
        last_daemon_scan_status: None,
        last_daemon_scan_error: None,
        last_run_at: None,
        last_success_at: None,
        last_failure_at: None,
        last_blocked_recovery_at: None,
        failure_count: 0,
    };
    let runner =
        CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);
    let shutdown = new_agent_shutdown_signal();

    let result = runner
        .run_project(
            &project,
            AgentTaskSelection::NextTodo,
            None,
            "test-holder",
            None,
            &shutdown,
        )
        .unwrap();

    assert_eq!(result.status, "idle");
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.codex_session_id.as_deref(), Some("session-42"));
    assert!(result.log_dir.starts_with(state_dir.join("runs")));
    assert!(
        fs::read_to_string(&result.stdout_path)
            .unwrap()
            .contains(AGENT_NO_TASKS_LEFT_MARKER)
    );
    let stderr = fs::read_to_string(&result.stderr_path).unwrap();
    assert!(stderr.contains(
            "arg=--sandbox\narg=danger-full-access\narg=--ask-for-approval\narg=never\narg=--enable\narg=goals\narg=--config\narg=model_provider=\"openai\"\narg=--model\narg=gpt-5.6-terra\narg=--config\narg=model_reasoning_effort=\"high\"\narg=--enable\narg=fast_mode\narg=--config\narg=service_tier=\"fast\"\narg=exec\narg=--skip-git-repo-check\narg=-C\n"
        ));
    assert!(!stderr.contains("arg=model_reasoning_effort=\"low\"\n"));
    assert!(stderr.contains(&format!("arg={}\n", project_root.display())));

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
fn assert_connected_no_session_launch_recovery(mutate_checkout: bool) {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root(if mutate_checkout {
        "git-runner-no-session-mutated"
    } else {
        "git-runner-no-session-unchanged"
    });
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    add_task(&project_root, "committed task", None).unwrap();
    initialize_test_git_repository(&project_root);
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
    let scheduler_holder = "git-no-session-scheduler";
    assert!(
        store
            .try_acquire_lease_blocking(
                project.id,
                scheduler_holder,
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap()
    );
    let fake_codex = root.join("fake-codex");
    fs::write(&fake_codex, "#!/bin/sh\nprintf 'NO_TASKS_LEFT\\n'\n").unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();
    let runner =
        CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);
    let run_token = "git-no-session-token";
    assert!(reserve_test_inline_worker(
        &store,
        project.id,
        run_token,
        scheduler_holder,
        std::process::id(),
        &agent_timestamp(),
    ));
    let lease_holder = agent_worker_lease_holder(run_token);

    let result = runner
        .run_project(
            &project,
            AgentTaskSelection::NextTodo,
            None,
            &lease_holder,
            Some(run_token),
            &new_agent_shutdown_signal(),
        )
        .unwrap();

    assert_eq!(result.status, "idle");
    assert!(result.codex_session_id.is_none());
    assert!(
        store
            .git_launch_state_blocking(project.id, run_token)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .finalize_worker_blocking(agent::AgentWorkerFinalization {
                worker_token: run_token,
                expected_worker_pid: Some(std::process::id()),
                expected_lease_holder: &lease_holder,
                status: result.status,
                finished_at: &agent_timestamp(),
                exit_code: result.exit_code,
                log_dir: Some(result.log_dir.to_string_lossy().as_ref()),
                stdout_path: Some(result.stdout_path.to_string_lossy().as_ref()),
                stderr_path: Some(result.stderr_path.to_string_lossy().as_ref()),
                summary: Some(&result.summary),
                codex_session_id: None,
                error: None,
            })
            .unwrap()
            .is_some()
    );
    if mutate_checkout {
        fs::write(project_root.join("unexpected.txt"), "changed after reap\n").unwrap();
        let error = prepare_agent_git_start_state_for_run(
            &store,
            &project,
            AgentTaskSelection::NextTodo,
            false,
            false,
            "replacement-run",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unconsumed launch boundary"));
        assert!(
            store
                .git_launch_state_blocking(project.id, run_token)
                .unwrap()
                .is_some()
        );
    } else {
        assert!(
            prepare_agent_git_start_state_for_run(
                &store,
                &project,
                AgentTaskSelection::NextTodo,
                false,
                false,
                "replacement-run",
            )
            .unwrap()
            .is_some()
        );
        assert!(
            store
                .git_launch_state_blocking(project.id, run_token)
                .unwrap()
                .is_none()
        );
    }

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn connected_no_session_reclaims_unchanged_launch_after_worker_finalization() {
    assert_connected_no_session_launch_recovery(false);
}

#[cfg(unix)]
#[test]
fn connected_no_session_preserves_launch_after_checkout_mutation() {
    assert_connected_no_session_launch_recovery(true);
}

#[cfg(unix)]
#[test]
fn codex_runner_registers_a_marker_derived_resume_without_a_session_banner() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("agent-known-session-without-banner");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- resumed task codex:session-known\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let lease_holder = "known-session-live-holder";
    assert!(
        store
            .try_acquire_lease_blocking(
                project.id,
                lease_holder,
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap()
    );

    let launched_marker = root.join("known-session-launched");
    let fake_codex = root.join("fake-codex");
    fs::write(
        &fake_codex,
        format!(
            "#!/bin/sh\nprintf launched > \"{}\"\nprintf 'NO_TASKS_LEFT\\n'\n",
            launched_marker.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();

    let runner =
        CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);
    assert!(
        runner
            .run_project(
                &project,
                AgentTaskSelection::ResumeDoing,
                None,
                "wrong-holder",
                None,
                &new_agent_shutdown_signal(),
            )
            .is_err()
    );
    assert!(!launched_marker.exists());
    assert!(
        store
            .session_control_blocking(project.id, "session-known")
            .unwrap()
            .is_none()
    );

    let result = runner
        .run_project(
            &project,
            AgentTaskSelection::ResumeDoing,
            None,
            lease_holder,
            None,
            &new_agent_shutdown_signal(),
        )
        .unwrap();

    assert_eq!(result.codex_session_id.as_deref(), Some("session-known"));
    assert!(launched_marker.exists());
    let control = store
        .session_control_blocking(project.id, "session-known")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::Running);
    assert!(control.child_pid.is_some());
    assert!(control.run_token.is_some());

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn codex_runner_renews_its_automated_project_lease_while_running() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("agent-codex-lease-renewal");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let lease_holder = "test-holder";
    assert!(
        store
            .try_acquire_lease_blocking(project.id, lease_holder, &agent_timestamp(), "1",)
            .unwrap()
    );

    let fake_codex = root.join("fake-codex");
    fs::write(
        &fake_codex,
        "#!/bin/sh\nsleep 0.2\nprintf 'NO_TASKS_LEFT\\n'\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();

    let mut runner =
        CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);
    runner.heartbeat_interval = Duration::from_millis(20);
    runner.lease_timeout = Duration::from_secs(2);
    runner.lease_renew_interval = Duration::from_millis(50);

    let result = runner
        .run_project(
            &project,
            AgentTaskSelection::NextTodo,
            None,
            lease_holder,
            None,
            &new_agent_shutdown_signal(),
        )
        .unwrap();
    assert_eq!(result.status, "idle");

    let lease = store
        .lease_for_project_blocking(project.id)
        .unwrap()
        .unwrap();
    assert_eq!(lease.holder, lease_holder);
    assert_ne!(lease.expires_at, "1");

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn codex_runner_links_the_session_while_the_task_is_doing() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("agent-live-session-link");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    add_task(&project_root, "live task", None).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "live-link")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;

    let started_marker = root.join("fake-codex-started");
    let fake_codex = root.join("fake-codex");
    fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\nprintf 'session id: session-live\n' >&2\nprintf 'started\n' > \"{}\"\nsleep 1\nprintf 'finished\n'\n",
                started_marker.display()
            ),
        )
        .unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();

    let mut project = tui_agent_project_for_test(project_id, "live-link").project;
    project.path = project_root.clone();
    let move_root = project_root.clone();
    let move_thread = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !started_marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        move_task(&move_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
    });
    let runner =
        CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);

    let result = runner
        .run_project(
            &project,
            AgentTaskSelection::NextTodo,
            None,
            "test-holder",
            None,
            &new_agent_shutdown_signal(),
        )
        .unwrap();
    move_thread.join().unwrap();

    assert_eq!(result.codex_session_id.as_deref(), Some("session-live"));
    let task = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing)
        .unwrap()
        .remove(0);
    assert_eq!(
        codex_session_for_task(&task).as_deref(),
        Some("session-live")
    );

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn codex_runner_resolves_the_latest_clt_default_for_new_runs() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("agent-codex-runner-default");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .upsert_model_provider_blocking(&agent::AgentModelProvider {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            env_key: Some("OPENROUTER_API_KEY".to_string()),
            built_in: false,
            enabled: true,
        })
        .unwrap();
    store
        .upsert_model_target_blocking(&agent::AgentModelTarget {
            provider_id: "openrouter".to_string(),
            model_id: "anthropic/claude-sonnet-4".to_string(),
            label: "Claude Sonnet 4".to_string(),
            enabled: true,
            favorite: true,
            reasoning_effort: Some("high".to_string()),
        })
        .unwrap();
    store
        .set_model_default_blocking("openrouter", "anthropic/claude-sonnet-4")
        .unwrap();

    let fake_codex = root.join("fake-codex");
    fs::write(
        &fake_codex,
        "#!/bin/sh\nprintf 'arg=%s\\n' \"$@\" >&2\nprintf 'NO_TASKS_LEFT\\n'\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();

    let project = agent::AgentProject {
        id: 44,
        path: project_root,
        name: "Default Target Project".to_string(),
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
        last_success_at: None,
        last_failure_at: None,
        last_blocked_recovery_at: None,
        failure_count: 0,
    };
    let runner = CodexAgentRunner::with_command(state_dir, Duration::from_secs(5), fake_codex);
    let result = runner
        .run_project(
            &project,
            AgentTaskSelection::NextTodo,
            None,
            "test-holder",
            None,
            &new_agent_shutdown_signal(),
        )
        .unwrap();
    let stderr = fs::read_to_string(result.stderr_path).unwrap();

    assert!(stderr.contains("arg=model_provider=\"openrouter\"\n"));
    assert!(stderr.contains("arg=--model\narg=anthropic/claude-sonnet-4\n"));
    assert!(stderr.contains("arg=model_reasoning_effort=\"high\"\n"));

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn codex_runner_marks_shutdown_as_interrupted() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("agent-codex-runner-shutdown");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    let fake_codex = root.join("fake-codex");
    let started_marker = root.join("fake-codex-started");
    fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\nprintf 'arg=%s\\n' \"$@\" >&2\nprintf 'started\\n'\nprintf 'started\\n' > \"{}\"\nsleep 10\n",
                started_marker.display()
            ),
        )
        .unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();

    let project = agent::AgentProject {
        id: 43,
        path: project_root,
        name: "Shutdown Project".to_string(),
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
        last_success_at: None,
        last_failure_at: None,
        last_blocked_recovery_at: None,
        failure_count: 0,
    };
    let runner =
        CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(10), fake_codex);
    let shutdown = new_agent_shutdown_signal();
    let shutdown_thread_signal = Arc::clone(&shutdown);
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !started_marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        shutdown_thread_signal.store(true, Ordering::SeqCst);
    });

    let result = runner
        .run_project(
            &project,
            AgentTaskSelection::NextTodo,
            None,
            "test-holder",
            None,
            &shutdown,
        )
        .unwrap();

    assert_eq!(result.status, "interrupted");
    let stderr = fs::read_to_string(&result.stderr_path).unwrap();
    assert!(stderr.contains("arg=--enable\narg=goals\narg=--disable\narg=fast_mode\narg=exec\n"));
    assert!(stderr.contains("agent is shutting down"));

    fs::remove_dir_all(root).unwrap();
}
