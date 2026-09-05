use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs,
    io::{self},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};

use crate::{
    agent::{
        self, AGENT_STATE_DIR_ENV, current_agent_platform, ensure_agent_state_dir,
        open_agent_store_at,
    },
    application::{
        AGENT_CODEX_PATH_ENV, AGENT_DAEMON_MODE_ENV, AGENT_INLINE_WORKER_SERVICE_LABEL_PREFIX,
        AGENT_LAUNCHD_LABEL, AGENT_SYSTEMD_UNIT, AGENT_WORKER_GENERATION,
        AGENT_WORKER_LAUNCHD_LABEL_PREFIX, AGENT_WORKER_SYSTEMD_UNIT_PREFIX, AgentWorkerLaunchSpec,
        XDG_RUNTIME_DIR_ENV,
    },
    runner::{agent_timestamp, agent_timestamp_after, agent_timestamp_seconds},
    scheduler::{agent_lease_is_reclaimable, agent_lease_renew_interval},
    session_control::InteractiveGuardianDisposition,
};

#[cfg(not(test))]
use crate::worker::cleanup_terminal_agent_worker_services;

#[cfg(unix)]
use std::{
    os::fd::{AsRawFd, FromRawFd},
    os::unix::process::CommandExt,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentPlatform {
    Macos,
    Linux,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentServiceAction {
    Start,
    Stop,
}

#[derive(Clone, Debug)]
pub(super) struct AgentServiceEnvironment {
    pub(super) codex_path_override: Option<PathBuf>,
    pub(super) path: OsString,
}

pub(super) fn truncate_agent_service_logs(state_dir: &Path) -> Result<u64> {
    let mut truncated = 0;
    for extension in ["out", "err"] {
        let path = PathBuf::from(state_dir_service_log_path(state_dir, extension));
        if path.exists() {
            fs::File::create(&path)
                .with_context(|| format!("Failed to truncate agent service log {:?}", path))?;
            truncated += 1;
        }
    }

    Ok(truncated)
}

pub(super) fn manage_agent_service(action: AgentServiceAction) -> Result<()> {
    let state_dir = ensure_agent_state_dir()?;
    manage_agent_service_at_with(action, &state_dir, |action, state_dir, executable| {
        match current_agent_platform() {
            AgentPlatform::Macos => manage_launchd_agent(action, state_dir, executable),
            AgentPlatform::Linux => manage_systemd_agent(action, state_dir, executable),
            AgentPlatform::Other => anyhow::bail!(
                "clt agent start/stop is only supported on macOS launchd and Linux user systemd."
            ),
        }
    })
}

fn manage_agent_service_at_with(
    action: AgentServiceAction,
    state_dir: &Path,
    manage_service: impl FnOnce(AgentServiceAction, &Path, &Path) -> Result<()>,
) -> Result<()> {
    // Stopping must work even when opening Turso requires registry recovery.
    let store = if action == AgentServiceAction::Start {
        let store = open_agent_store_at(state_dir)?;
        if agent_scheduler_service_is_loaded()? {
            ensure_no_live_legacy_agent_runs(&store)?;
        }
        Some(store)
    } else {
        None
    };
    let current_executable =
        std::env::current_exe().context("Failed to resolve current clt executable")?;
    let executable = if action == AgentServiceAction::Start {
        snapshot_agent_service_binary(state_dir, &current_executable)?
    } else {
        current_executable
    };

    manage_service(action, state_dir, &executable)?;

    if let Some(store) = store {
        #[cfg(not(test))]
        cleanup_terminal_agent_worker_services(state_dir, &store, None)?;
        garbage_collect_agent_binary_generations(state_dir, &store, &executable)?;
    }
    Ok(())
}

pub(super) fn stop_agent_services_for_recovery(
    state_dir: &Path,
    manifest: &serde_json::Value,
) -> Result<()> {
    let platform = current_agent_platform();
    let launchd_domain = if platform == AgentPlatform::Macos {
        Some(launchd_user_domain()?)
    } else {
        None
    };
    stop_agent_services_for_recovery_with(
        state_dir,
        manifest,
        platform,
        launchd_domain.as_deref(),
        |program, args| {
            let output = service_command(program, args)?.output().with_context(|| {
                format!("Failed to run {}", service_command_display(program, args))
            })?;
            Ok((
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
            ))
        },
        local_process_is_running,
    )
}

fn stop_agent_services_for_recovery_with(
    state_dir: &Path,
    manifest: &serde_json::Value,
    platform: AgentPlatform,
    launchd_domain: Option<&str>,
    mut service_command: impl FnMut(&str, &[&str]) -> Result<(bool, String)>,
    mut process_is_running: impl FnMut(u32) -> Option<bool>,
) -> Result<()> {
    if manifest.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        anyhow::bail!(
            "Unsupported agent recovery manifest in {}",
            state_dir.display()
        );
    }
    let tables = manifest
        .get("tables")
        .and_then(serde_json::Value::as_object)
        .context("Agent recovery manifest has no runtime tables")?;
    let workers = tables
        .get("agent_workers")
        .and_then(serde_json::Value::as_array)
        .context("Agent recovery manifest has no worker identities")?;
    let controls = tables
        .get("session_controls")
        .and_then(serde_json::Value::as_array)
        .context("Agent recovery manifest has no session control identities")?;
    let mut service_labels = Vec::new();
    let mut process_ids = HashSet::new();
    // Validate the complete manifest before issuing any service commands.
    for worker in workers {
        let state = worker
            .get("state")
            .and_then(serde_json::Value::as_str)
            .context("Agent recovery worker has no state")?;
        match state {
            "completed" | "abandoned" | "superseded" => continue,
            "dispatching" | "running" | "finalizing" => {}
            _ => anyhow::bail!("Agent recovery worker has unknown state {state:?}"),
        }
        let token = worker
            .get("worker_token")
            .and_then(serde_json::Value::as_str)
            .context("Agent recovery worker has no generation token")?;
        if token.is_empty()
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            anyhow::bail!("Agent recovery worker has an invalid generation token");
        }
        let label = worker
            .get("service_label")
            .and_then(serde_json::Value::as_str)
            .context("Agent recovery worker has no service identity")?;
        if label != format!("{AGENT_INLINE_WORKER_SERVICE_LABEL_PREFIX}{token}") {
            if label != agent_worker_service_label(platform, token)? {
                anyhow::bail!(
                    "Refusing to stop unverified service {label:?} for agent worker {token}"
                );
            }
            service_labels.push(label);
        }
        if let Some(pid) = recovery_manifest_pid(worker, "worker_pid")? {
            process_ids.insert(pid);
        }
    }
    for control in controls {
        if let Some(pid) = recovery_manifest_pid(control, "child_pid")? {
            process_ids.insert(pid);
        }
    }

    let mut stop_service = |label: &str| -> Result<()> {
        match platform {
            AgentPlatform::Macos => {
                let domain = launchd_domain.context("Missing launchd recovery service domain")?;
                let target = format!("{domain}/{label}");
                if service_command("launchctl", &["print", &target])?.0
                    && (!service_command("launchctl", &["bootout", &target])?.0
                        || service_command("launchctl", &["print", &target])?.0)
                {
                    anyhow::bail!("Agent recovery could not stop service {label}");
                }
            }
            AgentPlatform::Linux => {
                let (success, load_state) = service_command(
                    "systemctl",
                    &["--user", "show", "--property=LoadState", "--value", label],
                )?;
                match load_state.trim() {
                    "not-found" => return Ok(()),
                    "loaded" | "masked" if success => {}
                    _ => anyhow::bail!("Agent recovery could not inspect service {label}"),
                }
                // Stop loaded units even when inactive so a scheduled restart
                // cannot race the registry's exclusive recovery lock.
                if !service_command("systemctl", &["--user", "stop", label])?.0
                    || service_command("systemctl", &["--user", "is-active", "--quiet", label])?.0
                {
                    anyhow::bail!("Agent recovery could not stop service {label}");
                }
            }
            AgentPlatform::Other => {}
        }
        Ok(())
    };
    stop_service(match platform {
        AgentPlatform::Macos => AGENT_LAUNCHD_LABEL,
        AgentPlatform::Linux => AGENT_SYSTEMD_UNIT,
        AgentPlatform::Other => "",
    })?;
    for label in service_labels {
        stop_service(label)?;
    }
    for pid in process_ids {
        if process_is_running(pid) != Some(false) {
            anyhow::bail!(
                "Agent recovery requires worker/session process {pid} to exit. Stop its owning CLT session or wait for it to finish, then retry; registry at {} is unchanged.",
                state_dir.display()
            );
        }
    }
    Ok(())
}

