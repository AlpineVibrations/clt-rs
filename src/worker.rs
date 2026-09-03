use super::*;

pub(super) struct AgentWorkerReconciliationRequest<'a> {
    pub(super) state_dir: &'a Path,
    pub(super) store: &'a agent::TursoAgentStore,
    pub(super) now_seconds: u64,
}

pub(super) struct AgentWorkerReconciliationEffects<'a> {
    pub(super) process_is_running: &'a mut dyn FnMut(u32) -> Option<bool>,
    pub(super) launch_dispatching: &'a mut dyn FnMut(&AgentWorkerLaunchSpec) -> Result<()>,
    pub(super) drain_worker: &'a mut dyn FnMut(&agent::AgentWorkerRecord) -> Result<bool>,
    pub(super) timestamp: &'a mut dyn FnMut() -> String,
}

pub(super) struct AgentWorkerReconciliationResult {
    pub(super) active_workers: Vec<agent::AgentWorkerRecord>,
}

pub(super) struct AgentSessionLinkRequest<'a> {
    pub(super) job: &'a AgentRunJob,
    pub(super) session_id: &'a str,
    pub(super) run_status: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentSessionLinkResult {
    AlreadyLinked,
    ExactResumePreserved,
    Attached,
    NoEligibleTask,
}

pub(super) struct AgentResultRecordingRequest<'a> {
    pub(super) job: &'a AgentRunJob,
    pub(super) finalization_lease_holder: &'a str,
    pub(super) lease_transferred: bool,
    pub(super) status: &'static str,
    pub(super) started_at: &'a str,
    pub(super) finished_at: &'a str,
    pub(super) exit_code: Option<i64>,
    pub(super) log_dir: Option<&'a str>,
    pub(super) stdout_path: Option<&'a str>,
    pub(super) stderr_path: Option<&'a str>,
    pub(super) summary: &'a str,
    pub(super) codex_session_id: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AgentResultRecordingResult {
    pub(super) run_id: i64,
}

pub(super) fn next_agent_worker_token(project_id: i64) -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let sequence = AGENT_WORKER_GENERATION.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}-{:09}-p{project_id}-s{sequence}",
        elapsed.as_secs(),
        elapsed.subsec_nanos()
    )
}

pub(super) fn agent_worker_lease_holder(worker_token: &str) -> String {
    format!("clt-worker-{worker_token}")
}

pub(super) struct InlineAgentWorkerGeneration {
    worker_token: String,
}

impl InlineAgentWorkerGeneration {
    pub(super) fn register(worker_token: &str) -> Self {
        ACTIVE_INLINE_AGENT_WORKERS
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(worker_token.to_string());
        Self {
            worker_token: worker_token.to_string(),
        }
    }
}

impl Drop for InlineAgentWorkerGeneration {
    fn drop(&mut self) {
        ACTIVE_INLINE_AGENT_WORKERS
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.worker_token);
    }
}

pub(super) fn inline_agent_worker_generation_is_registered(worker_token: &str) -> bool {
    ACTIVE_INLINE_AGENT_WORKERS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(worker_token)
}

pub(super) fn reconcile_independent_agent_workers(
    state_dir: &Path,
) -> Result<Vec<agent::AgentWorkerRecord>> {
    let store = open_agent_store_at(state_dir)?;
    #[cfg(not(test))]
    cleanup_terminal_agent_worker_services(state_dir, &store, None)?;
    #[cfg(not(test))]
    return reconcile_independent_agent_workers_with(
        state_dir,
        &store,
        agent_timestamp_seconds(),
        |spec| prepare_agent_worker_service(spec).and_then(|_| launch_agent_worker_service(spec)),
        drain_agent_worker_service,
    );
    #[cfg(test)]
    reconcile_independent_agent_workers_with(
        state_dir,
        &store,
        agent_timestamp_seconds(),
        |_| Ok(()),
        |_| Ok(true),
    )
}

pub(super) fn agent_worker_observation_is_stale(
    raw: Option<&str>,
    now: u64,
    timeout_seconds: u64,
) -> bool {
    raw.and_then(|value| value.parse::<u64>().ok())
        .is_none_or(|observed| now.saturating_sub(observed) >= timeout_seconds)
}

pub(super) fn is_inline_agent_worker(worker: &agent::AgentWorkerRecord) -> bool {
    worker
        .service_label
        .starts_with(AGENT_INLINE_WORKER_SERVICE_LABEL_PREFIX)
}

pub(super) fn reconcile_independent_agent_workers_with(
    state_dir: &Path,
    store: &agent::TursoAgentStore,
    now: u64,
    mut launch_dispatching: impl FnMut(&AgentWorkerLaunchSpec) -> Result<()>,
    mut drain_worker: impl FnMut(&agent::AgentWorkerRecord) -> Result<bool>,
) -> Result<Vec<agent::AgentWorkerRecord>> {
    let mut process_is_running = local_process_is_running;
    let mut timestamp = agent_timestamp;
    Ok(reconcile_agent_worker_effects_stage(
        AgentWorkerReconciliationRequest {
            state_dir,
            store,
            now_seconds: now,
        },
        AgentWorkerReconciliationEffects {
            process_is_running: &mut process_is_running,
            launch_dispatching: &mut launch_dispatching,
            drain_worker: &mut drain_worker,
            timestamp: &mut timestamp,
        },
    )?
    .active_workers)
}

