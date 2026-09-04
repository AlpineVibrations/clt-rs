use std::{
    collections::HashSet,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};

use crate::{
    agent::{
        self, AGENT_DB_FILE, AgentGitMode, AgentSessionControlState, GitFinalizationState,
        agent_state_dir, open_agent_store, open_agent_store_at,
    },
    managed_git::{
        AgentGitProofContext, AgentGitStartState, capture_agent_git_resealed_manifest,
        capture_agent_git_staged_manifest, git_commit_is_ancestor, git_optional_stdout,
        require_agent_git_start_task_identity, resolve_git_commit, task_content_has_completed_note,
        verify_agent_git_start_state_unchanged,
    },
    platform::{AgentServiceEnvironment, agent_service_status, truncate_agent_service_logs},
    runner::{
        AgentRunner, AutomatedAgentChildContext, agent_timestamp, agent_timestamp_after,
        agent_timestamp_seconds, automated_agent_child_context, canonicalize_existing_path,
        format_agent_run_line, format_agent_timestamp, format_optional_agent_timestamp,
        print_agent_log_tail, resolve_agent_project_root,
    },
    scheduler::{agent_lease_holder_liveness, scan_agent_project},
    task::{
        ExpansionSummary, StatusStore, TASK_STATUSES, TaskBoard, TaskEntry, TaskSource, TaskStatus,
        acquire_board_mutation_lock, attach_codex_session_to_task_after_lock,
        cleanup_clt_atomic_task_temporaries, codex_session_id_from_task_content,
        convert_archive_to_directory, durable_task_identity, ensure_existing_board,
        expand_status_for_command, get_or_create_archive_status_store, get_tasks_dir,
        insert_content_into_directory, insert_content_into_markdown, move_path_into_directory,
        move_task_without_reordering_after_lock, normalize_status_arg, parse_one_based_task_index,
        read_markdown_entries, recoverable_codex_session_id_from_task_content, remove_task_entry,
        reorder_directory_task, reorder_markdown_task, task_content_with_codex_session,
        task_entry_at,
    },
    tui::{
        format_agent_daemon_runtime_status, load_task_agent_session_states,
        task_display_text_with_agent_flag,
    },
};

#[cfg(test)]
use crate::task::acquire_board_mutation_lock_with_contention_callback;
#[cfg(not(test))]
use crate::worker::cleanup_terminal_agent_worker_services;

pub(super) fn recover_agent_state() -> Result<()> {
    let state_dir = agent::ensure_agent_state_dir()?;
    anyhow::ensure!(
        state_dir.join(agent::recovery::SNAPSHOT_FILE).exists(),
        "No external agent registry snapshot exists at {}. Stop the scheduler with clt agent stop and preserve agent.db together with agent.db-wal; automatic reconstruction requires a snapshot from this version of CLT.",
        state_dir.display()
    );
    let report = agent::recovery::recover_registry_with(&state_dir, |manifest| {
        crate::platform::stop_agent_services_for_recovery(&state_dir, manifest)
    })?;
    println!(
        "Recovered agent registry by {}. Original database bundle retained at {}. Agents remain stopped; use clt agent start when ready.",
        if report.rebuilt_registry {
            "rebuilding it from external configuration and Git journals"
        } else {
            "rebuilding Turso coordination files"
        },
        report.quarantine.display()
    );
    Ok(())
}

const AGENT_EXTERNAL_COMPLETION_LEASE_SECONDS: u64 = 60;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum TaskDoneOutcome {
    Normal,
    Provisional,
    ExternalCompletion(String),
}

pub(super) const AGENT_PROJECT_ID_ENV: &str = "CLT_AGENT_PROJECT_ID";
pub(super) const AGENT_RUN_TOKEN_ENV: &str = "CLT_AGENT_RUN_TOKEN";
pub(super) const AGENT_FAILURE_BACKOFF_SECONDS_ENV: &str = "CLT_AGENT_FAILURE_BACKOFF_SECONDS";
pub(super) const AGENT_HEARTBEAT_TAIL_ENV: &str = "CLT_AGENT_HEARTBEAT_TAIL";
pub(super) const AGENT_LEASE_TIMEOUT_SECONDS_ENV: &str = "CLT_AGENT_LEASE_TIMEOUT_SECONDS";
pub(super) const AGENT_MAX_GLOBAL_JOBS_ENV: &str = "CLT_AGENT_MAX_GLOBAL_JOBS";
pub(super) const AGENT_POLL_INTERVAL_SECONDS_ENV: &str = "CLT_AGENT_POLL_INTERVAL_SECONDS";
pub(super) const AGENT_RUN_TIMEOUT_SECONDS_ENV: &str = "CLT_AGENT_RUN_TIMEOUT_SECONDS";
pub(super) const AGENT_DAEMON_MODE_ENV: &str = "CLT_AGENT_DAEMON_MODE";
pub(super) const AGENT_CODEX_PATH_ENV: &str = "CLT_AGENT_CODEX_PATH";
pub(super) const AGENT_SUCCESS_COOLDOWN_SECONDS_ENV: &str = "CLT_AGENT_SUCCESS_COOLDOWN_SECONDS";
pub(super) const XDG_RUNTIME_DIR_ENV: &str = "XDG_RUNTIME_DIR";
pub(super) const AGENT_DEFAULT_MAX_GLOBAL_JOBS: usize = 12;
pub(super) const AGENT_DEFAULT_FAILURE_BACKOFF_SECONDS: u64 = 5 * 60;
pub(super) const AGENT_DEFAULT_LEASE_TIMEOUT_SECONDS: u64 = 60 * 60;
pub(super) const AGENT_DEFAULT_POLL_INTERVAL_SECONDS: u64 = 15;
pub(super) const AGENT_EMPTY_REGISTRY_POLL_INTERVAL_SECONDS: u64 = 5;
pub(super) const AGENT_DAEMON_DATABASE_LOCK_RETRY_ATTEMPTS: usize = 20;
pub(super) const AGENT_DAEMON_DATABASE_LOCK_RETRY_MILLIS: u64 = 5;
pub(super) const AGENT_DEFAULT_RUN_TIMEOUT_SECONDS: u64 = 45 * 60;
pub(super) const AGENT_DEFAULT_SUCCESS_COOLDOWN_SECONDS: u64 = 5;
pub(super) const AGENT_DAEMON_CHECKIN_STALE_SECONDS: u64 = 45;
pub(super) const AGENT_LEASE_RENEW_MAX_INTERVAL_MILLIS: u64 = 15_000;
pub(super) const AGENT_WORKER_STARTUP_TIMEOUT_SECONDS: u64 = 60;
pub(super) const AGENT_WORKER_HEARTBEAT_TIMEOUT_SECONDS: u64 = 60;
pub(super) const AGENT_WORKER_PROTOCOL_VERSION: i64 = 2;
pub(super) const AGENT_INLINE_WORKER_SERVICE_LABEL_PREFIX: &str = "clt-inline-worker-";
pub(super) const AGENT_SESSION_CONTROL_POLL_MILLIS: u64 = 500;
#[cfg(unix)]
pub(super) const AGENT_SUPERVISOR_READY_TIMEOUT_SECONDS: u64 = 10;
#[cfg(all(unix, test))]
pub(super) const TEST_AUTOMATED_SUPERVISOR_ENV: &str = "CLT_TEST_AUTOMATED_SUPERVISOR";
#[cfg(all(unix, test))]
pub(super) const TEST_INTERACTIVE_EXEC_GATE_ENV: &str = "CLT_TEST_INTERACTIVE_EXEC_GATE";
pub(super) static INTERACTIVE_LEASE_GENERATION: AtomicU64 = AtomicU64::new(1);
pub(super) static AGENT_WORKER_GENERATION: AtomicU64 = AtomicU64::new(1);
pub(super) static ACTIVE_INLINE_AGENT_WORKERS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