fn recovery_manifest_pid(record: &serde_json::Value, field: &str) -> Result<Option<u32>> {
    let value = record
        .get(field)
        .with_context(|| format!("Agent recovery manifest is missing {field}"))?;
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .map(Some)
        .with_context(|| format!("Agent recovery manifest has invalid {field}"))
}

pub(super) fn ensure_agent_processes_stopped_for_recovery(
    state_dir: &Path,
    manifest: &serde_json::Value,
) -> Result<()> {
    for (table, field) in [
        ("agent_workers", "worker_pid"),
        ("session_controls", "child_pid"),
    ] {
        let records = manifest["tables"][table]
            .as_array()
            .with_context(|| format!("Agent recovery manifest has no {table}"))?;
        for record in records {
            if table == "agent_workers" {
                match record["state"].as_str() {
                    Some("completed" | "abandoned" | "superseded") => continue,
                    Some("dispatching" | "running" | "finalizing") => {}
                    _ => anyhow::bail!("Agent recovery worker has unknown state"),
                }
                anyhow::ensure!(
                    recovery_manifest_pid(record, field)?.is_some(),
                    "Automatic registry recovery cannot verify a worker that has not registered its PID; run clt agent recover"
                );
            } else if let Some(holder) = record["interactive_holder"].as_str() {
                anyhow::ensure!(
                    InteractiveGuardianDisposition::guardian_process_is_proven_dead(holder),
                    "Automatic registry recovery is waiting for interactive guardian {holder} to exit"
                );
            }
            if let Some(pid) = recovery_manifest_pid(record, field)? {
                anyhow::ensure!(
                    local_process_is_running(pid) == Some(false),
                    "Automatic registry recovery is waiting for worker/session process {pid} to exit (state: {})",
                    state_dir.display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

pub(super) fn agent_scheduler_service_is_loaded() -> Result<bool> {
    match current_agent_platform() {
        AgentPlatform::Macos => {
            let target = format!("{}/{}", launchd_user_domain()?, AGENT_LAUNCHD_LABEL);
            run_service_command_optional("launchctl", &["print", &target])
        }
        AgentPlatform::Linux => run_service_command_optional(
            "systemctl",
            &["--user", "is-active", "--quiet", AGENT_SYSTEMD_UNIT],
        ),
        AgentPlatform::Other => Ok(false),
    }
}

pub(super) fn ensure_no_live_legacy_agent_runs(store: &agent::TursoAgentStore) -> Result<()> {
    let now = agent_timestamp_seconds();
    let live_legacy = store
        .list_active_leases_blocking(&agent_timestamp())?
        .into_iter()
        .filter(|lease| lease.holder.starts_with("clt-agent-"))
        .filter(|lease| !agent_lease_is_reclaimable(lease, false, now))
        .collect::<Vec<_>>();
    if !live_legacy.is_empty() {
        let projects = live_legacy
            .iter()
            .map(|lease| lease.project_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "Refusing to restart or stop the scheduler while {} legacy in-process run(s) are active ({projects}). Let this one-time pre-independent-worker generation finish, then retry.",
            live_legacy.len()
        );
    }
    Ok(())
}

pub(super) fn garbage_collect_agent_binary_generations(
    state_dir: &Path,
    store: &agent::TursoAgentStore,
    scheduler_executable: &Path,
) -> Result<()> {
    let generation_root = state_dir.join("worker-generations");
    if !generation_root.exists() {
        return Ok(());
    }
    let mut preserved = HashSet::new();
    if let Some(parent) = scheduler_executable.parent() {
        preserved.insert(parent.to_path_buf());
    }
    for worker in store.list_active_workers_blocking()? {
        if let Some(parent) = worker.binary_path.parent() {
            preserved.insert(parent.to_path_buf());
        }
    }
    for entry in fs::read_dir(&generation_root).with_context(|| {
        format!("Failed to read agent binary generations at {generation_root:?}")
    })? {
        let entry = entry.context("Failed to read an agent binary generation entry")?;
        let path = entry.path();
        if entry
            .file_type()
            .context("Failed to inspect an agent binary generation entry")?
            .is_dir()
            && !preserved.contains(&path)
        {
            fs::remove_dir_all(&path).with_context(|| {
                format!("Failed to remove unreferenced agent binary generation {path:?}")
            })?;
        }
    }
    Ok(())
}

pub(super) fn snapshot_agent_service_binary(state_dir: &Path, source: &Path) -> Result<PathBuf> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let sequence = AGENT_WORKER_GENERATION.fetch_add(1, Ordering::Relaxed);
    let generation = format!(
        "{}-{:09}-p{}-{sequence}",
        elapsed.as_secs(),
        elapsed.subsec_nanos(),
        std::process::id()
    );
    let generation_dir = state_dir.join("worker-generations").join(generation);
    fs::create_dir_all(&generation_dir).with_context(|| {
        format!(
            "Failed to create agent binary generation directory {:?}",
            generation_dir
        )
    })?;
    let destination = generation_dir.join("clt");
    let temporary = generation_dir.join("clt.partial");
    fs::copy(source, &temporary).with_context(|| {
        format!(
            "Failed to snapshot CLT executable {} to {}",
            source.display(),
            temporary.display()
        )
    })?;
    fs::File::open(&temporary)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("Failed to sync agent binary snapshot {:?}", temporary))?;
    fs::rename(&temporary, &destination).with_context(|| {
        format!(
            "Failed to publish agent binary generation {}",
            destination.display()
        )
    })?;
    fs::File::open(&generation_dir)
        .and_then(|directory| directory.sync_all())
        .with_context(|| {
            format!(
                "Failed to sync agent binary generation directory {:?}",
                generation_dir
            )
        })?;

    Ok(destination)
}

pub(super) fn manage_launchd_agent(
    action: AgentServiceAction,
    state_dir: &Path,
    executable: &Path,
) -> Result<()> {
    let domain = launchd_user_domain()?;
    let plist_path = launchd_plist_path(&home_dir()?);
    let service_target = format!("{domain}/{AGENT_LAUNCHD_LABEL}");

    match action {
        AgentServiceAction::Start => {
            let service_env = resolve_agent_service_environment()?;
            if let Some(parent) = plist_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create launchd directory {:?}", parent))?;
            }
            fs::write(
                &plist_path,
                launchd_plist_content(executable, state_dir, &service_env),
            )
            .with_context(|| format!("Failed to write launchd plist {:?}", plist_path))?;

            if run_service_command_optional("launchctl", &["print", &service_target])? {
                run_service_command("launchctl", &["bootout", &service_target])?;
            }
            run_service_command(
                "launchctl",
                &["bootstrap", &domain, plist_path.to_string_lossy().as_ref()],
            )?;
            run_service_command("launchctl", &["kickstart", "-k", &service_target])?;
            println!(
                "Started clt agent launchd service {} ({})",
                AGENT_LAUNCHD_LABEL,
                plist_path.display()
            );
        }
        AgentServiceAction::Stop => {
            if run_service_command_optional("launchctl", &["print", &service_target])? {
                run_service_command("launchctl", &["bootout", &service_target])?;
                println!(
                    "Stopped clt agent scheduler {}. Active workers continue independently.",
                    AGENT_LAUNCHD_LABEL
                );
            } else if plist_path.exists() {
                println!(
                    "clt agent launchd service {} was not running",
                    AGENT_LAUNCHD_LABEL
                );
            } else {
                println!(
                    "No clt agent launchd service is installed at {}",
                    plist_path.display()
                );
            }
        }
    }

    Ok(())
}

pub(super) fn manage_systemd_agent(
    action: AgentServiceAction,
    state_dir: &Path,
    executable: &Path,
) -> Result<()> {
    let unit_path = systemd_user_unit_path(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )?;

    match action {
        AgentServiceAction::Start => {
            let service_env = resolve_agent_service_environment()?;
            if let Some(parent) = unit_path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create systemd user unit directory {:?}", parent)
                })?;
            }
            fs::write(
                &unit_path,
                systemd_unit_content(executable, state_dir, &service_env),
            )
            .with_context(|| format!("Failed to write systemd unit {:?}", unit_path))?;

            for args in systemd_start_command_args() {
                run_service_command("systemctl", args)?;
            }
            println!(
                "Started clt agent systemd user service {} ({})",
                AGENT_SYSTEMD_UNIT,
                unit_path.display()
            );
        }
        AgentServiceAction::Stop => {
            let was_active = run_service_command_optional(
                "systemctl",
                &["--user", "is-active", "--quiet", AGENT_SYSTEMD_UNIT],
            )?;
            let _ =
                run_service_command_optional("systemctl", &["--user", "stop", AGENT_SYSTEMD_UNIT])?;
            if was_active {
                println!(
                    "Stopped clt agent scheduler {}. Active workers continue independently.",
                    AGENT_SYSTEMD_UNIT
                );
            } else if unit_path.exists() {
                println!("clt agent systemd user service {AGENT_SYSTEMD_UNIT} was not running");
            } else {
                println!(
                    "No clt agent systemd user service is installed at {}",
                    unit_path.display()
                );
            }
        }
    }

    Ok(())
}