pub(super) fn reconcile_agent_worker_effects_stage(
    request: AgentWorkerReconciliationRequest<'_>,
    mut effects: AgentWorkerReconciliationEffects<'_>,
) -> Result<AgentWorkerReconciliationResult> {
    let AgentWorkerReconciliationRequest {
        state_dir,
        store,
        now_seconds: now,
    } = request;
    let workers = store.list_active_workers_blocking()?;
    for worker in workers {
        if worker.protocol_version > AGENT_WORKER_PROTOCOL_VERSION {
            println!(
                "Project {}: action=worker_protocol_wait worker_token={} worker_protocol={} scheduler_protocol={} path={}",
                worker.project_name,
                worker.worker_token,
                worker.protocol_version,
                AGENT_WORKER_PROTOCOL_VERSION,
                worker.project_path.display()
            );
            continue;
        }
        let lease = store.lease_for_project_blocking(worker.project_id)?;
        let owns_lease = lease
            .as_ref()
            .is_some_and(|lease| lease.holder == worker.lease_holder);
        let process_state = worker.worker_pid.and_then(&mut effects.process_is_running);
        let inline_worker = is_inline_agent_worker(&worker);
        let inline_launch_pending = inline_worker
            && store
                .git_launch_state_blocking(worker.project_id, &worker.worker_token)?
                .is_some();
        if inline_launch_pending {
            println!(
                "Project {}: action=inline_worker_launch_reap_wait worker_token={} path={}",
                worker.project_name,
                worker.worker_token,
                worker.project_path.display()
            );
            continue;
        }
        let inline_current_generation_registered = inline_worker
            && worker.worker_pid == Some(std::process::id())
            && inline_agent_worker_generation_is_registered(&worker.worker_token);
        let inline_current_generation_missing = inline_worker
            && worker.worker_pid == Some(std::process::id())
            && !inline_current_generation_registered;
        if inline_current_generation_registered
            && matches!(
                worker.state.as_str(),
                AGENT_WORKER_STATE_RUNNING | AGENT_WORKER_STATE_FINALIZING
            )
        {
            continue;
        }
        if inline_worker
            && worker.worker_pid != Some(std::process::id())
            && worker.worker_pid.is_some()
            && process_state != Some(false)
            && matches!(
                worker.state.as_str(),
                AGENT_WORKER_STATE_RUNNING | AGENT_WORKER_STATE_FINALIZING
            )
        {
            println!(
                "Project {}: action=inline_worker_liveness_unproven_wait worker_token={} worker_pid={} path={}",
                worker.project_name,
                worker.worker_token,
                worker.worker_pid.unwrap_or_default(),
                worker.project_path.display()
            );
            continue;
        }
        let startup_stale = worker.state == AGENT_WORKER_STATE_DISPATCHING
            && agent_worker_observation_is_stale(
                Some(&worker.created_at),
                now,
                AGENT_WORKER_STARTUP_TIMEOUT_SECONDS,
            );
        let heartbeat_stale = matches!(
            worker.state.as_str(),
            AGENT_WORKER_STATE_RUNNING | AGENT_WORKER_STATE_FINALIZING
        ) && agent_worker_observation_is_stale(
            worker.heartbeat_at.as_deref(),
            now,
            AGENT_WORKER_HEARTBEAT_TIMEOUT_SECONDS,
        );

        let abandonment_reason = if inline_worker && worker.state == AGENT_WORKER_STATE_DISPATCHING
        {
            Some("Inline worker reservation was not atomically claimed".to_string())
        } else if inline_current_generation_missing {
            Some("Inline worker generation is no longer owned by this CLT process".to_string())
        } else if !owns_lease {
            if process_state == Some(true) && !heartbeat_stale {
                println!(
                    "Project {}: action=worker_fenced_wait worker_token={} worker_pid={} path={}",
                    worker.project_name,
                    worker.worker_token,
                    worker.worker_pid.unwrap_or_default(),
                    worker.project_path.display()
                );
                None
            } else {
                Some("Worker no longer owns its project lease".to_string())
            }
        } else if startup_stale {
            Some(format!(
                "Worker did not claim its durable reservation within {} seconds",
                AGENT_WORKER_STARTUP_TIMEOUT_SECONDS
            ))
        } else if matches!(
            worker.state.as_str(),
            AGENT_WORKER_STATE_RUNNING | AGENT_WORKER_STATE_FINALIZING
        ) && process_state == Some(false)
        {
            Some("Worker process exited before durable finalization".to_string())
        } else if heartbeat_stale {
            Some(format!(
                "Worker heartbeat was stale for at least {} seconds",
                AGENT_WORKER_HEARTBEAT_TIMEOUT_SECONDS
            ))
        } else if worker.state == AGENT_WORKER_STATE_RUNNING && worker.worker_pid.is_none() {
            Some("Running worker is missing its process ID".to_string())
        } else {
            None
        };

        if let Some(reason) = abandonment_reason {
            let drained = match (effects.drain_worker)(&worker) {
                Ok(drained) => drained,
                Err(error) => {
                    eprintln!(
                        "Project {}: action=worker_drain_retry worker_token={} reason=\"{error:#}\" path={}",
                        worker.project_name,
                        worker.worker_token,
                        worker.project_path.display()
                    );
                    false
                }
            };
            if !drained {
                continue;
            }
            let permitted_successor_holder = lease
                .as_ref()
                .filter(|lease| lease.holder != worker.lease_holder)
                .map(|lease| lease.holder.as_str());
            let abandoned = store.abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: &worker.worker_token,
                expected_state: &worker.state,
                expected_worker_pid: worker.worker_pid,
                expected_heartbeat_at: worker.heartbeat_at.as_deref(),
                finished_at: &(effects.timestamp)(),
                error: &reason,
                permitted_successor_holder,
            })?;
            if abandoned {
                println!(
                    "Project {}: action=worker_abandoned worker_token={} reason=\"{}\" path={}",
                    worker.project_name,
                    worker.worker_token,
                    reason,
                    worker.project_path.display()
                );
            }
            continue;
        }

        if worker.state == AGENT_WORKER_STATE_DISPATCHING {
            let task_selection = match AgentTaskSelection::from_label(&worker.task_selection) {
                Ok(selection) => selection,
                Err(error) => {
                    let reason = format!("Invalid durable worker task selection: {error:#}");
                    if (effects.drain_worker)(&worker)? {
                        let _ = store.abandon_worker_blocking(agent::AgentWorkerAbandonment {
                            worker_token: &worker.worker_token,
                            expected_state: &worker.state,
                            expected_worker_pid: worker.worker_pid,
                            expected_heartbeat_at: worker.heartbeat_at.as_deref(),
                            finished_at: &(effects.timestamp)(),
                            error: &reason,
                            permitted_successor_holder: None,
                        })?;
                    }
                    continue;
                }
            };
            let spec = AgentWorkerLaunchSpec {
                state_dir: state_dir.to_path_buf(),
                executable: worker.binary_path.clone(),
                worker_token: worker.worker_token.clone(),
                project_id: worker.project_id,
                task_selection,
                resume_session_id: worker.resume_session_id.clone(),
                service_label: worker.service_label.clone(),
                command_arguments: Some(
                    serde_json::from_str::<Vec<String>>(&worker.command_arguments)
                        .with_context(|| {
                            format!(
                                "Failed to read persisted launch arguments for worker {}",
                                worker.worker_token
                            )
                        })?
                        .into_iter()
                        .map(OsString::from)
                        .collect(),
                ),
                service_env: AgentServiceEnvironment {
                    codex_path_override: worker.codex_path.clone(),
                    path: worker.path_env.clone(),
                },
            };
            if let Err(error) = (effects.launch_dispatching)(&spec) {
                eprintln!(
                    "Project {}: action=worker_dispatch_retry worker_token={} reason=\"{error:#}\" path={}",
                    worker.project_name,
                    worker.worker_token,
                    worker.project_path.display()
                );
            }
        }
    }

    Ok(AgentWorkerReconciliationResult {
        active_workers: store.list_active_workers_blocking()?,
    })
}

#[cfg(not(test))]
pub(super) fn cleanup_terminal_agent_worker_services(
    state_dir: &Path,
    store: &agent::TursoAgentStore,
    project_path: Option<&Path>,
) -> Result<()> {
    for worker in store.list_terminal_workers_blocking()? {
        if worker.service_cleaned_at.is_some()
            || project_path.is_some_and(|path| worker.project_path != path)
        {
            continue;
        }
        if !drain_agent_worker_service(&worker)? {
            anyhow::bail!(
                "Agent worker service {} is still active after its drain request",
                worker.service_label
            );
        }
        let worker_dir = agent_worker_dir(state_dir, &worker.worker_token);
        if worker_dir.exists() {
            fs::remove_dir_all(&worker_dir).with_context(|| {
                format!("Failed to remove terminal agent worker directory {worker_dir:?}")
            })?;
        }
        store.mark_worker_service_cleaned_blocking(&worker.worker_token, &agent_timestamp())?;
    }
    Ok(())
}

