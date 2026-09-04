use crate::test_support::prelude::*;
use crate::test_support::*;

#[cfg(unix)]
#[test]
fn local_process_probe_handles_current_and_unrepresentable_pids() {
    assert_eq!(local_process_is_running(std::process::id()), Some(true));
    assert_eq!(local_process_is_running(u32::MAX), Some(false));
}

#[cfg(unix)]
#[test]
fn agent_service_binary_snapshot_is_executable_and_immutable() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("agent-service-binary-snapshot");
    let state_dir = root.join("state/clt");
    let source = root.join("installed clt");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(&source, b"generation one").unwrap();
    let mut permissions = fs::metadata(&source).unwrap().permissions();
    permissions.set_mode(0o751);
    fs::set_permissions(&source, permissions).unwrap();

    let snapshot = snapshot_agent_service_binary(&state_dir, &source).unwrap();

    assert!(snapshot.starts_with(state_dir.join("worker-generations")));
    assert_eq!(snapshot.file_name(), Some(OsStr::new("clt")));
    assert_eq!(fs::read(&snapshot).unwrap(), b"generation one");
    assert_eq!(
        fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
        0o751
    );
    assert!(!snapshot.with_file_name("clt.partial").exists());

    fs::write(&source, b"generation two").unwrap();
    assert_eq!(fs::read(&snapshot).unwrap(), b"generation one");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scheduler_service_restart_refuses_a_live_legacy_owned_run() {
    let root = temp_root("agent-legacy-restart-fence");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let holder = format!("clt-agent-{}", std::process::id());
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

    let error = ensure_no_live_legacy_agent_runs(&store).unwrap_err();
    assert!(error.to_string().contains("legacy in-process run"));
    assert!(store.release_lease_blocking(project.id, &holder).unwrap());
    ensure_no_live_legacy_agent_runs(&store).unwrap();

    let scheduler_holder = agent_scheduler_lease_holder();
    assert!(
        store
            .try_acquire_lease_blocking(
                project.id,
                &scheduler_holder,
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap()
    );
    ensure_no_live_legacy_agent_runs(&store).unwrap();

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_binary_generation_gc_preserves_scheduler_and_active_worker_snapshots() {
    let root = temp_root("agent-generation-gc");
    let state_dir = root.join("state/clt");
    let generation_root = state_dir.join("worker-generations");
    let scheduler_dir = generation_root.join("scheduler");
    let worker_dir = generation_root.join("active-worker");
    let stale_dir = generation_root.join("stale");
    for directory in [&scheduler_dir, &worker_dir, &stale_dir] {
        fs::create_dir_all(directory).unwrap();
        fs::write(directory.join("clt"), b"snapshot").unwrap();
    }
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
    assert!(
        store
            .reserve_worker_blocking(agent::AgentWorkerReservation {
                project_id: project.id,
                worker_token: "generation-token",
                expected_lease_holder: "scheduler",
                max_active_workers: 12,
                protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                service_label: "clt-worker-generation-token",
                binary_path: &worker_dir.join("clt"),
                command_arguments: "[]",
                path_env: OsStr::new("/usr/bin:/bin"),
                codex_path: None,
                task_selection: "next_todo",
                resume_session_id: None,
                created_at: "101",
            })
            .unwrap()
    );

    garbage_collect_agent_binary_generations(&state_dir, &store, &scheduler_dir.join("clt"))
        .unwrap();
    assert!(scheduler_dir.exists());
    assert!(worker_dir.exists());
    assert!(!stale_dir.exists());

    assert!(
        store
            .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: "generation-token",
                expected_state: AGENT_WORKER_STATE_DISPATCHING,
                expected_worker_pid: None,
                expected_heartbeat_at: Some("101"),
                finished_at: "102",
                error: "test cleanup",
                permitted_successor_holder: None,
            })
            .unwrap()
    );
    garbage_collect_agent_binary_generations(&state_dir, &store, &scheduler_dir.join("clt"))
        .unwrap();
    assert!(scheduler_dir.exists());
    assert!(!worker_dir.exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn launchd_plist_runs_agent_daemon_with_state_dir() {
    let service_env = AgentServiceEnvironment {
        codex_path_override: None,
        path: OsString::from("/Users/alex/bin:/usr/bin:/bin"),
    };
    let plist = launchd_plist_content(
        Path::new("/Applications/CLT & Tools/clt"),
        Path::new("/Users/alex/Library/Application Support/clt"),
        &service_env,
    );

    assert!(plist.contains("<string>com.alpinevibrations.clt.agent</string>"));
    assert!(plist.contains("<string>/Applications/CLT &amp; Tools/clt</string>"));
    assert!(plist.contains("<string>agent</string>"));
    assert!(plist.contains("<string>daemon</string>"));
    assert!(plist.contains("<key>CLT_AGENT_STATE_DIR</key>"));
    assert!(plist.contains("<string>/Users/alex/Library/Application Support/clt</string>"));
    assert!(plist.contains("<key>CLT_AGENT_DAEMON_MODE</key>"));
    assert!(plist.contains("<string>service</string>"));
    assert!(!plist.contains("<key>CLT_AGENT_CODEX_PATH</key>"));
    assert!(!plist.lines().any(|line| line.trim() == "\\"));
    assert!(plist.contains("<key>PATH</key>"));
    assert!(plist.contains("<string>/Users/alex/bin:/usr/bin:/bin</string>"));
    assert!(plist.contains(
        "<string>/Users/alex/Library/Application Support/clt/agent-service.out</string>"
    ));
    assert!(plist.contains(
        "<string>/Users/alex/Library/Application Support/clt/agent-service.err</string>"
    ));
}

#[test]
fn launchd_worker_plist_runs_one_pinned_worker_generation() {
    let service_env = AgentServiceEnvironment {
        codex_path_override: Some(PathBuf::from("/Users/alex/Codex & Tools/codex")),
        path: OsString::from("/Users/alex/bin & tools:/usr/bin:/bin"),
    };
    let spec = AgentWorkerLaunchSpec {
        state_dir: PathBuf::from("/Users/alex/Library/Application Support/clt & worker state"),
        executable: PathBuf::from("/Users/alex/CLT & Tools/generations/one/clt"),
        worker_token: "1234567890-000000001-p42-s7".to_string(),
        project_id: 42,
        task_selection: AgentTaskSelection::ResumeSession,
        resume_session_id: Some("01234567-89ab-cdef-0123-456789abcdef".to_string()),
        service_label: "com.alpinevibrations.clt.agent.worker.1234567890-000000001-p42-s7"
            .to_string(),
        command_arguments: None,
        service_env: service_env.clone(),
    };

    let plist = launchd_worker_plist_content(&spec, &service_env);

    assert!(plist.contains(&format!("<string>{}</string>", spec.service_label)));
    assert!(plist.contains("<string>/Users/alex/CLT &amp; Tools/generations/one/clt</string>"));
    for argument in [
        "--local",
        "agent",
        "worker",
        "--state-dir",
        "--project-id",
        "42",
        "--worker-token",
        "1234567890-000000001-p42-s7",
        "--task-selection",
        "resume_session",
        "--resume-session-id",
        "01234567-89ab-cdef-0123-456789abcdef",
    ] {
        assert!(
            plist.contains(&format!("<string>{argument}</string>")),
            "missing worker argument {argument}"
        );
    }
    assert!(plist.contains(
        "<string>/Users/alex/Library/Application Support/clt &amp; worker state</string>"
    ));
    assert!(plist.contains("<key>CLT_AGENT_CODEX_PATH</key>"));
    assert!(plist.contains("<string>/Users/alex/Codex &amp; Tools/codex</string>"));
    assert!(plist.contains("<string>/Users/alex/bin &amp; tools:/usr/bin:/bin</string>"));
    assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
    assert!(plist.contains("<key>KeepAlive</key>\n  <false/>"));
    assert!(plist.contains("<key>ProcessType</key>\n  <string>Standard</string>"));
    assert!(plist.contains(
            "<string>/Users/alex/Library/Application Support/clt &amp; worker state/workers/1234567890-000000001-p42-s7/worker.out</string>"
        ));
    assert!(plist.contains(
            "<string>/Users/alex/Library/Application Support/clt &amp; worker state/workers/1234567890-000000001-p42-s7/worker.err</string>"
        ));
}

#[test]
fn launchd_user_domain_uses_non_root_uid() {
    assert_eq!(launchd_user_domain_for_uid(" 501\n").unwrap(), "gui/501");
}

#[test]
fn launchd_user_domain_rejects_root_uid() {
    let err = launchd_user_domain_for_uid("0").unwrap_err().to_string();

    assert!(err.contains("Refusing to manage the macOS launchd user agent as root"));
    assert!(err.contains("without sudo"));
}

#[test]
fn launchd_user_domain_rejects_invalid_uid() {
    let err = launchd_user_domain_for_uid("not-a-uid")
        .unwrap_err()
        .to_string();

    assert!(err.contains("invalid user id"));
}

#[test]
fn systemd_unit_runs_agent_daemon_with_state_dir() {
    let service_env = AgentServiceEnvironment {
        codex_path_override: None,
        path: OsString::from("/home/alex/bin:/usr/bin:/bin"),
    };
    let unit = systemd_unit_content(
        Path::new("/home/alex/bin/clt with spaces"),
        Path::new("/home/alex/.local/state/clt"),
        &service_env,
    );

    assert!(unit.contains("Description=CLT Codex agent"));
    assert!(unit.contains("ExecStart=\"/home/alex/bin/clt with spaces\" agent daemon"));
    assert!(unit.contains("Environment=\"CLT_AGENT_STATE_DIR=/home/alex/.local/state/clt\""));
    assert!(unit.contains("Environment=\"CLT_AGENT_DAEMON_MODE=service\""));
    assert!(!unit.contains("CLT_AGENT_CODEX_PATH"));
    assert!(unit.contains("Environment=\"PATH=/home/alex/bin:/usr/bin:/bin\""));
    assert!(unit.contains("Restart=always"));
    assert!(unit.contains("WantedBy=default.target"));
}

#[test]
fn systemd_worker_run_uses_a_separate_one_shot_user_service() {
    let service_env = AgentServiceEnvironment {
        codex_path_override: Some(PathBuf::from("/home/alex/Codex Tools/codex")),
        path: OsString::from("/home/alex/bin with spaces:/usr/bin:/bin"),
    };
    let spec = AgentWorkerLaunchSpec {
        state_dir: PathBuf::from("/home/alex/.local/state/clt worker"),
        executable: PathBuf::from("/home/alex/.local/state/clt generations/one/clt"),
        worker_token: "1234567890-000000001-p42-s7".to_string(),
        project_id: 42,
        task_selection: AgentTaskSelection::ResumeSession,
        resume_session_id: Some("01234567-89ab-cdef-0123-456789abcdef".to_string()),
        service_label: "clt-agent-worker-1234567890-000000001-p42-s7.service".to_string(),
        command_arguments: None,
        service_env: service_env.clone(),
    };

    let arguments = systemd_worker_run_args(&spec, &service_env)
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert_eq!(
        arguments,
        vec![
            "--user",
            "--unit=clt-agent-worker-1234567890-000000001-p42-s7.service",
            "--collect",
            "--service-type=exec",
            "--property=Restart=no",
            "--property=KillMode=control-group",
            "--setenv=CLT_AGENT_STATE_DIR=/home/alex/.local/state/clt worker",
            "--setenv=PATH=/home/alex/bin with spaces:/usr/bin:/bin",
            "--setenv=CLT_AGENT_CODEX_PATH=/home/alex/Codex Tools/codex",
            "--",
            "/home/alex/.local/state/clt generations/one/clt",
            "--local",
            "agent",
            "worker",
            "--state-dir",
            "/home/alex/.local/state/clt worker",
            "--project-id",
            "42",
            "--worker-token",
            "1234567890-000000001-p42-s7",
            "--task-selection",
            "resume_session",
            "--resume-session-id",
            "01234567-89ab-cdef-0123-456789abcdef",
        ]
    );
}

#[test]
fn service_definitions_preserve_explicit_codex_path_overrides() {
    let launchd_env = AgentServiceEnvironment {
        codex_path_override: Some(PathBuf::from("/Users/alex/bin/Codex & Tools/codex")),
        path: OsString::from("/Users/alex/bin:/usr/bin:/bin"),
    };
    let plist = launchd_plist_content(
        Path::new("/Applications/CLT/clt"),
        Path::new("/Users/alex/Library/Application Support/clt"),
        &launchd_env,
    );
    assert!(plist.contains("<key>CLT_AGENT_CODEX_PATH</key>"));
    assert!(plist.contains("<string>/Users/alex/bin/Codex &amp; Tools/codex</string>"));

    let systemd_env = AgentServiceEnvironment {
        codex_path_override: Some(PathBuf::from("/home/alex/bin/codex with spaces")),
        path: OsString::from("/home/alex/bin:/usr/bin:/bin"),
    };
    let unit = systemd_unit_content(
        Path::new("/home/alex/bin/clt"),
        Path::new("/home/alex/.local/state/clt"),
        &systemd_env,
    );
    assert!(unit.contains("Environment=\"CLT_AGENT_CODEX_PATH=/home/alex/bin/codex with spaces\""));
}

#[test]
fn systemd_start_restarts_service_after_reloading_unit() {
    assert_eq!(
        systemd_start_command_args(),
        [
            &["--user", "daemon-reload"][..],
            &["--user", "enable", "clt-agent.service"][..],
            &["--user", "restart", "clt-agent.service"][..],
        ]
    );
}

#[test]
fn systemd_user_runtime_dir_uses_numeric_uid() {
    assert_eq!(
        systemd_user_runtime_dir_for_uid(" 1000\n").unwrap(),
        PathBuf::from("/run/user/1000")
    );
}

#[test]
fn systemd_user_runtime_dir_rejects_invalid_uid() {
    let err = systemd_user_runtime_dir_for_uid("not-a-uid")
        .unwrap_err()
        .to_string();

    assert!(err.contains("invalid user id"));
}

#[test]
fn systemd_user_command_recovers_missing_runtime_dir() {
    let mut command = Command::new("systemctl");

    configure_systemd_user_command_with_runtime_dir(&mut command, None, "1000").unwrap();

    let configured_runtime_dir = command
        .get_envs()
        .find(|(key, _)| *key == OsStr::new(XDG_RUNTIME_DIR_ENV))
        .and_then(|(_, value)| value);
    assert_eq!(configured_runtime_dir, Some(OsStr::new("/run/user/1000")));
}

#[test]
fn systemd_user_command_preserves_inherited_runtime_dir() {
    let mut command = Command::new("systemctl");

    configure_systemd_user_command_with_runtime_dir(
        &mut command,
        Some(OsStr::new("/custom/runtime")),
        "1000",
    )
    .unwrap();

    assert!(
        command
            .get_envs()
            .all(|(key, _)| key != OsStr::new(XDG_RUNTIME_DIR_ENV))
    );
}

#[test]
fn systemd_run_user_service_command_receives_runtime_dir() {
    let command = service_command_with_systemd_user_configurer(
        "systemd-run",
        &[
            "--user",
            "--unit=clt-agent-worker-test.service",
            "--",
            "/bin/true",
        ],
        |command| configure_systemd_user_command_with_runtime_dir(command, None, "1000"),
    )
    .unwrap();

    assert_eq!(command.get_program(), OsStr::new("systemd-run"));
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        vec![
            OsStr::new("--user"),
            OsStr::new("--unit=clt-agent-worker-test.service"),
            OsStr::new("--"),
            OsStr::new("/bin/true"),
        ]
    );
    let configured_runtime_dir = command
        .get_envs()
        .find(|(key, _)| *key == OsStr::new(XDG_RUNTIME_DIR_ENV))
        .and_then(|(_, value)| value);
    assert_eq!(configured_runtime_dir, Some(OsStr::new("/run/user/1000")));
}

#[cfg(unix)]
#[test]
fn default_service_codex_command_is_left_for_path_lookup() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("agent-codex-path-lookup");
    fs::create_dir_all(&root).unwrap();
    let codex = root.join("codex");
    fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&codex, permissions).unwrap();

    let resolved = resolve_agent_codex_path_override_for_service(None, root.as_os_str()).unwrap();

    assert_eq!(resolved, None);
    validate_agent_codex_path(Path::new("codex"), root.as_os_str()).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn packaged_native_codex_binary_is_preferred_over_npm_shim() {
    use std::os::unix::fs::PermissionsExt;

    let Some((platform_package, target_triple, binary_name)) = codex_native_package() else {
        return;
    };

    let root = temp_root("agent-native-codex");
    let codex_js = root.join("node_modules/@openai/codex/bin/codex.js");
    let native_codex = root
        .join("node_modules/@openai")
        .join(platform_package)
        .join("vendor")
        .join(target_triple)
        .join("bin")
        .join(binary_name);
    fs::create_dir_all(codex_js.parent().unwrap()).unwrap();
    fs::create_dir_all(native_codex.parent().unwrap()).unwrap();
    fs::write(&codex_js, "#!/usr/bin/env node\n").unwrap();
    fs::write(&native_codex, "#!/bin/sh\n").unwrap();
    let mut permissions = fs::metadata(&native_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&native_codex, permissions).unwrap();

    assert_eq!(
        prefer_packaged_native_codex_binary(&codex_js),
        fs::canonicalize(&native_codex).unwrap()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn systemd_user_unit_path_prefers_xdg_config_home() {
    let unit_path = systemd_user_unit_path(
        Some(PathBuf::from("/tmp/config")),
        Some(PathBuf::from("/home/alex")),
    )
    .unwrap();

    assert_eq!(
        unit_path,
        PathBuf::from("/tmp/config/systemd/user/clt-agent.service")
    );
}

#[test]
fn systemd_user_unit_path_falls_back_to_home_config() {
    let unit_path = systemd_user_unit_path(None, Some(PathBuf::from("/home/alex"))).unwrap();

    assert_eq!(
        unit_path,
        PathBuf::from("/home/alex/.config/systemd/user/clt-agent.service")
    );
}