pub(super) fn agent_service_status(state_dir: &Path) -> String {
    match current_agent_platform() {
        AgentPlatform::Macos => launchd_service_status(),
        AgentPlatform::Linux => systemd_service_status(),
        AgentPlatform::Other => Ok("unsupported".to_string()),
    }
    .unwrap_or_else(|err| format!("unknown ({err}); state_dir={}", state_dir.display()))
}

pub(super) fn launchd_service_status() -> Result<String> {
    let plist_path = launchd_plist_path(&home_dir()?);
    let target = format!("{}/{}", launchd_user_domain()?, AGENT_LAUNCHD_LABEL);
    if run_service_command_optional("launchctl", &["print", &target])? {
        Ok("running".to_string())
    } else if plist_path.exists() {
        Ok("installed".to_string())
    } else {
        Ok("not-installed".to_string())
    }
}

pub(super) fn systemd_service_status() -> Result<String> {
    let unit_path = systemd_user_unit_path(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )?;
    if run_service_command_optional(
        "systemctl",
        &["--user", "is-active", "--quiet", AGENT_SYSTEMD_UNIT],
    )? {
        Ok("running".to_string())
    } else if unit_path.exists() {
        Ok("installed".to_string())
    } else {
        Ok("not-installed".to_string())
    }
}

pub(super) fn restart_running_agent_service() -> Result<()> {
    match current_agent_platform() {
        AgentPlatform::Macos => {
            let target = format!("{}/{}", launchd_user_domain()?, AGENT_LAUNCHD_LABEL);
            run_service_command_quiet("launchctl", &["kickstart", "-k", &target])
        }
        AgentPlatform::Linux => {
            run_service_command_quiet("systemctl", &["--user", "restart", AGENT_SYSTEMD_UNIT])
        }
        AgentPlatform::Other => anyhow::bail!(
            "Automatic agent service recovery is only supported on macOS launchd and Linux user systemd."
        ),
    }
}

pub(super) fn systemd_start_command_args() -> [&'static [&'static str]; 3] {
    [
        &["--user", "daemon-reload"],
        &["--user", "enable", AGENT_SYSTEMD_UNIT],
        &["--user", "restart", AGENT_SYSTEMD_UNIT],
    ]
}

pub(super) fn launchd_plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{AGENT_LAUNCHD_LABEL}.plist"))
}

pub(super) fn launchd_plist_content(
    executable: &Path,
    state_dir: &Path,
    service_env: &AgentServiceEnvironment,
) -> String {
    let executable = xml_escape(&executable.display().to_string());
    let stdout_path = xml_escape(&state_dir_service_log_path(state_dir, "out"));
    let stderr_path = xml_escape(&state_dir_service_log_path(state_dir, "err"));
    let state_dir = xml_escape(&state_dir.display().to_string());
    let codex_path_environment = service_env
        .codex_path_override
        .as_ref()
        .map(|codex_path| {
            let codex_path = xml_escape(&codex_path.display().to_string());
            format!(
                "    <key>{AGENT_CODEX_PATH_ENV}</key>\n\
    <string>{codex_path}</string>\n"
            )
        })
        .unwrap_or_default();
    let path = service_env.path.to_string_lossy();
    let path = xml_escape(path.as_ref());

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{AGENT_LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
    <string>agent</string>
    <string>daemon</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>{AGENT_STATE_DIR_ENV}</key>
    <string>{state_dir}</string>
    <key>{AGENT_DAEMON_MODE_ENV}</key>
    <string>service</string>
{codex_path_environment}    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{stdout_path}</string>
  <key>StandardErrorPath</key>
  <string>{stderr_path}</string>
</dict>
</plist>
"#
    )
}