#[cfg(not(test))]
pub(super) fn drain_agent_worker_service(worker: &agent::AgentWorkerRecord) -> Result<bool> {
    if is_inline_agent_worker(worker) {
        return Ok(true);
    }
    match current_agent_platform() {
        AgentPlatform::Macos => {
            let target = format!("{}/{}", launchd_user_domain()?, worker.service_label);
            if run_service_command_optional("launchctl", &["print", &target])? {
                let _ = run_service_command_optional("launchctl", &["bootout", &target])?;
            }
            Ok(!run_service_command_optional(
                "launchctl",
                &["print", &target],
            )?)
        }
        AgentPlatform::Linux => {
            let _ = run_service_command_optional(
                "systemctl",
                &["--user", "stop", &worker.service_label],
            )?;
            Ok(!run_service_command_optional(
                "systemctl",
                &["--user", "is-active", "--quiet", &worker.service_label],
            )?)
        }
        AgentPlatform::Other => Ok(true),
    }
}

pub(super) fn spawn_agent_daemon_run(
    runner: Arc<dyn AgentRunner>,
    job: AgentRunJob,
    shutdown: AgentShutdownSignal,
) -> AgentDaemonRun {
    let project_id = job.project.id;
    let project_name = job.project.name.clone();
    let project_path = job.project.path.clone();
    let handle =
        tokio::task::spawn_blocking(move || run_agent_job(job, runner.as_ref(), &shutdown));

    AgentDaemonRun {
        project_id,
        project_name,
        project_path,
        handle,
    }
}

pub(super) fn dispatch_independent_agent_worker(
    state_dir: &Path,
    executable: &Path,
    job: AgentRunJob,
) -> Result<()> {
    dispatch_independent_agent_worker_with(
        state_dir,
        executable,
        job,
        resolve_agent_service_environment()?,
        prepare_agent_worker_service,
        launch_agent_worker_service,
    )
}

#[cfg(test)]
pub(super) fn dispatch_independent_agent_worker_without_service(
    state_dir: &Path,
    executable: &Path,
    job: AgentRunJob,
) -> Result<()> {
    dispatch_independent_agent_worker_with(
        state_dir,
        executable,
        job,
        AgentServiceEnvironment {
            codex_path_override: None,
            path: OsString::from("/usr/bin:/bin"),
        },
        |_| Ok(()),
        |_| Ok(()),
    )
}

pub(super) fn dispatch_independent_agent_worker_with(
    state_dir: &Path,
    executable: &Path,
    job: AgentRunJob,
    service_env: AgentServiceEnvironment,
    prepare_worker: impl FnOnce(&AgentWorkerLaunchSpec) -> Result<()>,
    launch_worker: impl FnOnce(&AgentWorkerLaunchSpec) -> Result<()>,
) -> Result<()> {
    let platform = current_agent_platform();
    let worker_token = next_agent_worker_token(job.project.id);
    let service_label = agent_worker_service_label(platform, &worker_token)?;
    let spec = AgentWorkerLaunchSpec {
        state_dir: state_dir.to_path_buf(),
        executable: executable.to_path_buf(),
        worker_token: worker_token.clone(),
        project_id: job.project.id,
        task_selection: job.task_selection,
        resume_session_id: job.resume_session_id.clone(),
        service_label,
        command_arguments: None,
        service_env,
    };

    if let Err(error) = prepare_worker(&spec) {
        release_agent_job_lease_for_shutdown(&job)?;
        let worker_dir = agent_worker_dir(state_dir, &spec.worker_token);
        if worker_dir.exists() {
            let _ = fs::remove_dir_all(worker_dir);
        }
        return Err(error).context("Failed to prepare independent agent worker service");
    }

    let command_arguments = agent_worker_command_arguments(&spec)
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let command_arguments = serde_json::to_string(&command_arguments)
        .context("Failed to serialize independent worker launch arguments")?;
    let created_at = agent_timestamp();
    let reservation_result = with_agent_store_at(state_dir, |store| {
        store.reserve_worker_blocking(agent::AgentWorkerReservation {
            project_id: job.project.id,
            worker_token: &spec.worker_token,
            expected_lease_holder: &job.holder,
            max_active_workers: job.max_global_jobs,
            protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
            service_label: &spec.service_label,
            binary_path: &spec.executable,
            command_arguments: &command_arguments,
            path_env: &spec.service_env.path,
            codex_path: spec.service_env.codex_path_override.as_deref(),
            task_selection: spec.task_selection.label(),
            resume_session_id: spec.resume_session_id.as_deref(),
            created_at: &created_at,
        })
    });
    let reserved = match reservation_result {
        Ok(reserved) => reserved,
        Err(error) => {
            let release_result = release_agent_job_lease_for_shutdown(&job);
            return match release_result {
                Ok(()) => Err(error).context("Failed to reserve independent agent worker"),
                Err(release_error) => Err(error).context(format!(
                    "Failed to reserve independent agent worker; releasing its scheduler lease also failed: {release_error:#}"
                )),
            };
        }
    };
    if !reserved {
        release_agent_job_lease_for_shutdown(&job)?;
        anyhow::bail!(
            "Project {} lost its lease before worker {} could be reserved",
            job.project.id,
            spec.worker_token
        );
    }

    if let Err(launch_error) = launch_worker(&spec) {
        let abandoned = with_agent_store_at(state_dir, |store| {
            store.abandon_worker_blocking(agent::AgentWorkerAbandonment {
                worker_token: &spec.worker_token,
                expected_state: AGENT_WORKER_STATE_DISPATCHING,
                expected_worker_pid: None,
                expected_heartbeat_at: Some(&created_at),
                finished_at: &agent_timestamp(),
                error: &format!("Worker service launch failed: {launch_error:#}"),
                permitted_successor_holder: None,
            })
        })?;
        if abandoned {
            return Err(launch_error).context("Failed to launch independent agent worker");
        }
        println!(
            "Project {}: action=worker_launch_ambiguous worker_token={} service={} path={}",
            job.project.name,
            spec.worker_token,
            spec.service_label,
            job.project.path.display()
        );
        return Ok(());
    }

    println!(
        "Project {}: action=worker_dispatched worker_token={} service={} binary={} path={}",
        job.project.name,
        spec.worker_token,
        spec.service_label,
        spec.executable.display(),
        job.project.path.display()
    );
    Ok(())
}

pub(super) fn validate_agent_worker_token(worker_token: &str) -> Result<()> {
    if worker_token.is_empty()
        || !worker_token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        anyhow::bail!("Invalid independent agent worker token");
    }
    Ok(())
}