pub(super) const AGENT_LAUNCHD_LABEL: &str = "com.alpinevibrations.clt.agent";
pub(super) const AGENT_SYSTEMD_UNIT: &str = "clt-agent.service";
pub(super) const AGENT_WORKER_STATE_DISPATCHING: &str = "dispatching";
pub(super) const AGENT_WORKER_STATE_RUNNING: &str = "running";
pub(super) const AGENT_WORKER_STATE_FINALIZING: &str = "finalizing";
pub(super) const AGENT_WORKER_LAUNCHD_LABEL_PREFIX: &str = "com.alpinevibrations.clt.agent.worker";
pub(super) const AGENT_WORKER_SYSTEMD_UNIT_PREFIX: &str = "clt-agent-worker";
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentCleanSummary {
    pub(super) projects_reset: u64,
    pub(super) runs_deleted: u64,
    pub(super) leases_deleted: u64,
    pub(super) daemon_checkins_deleted: u64,
    pub(super) run_log_dirs_removed: u64,
    pub(super) service_logs_truncated: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentProjectScan {
    pub(super) status: AgentProjectScanStatus,
    pub(super) todo_count: usize,
    pub(super) blocked_todo_count: usize,
    pub(super) doing_count: usize,
    pub(super) blocked_doing_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentSchedulerPass {
    pub(super) scanned_projects: usize,
    pub(super) pending_projects: usize,
    pub(super) active_agent_jobs: usize,
    pub(super) skipped_active_lease: usize,
    pub(super) deferred_projects: usize,
    pub(super) runs_started: usize,
    pub(super) runs_recorded: usize,
}

pub(super) struct AgentRunJob {
    pub(super) state_dir: PathBuf,
    pub(super) project: agent::AgentProject,
    pub(super) holder: String,
    pub(super) worker_token: Option<String>,
    pub(super) max_global_jobs: usize,
    pub(super) task_selection: AgentTaskSelection,
    pub(super) resume_session_id: Option<String>,
    pub(super) blocked_task_count_before: usize,
    pub(super) done_task_contents_before: Vec<String>,
    pub(super) blocked_task_snapshots_before: Vec<BlockedTaskSnapshot>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct BlockedTaskSnapshot {
    pub(super) status: TaskStatus,
    pub(super) content: String,
}

pub(super) struct AgentRunCompletion {
    pub(super) run_id: i64,
    pub(super) project_name: String,
    pub(super) project_path: PathBuf,
    pub(super) status: &'static str,
    pub(super) summary: String,
    pub(super) stdout_path: Option<String>,
    pub(super) stderr_path: Option<String>,
}

pub(super) struct AgentSchedulerStart {
    pub(super) pass: AgentSchedulerPass,
    pub(super) jobs: Vec<AgentRunJob>,
}

pub(super) struct AgentDaemonRun {
    pub(super) project_id: i64,
    pub(super) project_name: String,
    pub(super) project_path: PathBuf,
    pub(super) handle: tokio::task::JoinHandle<Result<AgentRunCompletion>>,
}

pub(super) enum AgentDaemonExecutor {
    Inline(Arc<dyn AgentRunner>),
    Independent {
        executable: PathBuf,
        dispatch: AgentWorkerDispatchFn,
    },
}

pub(super) type AgentWorkerDispatchFn = fn(&Path, &Path, AgentRunJob) -> Result<()>;

#[derive(Clone, Debug)]
pub(super) struct AgentWorkerLaunchSpec {
    pub(super) state_dir: PathBuf,
    pub(super) executable: PathBuf,
    pub(super) worker_token: String,
    pub(super) project_id: i64,
    pub(super) task_selection: AgentTaskSelection,
    pub(super) resume_session_id: Option<String>,
    pub(super) service_label: String,
    pub(super) command_arguments: Option<Vec<OsString>>,
    pub(super) service_env: AgentServiceEnvironment,
}

#[derive(Clone)]
pub(super) struct AgentDaemonCheckinSource {
    pub(super) holder: String,
    pub(super) mode: String,
    pub(super) started_at: String,
}

pub(super) type AgentShutdownSignal = Arc<AtomicBool>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentTaskSelection {
    NextTodo,
    ResumeDoing,
    RecoverBlocked,
    ResumeSession,
}

impl AgentTaskSelection {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::NextTodo => "next_todo",
            Self::ResumeDoing => "resume_doing",
            Self::RecoverBlocked => "recover_blocked",
            Self::ResumeSession => "resume_session",
        }
    }

    pub(super) fn from_label(value: &str) -> Result<Self> {
        match value {
            "next_todo" => Ok(Self::NextTodo),
            "resume_doing" => Ok(Self::ResumeDoing),
            "recover_blocked" => Ok(Self::RecoverBlocked),
            "resume_session" => Ok(Self::ResumeSession),
            _ => anyhow::bail!("Unknown agent worker task selection: {value}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentLeaseHolderLiveness {
    CurrentProcess,
    Alive,
    Dead,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum AgentProjectScanStatus {
    Pending,
    Blocked,
    Empty,
    Missing,
    Uninitialized,
    Unavailable(String),
}

pub(super) fn new_agent_shutdown_signal() -> AgentShutdownSignal {
    Arc::new(AtomicBool::new(false))
}

pub(super) fn register_agent_project(
    store: &agent::TursoAgentStore,
    path: Option<&Path>,
    local: bool,
    default_root: &Path,
) -> Result<()> {
    let project_root = resolve_agent_project_root(path, local, default_root)?;
    if !ensure_existing_board(&project_root)? {
        anyhow::bail!(
            "Project {:?} does not have an initialized tasks board. Run 'clt init' there first.",
            project_root
        );
    }

    let name = project_display_name(&project_root);
    let created = store.register_project_blocking(&project_root, &name)?;
    if created {
        println!("Registered project: {} ({})", name, project_root.display());
    } else {
        println!(
            "Project already registered: {} ({})",
            name,
            project_root.display()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests;

pub(super) fn unregister_agent_project(
    store: &agent::TursoAgentStore,
    state_dir: &Path,
    path: Option<&Path>,
    local: bool,
    default_root: &Path,
) -> Result<()> {
    let project_root = resolve_agent_project_root(path, local, default_root)?;
    #[cfg(not(test))]
    cleanup_terminal_agent_worker_services(state_dir, store, Some(&project_root))?;
    #[cfg(test)]
    let _ = state_dir;
    if store.unregister_project_blocking(&project_root)? {
        println!("Unregistered project: {}", project_root.display());
    } else {
        println!("Project was not registered: {}", project_root.display());
    }

    Ok(())
}

pub(super) fn set_agent_project_enabled(
    store: &agent::TursoAgentStore,
    path: Option<&Path>,
    local: bool,
    default_root: &Path,
    enabled: bool,
) -> Result<()> {
    let project_root = resolve_agent_project_root(path, local, default_root)?;
    if store.set_project_enabled_for_path_blocking(&project_root, enabled)? {
        let action = if enabled { "Resumed" } else { "Paused" };
        println!("{} project: {}", action, project_root.display());
    } else {
        println!("Project was not registered: {}", project_root.display());
    }

    Ok(())
}

pub(super) fn retry_agent_project(
    store: &agent::TursoAgentStore,
    path: Option<&Path>,
    local: bool,
    default_root: &Path,
) -> Result<()> {
    let project_root = resolve_agent_project_root(path, local, default_root)?;
    if store.clear_project_failure_backoff_for_path_blocking(&project_root)? {
        println!(
            "Queued project for immediate retry: {}",
            project_root.display()
        );
    } else {
        println!("Project was not registered: {}", project_root.display());
    }
    Ok(())
}

pub(super) fn set_agent_project_git_mode(
    store: &agent::TursoAgentStore,
    path: Option<&Path>,
    local: bool,
    default_root: &Path,
    mode: AgentGitMode,
) -> Result<()> {
    let project_root = resolve_agent_project_root(path, local, default_root)?;
    if store.set_project_git_mode_for_path_blocking(&project_root, mode)? {
        println!(
            "Set Git mode to {} for project: {}",
            mode.label(),
            project_root.display()
        );
    } else {
        println!("Project was not registered: {}", project_root.display());
    }

    Ok(())
}

pub(super) fn list_agent_projects(store: &agent::TursoAgentStore) -> Result<()> {
    let projects = store.list_projects_blocking()?;
    if projects.is_empty() {
        println!("No registered projects.");
        return Ok(());
    }

    println!("Registered projects ({}):", projects.len());
    for project in &projects {
        let scan = scan_agent_project(&project.path);
        let scanned_at = store.record_project_scan_blocking(project.id)?;
        println!();
        println!(
            "{}",
            format_agent_project_summary(project, &scan, Some(scanned_at.as_str()))
        );
    }

    Ok(())
}

pub(super) fn show_agent_status(store: &agent::TursoAgentStore) -> Result<()> {
    let state_dir = agent_state_dir()?;
    let projects = store.list_projects_blocking()?;
    let active_leases = store.list_active_leases_blocking(&agent_timestamp())?;
    let active_workers = store.list_active_workers_blocking()?;
    let recent_runs = store.list_recent_runs_blocking(5)?;
    let daemon_checkins = store.list_daemon_checkins_blocking()?;
    let pending_git_finalizations = if store.pending_migration_version().is_some() {
        Vec::new()
    } else {
        store.list_pending_git_finalizations_blocking(None)?
    };
    let service_status = agent_service_status(&state_dir);
    let daemon_status = format_agent_daemon_runtime_status(
        &service_status,
        &daemon_checkins,
        agent_timestamp_seconds(),
    );
    let enabled_count = projects.iter().filter(|project| project.enabled).count();
    let scans: Vec<_> = projects
        .iter()
        .map(|project| (project.id, scan_agent_project(&project.path)))
        .collect();
    let pending_count = scans
        .iter()
        .filter(|(_, scan)| scan.has_schedulable_work())
        .count();

    println!("Agent status:");
    println!("state_dir={}", state_dir.display());
    println!("database={}", state_dir.join(AGENT_DB_FILE).display());
    println!("service={}", service_status);
    println!("daemon={}", daemon_status);
    if let Some(version) = store.pending_migration_version() {
        println!("schema=compatibility-mode pending_migration={version}");
    } else {
        println!("schema=current");
    }
    println!(
        "registered_projects={} enabled={} pending={} finalizing={} active_workers={} active_leases={}",
        projects.len(),
        enabled_count,
        pending_count,
        pending_git_finalizations.len(),
        active_workers.len(),
        active_leases.len()
    );

    if projects.is_empty() {
        println!("No registered projects.");
    } else {
        println!();
        println!("Projects:");
        for project in &projects {
            let scan = scans
                .iter()
                .find(|(project_id, _)| *project_id == project.id)
                .map(|(_, scan)| scan)
                .expect("scan recorded for each project");
            println!();
            println!(
                "{}",
                format_agent_project_summary(project, scan, project.last_scan_at.as_deref())
            );
        }
    }

    if !active_workers.is_empty() {
        println!();
        println!("Independent workers:");
        for worker in &active_workers {
            println!(
                "project={} {} state={} protocol={} token={} pid={} service={} binary={} path={}",
                worker.project_id,
                worker.project_name,
                worker.state,
                worker.protocol_version,
                worker.worker_token,
                worker
                    .worker_pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                worker.service_label,
                worker.binary_path.display(),
                worker.project_path.display()
            );
        }
    }

    if !active_leases.is_empty() {
        println!();
        println!("Active leases:");
        for lease in active_leases {
            println!(
                "project={} {} holder={} process={} acquired_at={} expires_at={} path={}",
                lease.project_id,
                lease.project_name,
                lease.holder,
                agent_lease_holder_liveness(&lease.holder).label(),
                format_agent_timestamp(&lease.acquired_at),
                format_agent_timestamp(&lease.expires_at),
                lease.project_path.display()
            );
        }
    }

    if !pending_git_finalizations.is_empty() {
        println!();
        println!("Git finalizations:");
        for finalization in pending_git_finalizations {
            let project_name = projects
                .iter()
                .find(|project| project.id == finalization.project_id)
                .map(|project| project.name.as_str())
                .unwrap_or("<unregistered>");
            println!(
                "project={} {} state={} session={} commit={} error={}",
                finalization.project_id,
                project_name,
                finalization.state.status_label(),
                finalization.codex_session_id,
                finalization.commit_oid.as_deref().unwrap_or("-"),
                finalization.last_error.as_deref().unwrap_or("-")
            );
        }
    }

    if !recent_runs.is_empty() {
        println!();
        println!("Recent runs:");
        for run in recent_runs {
            println!("{}", format_agent_run_line(&run));
        }
    }

    Ok(())
}

pub(super) fn format_agent_project_summary(
    project: &agent::AgentProject,
    scan: &AgentProjectScan,
    last_scan_at: Option<&str>,
) -> String {
    let state = if project.enabled { "enabled" } else { "paused" };
    let mut lines = vec![
        format!("{}. {} [{}]", project.id, project.name, state),
        format!(
            "   queue     pending: {:<3} todo: {:<3} todo-ready: {:<3} todo-blocked: {:<3} doing: {:<3} doing-blocked: {:<3} scan: {}",
            scan.pending_signal(),
            scan.todo_count,
            scan.available_todo_count(),
            scan.blocked_todo_count,
            scan.doing_count,
            scan.blocked_doing_count,
            scan.status_label()
        ),
    ];

    if let AgentProjectScanStatus::Unavailable(err) = &scan.status {
        lines.push(format!("   scan err  {err}"));
    }

    lines.extend([
        format!(
            "   activity  last scan: {}",
            format_optional_agent_timestamp(last_scan_at)
        ),
        format!(
            "             last run:  {}",
            format_optional_agent_timestamp(project.last_run_at.as_deref())
        ),
        format!(
            "             success:   {}",
            format_optional_agent_timestamp(project.last_success_at.as_deref())
        ),
        format!(
            "             failure:   {}",
            format_optional_agent_timestamp(project.last_failure_at.as_deref())
        ),
        format!(
            "             blocked:   {}",
            format_optional_agent_timestamp(project.last_blocked_recovery_at.as_deref())
        ),
        format!("   settings  git: {}", project.git_mode.label()),
        format!(
            "             target: {}  reasoning: {}  fast: {}",
            project
                .codex_model
                .as_deref()
                .map(|model| format!(
                    "{}/{}",
                    project.codex_provider.as_deref().unwrap_or("openai"),
                    model
                ))
                .unwrap_or_else(|| "CLT default".to_string()),
            project
                .codex_reasoning_effort
                .as_deref()
                .unwrap_or("default"),
            if project.codex_fast_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ),
        format!("   health    failures: {}", project.failure_count),
        format!("   path      {}", project.path.display()),
    ]);

    lines.join("\n")
}

pub(super) fn show_agent_logs(store: &agent::TursoAgentStore) -> Result<()> {
    let recent_runs = store.list_recent_runs_blocking(5)?;
    if recent_runs.is_empty() {
        println!("No agent runs recorded.");
        return Ok(());
    }

    println!("Recent agent logs:");
    for run in recent_runs {
        println!();
        println!("{}", format_agent_run_line(&run));
        print_agent_log_tail("stdout", run.stdout_path.as_deref())?;
        print_agent_log_tail("stderr", run.stderr_path.as_deref())?;
    }

    Ok(())
}

pub(super) fn clean_agent_state(store: &agent::TursoAgentStore, state_dir: &Path) -> Result<()> {
    let active_workers = store.list_active_workers_blocking()?;
    if !active_workers.is_empty() {
        anyhow::bail!(
            "Refusing to clean agent state while {} independent worker(s) are active. Wait for those Codex runs to finish; stopping the scheduler does not stop them.",
            active_workers.len()
        );
    }
    let active_leases = store.list_active_leases_blocking(&agent_timestamp())?;
    if !active_leases.is_empty() {
        anyhow::bail!(
            "Refusing to clean agent state while {} active lease(s) exist. Wait for active Codex runs to finish.",
            active_leases.len()
        );
    }

    #[cfg(not(test))]
    cleanup_terminal_agent_worker_services(state_dir, store, None)?;

    let mut summary = store.clean_agent_history_blocking(&agent_timestamp())?;
    summary.run_log_dirs_removed = remove_agent_run_logs(state_dir)?;
    summary.service_logs_truncated = truncate_agent_service_logs(state_dir)?;

    println!("Cleaned agent state:");
    println!("  projects reset: {}", summary.projects_reset);
    println!("  run records deleted: {}", summary.runs_deleted);
    println!("  stale leases deleted: {}", summary.leases_deleted);
    println!(
        "  daemon check-ins deleted: {}",
        summary.daemon_checkins_deleted
    );
    println!(
        "  run log directories removed: {}",
        summary.run_log_dirs_removed
    );
    println!(
        "  service logs truncated: {}",
        summary.service_logs_truncated
    );

    Ok(())
}

pub(super) fn remove_agent_run_logs(state_dir: &Path) -> Result<u64> {
    let runs_dir = state_dir.join("runs");
    if !runs_dir.exists() {
        fs::create_dir_all(&runs_dir)
            .with_context(|| format!("Failed to create agent run log directory {:?}", runs_dir))?;
        return Ok(0);
    }

    let removed = fs::read_dir(&runs_dir)
        .with_context(|| format!("Failed to read agent run log directory {:?}", runs_dir))?
        .count() as u64;
    fs::remove_dir_all(&runs_dir)
        .with_context(|| format!("Failed to remove agent run log directory {:?}", runs_dir))?;
    fs::create_dir_all(&runs_dir)
        .with_context(|| format!("Failed to recreate agent run log directory {:?}", runs_dir))?;

    Ok(removed)
}

pub(super) fn get_task_root(local: bool) -> Result<std::path::PathBuf> {
    get_task_root_at(&std::env::current_dir()?, local)
}

pub(super) fn get_task_root_at(start: &Path, local: bool) -> Result<PathBuf> {
    if local {
        return canonicalize_existing_path(start);
    }

    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let path_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
            canonicalize_existing_path(Path::new(&path_str))
        }
        _ => canonicalize_existing_path(start),
    }
}

pub(super) fn project_display_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| root.display().to_string())
}

#[derive(Clone, Debug)]
pub(super) struct ManagedTaskWorkflow {
    root: PathBuf,
    board: TaskBoard,
}

impl ManagedTaskWorkflow {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            board: TaskBoard::for_project(root),
        }
    }

    pub(super) fn move_task(
        &self,
        from: TaskStatus,
        to: TaskStatus,
        task_index: &str,
    ) -> Result<()> {
        move_task(&self.root, from, to, task_index)
    }

    pub(super) fn complete_task(
        &self,
        from: TaskStatus,
        task_index: &str,
    ) -> Result<TaskDoneOutcome> {
        move_task_to_done(&self.root, from, task_index)
    }

    pub(super) fn reseal_completed_task(&self, task_index: &str) -> Result<bool> {
        reseal_provisional_done_task(&self.root, task_index)
    }

    pub(super) fn delete_task(&self, status: TaskStatus, task_index: &str) -> Result<()> {
        delete_task_in_board(self.board.path(), status, task_index)
    }
}

pub(super) fn ensure_status_conversion_allowed(board_dir: &Path, status: TaskStatus) -> Result<()> {
    if board_dir.join(status.as_str()).is_dir() {
        return Ok(());
    }
    for entry in TaskBoard::new(board_dir).entries(status)? {
        ensure_managed_git_task_mutation_allowed(board_dir, &entry, false, None)?;
    }
    Ok(())
}

pub(super) fn ensure_archive_conversion_allowed(
    board_dir: &Path,
    archive_file: &Path,
) -> Result<()> {
    if !archive_file.is_file() {
        return Ok(());
    }
    for entry in read_markdown_entries(archive_file)? {
        ensure_managed_git_task_mutation_allowed(board_dir, &entry, false, None)?;
    }
    Ok(())
}

pub(super) fn expand_tasks(root: &Path, filter_status: Option<String>) -> Result<()> {
    let board_dir = get_tasks_dir(root);
    let statuses: Vec<TaskStatus> = match filter_status {
        Some(status) => vec![normalize_status_arg(&status)?],
        None => TASK_STATUSES.to_vec(),
    };
    let _mutation_lock = acquire_board_mutation_lock(&board_dir)?;

    for status in statuses {
        ensure_status_conversion_allowed(&board_dir, status)?;
        match expand_status_for_command(&board_dir, status)? {
            ExpansionSummary::AlreadyDirectory { status, dir } => {
                println!("{} is already folder-backed at {:?}", status, dir);
            }
            ExpansionSummary::Expanded {
                status,
                dir,
                backup,
                task_count,
            } => {
                println!(
                    "Expanded {} to {:?} with {} task file(s). Backup: {:?}",
                    status, dir, task_count, backup
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
pub(super) fn delete_task(root: &Path, status: &str, task_index_str: &str) -> Result<()> {
    delete_task_in_board(
        &get_tasks_dir(root),
        TaskStatus::parse(status)?,
        task_index_str,
    )
}

pub(super) fn reseal_provisional_done_task(root: &Path, task_index_str: &str) -> Result<bool> {
    let Some(context) = automated_agent_child_context()? else {
        return Ok(false);
    };
    let store = open_agent_store()?;
    let project = store
        .list_projects_blocking()?
        .into_iter()
        .find(|project| project.id == context.project_id)
        .with_context(|| {
            format!(
                "Automated agent project {} is no longer registered",
                context.project_id
            )
        })?;
    if project.git_mode == AgentGitMode::Off {
        return Ok(false);
    }
    let root = canonicalize_existing_path(root)?;
    if project.path != root {
        anyhow::bail!(
            "Automated agent context project {} targets {}, not {}",
            context.project_id,
            project.path.display(),
            root.display()
        );
    }
    let task_index = parse_one_based_task_index(task_index_str)?;
    let board_dir = get_tasks_dir(&root);
    let _mutation_lock = acquire_board_mutation_lock(&board_dir)?;
    let entry = TaskBoard::new(&board_dir).entry(TaskStatus::Done, task_index)?;
    let session_id = codex_session_id_from_task_content(&entry.content).context(
        "Resealing a provisional Done task requires its terminal codex:<session-id> marker",
    )?;
    if !task_content_has_completed_note(&entry.content) {
        anyhow::bail!("Resealing requires the task's dated COMPLETED note");
    }
    let finalization = store
        .git_finalization_blocking(project.id, session_id)?
        .with_context(|| format!("Task {session_id} has no managed Git finalization"))?;
    if finalization.state != GitFinalizationState::CommitPending {
        anyhow::bail!(
            "Task {session_id} can only be resealed while its Git journal is COMMIT-PENDING, not {}",
            finalization.state.status_label()
        );
    }
    let task_identity = finalization
        .task_identity
        .as_deref()
        .context("The provisional Done task has no durable identity")?;
    if durable_task_identity(&entry.content).as_deref() != Some(task_identity) {
        anyhow::bail!("The provisional Done task no longer matches its durable identity");
    }
    let starting_head = finalization
        .starting_head
        .as_deref()
        .context("The provisional Done task has no frozen starting commit")?;
    let manifest = capture_agent_git_resealed_manifest(
        AgentGitProofContext {
            store: &store,
            project_id: project.id,
        },
        &root,
        &finalization.worktree_baseline,
        session_id,
        task_identity,
        starting_head,
        finalization.branch_ref.as_deref(),
    )?;
    let resealed = store.reseal_git_finalization_manifest_blocking(
        project.id,
        session_id,
        finalization.generation,
        task_identity,
        &manifest,
        &context.run_token,
        &agent_timestamp(),
    )?;
    if !resealed {
        anyhow::bail!(
            "Task {session_id} lost its running-session fence while CLT was resealing the corrected manifest"
        );
    }
    Ok(true)
}

pub(super) fn delete_task_in_board(
    board_dir: &Path,
    status: TaskStatus,
    task_index_str: &str,
) -> Result<()> {
    let task_index = parse_one_based_task_index(task_index_str)?;
    let _mutation_lock = acquire_board_mutation_lock(board_dir)?;
    let board = TaskBoard::new(board_dir);
    let entry = board.entry(status, task_index)?;
    ensure_managed_git_task_mutation_allowed(board_dir, &entry, false, None)?;
    board.remove_entry(status, &entry)
}

pub(super) fn ensure_managed_git_task_mutation_allowed(
    board_dir: &Path,
    entry: &TaskEntry,
    allow_working_preserving_mutation: bool,
    proposed_working_content: Option<&str>,
) -> Result<()> {
    let Some(session_id) = recoverable_codex_session_id_from_task_content(&entry.content) else {
        return Ok(());
    };
    let state_dir = agent_state_dir()?;
    if !state_dir.join(AGENT_DB_FILE).is_file() {
        return Ok(());
    }
    let canonical_board = fs::canonicalize(board_dir).unwrap_or_else(|_| board_dir.to_path_buf());
    let store = open_agent_store_at(&state_dir)?;
    if store.pending_migration_version().is_some() {
        return Ok(());
    }
    let Some(project) = store
        .list_projects_blocking()?
        .into_iter()
        .filter(|project| canonical_board.starts_with(project.path.join("tasks")))
        .max_by_key(|project| project.path.as_os_str().len())
    else {
        return Ok(());
    };
    let Some(finalization) = store.git_finalization_blocking(project.id, session_id)? else {
        return Ok(());
    };
    if finalization.state.is_terminal() {
        return Ok(());
    }
    if allow_working_preserving_mutation && finalization.state == GitFinalizationState::Working {
        if let Some(proposed_content) = proposed_working_content {
            let bound_identity = finalization.task_identity.as_deref().context(
                "The Working Git journal has no durable task identity for this content update",
            )?;
            ensure_working_task_content_preserves_identity(
                session_id,
                bound_identity,
                proposed_content,
            )?;
        }
        return Ok(());
    }
    anyhow::bail!(
        "Task {session_id} has a managed Git journal in {}; resume that exact agent session instead of changing, moving, archiving, reordering, or deleting its durable task evidence",
        finalization.state.status_label()
    )
}

pub(super) fn ensure_working_task_content_preserves_identity(
    session_id: &str,
    bound_identity: &str,
    proposed_content: &str,
) -> Result<()> {
    if durable_task_identity(proposed_content).as_deref() != Some(bound_identity) {
        anyhow::bail!(
            "Task {session_id} has a Working Git journal; content edits may add outcome notes but cannot change its durable task payload"
        );
    }
    Ok(())
}

pub(super) fn move_task(
    root: &Path,
    from: TaskStatus,
    to: TaskStatus,
    task_index_str: &str,
) -> Result<()> {
    if from == TaskStatus::Todo
        && to == TaskStatus::Doing
        && let Some(context) = automated_agent_child_context()?
    {
        let store = open_agent_store()?;
        let project = store
            .list_projects_blocking()?
            .into_iter()
            .find(|project| project.id == context.project_id)
            .with_context(|| {
                format!(
                    "Automated agent project {} is no longer registered",
                    context.project_id
                )
            })?;
        if project.git_mode != AgentGitMode::Off {
            return move_task_to_doing_with_agent_git_journal(
                root,
                task_index_str,
                &context,
                &project,
                &store,
            );
        }
    }
    move_task_in_board(&get_tasks_dir(root), from, to, task_index_str).map(|_| ())
}

pub(super) fn running_session_for_automated_child(
    store: &agent::TursoAgentStore,
    context: &AutomatedAgentChildContext,
) -> Result<String> {
    for _ in 0..40 {
        let matches = store
            .session_controls_for_project_blocking(context.project_id)?
            .into_iter()
            .filter(|control| {
                control.state == AgentSessionControlState::Running
                    && control.run_token.as_deref() == Some(context.run_token.as_str())
            })
            .map(|control| control.codex_session_id)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [session_id] => return Ok(session_id.clone()),
            [] => thread::sleep(Duration::from_millis(50)),
            _ => anyhow::bail!(
                "Automated run {} owns more than one running Codex session",
                context.run_token
            ),
        }
    }
    anyhow::bail!(
        "CLT could not observe the running Codex session for automated run {}; retry the Todo-to-Doing move",
        context.run_token
    )
}

pub(super) fn move_task_to_doing_with_agent_git_journal(
    root: &Path,
    task_index_str: &str,
    context: &AutomatedAgentChildContext,
    project: &agent::AgentProject,
    store: &agent::TursoAgentStore,
) -> Result<()> {
    let root = canonicalize_existing_path(root)?;
    if project.path != root {
        anyhow::bail!(
            "Automated agent context project {} targets {}, not {}",
            context.project_id,
            project.path.display(),
            root.display()
        );
    }
    let session_id = running_session_for_automated_child(store, context)?;
    let task_index = parse_one_based_task_index(task_index_str)?;
    let board_dir = get_tasks_dir(&root);
    let _mutation_lock = acquire_board_mutation_lock(&board_dir)?;
    cleanup_clt_atomic_task_temporaries(&board_dir)?;
    let board = TaskBoard::new(&board_dir);
    let entry = board.entry(TaskStatus::Todo, task_index)?;
    let task_identity = durable_task_identity(&entry.content)
        .context("Automated Git task has no durable task payload")?;
    let existing = store
        .git_finalization_blocking(project.id, &session_id)?
        .with_context(|| {
            format!(
                "Automated run {} has no atomically registered Working Git journal; CLT will not move the proof boundary after launch",
                context.run_token
            )
        })?;
    if existing.state != GitFinalizationState::Working
        || existing.owner_run_token.as_deref() != Some(context.run_token.as_str())
    {
        anyhow::bail!(
            "Codex session {session_id} does not own the Working Git journal for automated run {}",
            context.run_token
        );
    }
    if existing.git_mode != project.git_mode {
        anyhow::bail!(
            "Automated run {} froze Git mode {}, but the project now uses {}",
            context.run_token,
            existing.git_mode.label(),
            project.git_mode.label()
        );
    }
    let git = AgentGitStartState {
        starting_head: existing
            .starting_head
            .clone()
            .context("The atomically registered Working journal has no starting commit")?,
        branch_ref: existing.branch_ref.clone(),
        upstream_ref: existing.upstream_ref.clone(),
        worktree_baseline: existing.worktree_baseline.clone(),
    };
    verify_agent_git_start_state_unchanged(&root, project.git_mode, &git)?;
    require_agent_git_start_task_identity(&root, &git.starting_head, &task_identity)?;
    if existing
        .task_identity
        .as_deref()
        .is_some_and(|identity| identity != task_identity)
    {
        anyhow::bail!("Codex session {session_id} already has an incompatible Git task identity");
    }
    if existing.task_identity.is_none()
        && !store.compare_and_set_git_finalization_with_identity_blocking(
            project.id,
            &session_id,
            existing.generation,
            GitFinalizationState::Working,
            &task_identity,
            Some(&context.run_token),
            &agent_timestamp(),
        )?
    {
        anyhow::bail!(
            "Codex session {session_id} lost its running-session fence while binding Todo activation"
        );
    }
    attach_codex_session_to_task_after_lock(&root, TaskStatus::Todo, &entry, &session_id, || {})?;
    board.move_task_without_reordering_after_lock(
        TaskStatus::Todo,
        TaskStatus::Doing,
        task_index,
    )?;
    Ok(())
}

pub(super) fn move_task_to_done(
    root: &Path,
    from: TaskStatus,
    task_index_str: &str,
) -> Result<TaskDoneOutcome> {
    let Some(context) = automated_agent_child_context()? else {
        return Ok(
            match move_task_in_board(&get_tasks_dir(root), from, TaskStatus::Done, task_index_str)?
            {
                Some(session_id) => TaskDoneOutcome::ExternalCompletion(session_id),
                None => TaskDoneOutcome::Normal,
            },
        );
    };
    Ok(
        if move_task_to_done_with_agent_context(root, from, task_index_str, &context)? {
            TaskDoneOutcome::Provisional
        } else {
            TaskDoneOutcome::Normal
        },
    )
}

pub(super) fn move_task_to_done_with_agent_context(
    root: &Path,
    from: TaskStatus,
    task_index_str: &str,
    context: &AutomatedAgentChildContext,
) -> Result<bool> {
    let store = open_agent_store()?;
    move_task_to_done_with_agent_store(root, from, task_index_str, context, &store)
}

pub(super) fn move_task_to_done_with_agent_store(
    root: &Path,
    from: TaskStatus,
    task_index_str: &str,
    context: &AutomatedAgentChildContext,
    store: &agent::TursoAgentStore,
) -> Result<bool> {
    let project = store
        .list_projects_blocking()?
        .into_iter()
        .find(|project| project.id == context.project_id)
        .with_context(|| {
            format!(
                "Automated agent project {} is no longer registered",
                context.project_id
            )
        })?;
    let root = canonicalize_existing_path(root)?;
    if project.path != root {
        anyhow::bail!(
            "Automated agent context project {} targets {}, not {}",
            context.project_id,
            project.path.display(),
            root.display()
        );
    }
    let task_index = parse_one_based_task_index(task_index_str)?;
    let board_dir = get_tasks_dir(&root);
    let _mutation_lock = acquire_board_mutation_lock(&board_dir)?;
    cleanup_clt_atomic_task_temporaries(&board_dir)?;
    let board = TaskBoard::new(&board_dir);
    let entry = board.entry(from, task_index)?;
    let session_id = codex_session_id_from_task_content(&entry.content).with_context(|| {
        "Automated Git completion requires the selected task's terminal codex:<session-id> marker"
    })?;
    if !task_content_has_completed_note(&entry.content) {
        anyhow::bail!(
            "Automated Git completion requires a dated COMPLETED YYYY-MM-DD: note before moving the task to Done"
        );
    }
    let Some(mut finalization) = store.git_finalization_blocking(project.id, session_id)? else {
        if project.git_mode == AgentGitMode::Off {
            move_task_in_board_after_lock(&board_dir, from, TaskStatus::Done, task_index)?;
            return Ok(false);
        }
        anyhow::bail!(
            "Automated Git completion has no start-state journal for Codex session {session_id}; keep the task in Doing and resume the same session"
        );
    };
    if finalization.git_mode == AgentGitMode::Off {
        anyhow::bail!("Automated Git completion journal unexpectedly has Git mode off");
    }
    let Some(task_identity) = finalization.task_identity.clone() else {
        anyhow::bail!(
            "Automated Git completion task identity was not bound to Codex session {session_id} before Done was requested"
        );
    };
    let selected_identity = durable_task_identity(&entry.content)
        .context("Automated Git completion could not identify the selected task payload")?;
    if selected_identity != task_identity {
        anyhow::bail!(
            "Automated Git completion task content no longer matches the task bound to Codex session {session_id}; restore the original task payload and keep only dated outcome-note changes"
        );
    }
    if finalization.state == GitFinalizationState::Working {
        let current_branch = git_optional_stdout(
            &root,
            &["symbolic-ref", "-q", "HEAD"],
            &[1],
            "verify the frozen task branch",
        )?;
        if current_branch.as_deref() != finalization.branch_ref.as_deref() {
            anyhow::bail!(
                "Automated Git task changed branches after entering Doing; return to the frozen branch before `clt done`"
            );
        }
        let starting_head = finalization
            .starting_head
            .as_deref()
            .context("Automated Git task has no frozen starting commit")?;
        let current_head = resolve_git_commit(&root, "HEAD", "verify the task manifest parent")?;
        if !git_commit_is_ancestor(&root, starting_head, &current_head)? {
            anyhow::bail!(
                "Automated Git task history diverged from its frozen starting commit; CLT will not seal an ambiguous manifest"
            );
        }
        let manifest = capture_agent_git_staged_manifest(
            AgentGitProofContext {
                store,
                project_id: project.id,
            },
            &root,
            &finalization.worktree_baseline,
            session_id,
            &task_identity,
            starting_head,
            finalization.branch_ref.as_deref(),
        )?;
        let tracked = store.track_git_finalization_with_manifest_blocking(
            project.id,
            session_id,
            finalization.generation,
            &task_identity,
            &manifest,
            &context.run_token,
            &agent_timestamp(),
        )?;
        if !tracked {
            anyhow::bail!(
                "Automated Git completion lost its running-session fence before recording completion intent"
            );
        }
        finalization = store
            .git_finalization_blocking(project.id, session_id)?
            .context("The tracked Git completion intent could not be read back")?;
    } else if finalization.state == GitFinalizationState::Tracking {
        let owned = store.compare_and_set_owned_git_finalization_blocking(
            project.id,
            session_id,
            finalization.generation,
            GitFinalizationState::Tracking,
            &context.run_token,
            None,
            None,
            &agent_timestamp(),
        )?;
        if !owned {
            anyhow::bail!(
                "Automated Git completion lost its running-session fence before moving the task"
            );
        }
        finalization = store
            .git_finalization_blocking(project.id, session_id)?
            .context("The owned Git completion intent could not be read back")?;
    }
    if finalization.codex_session_id != session_id
        || finalization.state != GitFinalizationState::Tracking
        || finalization.task_identity.as_deref() != Some(task_identity.as_str())
    {
        anyhow::bail!(
            "Task {} already has an incompatible Git finalization in state {}",
            session_id,
            finalization.state.database_value()
        );
    }

    move_task_without_reordering_after_lock(&board_dir, from, TaskStatus::Done, task_index)?;
    let updated = store.compare_and_set_owned_git_finalization_blocking(
        project.id,
        session_id,
        finalization.generation,
        GitFinalizationState::CommitPending,
        &context.run_token,
        None,
        None,
        &agent_timestamp(),
    )?;
    if !updated {
        anyhow::bail!(
            "The task moved to provisional Done, but its Git finalization changed concurrently; CLT will reconcile it before scheduling other work"
        );
    }
    Ok(true)
}

fn external_completion_lease_holder() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("clt-external-completion-{}-{nonce}", std::process::id())
}

fn agent_project_for_board(
    store: &agent::TursoAgentStore,
    board_dir: &Path,
) -> Result<Option<agent::AgentProject>> {
    let canonical_board = fs::canonicalize(board_dir).unwrap_or_else(|_| board_dir.to_path_buf());
    Ok(store
        .list_projects_blocking()?
        .into_iter()
        .filter(|project| canonical_board.starts_with(project.path.join("tasks")))
        .max_by_key(|project| project.path.as_os_str().len()))
}

fn move_user_task_to_done_with_store_after_lock(
    board_dir: &Path,
    from: TaskStatus,
    task_index: usize,
    entry: &TaskEntry,
    store: &agent::TursoAgentStore,
) -> Result<Option<String>> {
    let Some(session_id) = recoverable_codex_session_id_from_task_content(&entry.content) else {
        move_task_in_board_after_lock(board_dir, from, TaskStatus::Done, task_index)?;
        return Ok(None);
    };
    let Some(project) = agent_project_for_board(store, board_dir)? else {
        move_task_in_board_after_lock(board_dir, from, TaskStatus::Done, task_index)?;
        return Ok(None);
    };
    let Some(finalization) = store.git_finalization_blocking(project.id, session_id)? else {
        move_task_in_board_after_lock(board_dir, from, TaskStatus::Done, task_index)?;
        return Ok(None);
    };
    if finalization.state.is_terminal() {
        move_task_in_board_after_lock(board_dir, from, TaskStatus::Done, task_index)?;
        return Ok(None);
    }
    if finalization.state != GitFinalizationState::Working {
        anyhow::bail!(
            "Task {session_id} has a managed Git journal in {}; its commit proof is already sealed and cannot be replaced by an external completion",
            finalization.state.status_label()
        );
    }
    let bound_identity = finalization
        .task_identity
        .as_deref()
        .context("The Working Git journal has no durable task identity")?;
    if durable_task_identity(&entry.content).as_deref() != Some(bound_identity) {
        anyhow::bail!(
            "Task {session_id} no longer matches its Working Git journal; restore its durable task payload before accepting external completion"
        );
    }

    let acquired_at = agent_timestamp();
    let expires_at = agent_timestamp_after(AGENT_EXTERNAL_COMPLETION_LEASE_SECONDS);
    let lease_holder = external_completion_lease_holder();
    if !store.accept_external_git_completion_blocking(
        project.id,
        session_id,
        finalization.generation,
        bound_identity,
        &lease_holder,
        &acquired_at,
        &expires_at,
    )? {
        let current = store.git_finalization_blocking(project.id, session_id)?;
        let state = current
            .as_ref()
            .map(|journal| journal.state.status_label())
            .unwrap_or("MISSING");
        anyhow::bail!(
            "Task {session_id} changed while CLT was accepting external completion (journal: {state}); retry the Done move"
        );
    }

    let move_result = move_task_in_board_after_lock(board_dir, from, TaskStatus::Done, task_index);
    let release_result = store.release_lease_blocking(project.id, &lease_holder);
    if let Err(release_error) = release_result {
        return match move_result {
            Ok(()) => Err(release_error).with_context(|| {
                format!(
                    "Task {session_id} moved to Done, but CLT could not release its external-completion fence"
                )
            }),
            Err(move_error) => Err(move_error).with_context(|| {
                format!(
                    "CLT also could not release the external-completion fence: {release_error:#}"
                )
            }),
        };
    }
    move_result?;
    Ok(Some(session_id.to_string()))
}

fn move_user_task_to_done_after_lock(
    board_dir: &Path,
    from: TaskStatus,
    task_index: usize,
    entry: &TaskEntry,
) -> Result<Option<String>> {
    let Some(_session_id) = recoverable_codex_session_id_from_task_content(&entry.content) else {
        move_task_in_board_after_lock(board_dir, from, TaskStatus::Done, task_index)?;
        return Ok(None);
    };
    let state_dir = agent_state_dir()?;
    if !state_dir.join(AGENT_DB_FILE).is_file() {
        move_task_in_board_after_lock(board_dir, from, TaskStatus::Done, task_index)?;
        return Ok(None);
    }
    let store = open_agent_store_at(&state_dir)?;
    if store.pending_migration_version().is_some() {
        move_task_in_board_after_lock(board_dir, from, TaskStatus::Done, task_index)?;
        return Ok(None);
    }
    move_user_task_to_done_with_store_after_lock(board_dir, from, task_index, entry, &store)
}

pub(super) fn move_task_in_board(
    board_dir: &Path,
    from: TaskStatus,
    to: TaskStatus,
    task_index_str: &str,
) -> Result<Option<String>> {
    let task_index = parse_one_based_task_index(task_index_str)?;
    let _mutation_lock = acquire_board_mutation_lock(board_dir)?;
    let board = TaskBoard::new(board_dir);
    let entry = board.entry(from, task_index)?;
    if to == TaskStatus::Done {
        return move_user_task_to_done_after_lock(board_dir, from, task_index, &entry);
    }
    ensure_managed_git_task_mutation_allowed(
        board_dir,
        &entry,
        from.is_active() && to.is_active(),
        None,
    )?;
    move_task_in_board_after_lock(board_dir, from, to, task_index)?;
    Ok(None)
}

#[cfg(test)]
pub(super) fn move_task_to_done_in_board_with_store(
    board_dir: &Path,
    from: TaskStatus,
    task_index_str: &str,
    store: &agent::TursoAgentStore,
) -> Result<Option<String>> {
    let task_index = parse_one_based_task_index(task_index_str)?;
    let _mutation_lock = acquire_board_mutation_lock(board_dir)?;
    let entry = task_entry_at(board_dir, from, task_index)?;
    move_user_task_to_done_with_store_after_lock(board_dir, from, task_index, &entry, store)
}

#[cfg(test)]
pub(super) fn move_task_in_board_with_contention_callback(
    board_dir: &Path,
    from: TaskStatus,
    to: TaskStatus,
    task_index_str: &str,
    on_contention: impl FnOnce(),
) -> Result<()> {
    let task_index = parse_one_based_task_index(task_index_str)?;
    let _mutation_lock =
        acquire_board_mutation_lock_with_contention_callback(board_dir, on_contention)?;
    move_task_in_board_after_lock(board_dir, from, to, task_index)
}

pub(super) fn move_task_in_board_after_lock(
    board_dir: &Path,
    from: TaskStatus,
    to: TaskStatus,
    task_index: usize,
) -> Result<()> {
    ensure_status_conversion_allowed(board_dir, to)?;
    TaskBoard::new(board_dir).move_task_after_lock(from, to, task_index)
}

pub(super) fn move_task_to_archive_in_board(
    board_dir: &Path,
    from: TaskStatus,
    task_index_str: &str,
) -> Result<()> {
    let task_index = parse_one_based_task_index(task_index_str)?;
    let _mutation_lock = acquire_board_mutation_lock(board_dir)?;
    let entry = task_entry_at(board_dir, from, task_index)?;
    ensure_managed_git_task_mutation_allowed(board_dir, &entry, false, None)?;

    match (
        &entry.source,
        get_or_create_archive_status_store(board_dir)?,
    ) {
        (TaskSource::Path { path, .. }, StatusStore::Directory(archive_dir)) => {
            move_path_into_directory(path, &archive_dir, None)?;
        }
        (TaskSource::Path { path, .. }, StatusStore::MarkdownFile(archive_file)) => {
            ensure_archive_conversion_allowed(board_dir, &archive_file)?;
            let archive_dir = convert_archive_to_directory(&archive_file)?;
            move_path_into_directory(path, &archive_dir, None)?;
        }
        (TaskSource::MarkdownLine { .. }, StatusStore::Directory(archive_dir)) => {
            insert_content_into_directory(&archive_dir, None, &entry.content)?;
            remove_task_entry(board_dir, from, &entry)?;
        }
        (TaskSource::MarkdownLine { .. }, StatusStore::MarkdownFile(archive_file)) => {
            insert_content_into_markdown(&archive_file, None, &entry.content)?;
            remove_task_entry(board_dir, from, &entry)?;
        }
    }

    Ok(())
}

pub(super) fn update_task_in_board(
    board_dir: &Path,
    status: TaskStatus,
    task_index: usize,
    new_description: &str,
) -> Result<()> {
    let _mutation_lock = acquire_board_mutation_lock(board_dir)?;
    update_task_in_board_after_lock(board_dir, status, task_index, new_description)
}

#[cfg(test)]
pub(super) fn update_task_in_board_with_contention_callback(
    board_dir: &Path,
    status: TaskStatus,
    task_index: usize,
    new_description: &str,
    on_contention: impl FnOnce(),
) -> Result<()> {
    let _mutation_lock =
        acquire_board_mutation_lock_with_contention_callback(board_dir, on_contention)?;
    update_task_in_board_after_lock(board_dir, status, task_index, new_description)
}

pub(super) fn update_task_in_board_after_lock(
    board_dir: &Path,
    status: TaskStatus,
    task_index: usize,
    new_description: &str,
) -> Result<()> {
    let board = TaskBoard::new(board_dir);
    let entry = board.entry(status, task_index)?;
    let session_id = recoverable_codex_session_id_from_task_content(&entry.content);
    let updated_content = match session_id {
        Some(session_id) => task_content_with_codex_session(new_description, session_id),
        None => new_description.trim_end().to_string(),
    };
    ensure_managed_git_task_mutation_allowed(board_dir, &entry, true, Some(&updated_content))?;

    board.write_entry_content(status, &entry, &updated_content)
}

pub(super) fn reorder_task_in_board(
    board_dir: &Path,
    status: TaskStatus,
    from_idx: usize,
    to_idx: usize,
) -> Result<()> {
    let _mutation_lock = acquire_board_mutation_lock(board_dir)?;
    let board = TaskBoard::new(board_dir);
    let entry = board.entry(status, from_idx + 1)?;
    ensure_managed_git_task_mutation_allowed(board_dir, &entry, false, None)?;
    match board.status_store(status)? {
        StatusStore::MarkdownFile(path) => reorder_markdown_task(&path, from_idx, to_idx),
        StatusStore::Directory(path) => reorder_directory_task(&path, from_idx, to_idx),
    }
}

pub(super) fn list_tasks(root: &Path, filter_status: Option<String>) -> Result<()> {
    let board = TaskBoard::for_project(root);
    let session_states = load_task_agent_session_states(root);

    if let Some(ref s) = filter_status {
        let status_name = match s.as_str() {
            "0" => TaskStatus::Backlog.as_str(),
            "1" => TaskStatus::Todo.as_str(),
            "2" => TaskStatus::Doing.as_str(),
            "3" => TaskStatus::Done.as_str(),
            status => status,
        };

        println!("\n--- {} ---", status_name.to_uppercase());
        let status = TaskStatus::parse(status_name)?;
        for (index, entry) in board.entries(status)?.iter().enumerate() {
            println!(
                "{}. {}{}",
                index + 1,
                task_display_text_with_agent_flag(entry, status, &session_states),
                if entry.has_subtasks {
                    " [subtasks]"
                } else {
                    ""
                }
            );
        }
    } else {
        for status in TASK_STATUSES {
            println!("\n--- {} ---", status.as_str().to_uppercase());
            for (index, entry) in board.entries(status)?.iter().enumerate() {
                println!(
                    "{}. {}{}",
                    index + 1,
                    task_display_text_with_agent_flag(entry, status, &session_states),
                    if entry.has_subtasks {
                        " [subtasks]"
                    } else {
                        ""
                    }
                );
            }
        }
    }
    Ok(())
}