pub(super) fn systemd_user_unit_path(
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    let config_home = xdg_config_home
        .or_else(|| home.map(|home| home.join(".config")))
        .ok_or_else(|| anyhow::anyhow!("HOME is required to resolve the systemd user unit path"))?;

    Ok(config_home.join("systemd/user").join(AGENT_SYSTEMD_UNIT))
}

pub(super) fn systemd_unit_content(
    executable: &Path,
    state_dir: &Path,
    service_env: &AgentServiceEnvironment,
) -> String {
    let codex_path_environment = service_env
        .codex_path_override
        .as_ref()
        .map(|codex_path| {
            format!(
                "Environment={}\n",
                systemd_env_assignment(AGENT_CODEX_PATH_ENV, &codex_path.display().to_string())
            )
        })
        .unwrap_or_default();

    format!(
        "[Unit]\n\
Description=CLT Codex agent\n\
After=default.target\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart={} agent daemon\n\
Environment={}\n\
Environment={}\n\
{codex_path_environment}\
Environment={}\n\
Restart=always\n\
RestartSec=10\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        systemd_quote_arg(&executable.display().to_string()),
        systemd_env_assignment(AGENT_STATE_DIR_ENV, &state_dir.display().to_string()),
        systemd_env_assignment(AGENT_DAEMON_MODE_ENV, "service"),
        systemd_env_assignment("PATH", service_env.path.to_string_lossy().as_ref())
    )
}

pub(super) fn agent_worker_service_label(
    platform: AgentPlatform,
    worker_token: &str,
) -> Result<String> {
    match platform {
        AgentPlatform::Macos => Ok(format!(
            "{AGENT_WORKER_LAUNCHD_LABEL_PREFIX}.{worker_token}"
        )),
        AgentPlatform::Linux => Ok(format!(
            "{AGENT_WORKER_SYSTEMD_UNIT_PREFIX}-{worker_token}.service"
        )),
        AgentPlatform::Other => {
            anyhow::bail!("Independent agent workers require macOS launchd or Linux systemd")
        }
    }
}

pub(super) fn agent_worker_dir(state_dir: &Path, worker_token: &str) -> PathBuf {
    state_dir.join("workers").join(worker_token)
}

pub(super) fn agent_worker_launchd_plist_path(state_dir: &Path, worker_token: &str) -> PathBuf {
    agent_worker_dir(state_dir, worker_token).join("worker.plist")
}

pub(super) fn agent_worker_command_arguments(spec: &AgentWorkerLaunchSpec) -> Vec<OsString> {
    if let Some(arguments) = spec.command_arguments.as_ref() {
        return arguments.clone();
    }
    let mut arguments = vec![
        OsString::from("--local"),
        OsString::from("agent"),
        OsString::from("worker"),
        OsString::from("--state-dir"),
        spec.state_dir.as_os_str().to_os_string(),
        OsString::from("--project-id"),
        OsString::from(spec.project_id.to_string()),
        OsString::from("--worker-token"),
        OsString::from(&spec.worker_token),
        OsString::from("--task-selection"),
        OsString::from(spec.task_selection.label()),
    ];
    if let Some(session_id) = spec.resume_session_id.as_deref() {
        arguments.push(OsString::from("--resume-session-id"));
        arguments.push(OsString::from(session_id));
    }
    arguments
}

pub(super) fn launchd_worker_plist_content(
    spec: &AgentWorkerLaunchSpec,
    service_env: &AgentServiceEnvironment,
) -> String {
    let executable = xml_escape(&spec.executable.display().to_string());
    let label = xml_escape(&spec.service_label);
    let worker_dir = agent_worker_dir(&spec.state_dir, &spec.worker_token);
    let stdout_path = xml_escape(&worker_dir.join("worker.out").display().to_string());
    let stderr_path = xml_escape(&worker_dir.join("worker.err").display().to_string());
    let state_dir = xml_escape(&spec.state_dir.display().to_string());
    let path = xml_escape(service_env.path.to_string_lossy().as_ref());
    let codex_path_environment = service_env
        .codex_path_override
        .as_ref()
        .map(|codex_path| {
            format!(
                "    <key>{AGENT_CODEX_PATH_ENV}</key>\n    <string>{}</string>\n",
                xml_escape(&codex_path.display().to_string())
            )
        })
        .unwrap_or_default();
    let arguments = agent_worker_command_arguments(spec)
        .into_iter()
        .map(|argument| {
            format!(
                "    <string>{}</string>\n",
                xml_escape(argument.to_string_lossy().as_ref())
            )
        })
        .collect::<String>();

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
{arguments}  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>{AGENT_STATE_DIR_ENV}</key>
    <string>{state_dir}</string>
{codex_path_environment}    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
  <key>ProcessType</key>
  <string>Standard</string>
  <key>StandardOutPath</key>
  <string>{stdout_path}</string>
  <key>StandardErrorPath</key>
  <string>{stderr_path}</string>
</dict>
</plist>
"#
    )
}

pub(super) fn systemd_worker_run_args(
    spec: &AgentWorkerLaunchSpec,
    service_env: &AgentServiceEnvironment,
) -> Vec<OsString> {
    let mut arguments = vec![
        OsString::from("--user"),
        OsString::from(format!("--unit={}", spec.service_label)),
        OsString::from("--collect"),
        OsString::from("--service-type=exec"),
        OsString::from("--property=Restart=no"),
        OsString::from("--property=KillMode=control-group"),
        OsString::from(format!(
            "--setenv={AGENT_STATE_DIR_ENV}={}",
            spec.state_dir.display()
        )),
        OsString::from(format!(
            "--setenv=PATH={}",
            service_env.path.to_string_lossy()
        )),
    ];
    if let Some(codex_path) = service_env.codex_path_override.as_deref() {
        arguments.push(OsString::from(format!(
            "--setenv={AGENT_CODEX_PATH_ENV}={}",
            codex_path.display()
        )));
    }
    arguments.push(OsString::from("--"));
    arguments.push(spec.executable.as_os_str().to_os_string());
    arguments.extend(agent_worker_command_arguments(spec));
    arguments
}

pub(super) fn prepare_agent_worker_service(spec: &AgentWorkerLaunchSpec) -> Result<()> {
    let worker_dir = agent_worker_dir(&spec.state_dir, &spec.worker_token);
    fs::create_dir_all(&worker_dir)
        .with_context(|| format!("Failed to create agent worker directory {:?}", worker_dir))?;
    if current_agent_platform() == AgentPlatform::Macos {
        let plist_path = agent_worker_launchd_plist_path(&spec.state_dir, &spec.worker_token);
        if !plist_path.exists() {
            let temporary = plist_path.with_extension("plist.partial");
            fs::write(
                &temporary,
                launchd_worker_plist_content(spec, &spec.service_env),
            )
            .with_context(|| format!("Failed to write agent worker plist {:?}", temporary))?;
            fs::rename(&temporary, &plist_path).with_context(|| {
                format!("Failed to publish agent worker plist {:?}", plist_path)
            })?;
        }
    }
    Ok(())
}