pub(super) fn run_independent_agent_worker(
    state_dir: &Path,
    project_id: i64,
    worker_token: &str,
    task_selection: AgentTaskSelection,
    resume_session_id: Option<&str>,
) -> Result<()> {
    validate_agent_worker_token(worker_token)?;
    let store = open_agent_store_at(state_dir)?;
    let reserved = store
        .list_active_workers_blocking()?
        .into_iter()
        .find(|worker| worker.worker_token == worker_token)
        .with_context(|| format!("Agent worker reservation {worker_token} is no longer active"))?;
    if reserved.project_id != project_id
        || reserved.protocol_version != AGENT_WORKER_PROTOCOL_VERSION
        || reserved.task_selection != task_selection.label()
        || reserved.resume_session_id.as_deref() != resume_session_id
        || reserved.lease_holder != agent_worker_lease_holder(worker_token)
    {
        anyhow::bail!("Agent worker {worker_token} arguments do not match its durable reservation");
    }

    let started_at = agent_timestamp();
    if !store.claim_worker_blocking(worker_token, std::process::id(), &started_at)? {
        anyhow::bail!("Agent worker {worker_token} lost its startup fence");
    }
    let result = (|| -> Result<()> {
        let project = store
            .list_projects_blocking()?
            .into_iter()
            .find(|project| project.id == project_id)
            .with_context(|| format!("Registered project {project_id} no longer exists"))?;
        let scan = scan_agent_project(&project.path);
        let done_task_contents_before = completed_task_contents(&project.path).unwrap_or_default();
        let blocked_task_snapshots_before =
            blocked_task_snapshots(&project.path).unwrap_or_default();
        let holder = agent_worker_lease_holder(worker_token);
        let runner = CodexAgentRunner::new_with_worker_token(
            state_dir.to_path_buf(),
            Some(worker_token.to_string()),
        )?;
        let completion = run_agent_job(
            AgentRunJob {
                state_dir: state_dir.to_path_buf(),
                project,
                holder,
                worker_token: Some(worker_token.to_string()),
                max_global_jobs: agent_max_global_jobs()?,
                task_selection,
                resume_session_id: resume_session_id.map(str::to_string),
                blocked_task_count_before: scan.blocked_task_count(),
                done_task_contents_before,
                blocked_task_snapshots_before,
            },
            &runner,
            &new_agent_shutdown_signal(),
        )?;
        print_agent_run_completion(&completion)
    })();

    if let Err(error) = &result {
        abandon_agent_worker_after_error(state_dir, worker_token, error)?;
    }
    result
}

pub(super) fn abandon_agent_worker_after_error(
    state_dir: &Path,
    worker_token: &str,
    error: &anyhow::Error,
) -> Result<()> {
    let store = open_agent_store_at(state_dir)?;
    let Some(worker) = store
        .list_active_workers_blocking()?
        .into_iter()
        .find(|worker| worker.worker_token == worker_token)
    else {
        return Ok(());
    };
    if is_inline_agent_worker(&worker)
        && store
            .git_launch_state_blocking(worker.project_id, worker_token)?
            .is_some()
    {
        eprintln!(
            "Agent inline worker {worker_token} is preserving its durable fence until the supervised Codex child consumes the launch boundary or is proven reaped"
        );
        return Ok(());
    }
    let lease = store.lease_for_project_blocking(worker.project_id)?;
    let permitted_successor_holder = lease
        .as_ref()
        .filter(|lease| lease.holder != worker.lease_holder)
        .map(|lease| lease.holder.as_str());
    let abandoned = store.abandon_worker_blocking(agent::AgentWorkerAbandonment {
        worker_token,
        expected_state: &worker.state,
        expected_worker_pid: worker.worker_pid,
        expected_heartbeat_at: worker.heartbeat_at.as_deref(),
        finished_at: &agent_timestamp(),
        error: &format!("Independent worker failed: {error:#}"),
        permitted_successor_holder,
    })?;
    if !abandoned {
        eprintln!(
            "Agent worker {worker_token} changed ownership before its failure could be recorded"
        );
    }
    Ok(())
}

pub(super) fn run_agent_job(
    mut job: AgentRunJob,
    runner: &dyn AgentRunner,
    shutdown: &AgentShutdownSignal,
) -> Result<AgentRunCompletion> {
    let _inline_generation = ensure_durable_inline_git_worker(&mut job)?;
    let state_dir = job.state_dir.clone();
    let worker_token = job.worker_token.clone();
    match run_agent_job_inner(job, runner, shutdown) {
        Ok(completion) => Ok(completion),
        Err(error) => {
            let Some(worker_token) = worker_token.as_deref() else {
                return Err(error);
            };
            if let Err(abandonment_error) =
                abandon_agent_worker_after_error(&state_dir, worker_token, &error)
            {
                return Err(error.context(format!(
                    "Abandoning durable worker {worker_token} after the run error also failed: {abandonment_error:#}"
                )));
            }
            Err(error)
        }
    }
}

pub(super) fn ensure_durable_inline_git_worker(
    job: &mut AgentRunJob,
) -> Result<Option<InlineAgentWorkerGeneration>> {
    if job.worker_token.is_some() || !agent_job_uses_git(job)? {
        return Ok(None);
    }

    let worker_token = next_agent_worker_token(job.project.id);
    let generation = InlineAgentWorkerGeneration::register(&worker_token);
    let lease_holder = agent_worker_lease_holder(&worker_token);
    let service_label = format!("{AGENT_INLINE_WORKER_SERVICE_LABEL_PREFIX}{worker_token}");
    let binary_path = std::env::current_exe()
        .context("Failed to resolve the current CLT executable for inline worker ownership")?;
    let command_arguments = serde_json::to_string(
        &std::env::args_os()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    )
    .context("Failed to serialize inline worker invocation")?;
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let codex_path = agent_codex_path_env();
    let started_at = agent_timestamp();
    let reservation_result = with_agent_store_at(&job.state_dir, |store| {
        store.reserve_and_claim_worker_blocking(
            agent::AgentWorkerReservation {
                project_id: job.project.id,
                worker_token: &worker_token,
                expected_lease_holder: &job.holder,
                max_active_workers: job.max_global_jobs,
                protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                service_label: &service_label,
                binary_path: &binary_path,
                command_arguments: &command_arguments,
                path_env: &path_env,
                codex_path: codex_path.as_deref(),
                task_selection: job.task_selection.label(),
                resume_session_id: job.resume_session_id.as_deref(),
                created_at: &started_at,
            },
            std::process::id(),
            &started_at,
        )
    });
    let reserved = match reservation_result {
        Ok(reserved) => reserved,
        Err(error) => {
            let committed = with_agent_store_at(&job.state_dir, |store| {
                Ok(store
                    .list_active_workers_blocking()?
                    .into_iter()
                    .any(|worker| {
                        worker.worker_token == worker_token
                            && worker.project_id == job.project.id
                            && worker.state == AGENT_WORKER_STATE_RUNNING
                            && worker.worker_pid == Some(std::process::id())
                            && worker.lease_holder == lease_holder
                    }))
            });
            match committed {
                Ok(true) => true,
                Ok(false) => {
                    let release_result = with_agent_store_at(&job.state_dir, |store| {
                        store
                            .release_lease_blocking(job.project.id, &job.holder)
                            .map(|_| ())
                    });
                    return match release_result {
                        Ok(()) => Err(error).context(
                            "Failed to atomically reserve and claim inline agent ownership",
                        ),
                        Err(release_error) => Err(error).context(format!(
                            "Failed to atomically reserve and claim inline agent ownership; releasing its scheduler lease also failed: {release_error:#}"
                        )),
                    };
                }
                Err(inspection_error) => {
                    return Err(error).context(format!(
                        "Inline worker reservation outcome was ambiguous and its durable state could not be inspected: {inspection_error:#}"
                    ));
                }
            }
        }
    };
    if !reserved {
        let release_result = with_agent_store_at(&job.state_dir, |store| {
            store
                .release_lease_blocking(job.project.id, &job.holder)
                .map(|_| ())
        });
        return match release_result {
            Ok(()) => Err(anyhow::anyhow!(
                "Project {} lost its lease or global worker capacity before inline ownership could be reserved",
                job.project.id
            )),
            Err(release_error) => Err(release_error).context(format!(
                "Project {} could not reserve inline ownership or release its scheduler lease",
                job.project.id
            )),
        };
    }

    job.holder = lease_holder;
    job.worker_token = Some(worker_token);
    Ok(Some(generation))
}