pub(super) fn launch_agent_worker_service(spec: &AgentWorkerLaunchSpec) -> Result<()> {
    match current_agent_platform() {
        AgentPlatform::Macos => {
            let domain = launchd_user_domain()?;
            let target = format!("{domain}/{}", spec.service_label);
            if run_service_command_optional("launchctl", &["print", &target])? {
                return Ok(());
            }
            let plist_path = agent_worker_launchd_plist_path(&spec.state_dir, &spec.worker_token);
            let plist = plist_path.to_string_lossy();
            let status = service_command("launchctl", &["bootstrap", &domain, plist.as_ref()])?
                .status()
                .with_context(|| {
                    format!("Failed to bootstrap agent worker {}", spec.service_label)
                })?;
            if status.success() || run_service_command_optional("launchctl", &["print", &target])? {
                Ok(())
            } else {
                anyhow::bail!(
                    "launchctl bootstrap failed for agent worker {} with status {}",
                    spec.service_label,
                    status
                )
            }
        }
        AgentPlatform::Linux => {
            if run_service_command_optional(
                "systemctl",
                &["--user", "is-active", "--quiet", &spec.service_label],
            )? {
                return Ok(());
            }
            let arguments = systemd_worker_run_args(spec, &spec.service_env);
            let refs = arguments
                .iter()
                .map(|argument| argument.to_string_lossy())
                .collect::<Vec<_>>();
            let refs = refs
                .iter()
                .map(|argument| argument.as_ref())
                .collect::<Vec<_>>();
            let status = service_command("systemd-run", &refs)?
                .status()
                .with_context(|| format!("Failed to launch agent worker {}", spec.service_label))?;
            if status.success()
                || run_service_command_optional(
                    "systemctl",
                    &["--user", "is-active", "--quiet", &spec.service_label],
                )?
            {
                Ok(())
            } else {
                anyhow::bail!(
                    "systemd-run failed for agent worker {} with status {}",
                    spec.service_label,
                    status
                )
            }
        }
        AgentPlatform::Other => {
            anyhow::bail!("Independent agent workers require macOS launchd or Linux systemd")
        }
    }
}

pub(super) fn resolve_agent_service_environment() -> Result<AgentServiceEnvironment> {
    let path = agent_service_path_env();
    let codex_path_override =
        resolve_agent_codex_path_override_for_service(agent_codex_path_env().as_deref(), &path)?;
    let codex_command = codex_path_override
        .as_deref()
        .unwrap_or_else(|| Path::new("codex"));
    validate_agent_codex_path(codex_command, &path)?;

    Ok(AgentServiceEnvironment {
        codex_path_override,
        path,
    })
}

pub(super) fn agent_service_path_env() -> OsString {
    std::env::var_os("PATH")
        .filter(|path| !os_value_is_blank(path.as_os_str()))
        .unwrap_or_else(|| {
            OsString::from("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        })
}

pub(super) fn resolve_agent_codex_path_override_for_service(
    configured: Option<&Path>,
    path_env: &OsStr,
) -> Result<Option<PathBuf>> {
    if let Some(configured) = configured {
        let resolved =
            resolve_agent_command_candidate(configured, path_env).with_context(|| {
                format!(
                    "Failed to resolve {}={}",
                    AGENT_CODEX_PATH_ENV,
                    configured.display()
                )
            })?;
        return Ok(Some(prefer_packaged_native_codex_binary(&resolved)));
    }

    find_executable_on_path("codex", path_env).ok_or_else(|| {
        anyhow::anyhow!(
            "Failed to find `codex` in PATH while installing the agent service. Install the Codex CLI, start the service from a shell where `codex --version` works, or set {AGENT_CODEX_PATH_ENV} to the Codex executable path."
        )
    })?;
    Ok(None)
}

#[cfg(unix)]
pub(super) fn restore_parent_terminal_after_interactive_guardian() -> Result<()> {
    let terminal = interactive_terminal_input()?;
    // SAFETY: getpgrp has no arguments and returns this CLT process's group.
    let parent_process_group = unsafe { libc::getpgrp() };
    set_terminal_foreground_process_group(&terminal, parent_process_group).with_context(|| {
        format!(
            "Failed to restore CLT terminal foreground group {parent_process_group} after the interactive guardian exited"
        )
    })
}

#[cfg(not(unix))]
pub(super) fn restore_parent_terminal_after_interactive_guardian() -> Result<()> {
    Ok(())
}

pub(super) fn agent_codex_path_env() -> Option<PathBuf> {
    std::env::var_os(AGENT_CODEX_PATH_ENV)
        .filter(|value| !os_value_is_blank(value.as_os_str()))
        .map(PathBuf::from)
}

pub(super) fn resolve_agent_command_candidate(
    candidate: &Path,
    path_env: &OsStr,
) -> Result<PathBuf> {
    if candidate.is_absolute() || path_has_separator(candidate) {
        if agent_command_is_executable(candidate) {
            return Ok(candidate.to_path_buf());
        }

        anyhow::bail!("{} is not an executable file", candidate.display());
    }

    let program = candidate.to_string_lossy();
    find_executable_on_path(program.as_ref(), path_env)
        .ok_or_else(|| anyhow::anyhow!("{} was not found in PATH", candidate.display()))
}

pub(super) fn path_has_separator(path: &Path) -> bool {
    path.components().count() > 1
}

pub(super) fn find_executable_on_path(program: &str, path_env: &OsStr) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }

    std::env::split_paths(path_env)
        .map(|dir| dir.join(program))
        .find(|candidate| agent_command_is_executable(candidate))
}

pub(super) fn agent_command_is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

pub(super) fn prefer_packaged_native_codex_binary(command: &Path) -> PathBuf {
    let Ok(canonical_command) = fs::canonicalize(command) else {
        return command.to_path_buf();
    };
    if canonical_command.file_name() != Some(OsStr::new("codex.js")) {
        return command.to_path_buf();
    }

    let Some((platform_package, target_triple, binary_name)) = codex_native_package() else {
        return command.to_path_buf();
    };
    let Some(codex_package_dir) = canonical_command.parent().and_then(Path::parent) else {
        return command.to_path_buf();
    };
    let Some(node_modules_dir) = codex_package_dir.parent().and_then(Path::parent) else {
        return command.to_path_buf();
    };

    let native_binary = node_modules_dir
        .join("@openai")
        .join(platform_package)
        .join("vendor")
        .join(target_triple)
        .join("bin")
        .join(binary_name);
    if agent_command_is_executable(&native_binary) {
        native_binary
    } else {
        command.to_path_buf()
    }
}

pub(super) fn codex_native_package() -> Option<(&'static str, &'static str, &'static str)> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some(("codex-darwin-arm64", "aarch64-apple-darwin", "codex"))
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some(("codex-darwin-x64", "x86_64-apple-darwin", "codex"))
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some(("codex-linux-arm64", "aarch64-unknown-linux-musl", "codex"))
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some(("codex-linux-x64", "x86_64-unknown-linux-musl", "codex"))
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        Some(("codex-win32-arm64", "aarch64-pc-windows-msvc", "codex.exe"))
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some(("codex-win32-x64", "x86_64-pc-windows-msvc", "codex.exe"))
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64")
    )))]
    {
        None
    }
}

pub(super) fn validate_agent_codex_path(codex_path: &Path, path_env: &OsStr) -> Result<()> {
    let output = Command::new(codex_path)
        .arg("--version")
        .env("PATH", path_env)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| {
            format!(
                "Failed to validate Codex executable {}",
                codex_path.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let detail = if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        };
        anyhow::bail!(
            "Codex executable {} failed `--version` with status {}{detail}",
            codex_path.display(),
            output.status
        );
    }

    Ok(())
}

pub(super) fn os_value_is_blank(value: &OsStr) -> bool {
    value.to_string_lossy().trim().is_empty()
}

pub(super) fn state_dir_service_log_path(state_dir: &Path, extension: &str) -> String {
    state_dir
        .join(format!("agent-service.{extension}"))
        .display()
        .to_string()
}

pub(super) fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is required for agent service management"))
}

pub(super) fn launchd_user_domain() -> Result<String> {
    launchd_user_domain_for_uid(&current_user_id()?)
}

pub(super) fn current_user_id() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("Failed to determine current user id with id -u")?;
    if !output.status.success() {
        anyhow::bail!("id -u failed with status {}", output.status);
    }

    let uid = String::from_utf8(output.stdout)
        .context("id -u produced non-UTF-8 output")?
        .trim()
        .to_string();
    if uid.is_empty() {
        anyhow::bail!("id -u produced an empty user id");
    }

    Ok(uid)
}

pub(super) fn launchd_user_domain_for_uid(uid: &str) -> Result<String> {
    let uid = uid.trim();
    if uid.is_empty() {
        anyhow::bail!("id -u produced an empty user id");
    }
    if !uid.chars().all(|ch| ch.is_ascii_digit()) {
        anyhow::bail!("id -u produced an invalid user id: {uid}");
    }
    if uid == "0" {
        let sudo_user = std::env::var("SUDO_USER")
            .ok()
            .filter(|user| !user.trim().is_empty())
            .map(|user| format!(" for sudo user {user}"))
            .unwrap_or_default();
        anyhow::bail!(
            "Refusing to manage the macOS launchd user agent as root{sudo_user}. Run `clt agent start` or `clt agent stop` without sudo from the logged-in macOS user session."
        );
    }

    Ok(format!("gui/{uid}"))
}

pub(super) fn run_service_command(program: &str, args: &[&str]) -> Result<()> {
    let status = service_command(program, args)?
        .status()
        .with_context(|| format!("Failed to run {}", service_command_display(program, args)))?;
    if !status.success() {
        anyhow::bail!(
            "{} failed with status {}",
            service_command_display(program, args),
            status
        );
    }

    Ok(())
}

pub(super) fn run_service_command_optional(program: &str, args: &[&str]) -> Result<bool> {
    // Status probes are called from the TUI refresh path, so child output must
    // never inherit the terminal and overwrite the alternate screen.
    let status = service_command(program, args)?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("Failed to run {}", service_command_display(program, args)))?;

    Ok(status.success())
}

pub(super) fn run_service_command_quiet(program: &str, args: &[&str]) -> Result<()> {
    let status = service_command(program, args)?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("Failed to run {}", service_command_display(program, args)))?;
    if !status.success() {
        anyhow::bail!(
            "{} failed with status {}",
            service_command_display(program, args),
            status
        );
    }

    Ok(())
}

pub(super) fn service_command(program: &str, args: &[&str]) -> Result<Command> {
    service_command_with_systemd_user_configurer(program, args, configure_systemd_user_command)
}

pub(super) fn service_command_with_systemd_user_configurer(
    program: &str,
    args: &[&str],
    configure_systemd_user: impl FnOnce(&mut Command) -> Result<()>,
) -> Result<Command> {
    let mut command = Command::new(program);
    command.args(args);

    if matches!(program, "systemctl" | "systemd-run") && args.contains(&"--user") {
        configure_systemd_user(&mut command)?;
    }

    Ok(command)
}

pub(super) fn configure_systemd_user_command(command: &mut Command) -> Result<()> {
    let inherited_runtime_dir = std::env::var_os(XDG_RUNTIME_DIR_ENV);
    if inherited_runtime_dir
        .as_deref()
        .is_some_and(|value| !os_value_is_blank(value))
    {
        return Ok(());
    }

    let uid = current_user_id()?;
    let runtime_dir = systemd_user_runtime_dir_for_uid(&uid)?;
    let bus_path = runtime_dir.join("bus");
    if !runtime_dir.is_dir() || !bus_path.exists() {
        anyhow::bail!(
            "Linux systemd user bus is unavailable at {}. Log in through a systemd/PAM user session, or enable lingering with `sudo loginctl enable-linger <user>` before managing the clt agent service.",
            bus_path.display()
        );
    }
    configure_systemd_user_command_with_runtime_dir(command, None, &uid)
}

pub(super) fn configure_systemd_user_command_with_runtime_dir(
    command: &mut Command,
    inherited_runtime_dir: Option<&OsStr>,
    uid: &str,
) -> Result<()> {
    if inherited_runtime_dir.is_some_and(|value| !os_value_is_blank(value)) {
        return Ok(());
    }

    command.env(XDG_RUNTIME_DIR_ENV, systemd_user_runtime_dir_for_uid(uid)?);
    Ok(())
}

pub(super) fn systemd_user_runtime_dir_for_uid(uid: &str) -> Result<PathBuf> {
    let uid = uid.trim();
    if uid.is_empty() || !uid.chars().all(|ch| ch.is_ascii_digit()) {
        anyhow::bail!("id -u produced an invalid user id: {uid}");
    }

    Ok(Path::new("/run/user").join(uid))
}

pub(super) fn service_command_display(program: &str, args: &[&str]) -> String {
    let mut parts = vec![program.to_string()];
    parts.extend(args.iter().map(|arg| (*arg).to_string()));
    parts.join(" ")
}

pub(super) fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(super) fn systemd_env_assignment(key: &str, value: &str) -> String {
    format!(
        "\"{}={}\"",
        systemd_escape_double_quoted(key),
        systemd_escape_double_quoted(value)
    )
}

pub(super) fn systemd_quote_arg(raw: &str) -> String {
    if raw
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        raw.to_string()
    } else {
        format!("\"{}\"", systemd_escape_double_quoted(raw))
    }
}

pub(super) fn systemd_escape_double_quoted(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(unix)]
pub(super) fn interactive_terminal_input() -> Result<fs::File> {
    // The TUI deliberately preserves its inherited terminal as fd 0 through
    // both guardian execs. On sandboxed macOS, opening a fresh /dev/tty alias
    // can succeed while kqueue and terminal ioctls on that alias fail with
    // EINVAL/EPERM, which makes crossterm's EventStream panic during startup.
    // Duplicating the inherited descriptor preserves the terminal's original
    // kernel identity and entitlement.
    // SAFETY: isatty only inspects the process's standard-input descriptor.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
        anyhow::bail!("Interactive Codex requires an inherited terminal on standard input");
    }
    // SAFETY: F_DUPFD_CLOEXEC atomically duplicates the live terminal fd and
    // gives this owner an independently closeable descriptor.
    let terminal_fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    if terminal_fd < 0 {
        return Err(io::Error::last_os_error())
            .context("Failed to retain the inherited terminal for interactive Codex");
    }
    // SAFETY: fcntl returned a new owned descriptor on success.
    Ok(unsafe { fs::File::from_raw_fd(terminal_fd) })
}