pub(super) fn agent_job_uses_git(job: &AgentRunJob) -> Result<bool> {
    if job.project.git_mode != AgentGitMode::Off {
        return Ok(true);
    }
    let Some(session_id) = job.resume_session_id.as_deref() else {
        return Ok(false);
    };
    with_agent_store_at(&job.state_dir, |store| {
        Ok(store
            .git_finalization_blocking(job.project.id, session_id)?
            .is_some_and(|finalization| finalization.git_mode != AgentGitMode::Off))
    })
}

pub(super) fn run_agent_job_inner(
    job: AgentRunJob,
    runner: &dyn AgentRunner,
    shutdown: &AgentShutdownSignal,
) -> Result<AgentRunCompletion> {
    if job.worker_token.is_none() && job.task_selection == AgentTaskSelection::ResumeDoing {
        with_agent_store_at(&job.state_dir, |store| {
            store
                .supersede_abandoned_workers_for_lease_blocking(job.project.id, &job.holder)
                .map(|_| ())
        })?;
    }
    let git_finalization_before_run = job
        .resume_session_id
        .as_deref()
        .map(|session_id| {
            with_agent_store_at(&job.state_dir, |store| {
                store.git_finalization_blocking(job.project.id, session_id)
            })
        })
        .transpose()?
        .flatten();
    let git_proof_recovery_before_run = job
        .resume_session_id
        .as_deref()
        .map(|session_id| {
            with_agent_store_at(&job.state_dir, |store| {
                Ok(store
                    .session_control_blocking(job.project.id, session_id)?
                    .and_then(|control| control.run_token)
                    .is_some_and(|run_token| {
                        run_token.starts_with(AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX)
                    }))
            })
        })
        .transpose()?
        .unwrap_or(false);
    let started_at = agent_timestamp();
    let run_result = runner.run_project(AgentRunRequest {
        project: &job.project,
        task_selection: job.task_selection,
        resume_session_id: job.resume_session_id.as_deref(),
        lease_holder: &job.holder,
        run_token: job.worker_token.as_deref(),
        shutdown,
    });
    renew_agent_job_worker_fence(&job)?;
    let finished_at = agent_timestamp();

    let (
        mut status,
        exit_code,
        log_dir,
        stdout_path,
        stderr_path,
        mut summary,
        codex_session_id,
        session_run_token,
        mut control_action,
    ) = match run_result {
        Ok(mut result) => {
            if matches!(result.status, "success" | "idle")
                && blocked_recovery_made_no_progress(&job)
            {
                result.status = "blocked";
                result.summary = format!(
                    "Blocked-task recovery left {} blocked task(s) unresolved across todo and doing; retry after the recovery backoff. Runner result: {}",
                    job.blocked_task_count_before, result.summary
                );
            }

            if let Some(session_id) = result.codex_session_id.as_deref()
                && let Err(error) = link_agent_session_after_run_stage(AgentSessionLinkRequest {
                    job: &job,
                    session_id,
                    run_status: result.status,
                })
            {
                result.status = "failure";
                result.summary = format!(
                    "{} Failed to persist the Codex session marker on the task: {error:#}",
                    result.summary
                );
            }

            (
                result.status,
                result.exit_code,
                Some(result.log_dir.display().to_string()),
                Some(result.stdout_path.display().to_string()),
                Some(result.stderr_path.display().to_string()),
                result.summary,
                result.codex_session_id,
                result.session_run_token,
                result.control_action,
            )
        }
        Err(err) => (
            "failure",
            None,
            None,
            None,
            None,
            format!("Codex runner failed before completion: {err:#}"),
            job.resume_session_id.clone(),
            None,
            None,
        ),
    };

    let mut git_finalization_pending = false;
    let mut autonomous_git_push_pending = false;
    if control_action.is_none()
        && let Some(session_id) = codex_session_id
            .as_deref()
            .or(job.resume_session_id.as_deref())
    {
        let finalization_result = with_agent_store_at(&job.state_dir, |store| {
            let Some(finalization) = store.git_finalization_blocking(job.project.id, session_id)?
            else {
                return Ok(None);
            };
            let initial_state = finalization.state;
            reconcile_agent_git_finalization(
                store,
                &job.project.path,
                finalization,
                session_run_token.as_deref(),
                None,
            )
            .map(|finalization| Some((initial_state, finalization)))
        });

        match finalization_result {
            Ok(Some((initial_state, finalization)))
                if finalization.state == GitFinalizationState::Completed
                    && (!initial_state.is_terminal()
                        || git_finalization_before_run.as_ref().is_some_and(|before| {
                            !before.state.is_terminal()
                                && before.codex_session_id == finalization.codex_session_id
                                && finalization.generation > before.generation
                        })
                        || session_run_token.as_deref().is_some_and(|run_token| {
                            finalization.owner_run_token.as_deref() == Some(run_token)
                        })
                        || git_proof_recovery_before_run) =>
            {
                status = "success";
                let commit = finalization
                    .commit_oid
                    .as_deref()
                    .map(|oid| &oid[..oid.len().min(12)])
                    .unwrap_or("unknown");
                summary =
                    format!("CLT proved the task-specific Git finalization at commit {commit}.");
            }
            Ok(Some((_, finalization))) if finalization.state == GitFinalizationState::Working => {
                let linked_task = terminal_task_for_codex_session_in_board(
                    &get_tasks_dir(&job.project.path),
                    &finalization.codex_session_id,
                )?;
                let durably_blocked = linked_task.as_ref().is_some_and(|(task_status, task)| {
                    task_status.is_active() && task_entry_is_blocked(task)
                });
                if durably_blocked {
                    status = "blocked";
                    summary = "The linked task recorded a durable blocker; its pre-run Git journal remains available for a later recovery without finalizing the task.".to_string();
                } else if status == "idle" && linked_task.is_none() {
                    if let Some(owner_run_token) = session_run_token
                        .as_deref()
                        .or(finalization.owner_run_token.as_deref())
                    {
                        let cancelled = with_agent_store_at(&job.state_dir, |store| {
                            cancel_unlinked_working_git_finalization(
                                store,
                                &job.project.path,
                                &finalization,
                                owner_run_token,
                            )
                        })?;
                        if !cancelled {
                            git_finalization_pending = true;
                            status = "failure";
                            summary = "The unused Git journal or its task link changed before CLT could cancel it safely."
                                .to_string();
                        }
                    } else {
                        git_finalization_pending = true;
                        status = "failure";
                        summary = "An unused Git journal could not be cancelled because its running generation was unavailable.".to_string();
                    }
                } else if status != "blocked" {
                    git_finalization_pending = true;
                    status = "failure";
                    summary = "The task's Git journal remains WORKING; CLT preserved this exact Codex session because the task is neither durably blocked nor finalized.".to_string();
                }
            }
            Ok(Some((_, finalization)))
                if finalization.state == GitFinalizationState::PushPending =>
            {
                autonomous_git_push_pending = true;
                status = "failure";
                summary = "The sealed task commit remains PUSH-PENDING; CLT will retry its exact frozen-OID publication without resuming Codex or scheduling another project task.".to_string();
            }
            Ok(Some((_, finalization))) if !finalization.state.is_terminal() => {
                git_finalization_pending = true;
                status = "failure";
                summary = format!(
                    "Task Git finalization remains {}; CLT will resume this exact Codex session before scheduling other project work.",
                    finalization.state.status_label()
                );
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                if job.project.git_mode != AgentGitMode::Off
                    && worktree_contains_completed_done_task(&job.project.path, session_id)?
                {
                    git_finalization_pending = true;
                    status = "failure";
                    summary = "The task entered Done without its required durable Git finalization record; CLT preserved the exact session for recovery.".to_string();
                }
            }
            Err(error) => {
                status = "failure";
                let autonomous_push_pending = with_agent_store_at(&job.state_dir, |store| {
                    Ok(store
                        .git_finalization_blocking(job.project.id, session_id)?
                        .is_some_and(|finalization| {
                            finalization.state == GitFinalizationState::PushPending
                        }))
                })?;
                if autonomous_push_pending {
                    autonomous_git_push_pending = true;
                    summary = format!(
                        "The sealed task commit remains PUSH-PENDING after CLT's bounded publication attempt; CLT will retry without resuming Codex: {error:#}"
                    );
                } else {
                    git_finalization_pending = true;
                    summary = format!(
                        "Git finalization proof failed and remains resumable in this exact session: {error:#}"
                    );
                }
            }
        }
    }

    let control_resolution_result: Result<()> = (|| {
        if control_action.is_none()
            && let (Some(session_id), Some(run_token)) =
                (codex_session_id.as_deref(), session_run_token.as_deref())
        {
            loop {
                let control = with_agent_store_at(&job.state_dir, |store| {
                    store.session_control_blocking(job.project.id, session_id)
                })?;
                let Some(control) = control else {
                    break;
                };
                if control.run_token.as_deref() != Some(run_token) {
                    break;
                }
                if let Some(action) = control.state.requested_action() {
                    control_action = Some(action);
                    break;
                }
                if control.state != AgentSessionControlState::Running {
                    break;
                }
                let preserve_for_exact_retry = !autonomous_git_push_pending
                    && (git_finalization_pending
                        || job.task_selection == AgentTaskSelection::ResumeSession
                            && !matches!(status, "success" | "idle" | "blocked"));
                if preserve_for_exact_retry {
                    break;
                }
                let cleared = with_agent_store_at(&job.state_dir, |store| {
                    store.clear_running_session_control_blocking(
                        job.project.id,
                        session_id,
                        Some(run_token),
                    )
                })?;
                if cleared {
                    break;
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = control_resolution_result {
        status = "failure";
        summary = format!("{summary} Failed to resolve Codex session control: {error:#}");
    }

    if let Some(action) = control_action {
        match action {
            AgentSessionControlAction::Stop => {
                status = "stopped";
                summary = "Codex task session stopped and remains resumable.".to_string();
            }
            AgentSessionControlAction::Interrupt => {
                status = "handoff";
                summary = "Codex task session is ready for interactive handoff.".to_string();
            }
        }
    }

    renew_agent_job_worker_fence(&job)?;

    let mut lease_transferred = false;
    let mut finalization_lease_holder = job.holder.clone();
    let session_lifecycle_result: Result<()> = (|| match control_action {
        Some(AgentSessionControlAction::Stop) => {
            let session_id = codex_session_id
                .as_deref()
                .context("Stopped Codex run did not report the session ID needed for resumption")?;
            let run_token = session_run_token
                .as_deref()
                .context("Stopped Codex run did not report its session-control generation")?;
            let stopped = with_agent_store_at(&job.state_dir, |store| {
                store.complete_session_stop_blocking(job.project.id, session_id, run_token)
            })?;
            let already_stopped = !stopped
                && with_agent_store_at(&job.state_dir, |store| {
                    Ok(store
                        .session_control_blocking(job.project.id, session_id)?
                        .is_some_and(|control| {
                            control.state == AgentSessionControlState::Stopped
                                && control.run_token.as_deref() == Some(run_token)
                                && control.child_pid.is_none()
                        }))
                })?;
            if !stopped && !already_stopped {
                anyhow::bail!(
                    "Codex session {session_id} changed before its stop could be finalized"
                );
            }
            Ok(())
        }
        Some(AgentSessionControlAction::Interrupt) => {
            let session_id = codex_session_id.as_deref().context(
                "Interrupted Codex run did not report the session ID needed for handoff",
            )?;
            let run_token = session_run_token
                .as_deref()
                .context("Interrupted Codex run did not report its session-control generation")?;
            let lease_timeout_seconds = agent_lease_timeout()?.as_secs().max(60);
            let holder = with_agent_store_at(&job.state_dir, |store| {
                store.complete_session_interrupt_handoff_blocking(
                    job.project.id,
                    session_id,
                    run_token,
                    &job.holder,
                    lease_timeout_seconds,
                )
            })?;
            let successor_holder = if let Some(holder) = holder {
                Some(holder)
            } else {
                with_agent_store_at(&job.state_dir, |store| {
                    let control = store.session_control_blocking(job.project.id, session_id)?;
                    let lease = store.lease_for_project_blocking(job.project.id)?;
                    Ok(control.and_then(|control| {
                        let holder = control.interactive_holder?;
                        (control.state == AgentSessionControlState::ReadyInteractive
                            && control.run_token.as_deref() == Some(run_token)
                            && control.child_pid.is_none()
                            && lease.as_ref().is_some_and(|lease| lease.holder == holder))
                        .then_some(holder)
                    }))
                })?
            };
            let successor_holder = successor_holder.with_context(|| {
                format!(
                    "Codex session {session_id} changed before its interactive handoff completed"
                )
            })?;
            lease_transferred = true;
            finalization_lease_holder = successor_holder;
            Ok(())
        }
        None => {
            if let Some(session_id) = codex_session_id
                .as_deref()
                .or(job.resume_session_id.as_deref())
            {
                let retry_exact_resume = !autonomous_git_push_pending
                    && (git_finalization_pending
                        || job.task_selection == AgentTaskSelection::ResumeSession
                            && !matches!(status, "success" | "idle" | "blocked"));
                if !retry_exact_resume {
                    with_agent_store_at(&job.state_dir, |store| {
                        store
                            .clear_running_session_control_blocking(
                                job.project.id,
                                session_id,
                                session_run_token.as_deref(),
                            )
                            .map(|_| ())
                    })?;
                }
            }
            Ok(())
        }
    })();

    if let Err(error) = &session_lifecycle_result {
        status = "failure";
        summary = format!("{summary} Failed to finalize Codex session control: {error:#}");
    }

    let recording_result = record_agent_result_stage(AgentResultRecordingRequest {
        job: &job,
        finalization_lease_holder: &finalization_lease_holder,
        lease_transferred,
        status,
        started_at: &started_at,
        finished_at: &finished_at,
        exit_code,
        log_dir: log_dir.as_deref(),
        stdout_path: stdout_path.as_deref(),
        stderr_path: stderr_path.as_deref(),
        summary: &summary,
        codex_session_id: codex_session_id.as_deref(),
    });
    let run_id = recording_result?.run_id;
    session_lifecycle_result?;

    Ok(AgentRunCompletion {
        run_id,
        project_name: job.project.name,
        project_path: job.project.path,
        status,
        summary,
        stdout_path,
        stderr_path,
    })
}

pub(super) fn record_agent_result_stage(
    request: AgentResultRecordingRequest<'_>,
) -> Result<AgentResultRecordingResult> {
    let AgentResultRecordingRequest {
        job,
        finalization_lease_holder,
        lease_transferred,
        status,
        started_at,
        finished_at,
        exit_code,
        log_dir,
        stdout_path,
        stderr_path,
        summary,
        codex_session_id,
    } = request;
    let run_record_result = if let Some(worker_token) = job.worker_token.as_deref() {
        with_agent_store_at(&job.state_dir, |store| {
            store
                .finalize_worker_blocking(agent::AgentWorkerFinalization {
                    worker_token,
                    expected_worker_pid: Some(std::process::id()),
                    expected_lease_holder: finalization_lease_holder,
                    status,
                    finished_at,
                    exit_code,
                    log_dir,
                    stdout_path,
                    stderr_path,
                    summary: Some(summary),
                    codex_session_id,
                    error: None,
                })
                .and_then(|run_id| {
                    run_id.with_context(|| {
                        format!("Agent worker {worker_token} lost its finalization fence")
                    })
                })
        })
    } else {
        with_agent_store_at(&job.state_dir, |store| {
            store.record_run_outcome_blocking(agent::AgentRunOutcome {
                project_id: job.project.id,
                status,
                started_at,
                finished_at: Some(finished_at),
                exit_code,
                log_dir,
                stdout_path,
                stderr_path,
                summary: Some(summary),
                codex_session_id,
            })
        })
    };
    let release_result = if job.worker_token.is_some() || lease_transferred {
        Ok(())
    } else {
        with_agent_store_at(&job.state_dir, |store| {
            store
                .release_lease_blocking(job.project.id, &job.holder)
                .map(|_| ())
        })
    };
    let run_id = run_record_result?;
    release_result?;
    Ok(AgentResultRecordingResult { run_id })
}

pub(super) fn renew_agent_job_worker_fence(job: &AgentRunJob) -> Result<()> {
    let Some(worker_token) = job.worker_token.as_deref() else {
        return Ok(());
    };
    let expires_at = agent_timestamp_after(agent_lease_timeout()?.as_secs());
    let renewed = with_agent_store_at(&job.state_dir, |store| {
        store.renew_worker_blocking(
            worker_token,
            std::process::id(),
            &agent_timestamp(),
            &expires_at,
        )
    })?;
    if !renewed {
        let snapshot = with_agent_store_at(&job.state_dir, |store| {
            store.worker_fence_snapshot_blocking(worker_token, std::process::id())
        })?;
        anyhow::bail!("Agent worker {worker_token} lost its durable ownership fence ({snapshot})");
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn attach_codex_session_after_run(
    job: &AgentRunJob,
    session_id: &str,
    run_status: &str,
) -> Result<()> {
    link_agent_session_after_run_stage(AgentSessionLinkRequest {
        job,
        session_id,
        run_status,
    })
    .map(|_| ())
}

pub(super) fn link_agent_session_after_run_stage(
    request: AgentSessionLinkRequest<'_>,
) -> Result<AgentSessionLinkResult> {
    let AgentSessionLinkRequest {
        job,
        session_id,
        run_status,
    } = request;
    let _mutation_lock = acquire_board_mutation_lock(&get_tasks_dir(&job.project.path))?;
    if terminal_task_for_codex_session_in_board(&get_tasks_dir(&job.project.path), session_id)?
        .is_some()
    {
        return Ok(AgentSessionLinkResult::AlreadyLinked);
    }
    if job.task_selection == AgentTaskSelection::ResumeSession {
        if task_status_for_codex_session(&job.project.path, session_id)?.is_some() {
            return Ok(AgentSessionLinkResult::ExactResumePreserved);
        }
        anyhow::bail!(
            "The task marker for exact Codex session {session_id} disappeared before handback"
        );
    }

    let completed = newly_completed_task(&job.project.path, &job.done_task_contents_before)?
        .map(|entry| (TaskStatus::Done, entry));
    let blocked = blocked_task_after_run(
        &job.project.path,
        &job.blocked_task_snapshots_before,
        completed.is_none() && job.task_selection != AgentTaskSelection::NextTodo,
    )?;
    let task = match (completed, blocked) {
        (Some(task), None) | (None, Some(task)) => Some(task),
        (None, None) | (Some(_), Some(_)) => None,
    };

    if let Some((status, entry)) = task {
        attach_codex_session_to_task_after_lock(
            &job.project.path,
            status,
            &entry,
            session_id,
            || {},
        )?;
        Ok(AgentSessionLinkResult::Attached)
    } else if matches!(run_status, "success" | "blocked" | "stopped" | "handoff") {
        anyhow::bail!(
            "Could not identify exactly one completed or blocked task for Codex session {session_id}"
        );
    } else {
        Ok(AgentSessionLinkResult::NoEligibleTask)
    }
}

pub(super) fn task_contents_for_status(
    project_root: &Path,
    status: TaskStatus,
) -> Result<Vec<String>> {
    Ok(read_task_entries(&get_tasks_dir(project_root), status)?
        .into_iter()
        .map(|entry| entry.content)
        .collect())
}

pub(super) fn automated_codex_session_to_resume(
    project_root: &Path,
    task_selection: AgentTaskSelection,
) -> Result<Option<String>> {
    let tasks = match task_selection {
        AgentTaskSelection::NextTodo => return Ok(None),
        AgentTaskSelection::ResumeDoing => {
            read_task_entries(&get_tasks_dir(project_root), TaskStatus::Doing)?
        }
        AgentTaskSelection::RecoverBlocked => blocked_tasks(project_root)?
            .into_iter()
            .map(|(_, task)| task)
            .collect(),
        AgentTaskSelection::ResumeSession => return Ok(None),
    };

    if tasks.len() != 1 {
        return Ok(None);
    }

    Ok(tasks.first().and_then(codex_session_for_task))
}

pub(super) fn attach_codex_session_to_active_task(
    project_root: &Path,
    task_selection: AgentTaskSelection,
    doing_task_contents_before: &[String],
    blocked_task_snapshots_before: &[BlockedTaskSnapshot],
    session_id: &str,
) -> Result<bool> {
    let _mutation_lock = acquire_board_mutation_lock(&get_tasks_dir(project_root))?;
    if task_status_for_codex_session(project_root, session_id)?.is_some() {
        return Ok(true);
    }

    let tasks = read_task_entries(&get_tasks_dir(project_root), TaskStatus::Doing)?;
    let newly_started = newly_added_task_entry(doing_task_contents_before, &tasks);
    let task = match task_selection {
        AgentTaskSelection::NextTodo => newly_started,
        AgentTaskSelection::ResumeDoing => (tasks.len() == 1).then(|| tasks.first()).flatten(),
        AgentTaskSelection::RecoverBlocked => newly_started.or_else(|| {
            let task = (tasks.len() == 1).then(|| tasks.first()).flatten()?;
            let snapshot = (blocked_task_snapshots_before.len() == 1)
                .then(|| blocked_task_snapshots_before.first())
                .flatten()?;
            (snapshot.status == TaskStatus::Doing && snapshot.content == task.content.trim_end())
                .then_some(task)
        }),
        AgentTaskSelection::ResumeSession => None,
    };
    let Some(task) = task else {
        return Ok(false);
    };

    attach_codex_session_to_task_after_lock(
        project_root,
        TaskStatus::Doing,
        task,
        session_id,
        || {},
    )?;
    Ok(true)
}

pub(super) fn completed_task_contents(project_root: &Path) -> Result<Vec<String>> {
    Ok(
        read_task_entries(&get_tasks_dir(project_root), TaskStatus::Done)?
            .into_iter()
            .map(|entry| entry.content)
            .collect(),
    )
}

pub(super) fn blocked_task_snapshots(project_root: &Path) -> Result<Vec<BlockedTaskSnapshot>> {
    Ok(blocked_tasks(project_root)?
        .into_iter()
        .map(|(status, entry)| BlockedTaskSnapshot {
            status,
            content: entry.content.trim_end().to_string(),
        })
        .collect())
}

pub(super) fn blocked_tasks(project_root: &Path) -> Result<Vec<(TaskStatus, TaskEntry)>> {
    let board_dir = get_tasks_dir(project_root);
    let mut tasks = Vec::new();

    for status in [TaskStatus::Todo, TaskStatus::Doing] {
        tasks.extend(
            read_task_entries(&board_dir, status)?
                .into_iter()
                .filter(task_entry_is_blocked)
                .map(|entry| (status, entry)),
        );
    }

    Ok(tasks)
}

pub(super) fn blocked_task_after_run(
    project_root: &Path,
    snapshots_before: &[BlockedTaskSnapshot],
    allow_unchanged_single_task: bool,
) -> Result<Option<(TaskStatus, TaskEntry)>> {
    let tasks_after = blocked_tasks(project_root)?;
    let mut remaining = std::collections::HashMap::<BlockedTaskSnapshot, usize>::new();
    for snapshot in snapshots_before {
        *remaining.entry(snapshot.clone()).or_default() += 1;
    }

    let changed_tasks = tasks_after
        .iter()
        .filter(|(status, entry)| {
            let snapshot = BlockedTaskSnapshot {
                status: *status,
                content: entry.content.trim_end().to_string(),
            };
            let Some(count) = remaining.get_mut(&snapshot) else {
                return true;
            };
            if *count == 0 {
                true
            } else {
                *count -= 1;
                false
            }
        })
        .collect::<Vec<_>>();
    let changed_task = (changed_tasks.len() == 1).then(|| changed_tasks[0]);

    Ok(changed_task
        .or_else(|| {
            (allow_unchanged_single_task && tasks_after.len() == 1)
                .then(|| tasks_after.first())
                .flatten()
        })
        .cloned())
}

pub(super) fn newly_completed_task(
    project_root: &Path,
    contents_before: &[String],
) -> Result<Option<TaskEntry>> {
    let entries_after = read_task_entries(&get_tasks_dir(project_root), TaskStatus::Done)?;
    Ok(newly_added_task_entry(contents_before, &entries_after).cloned())
}

pub(super) fn newly_added_task_entry<'a>(
    contents_before: &[String],
    entries_after: &'a [TaskEntry],
) -> Option<&'a TaskEntry> {
    if entries_after.len() <= contents_before.len() {
        return None;
    }

    if entries_after.len() == contents_before.len() + 1 {
        let candidates = (0..entries_after.len())
            .filter(|skipped| {
                entries_after
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| index != skipped)
                    .map(|(_, entry)| entry.content.as_str())
                    .eq(contents_before.iter().map(String::as_str))
            })
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [index] => return entries_after.get(*index),
            [_, _, ..] => return None,
            [] => {}
        }
    }

    let mut remaining = std::collections::HashMap::<&str, usize>::new();
    for content in contents_before {
        *remaining.entry(content.as_str()).or_default() += 1;
    }

    let unmatched = entries_after
        .iter()
        .filter(|entry| {
            let Some(count) = remaining.get_mut(entry.content.as_str()) else {
                return true;
            };
            if *count == 0 {
                true
            } else {
                *count -= 1;
                false
            }
        })
        .collect::<Vec<_>>();
    (unmatched.len() == 1).then(|| unmatched[0])
}

pub(super) fn blocked_recovery_made_no_progress(job: &AgentRunJob) -> bool {
    if job.task_selection != AgentTaskSelection::RecoverBlocked {
        return false;
    }

    let scan = scan_agent_project(&job.project.path);
    scan.blocked_task_count() >= job.blocked_task_count_before
}

pub(super) fn release_agent_job_lease_for_shutdown(job: &AgentRunJob) -> Result<()> {
    let released = with_agent_store_at(&job.state_dir, |store| {
        store.release_lease_blocking(job.project.id, &job.holder)
    })?;

    if released {
        println!(
            "Project {}: action=release reason=shutdown_before_run lease_holder={} path={}",
            job.project.name,
            job.holder,
            job.project.path.display()
        );
    }

    Ok(())
}

pub(super) fn print_agent_run_completion(completion: &AgentRunCompletion) -> Result<()> {
    println!(
        "Recorded scheduler run {} for {} ({}) status={}",
        completion.run_id,
        completion.project_name,
        completion.project_path.display(),
        completion.status
    );
    print_agent_run_failure_details(
        completion.status,
        &completion.summary,
        completion.stdout_path.as_deref(),
        completion.stderr_path.as_deref(),
    )
}

pub(super) fn print_agent_run_failure_details(
    status: &str,
    summary: &str,
    stdout_path: Option<&str>,
    stderr_path: Option<&str>,
) -> Result<()> {
    let summary_label = match status {
        "failure" | "timeout" => "run_failure_summary",
        "blocked" => "run_blocked_summary",
        _ => return Ok(()),
    };

    println!("{summary_label}={summary}");
    println!("run_stdout={}", stdout_path.unwrap_or("<not recorded>"));
    println!("run_stderr={}", stderr_path.unwrap_or("<not recorded>"));
    print_agent_log_tail_with_limit("stderr_tail", stderr_path, 20)
}

pub(super) fn print_agent_run_heartbeat(
    project: &agent::AgentProject,
    elapsed: Duration,
    timeout: Duration,
    stdout_path: &Path,
    stderr_path: &Path,
    last_stderr_bytes: &mut u64,
) -> Result<()> {
    let stdout_bytes = file_size(stdout_path);
    let stderr_bytes = file_size(stderr_path);
    let print_tail = agent_heartbeat_tail_enabled()?;

    println!(
        "Project {}: action=still_running elapsed_seconds={} timeout_seconds={} stdout_bytes={} stderr_bytes={} stdout={} stderr={} path={}",
        project.name,
        elapsed.as_secs(),
        timeout.as_secs(),
        format_optional_u64(stdout_bytes),
        format_optional_u64(stderr_bytes),
        stdout_path.display(),
        stderr_path.display(),
        project.path.display()
    );

    if let Some(stderr_bytes) = stderr_bytes {
        if print_tail && stderr_bytes > *last_stderr_bytes {
            print_agent_log_tail_if_nonempty("stderr_tail", stderr_path, 5)?;
        }
        *last_stderr_bytes = stderr_bytes;
    }

    Ok(())
}

pub(super) fn print_agent_log_tail_if_nonempty(
    label: &str,
    path: &Path,
    limit: usize,
) -> Result<()> {
    match fs::read_to_string(path) {
        Ok(content) => {
            let tail = tail_lines(&content, limit);
            if !tail.is_empty() {
                println!("{label}={}", path.display());
                for line in tail {
                    println!("  {line}");
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            println!("{label}=<unreadable: {}>", err);
        }
    }

    Ok(())
}

pub(super) fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|metadata| metadata.len())
}

pub(super) fn format_optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}