#[cfg(windows)]
pub(super) fn interactive_terminal_input() -> Result<fs::File> {
    fs::File::open("CONIN$").context("Failed to open the terminal for interactive Codex")
}

#[cfg(not(any(unix, windows)))]
pub(super) fn interactive_terminal_input() -> Result<fs::File> {
    anyhow::bail!("Interactive Codex is not supported on this platform")
}

#[cfg(unix)]
pub(super) struct InteractiveTerminalForeground {
    terminal: fs::File,
    previous_process_group: libc::pid_t,
    restored: bool,
}

#[cfg(unix)]
impl InteractiveTerminalForeground {
    pub(super) fn capture(terminal: &fs::File) -> Result<Self> {
        let terminal = terminal
            .try_clone()
            .context("Failed to retain terminal control for interactive Codex")?;
        let previous_process_group = terminal_foreground_process_group(&terminal)?;
        Ok(Self {
            terminal,
            previous_process_group,
            restored: false,
        })
    }

    pub(super) fn give_to_child(&self, child: &Child) -> Result<()> {
        let process_group =
            i32::try_from(child.id()).context("Interactive Codex process ID exceeded pid_t")?;
        set_terminal_foreground_process_group(&self.terminal, process_group).with_context(|| {
            format!(
                "Failed to give terminal foreground control to interactive Codex group {process_group}"
            )
        })?;
        if !signal_agent_process_group(process_group, libc::SIGCONT)
            .context("Failed to continue the foreground interactive Codex group")?
        {
            anyhow::bail!(
                "Interactive Codex process group {process_group} exited during terminal handoff"
            );
        }
        Ok(())
    }

    pub(super) fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        set_terminal_foreground_process_group(&self.terminal, self.previous_process_group)
            .with_context(|| {
                format!(
                    "Failed to restore terminal foreground group {}",
                    self.previous_process_group
                )
            })?;
        self.restored = true;
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for InteractiveTerminalForeground {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(not(unix))]
pub(super) struct InteractiveTerminalForeground;

#[cfg(not(unix))]
impl InteractiveTerminalForeground {
    pub(super) fn capture(_terminal: &fs::File) -> Result<Self> {
        Ok(Self)
    }

    pub(super) fn give_to_child(&self, _child: &Child) -> Result<()> {
        Ok(())
    }

    pub(super) fn restore(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
pub(super) fn terminal_foreground_process_group(terminal: &fs::File) -> Result<libc::pid_t> {
    loop {
        // SAFETY: `terminal` owns a valid descriptor for /dev/tty for the
        // duration of this call.
        let process_group = unsafe { libc::tcgetpgrp(terminal.as_raw_fd()) };
        if process_group >= 0 {
            return Ok(process_group);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error).context("Failed to read terminal foreground process group");
        }
    }
}

#[cfg(unix)]
pub(super) fn set_terminal_foreground_process_group(
    terminal: &fs::File,
    process_group: libc::pid_t,
) -> Result<()> {
    with_sigttou_blocked(|| {
        loop {
            // SAFETY: `terminal` owns a valid descriptor for /dev/tty and the
            // positive PGID belongs to a process in the guardian's session.
            if unsafe { libc::tcsetpgrp(terminal.as_raw_fd(), process_group) } == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error).with_context(|| {
                    format!("Failed to set terminal foreground process group {process_group}")
                });
            }
        }
    })
}

#[cfg(unix)]
pub(super) fn with_sigttou_blocked(operation: impl FnOnce() -> Result<()>) -> Result<()> {
    // SAFETY: sigset_t is an integer/bitset POD accepted after sigemptyset
    // initializes it, and pthread_sigmask writes the previous mask before use.
    let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `blocked` points to valid writable sigset_t storage.
    if unsafe { libc::sigemptyset(&mut blocked) } != 0 {
        return Err(io::Error::last_os_error()).context("Failed to initialize SIGTTOU mask");
    }
    // SAFETY: `blocked` remains valid for the call and SIGTTOU is a valid signal.
    if unsafe { libc::sigaddset(&mut blocked, libc::SIGTTOU) } != 0 {
        return Err(io::Error::last_os_error()).context("Failed to block SIGTTOU");
    }

    // SAFETY: pthread_sigmask receives valid masks and initializes `previous`.
    let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: both sigset_t pointers are valid for the duration of the call.
    let block_result = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous) };
    if block_result != 0 {
        return Err(io::Error::from_raw_os_error(block_result))
            .context("Failed to block SIGTTOU for terminal handoff");
    }

    let operation_result = operation();
    // SAFETY: `previous` was initialized by the successful pthread_sigmask call.
    let restore_result =
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) };
    match (operation_result, restore_result) {
        (result, 0) => result,
        (Ok(()), error) => Err(io::Error::from_raw_os_error(error))
            .context("Failed to restore the terminal signal mask"),
        (Err(error), restore_error) => Err(error.context(format!(
            "restoring the terminal signal mask also failed: {}",
            io::Error::from_raw_os_error(restore_error)
        ))),
    }
}

pub(super) fn configure_interactive_child_command(command: &mut Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
}

pub(super) fn restore_interactive_terminal_before_handoff(
    terminal_foreground: &mut InteractiveTerminalForeground,
    store: &agent::TursoAgentStore,
    project_id: i64,
    guardian_holder: &str,
    lease_timeout: Duration,
    parent_connected: &AtomicBool,
) {
    let holds_project_lease = InteractiveGuardianDisposition::from_guardian_holder(guardian_holder)
        .is_none_or(InteractiveGuardianDisposition::holds_project_lease);
    let renewal_interval = agent_lease_renew_interval(lease_timeout);
    let mut last_renewal = Instant::now();
    let mut last_warning: Option<Instant> = None;
    loop {
        match terminal_foreground.restore() {
            Ok(()) => return,
            Err(error) if !parent_connected.load(Ordering::SeqCst) => {
                eprintln!(
                    "Interactive guardian could not restore its disconnected parent's terminal foreground: {error:#}"
                );
                return;
            }
            Err(error) => {
                let should_warn =
                    last_warning.is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                if should_warn {
                    eprintln!(
                        "Interactive guardian is retrying terminal foreground restoration before session handback: {error:#}"
                    );
                    last_warning = Some(Instant::now());
                }
            }
        }

        if holds_project_lease && last_renewal.elapsed() >= renewal_interval {
            let expires_at = agent_timestamp_after(lease_timeout.as_secs().max(60));
            let _ = store.renew_lease_blocking(project_id, guardian_holder, &expires_at);
            last_renewal = Instant::now();
        }
        thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(unix)]
pub(super) fn interactive_child_exited_without_reaping(child: &Child) -> Result<bool> {
    // WNOWAIT leaves an exited leader waitable, anchoring its PGID until CLT
    // has drained or terminated every descendant in that group.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `info` is valid writable storage, the child PID identifies
        // a direct child, and these waitid flags only observe its exit state.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            // SAFETY: waitid initialized siginfo_t; si_pid is zero when WNOHANG
            // found no waitable child state.
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error).context("Failed to observe interactive Codex leader");
        }
    }
}

#[cfg(unix)]
pub(super) fn local_process_is_running(pid: u32) -> Option<bool> {
    if pid == 0 || pid > libc::pid_t::MAX as u32 {
        return Some(false);
    }

    // SAFETY: signal 0 does not modify the target process; it only asks the
    // kernel whether this PID exists and whether we may signal it.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return Some(true);
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Some(false),
        Some(libc::EPERM) => Some(true),
        _ => None,
    }
}

#[cfg(not(unix))]
pub(super) fn local_process_is_running(_pid: u32) -> Option<bool> {
    None
}

#[cfg(unix)]
pub(super) fn automated_agent_process_group_is_running(pid: u32) -> Option<bool> {
    if local_process_is_running(pid) == Some(true) {
        return Some(true);
    }
    if pid == 0 || pid > libc::pid_t::MAX as u32 {
        return Some(false);
    }
    let process_group = pid as libc::pid_t;
    agent_process_group_exists(process_group).ok()
}

#[cfg(not(unix))]
pub(super) fn automated_agent_process_group_is_running(pid: u32) -> Option<bool> {
    local_process_is_running(pid)
}

pub(super) fn configure_agent_child_command(command: &mut Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
}

#[cfg(unix)]
pub(super) fn recover_rejected_agent_process_group_signal(
    child: &mut Child,
    process_group: libc::pid_t,
    signal_error: anyhow::Error,
    group_exists: impl FnOnce(libc::pid_t) -> Result<bool>,
) -> Result<Option<ExitStatus>> {
    let Some(status) = child
        .try_wait()
        .context("Failed to poll Codex leader after process-group signal rejection")?
    else {
        return Err(signal_error);
    };

    match group_exists(process_group) {
        Ok(false) => Ok(Some(status)),
        Ok(true) => Err(signal_error),
        Err(probe_error) => {
            let context = format!(
                "Codex leader was reaped, but its process group could not be proven absent: {probe_error:#}"
            );
            Err(signal_error.context(context))
        }
    }
}

#[cfg(unix)]
pub(super) fn stop_agent_child_process(child: &mut Child) -> Result<Option<ExitStatus>> {
    let process_group = i32::try_from(child.id()).context("Codex process ID exceeded pid_t")?;
    let term_sent = match signal_agent_process_group(process_group, libc::SIGTERM)
        .context("Failed to request Codex process-group termination")
    {
        Ok(term_sent) => term_sent,
        Err(signal_error) => {
            // Darwin can reject a group signal with EPERM when the group contains
            // only its unreaped zombie leader. Reap first, then accept the
            // rejection only when signal zero proves that exact group disappeared.
            return recover_rejected_agent_process_group_signal(
                child,
                process_group,
                signal_error,
                agent_process_group_exists,
            );
        }
    };

    if term_sent {
        // Keep the leader unreaped during the grace period. Its retained PID anchors
        // the process-group ID, so escalation cannot race with PGID reuse and signal
        // an unrelated group after the leader exits ahead of its descendants.
        thread::sleep(Duration::from_secs(2));
        if let Err(signal_error) = signal_agent_process_group(process_group, libc::SIGKILL)
            .context("Failed to force-stop Codex process group")
        {
            // Darwin and restricted sandboxes can reject a group signal after
            // TERM has already left only a zombie leader. Reap that leader and
            // accept the rejection only when signal-zero proves the group gone.
            return recover_rejected_agent_process_group_signal(
                child,
                process_group,
                signal_error,
                agent_process_group_exists,
            );
        }
    }

    let status = child
        .wait()
        .context("Failed to reap Codex process leader")?;
    let proof_started = Instant::now();
    loop {
        if !agent_process_group_exists(process_group)
            .context("Failed to verify Codex process-group shutdown")?
        {
            return Ok(Some(status));
        }
        if proof_started.elapsed() >= Duration::from_secs(5) {
            anyhow::bail!("Codex process group {process_group} remained present after force-stop");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(not(unix))]
pub(super) fn stop_agent_child_process(child: &mut Child) -> Result<Option<ExitStatus>> {
    request_agent_child_termination(child)?;

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if let Some(status) = child
            .try_wait()
            .context("Failed to poll Codex process after termination request")?
        {
            return Ok(Some(status));
        }

        thread::sleep(Duration::from_millis(25));
    }

    child.kill().context("Failed to force-stop Codex process")?;
    Ok(child.wait().ok())
}

#[cfg(unix)]
pub(super) fn stop_interactive_child_process(child: &mut Child) -> Result<Option<ExitStatus>> {
    stop_agent_child_process(child)
}

#[cfg(not(unix))]
pub(super) fn stop_interactive_child_process(child: &mut Child) -> Result<Option<ExitStatus>> {
    request_interactive_child_termination(child)?;

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if let Some(status) = child
            .try_wait()
            .context("Failed to poll interactive Codex after termination request")?
        {
            return Ok(Some(status));
        }
        thread::sleep(Duration::from_millis(25));
    }

    child
        .kill()
        .context("Failed to force-stop interactive Codex")?;
    let status = child
        .wait()
        .context("Failed to reap force-stopped interactive Codex")?;
    Ok(Some(status))
}

#[cfg(not(unix))]
pub(super) fn request_interactive_child_termination(child: &mut Child) -> Result<()> {
    if child
        .try_wait()
        .context("Failed to poll interactive Codex before direct termination")?
        .is_some()
    {
        return Ok(());
    }
    match child.kill() {
        Ok(()) => Ok(()),
        Err(error) => {
            if child
                .try_wait()
                .context("Failed to poll interactive Codex after direct termination")?
                .is_some()
            {
                Ok(())
            } else {
                Err(error).context("Failed to stop interactive Codex directly")
            }
        }
    }
}

#[cfg(unix)]
pub(super) fn signal_agent_process_group(
    process_group: libc::pid_t,
    signal: libc::c_int,
) -> Result<bool> {
    // SAFETY: `kill` has no pointer arguments. The negated, positive PGID targets
    // the dedicated process group created by `configure_agent_child_command`.
    if unsafe { libc::kill(-process_group, signal) } == 0 {
        return Ok(true);
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(error).with_context(|| {
            format!("Failed to signal Codex process group {process_group} with {signal}")
        })
    }
}

#[cfg(unix)]
pub(super) fn agent_process_group_exists(process_group: libc::pid_t) -> Result<bool> {
    // Signal zero performs permission and existence checks without delivering a
    // signal. EPERM still proves that at least one member of the group exists.
    // SAFETY: `kill` has no pointer arguments and signal zero has no side effects.
    if unsafe { libc::kill(-process_group, 0) } == 0 {
        return Ok(true);
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error)
            .with_context(|| format!("Failed to inspect Codex process group {process_group}")),
    }
}

#[cfg(not(unix))]
pub(super) fn request_agent_child_termination(child: &mut Child) -> Result<()> {
    child
        .kill()
        .context("Failed to stop Codex process directly")?;
    Ok(())
}
