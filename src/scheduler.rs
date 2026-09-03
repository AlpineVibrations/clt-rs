use super::*;

pub(super) fn run_agent_daemon() -> Result<()> {
    let state_dir = ensure_agent_state_dir()?;
    let poll_interval = agent_poll_interval()?;
    wait_for_deferred_agent_migrations(&state_dir, poll_interval)?;
    if current_agent_platform() == AgentPlatform::Other {
        let runner: Arc<dyn AgentRunner> = Arc::new(CodexAgentRunner::new(state_dir.clone())?);
        return run_agent_daemon_loop_with_executor(
            &state_dir,
            AgentDaemonExecutor::Inline(runner),
            poll_interval,
            None,
            new_agent_shutdown_signal(),
        );
    }
    let current_executable =
        std::env::current_exe().context("Failed to resolve the agent scheduler executable")?;
    let generation_root = state_dir.join("worker-generations");
    let executable = if current_executable.starts_with(&generation_root) {
        current_executable
    } else {
        snapshot_agent_service_binary(&state_dir, &current_executable)?
    };
    run_agent_daemon_loop_with_executor(
        &state_dir,
        AgentDaemonExecutor::Independent {
            executable,
            dispatch: dispatch_independent_agent_worker,
        },
        poll_interval,
        None,
        new_agent_shutdown_signal(),
    )
}

pub(super) fn wait_for_deferred_agent_migrations(
    state_dir: &Path,
    poll_interval: Duration,
) -> Result<()> {
    let retry_interval = std::cmp::min(poll_interval, Duration::from_secs(5));
    let mut announced_version = None;
    loop {
        let store = open_agent_store_at(state_dir)?;
        let Some(version) = store.pending_migration_version() else {
            return Ok(());
        };
        drop(store);

        // Compatibility-mode reconciliation intentionally uses only the shared
        // worker/control schema. It must stay usable by a scheduler generation
        // that has deferred a migration newer than the pinned workers.
        let workers = reconcile_independent_agent_workers(state_dir)?;
        if workers.is_empty() {
            continue;
        }
        if announced_version != Some(version) {
            println!(
                "Agent schema migration {version} is deferred while {} pinned worker(s) finish; controls and crash recovery remain active.",
                workers.len()
            );
            announced_version = Some(version);
        }
        thread::sleep(retry_interval);
    }
}

#[cfg(test)]
pub(super) fn run_agent_daemon_loop(
    state_dir: &Path,
    runner: Arc<dyn AgentRunner>,
    poll_interval: Duration,
    max_passes: Option<usize>,
) -> Result<()> {
    run_agent_daemon_loop_with_shutdown(
        state_dir,
        runner,
        poll_interval,
        max_passes,
        new_agent_shutdown_signal(),
    )
}

#[cfg(test)]
pub(super) fn run_agent_daemon_loop_with_shutdown(
    state_dir: &Path,
    runner: Arc<dyn AgentRunner>,
    poll_interval: Duration,
    max_passes: Option<usize>,
    shutdown: AgentShutdownSignal,
) -> Result<()> {
    run_agent_daemon_loop_with_executor(
        state_dir,
        AgentDaemonExecutor::Inline(runner),
        poll_interval,
        max_passes,
        shutdown,
    )
}

pub(super) fn run_agent_daemon_loop_with_executor(
    state_dir: &Path,
    executor: AgentDaemonExecutor,
    poll_interval: Duration,
    max_passes: Option<usize>,
    shutdown: AgentShutdownSignal,
) -> Result<()> {
    if max_passes == Some(0) {
        return Ok(());
    }

    let max_global_jobs = agent_max_global_jobs()?;
    println!(
        "Starting clt agent daemon with poll_interval_seconds={} empty_registry_poll_interval_seconds={} max_global_jobs={} state_dir={} database={}",
        poll_interval.as_secs(),
        AGENT_EMPTY_REGISTRY_POLL_INTERVAL_SECONDS,
        max_global_jobs,
        state_dir.display(),
        state_dir.join(AGENT_DB_FILE).display()
    );

    let runtime = tokio::runtime::Runtime::new()
        .context("Failed to create async runtime for agent daemon")?;
    runtime.block_on(run_agent_daemon_loop_async(
        state_dir.to_path_buf(),
        executor,
        poll_interval,
        max_passes,
        shutdown,
    ))
}

pub(super) async fn run_agent_daemon_loop_async(
    state_dir: PathBuf,
    executor: AgentDaemonExecutor,
    poll_interval: Duration,
    max_passes: Option<usize>,
    shutdown: AgentShutdownSignal,
) -> Result<()> {
    let daemon_checkin = AgentDaemonCheckinSource::current_with_holder(match &executor {
        AgentDaemonExecutor::Inline(_) => agent_lease_holder(),
        AgentDaemonExecutor::Independent { .. } => agent_scheduler_lease_holder(),
    });
    let mut scheduled_passes = 0;
    let mut active_passes: Vec<tokio::task::JoinHandle<Result<AgentSchedulerStart>>> = Vec::new();
    let mut active_runs: Vec<AgentDaemonRun> = Vec::new();
    let mut next_sleep = poll_interval;

    loop {
        let mut run_index = 0;
        while run_index < active_runs.len() {
            if !active_runs[run_index].handle.is_finished() {
                run_index += 1;
                continue;
            }

            let run = active_runs.swap_remove(run_index);
            match run.handle.await.context("Agent run task failed")? {
                Ok(completion) => {
                    print_agent_run_completion(&completion)?;
                }
                Err(err) => {
                    println!(
                        "Project {}: action=run_task_failed reason=\"{err:#}\" path={}",
                        run.project_name,
                        run.project_path.display()
                    );
                }
            }
        }

        let mut index = 0;
        while index < active_passes.len() {
            if !active_passes[index].is_finished() {
                index += 1;
                continue;
            }

            let handle = active_passes.swap_remove(index);
            let start = match handle.await {
                Ok(Ok(start)) => start,
                Ok(Err(error)) => {
                    eprintln!("Agent scheduler pass failed; the daemon will retry: {error:#}");
                    next_sleep = poll_interval;
                    continue;
                }
                Err(error) => {
                    eprintln!("Agent scheduler pass task failed; the daemon will retry: {error}");
                    next_sleep = poll_interval;
                    continue;
                }
            };
            print_agent_scheduler_pass(&start.pass);
            next_sleep = agent_daemon_sleep_interval(&start.pass, poll_interval);
            for job in start.jobs {
                if shutdown.load(Ordering::SeqCst) {
                    release_agent_job_lease_for_shutdown(&job)?;
                } else {
                    match &executor {
                        AgentDaemonExecutor::Inline(runner) => {
                            active_runs.push(spawn_agent_daemon_run(
                                Arc::clone(runner),
                                job,
                                Arc::clone(&shutdown),
                            ));
                        }
                        AgentDaemonExecutor::Independent {
                            executable,
                            dispatch,
                        } => {
                            let dispatch_state_dir = state_dir.clone();
                            let dispatch_executable = executable.clone();
                            let dispatch = *dispatch;
                            let project_id = job.project.id;
                            let lease_holder = job.holder.clone();
                            match tokio::task::spawn_blocking(move || {
                                dispatch(&dispatch_state_dir, &dispatch_executable, job)
                            })
                            .await
                            {
                                Ok(Ok(())) => {}
                                Ok(Err(error)) => {
                                    eprintln!("Agent worker dispatch failed: {error:#}");
                                }
                                Err(error) => {
                                    eprintln!(
                                        "Independent agent worker dispatch task failed: {error}"
                                    );
                                    release_agent_dispatch_lease_best_effort(
                                        state_dir.clone(),
                                        project_id,
                                        lease_holder,
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                }
            }
        }

        let scheduling_stopped = shutdown.load(Ordering::SeqCst)
            || max_passes.is_some_and(|max_passes| scheduled_passes >= max_passes);
        if scheduling_stopped && active_passes.is_empty() && active_runs.is_empty() {
            clear_agent_daemon_checkin_best_effort(&state_dir, &daemon_checkin.holder).await;
            return Ok(());
        }

        if !scheduling_stopped && active_passes.is_empty() {
            let task_state_dir = state_dir.clone();
            let active_project_ids = active_runs.iter().map(|run| run.project_id).collect();
            let task_daemon_checkin = daemon_checkin.clone();
            active_passes.push(tokio::task::spawn_blocking(move || {
                run_agent_daemon_scheduler_pass_with_active_and_checkin(
                    &task_state_dir,
                    active_project_ids,
                    Some(&task_daemon_checkin),
                )
            }));
            scheduled_passes += 1;
        }

        wait_for_agent_daemon_sleep_or_shutdown(next_sleep, max_passes, &shutdown).await?;
    }
}

pub(super) async fn wait_for_agent_daemon_sleep_or_shutdown(
    sleep_duration: Duration,
    max_passes: Option<usize>,
    shutdown: &AgentShutdownSignal,
) -> Result<()> {
    if max_passes.is_some() || shutdown.load(Ordering::SeqCst) {
        tokio::time::sleep(sleep_duration).await;
        return Ok(());
    }

    tokio::select! {
        _ = tokio::time::sleep(sleep_duration) => Ok(()),
        signal = tokio::signal::ctrl_c() => {
            signal.context("Failed to listen for Ctrl-C shutdown signal")?;
            println!("Received Ctrl-C; stopping the scheduler. Independent workers continue.");
            shutdown.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
}

pub(super) fn agent_daemon_sleep_interval(
    pass: &AgentSchedulerPass,
    poll_interval: Duration,
) -> Duration {
    if pass.scanned_projects == 0 {
        std::cmp::min(
            poll_interval,
            Duration::from_secs(AGENT_EMPTY_REGISTRY_POLL_INTERVAL_SECONDS),
        )
    } else {
        poll_interval
    }
}

pub(super) fn run_agent_once() -> Result<AgentSchedulerPass> {
    let state_dir = ensure_agent_state_dir()?;
    let store = open_agent_store_at(&state_dir)?;
    if let Some(version) = store.pending_migration_version() {
        anyhow::bail!(
            "Agent schema migration {version} is deferred while pinned workers are active; this foreground scheduler will not start new work until they finish"
        );
    }
    drop(store);
    let runner = CodexAgentRunner::new(state_dir.clone())?;
    run_agent_once_with_runner(&state_dir, &runner)
}

pub(super) fn print_agent_scheduler_pass(pass: &AgentSchedulerPass) {
    println!(
        "Scheduler pass complete: scanned={} pending_projects={} active_agent_jobs={} skipped_active_lease={} deferred_projects={} runs_started={} runs_recorded={}",
        pass.scanned_projects,
        pass.pending_projects,
        pass.active_agent_jobs,
        pass.skipped_active_lease,
        pass.deferred_projects,
        pass.runs_started,
        pass.runs_recorded
    );
}

pub(super) fn run_agent_once_with_runner(
    state_dir: &Path,
    runner: &dyn AgentRunner,
) -> Result<AgentSchedulerPass> {
    let shutdown = new_agent_shutdown_signal();
    let mut start = run_agent_scheduler_pass(state_dir, true, &[])?;
    for job in start.jobs {
        let completion = run_agent_job(job, runner, &shutdown)?;
        print_agent_run_completion(&completion)?;
        start.pass.runs_recorded += 1;
    }

    Ok(start.pass)
}

#[cfg(test)]
pub(super) fn run_agent_daemon_scheduler_pass(state_dir: &Path) -> Result<AgentSchedulerStart> {
    run_agent_daemon_scheduler_pass_with_active(state_dir, Vec::new())
}

#[cfg(test)]
pub(super) fn run_agent_daemon_scheduler_pass_with_active(
    state_dir: &Path,
    active_project_ids: Vec<i64>,
) -> Result<AgentSchedulerStart> {
    let daemon_checkin = AgentDaemonCheckinSource::current();
    run_agent_daemon_scheduler_pass_with_active_and_checkin(
        state_dir,
        active_project_ids,
        Some(&daemon_checkin),
    )
}

pub(super) fn run_agent_daemon_scheduler_pass_with_active_and_checkin(
    state_dir: &Path,
    active_project_ids: Vec<i64>,
    daemon_checkin: Option<&AgentDaemonCheckinSource>,
) -> Result<AgentSchedulerStart> {
    run_agent_daemon_database_operation_with_recovery(
        || {
            run_agent_scheduler_pass_with_daemon_checkin(
                state_dir,
                false,
                &active_project_ids,
                daemon_checkin,
            )
        },
        || {
            with_agent_store_at(state_dir, |store| {
                store.rebuild_active_worker_project_index_blocking()
            })
        },
    )
}

pub(super) fn run_agent_daemon_database_operation_with_recovery<T>(
    mut operation: impl FnMut() -> Result<T>,
    mut rebuild_active_worker_index: impl FnMut() -> Result<()>,
) -> Result<T> {
    let mut database_lock_attempts = 0;
    let mut worker_index_rebuild_attempted = false;
    loop {
        match operation() {
            Ok(result) => return Ok(result),
            Err(err)
                if agent_error_indicates_damaged_active_worker_index(&err)
                    && !worker_index_rebuild_attempted =>
            {
                worker_index_rebuild_attempted = true;
                let original_error = format!("{err:#}");
                rebuild_active_worker_index().with_context(|| {
                    format!(
                        "Failed to rebuild {AGENT_WORKERS_ACTIVE_PROJECT_INDEX} after scheduler error: {original_error}"
                    )
                })?;
                eprintln!(
                    "Scheduler pass recovery: rebuilt index={AGENT_WORKERS_ACTIVE_PROJECT_INDEX}; retrying"
                );
            }
            Err(err)
                if agent_error_is_database_locked(&err)
                    && database_lock_attempts < AGENT_DAEMON_DATABASE_LOCK_RETRY_ATTEMPTS =>
            {
                database_lock_attempts += 1;
                println!(
                    "Scheduler pass retry: reason=database_locked attempt={} max_attempts={}",
                    database_lock_attempts, AGENT_DAEMON_DATABASE_LOCK_RETRY_ATTEMPTS
                );
                thread::sleep(Duration::from_millis(
                    AGENT_DAEMON_DATABASE_LOCK_RETRY_MILLIS,
                ));
            }
            Err(err) => return Err(err),
        }
    }
}

pub(super) fn agent_error_indicates_damaged_active_worker_index(err: &anyhow::Error) -> bool {
    let rendered = format!("{err:#}");
    rendered.contains("IdxDelete: no matching index entry found for key")
        && rendered.contains("worker")
}

pub(super) fn agent_error_is_database_locked(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("database is locked"))
}

pub(super) fn run_agent_scheduler_pass(
    state_dir: &Path,
    reclaim_current_process_leases: bool,
    active_project_ids: &[i64],
) -> Result<AgentSchedulerStart> {
    run_agent_scheduler_pass_with_daemon_checkin(
        state_dir,
        reclaim_current_process_leases,
        active_project_ids,
        None,
    )
}

pub(super) fn run_agent_scheduler_pass_with_daemon_checkin(
    state_dir: &Path,
    reclaim_current_process_leases: bool,
    active_project_ids: &[i64],
    daemon_checkin: Option<&AgentDaemonCheckinSource>,
) -> Result<AgentSchedulerStart> {
    run_agent_scheduler_pass_with_max_global_jobs(
        state_dir,
        reclaim_current_process_leases,
        active_project_ids,
        agent_max_global_jobs()?,
        daemon_checkin,
    )
}

pub(super) fn run_agent_scheduler_pass_with_max_global_jobs(
    state_dir: &Path,
    reclaim_current_process_leases: bool,
    active_project_ids: &[i64],
    max_global_jobs: usize,
    daemon_checkin: Option<&AgentDaemonCheckinSource>,
) -> Result<AgentSchedulerStart> {
    if max_global_jobs == 0 {
        anyhow::bail!("{AGENT_MAX_GLOBAL_JOBS_ENV} must be greater than zero");
    }

    if let Some(checkin) = daemon_checkin {
        with_agent_store_at(state_dir, |store| {
            record_agent_daemon_checkin(store, checkin)
        })?;
    }
    let now = agent_timestamp_seconds();
    let durable_workers = reconcile_independent_agent_workers(state_dir)?;
    let mut busy_project_ids = durable_workers
        .iter()
        .map(|worker| worker.project_id)
        .collect::<HashSet<_>>();
    busy_project_ids.extend(active_project_ids.iter().copied());
    let leased_project_ids = with_agent_store_at(state_dir, |store| {
        Ok(store
            .list_active_leases_blocking(&agent_timestamp())?
            .into_iter()
            .filter(|lease| !agent_lease_is_reclaimable(lease, reclaim_current_process_leases, now))
            .map(|lease| lease.project_id)
            .collect::<Vec<_>>())
    })?;
    busy_project_ids.extend(leased_project_ids);
    let completed_to_acknowledge = with_agent_store_at(state_dir, |store| {
        store.list_unacknowledged_completed_git_finalizations_blocking(None)
    })?;
    for finalization in completed_to_acknowledge
        .iter()
        .filter(|finalization| !busy_project_ids.contains(&finalization.project_id))
    {
        with_agent_store_at(state_dir, |store| {
            store
                .acknowledge_completed_git_finalization_session_blocking(
                    finalization.project_id,
                    &finalization.codex_session_id,
                )
                .map(|_| ())
        })?;
    }
    let abandoned_project_ids = with_agent_store_at(state_dir, |store| {
        Ok(store
            .list_terminal_workers_blocking()?
            .into_iter()
            .filter(|worker| worker.state == "abandoned")
            .map(|worker| worker.project_id)
            .collect::<Vec<_>>())
    })?;
    let mut durable_project_ids = durable_workers
        .iter()
        .map(|worker| worker.project_id)
        .collect::<Vec<_>>();
    durable_project_ids.extend_from_slice(active_project_ids);
    durable_project_ids.sort_unstable();
    durable_project_ids.dedup();

    let holder = daemon_checkin
        .map(|checkin| checkin.holder.clone())
        .unwrap_or_else(agent_lease_holder);
    let lease_timeout = agent_lease_timeout()?;
    let success_cooldown = agent_success_cooldown()?;
    let failure_backoff = agent_failure_backoff()?;
    let mut pass = AgentSchedulerPass {
        scanned_projects: 0,
        pending_projects: 0,
        active_agent_jobs: durable_project_ids.len(),
        skipped_active_lease: 0,
        deferred_projects: 0,
        runs_started: 0,
        runs_recorded: 0,
    };
    let mut jobs = Vec::new();

    let projects = with_agent_store_at(state_dir, |store| store.list_projects_blocking())?;

    for mut project in projects {
        if durable_project_ids.contains(&project.id) {
            if project.enabled {
                pass.scanned_projects += 1;
            }
            continue;
        }
        if !project.enabled {
            let existing_lease = agent_lease_for_project(state_dir, project.id)?;
            reconcile_stale_agent_session_controls(
                state_dir,
                project.id,
                existing_lease.as_ref(),
                reclaim_current_process_leases,
                now,
            )?;
            if let Some(lease) = existing_lease.as_ref() {
                try_reclaim_inactive_agent_lease(
                    state_dir,
                    &project,
                    None,
                    lease,
                    reclaim_current_process_leases,
                )?;
            }
            continue;
        }

        pass.scanned_projects += 1;

        let scan = scan_agent_project(&project.path);
        with_agent_store_at(state_dir, |store| {
            store.record_project_daemon_scan_blocking(
                project.id,
                scan.status_label(),
                scan.error_message(),
            )
        })?;

        let mut existing_lease = agent_lease_for_project(state_dir, project.id)?;
        reconcile_stale_agent_session_controls(
            state_dir,
            project.id,
            existing_lease.as_ref(),
            reclaim_current_process_leases,
            now,
        )?;
        existing_lease = agent_lease_for_project(state_dir, project.id)?;
        let finalizations_before_reconcile = with_agent_store_at(state_dir, |store| {
            store.list_pending_git_finalizations_blocking(Some(project.id))
        })?;
        if !finalizations_before_reconcile.is_empty() {
            for finalization in finalizations_before_reconcile
                .iter()
                .filter(|finalization| {
                    finalization.state.is_finalizing()
                        && finalization.state != GitFinalizationState::PushPending
                })
            {
                with_agent_store_at(state_dir, |store| {
                    store
                        .ensure_pending_git_finalization_resume_requested_blocking(
                            project.id,
                            &finalization.codex_session_id,
                        )
                        .map(|_| ())
                })?;
            }
            for finalization in finalizations_before_reconcile
                .iter()
                .filter(|finalization| finalization.state == GitFinalizationState::PushPending)
            {
                with_agent_store_at(state_dir, |store| {
                    store
                        .clear_autonomous_push_resume_request_blocking(
                            project.id,
                            &finalization.codex_session_id,
                        )
                        .map(|_| ())
                })?;
            }
        }
        let finalizations = if finalizations_before_reconcile.is_empty() {
            Vec::new()
        } else {
            let Some(finalization_lease) = try_acquire_agent_git_finalization_lease(
                state_dir,
                &project,
                reclaim_current_process_leases,
            )?
            else {
                pass.skipped_active_lease += 1;
                print_active_lease_skip(
                    &project,
                    &scan,
                    agent_lease_for_project(state_dir, project.id)?.as_ref(),
                );
                continue;
            };

            let reconciliation_result = (|| -> Result<Vec<agent::GitFinalizationRecord>> {
                let current = with_agent_store_at(state_dir, |store| {
                    store.list_pending_git_finalizations_blocking(Some(project.id))
                })?;
                if agent_git_push_retry_backoff_remaining(&current, now, failure_backoff).is_some()
                {
                    return Ok(current);
                }
                let reconciled = match reconcile_pending_agent_git_finalizations(
                    state_dir,
                    &project,
                    Some(&finalization_lease),
                ) {
                    Ok(finalizations)
                        if finalizations.iter().any(|finalization| {
                            finalization.state == GitFinalizationState::PushPending
                        }) =>
                    {
                        record_agent_git_push_retry_error_message(
                            state_dir,
                            project.id,
                            "CLT's bounded publication attempt made no remotely provable progress; the exact frozen publication remains pending",
                            &finalization_lease,
                        )?;
                        with_agent_store_at(state_dir, |store| {
                            store.list_pending_git_finalizations_blocking(Some(project.id))
                        })?
                    }
                    Ok(finalizations) => finalizations,
                    Err(error) => {
                        if let Err(fence_error) = finalization_lease.ensure_owned() {
                            return Err(fence_error.context(format!(
                                "Git reconciliation stopped after losing its ownership fence: {error:#}"
                            )));
                        }
                        record_agent_git_push_retry_error(
                            state_dir,
                            project.id,
                            &error,
                            &finalization_lease,
                        )?;
                        eprintln!(
                            "Project {}: action=git_finalization_wait reason=proof_error error={error:#} path={}",
                            project.name,
                            project.path.display()
                        );
                        with_agent_store_at(state_dir, |store| {
                            store.list_pending_git_finalizations_blocking(Some(project.id))
                        })?
                    }
                };
                for finalization in reconciled
                    .iter()
                    .filter(|finalization| finalization.state == GitFinalizationState::Working)
                {
                    finalization_lease.ensure_owned()?;
                    with_agent_store_at(state_dir, |store| {
                        repair_working_git_task_link(store, &project.path, finalization)
                    })?;
                    finalization_lease.ensure_owned()?;
                }
                Ok(reconciled)
            })();
            let release_result = finalization_lease.release();
            let reconciled = reconciliation_result?;
            release_result?;
            existing_lease = agent_lease_for_project(state_dir, project.id)?;
            reconciled
        };
        let completed_finalizations = finalizations
            .iter()
            .filter(|finalization| finalization.state == GitFinalizationState::Completed)
            .collect::<Vec<_>>();
        let rolled_forward_git_finalization = !completed_finalizations.is_empty();
        if !completed_finalizations.is_empty() {
            with_agent_store_at(state_dir, |store| {
                for finalization in &completed_finalizations {
                    store.acknowledge_completed_git_finalization_session_blocking(
                        project.id,
                        &finalization.codex_session_id,
                    )?;
                }
                Ok(())
            })?;
            project = with_agent_store_at(state_dir, |store| {
                store
                    .list_projects_blocking()?
                    .into_iter()
                    .find(|candidate| candidate.id == project.id)
                    .context("Acknowledged Git finalization project disappeared")
            })?;
        }
        if let Some(finalization) = finalizations
            .iter()
            .find(|finalization| finalization.state == GitFinalizationState::PushPending)
        {
            with_agent_store_at(state_dir, |store| {
                store
                    .clear_autonomous_push_resume_request_blocking(
                        project.id,
                        &finalization.codex_session_id,
                    )
                    .map(|_| ())
            })?;
            if let Some(remaining) =
                agent_git_push_retry_backoff_remaining(&finalizations, now, failure_backoff)
            {
                println!(
                    "Project {}: action=skip reason=autonomous_git_push_backoff remaining_seconds={} session={} path={}",
                    project.name,
                    remaining,
                    finalization.codex_session_id,
                    project.path.display()
                );
            } else {
                println!(
                    "Project {}: action=skip reason=autonomous_git_push_pending session={} path={}",
                    project.name,
                    finalization.codex_session_id,
                    project.path.display()
                );
            }
            continue;
        }
        let pending_git_finalization = finalizations
            .iter()
            .find(|finalization| finalization.state.is_finalizing());
        let blocked_recovery_backoff_active = scan.has_blocked_task()
            && blocked_recovery_backoff_reason(&project, now, failure_backoff).is_some();
        let mut working_git_finalization = None;
        for finalization in finalizations
            .iter()
            .filter(|finalization| finalization.state == GitFinalizationState::Working)
        {
            let Some((status, task)) = terminal_task_for_codex_session_in_board(
                &get_tasks_dir(&project.path),
                &finalization.codex_session_id,
            )?
            else {
                continue;
            };
            let eligible = status.is_active()
                && (!task_entry_is_blocked(&task) || !blocked_recovery_backoff_active);
            if eligible {
                working_git_finalization = Some(finalization);
                break;
            }
        }
        let git_session_to_resume = pending_git_finalization.or(working_git_finalization);
        let resume_session_id = if let Some(finalization) = git_session_to_resume {
            let ready = with_agent_store_at(state_dir, |store| {
                store.ensure_pending_git_finalization_resume_requested_blocking(
                    project.id,
                    &finalization.codex_session_id,
                )
            })?;
            if !ready {
                println!(
                    "Project {}: action=skip reason=git_finalization_session_suspended state={} session={} path={}",
                    project.name,
                    finalization.state.status_label(),
                    finalization.codex_session_id,
                    project.path.display()
                );
                continue;
            }
            Some(finalization.codex_session_id.clone())
        } else {
            resumable_codex_session_for_project(state_dir, &project, now)?
        };
        let controls_after_initial_check = if resume_session_id.is_none() {
            with_agent_store_at(state_dir, |store| {
                store.session_controls_for_project_blocking(project.id)
            })?
        } else {
            Vec::new()
        };
        let has_suspended_session = resume_session_id.is_none()
            && session_controls_suspend_project(&controls_after_initial_check);
        if has_suspended_session {
            println!(
                "Project {}: action=skip reason=session_suspended todo={} doing={} scan_status={} path={}",
                project.name,
                scan.todo_count,
                scan.doing_count,
                scan.status_label(),
                project.path.display()
            );
            continue;
        }
        let resume_abandoned_worker =
            scan.doing_count > 0 && abandoned_project_ids.contains(&project.id);
        let resume_interrupted_task = scan.doing_count > 0
            && (resume_abandoned_worker
                || existing_lease.as_ref().is_some_and(|lease| {
                    agent_lease_is_reclaimable(lease, reclaim_current_process_leases, now)
                }));
        let task_selection = if resume_session_id.is_some() {
            Some(AgentTaskSelection::ResumeSession)
        } else if resume_interrupted_task {
            Some(AgentTaskSelection::ResumeDoing)
        } else if scan.has_blocked_task() && !blocked_recovery_backoff_active {
            Some(AgentTaskSelection::RecoverBlocked)
        } else if scan.has_pending_task() {
            Some(AgentTaskSelection::NextTodo)
        } else if scan.has_blocked_task() {
            Some(AgentTaskSelection::RecoverBlocked)
        } else {
            None
        };

        let Some(task_selection) = task_selection else {
            println!(
                "Project {}: action=idle reason=no_pending_tasks todo={} ready_todo={} blocked_todo={} doing={} blocked_doing={} scan_status={} path={}",
                project.name,
                scan.todo_count,
                scan.available_todo_count(),
                scan.blocked_todo_count,
                scan.doing_count,
                scan.blocked_doing_count,
                scan.status_label(),
                project.path.display()
            );
            continue;
        };

        if let Some(reason) = (!rolled_forward_git_finalization)
            .then(|| {
                agent_task_cooldown_reason(
                    &project,
                    task_selection,
                    now,
                    success_cooldown,
                    failure_backoff,
                )
            })
            .flatten()
        {
            println!(
                "Project {}: action=skip reason=\"{}\" work={} todo={} ready_todo={} blocked_todo={} doing={} blocked_doing={} scan_status={} path={}",
                project.name,
                reason,
                task_selection.label(),
                scan.todo_count,
                scan.available_todo_count(),
                scan.blocked_todo_count,
                scan.doing_count,
                scan.blocked_doing_count,
                scan.status_label(),
                project.path.display()
            );
            continue;
        }

        pass.pending_projects += 1;
        if pass.active_agent_jobs + jobs.len() >= max_global_jobs {
            pass.deferred_projects += 1;
            println!(
                "Project {}: action=defer reason=max_global_jobs_reached max_global_jobs={} active_agent_jobs={} work={} todo={} ready_todo={} blocked_todo={} doing={} blocked_doing={} scan_status={} path={}",
                project.name,
                max_global_jobs,
                pass.active_agent_jobs,
                task_selection.label(),
                scan.todo_count,
                scan.available_todo_count(),
                scan.blocked_todo_count,
                scan.doing_count,
                scan.blocked_doing_count,
                scan.status_label(),
                project.path.display()
            );
            continue;
        }

        let mut acquired_at = agent_timestamp();
        let mut expires_at = agent_timestamp_after(lease_timeout.as_secs());
        let mut acquired = with_agent_store_at(state_dir, |store| {
            store.try_acquire_lease_blocking(project.id, &holder, &acquired_at, &expires_at)
        })?;
        if !acquired {
            let lease = agent_lease_for_project(state_dir, project.id)?;
            if let Some(lease) = lease.as_ref()
                && try_reclaim_inactive_agent_lease(
                    state_dir,
                    &project,
                    Some(&scan),
                    lease,
                    reclaim_current_process_leases,
                )?
            {
                let reacquired_at = agent_timestamp();
                let reexpires_at = agent_timestamp_after(lease_timeout.as_secs());
                acquired_at = reacquired_at;
                expires_at = reexpires_at;
                acquired = with_agent_store_at(state_dir, |store| {
                    store.try_acquire_lease_blocking(project.id, &holder, &acquired_at, &expires_at)
                })?;
            }

            if !acquired {
                pass.skipped_active_lease += 1;
                let lease = agent_lease_for_project(state_dir, project.id)?;
                print_active_lease_skip(&project, &scan, lease.as_ref());
                continue;
            }
        }

        if resume_session_id.is_none() {
            let controls = with_agent_store_at(state_dir, |store| {
                store.session_controls_for_project_blocking(project.id)
            })?;
            if session_controls_suspend_project(&controls) {
                with_agent_store_at(state_dir, |store| {
                    store
                        .release_lease_blocking(project.id, &holder)
                        .map(|_| ())
                })?;
                println!(
                    "Project {}: action=skip reason=session_state_changed_before_start path={}",
                    project.name,
                    project.path.display()
                );
                continue;
            }
        }
        println!(
            "Project {}: action=running work={} todo={} ready_todo={} blocked_todo={} doing={} blocked_doing={} scan_status={} lease_holder={} lease_acquired_at={} lease_expires_at={} path={}",
            project.name,
            task_selection.label(),
            scan.todo_count,
            scan.available_todo_count(),
            scan.blocked_todo_count,
            scan.doing_count,
            scan.blocked_doing_count,
            scan.status_label(),
            holder,
            format_agent_timestamp(&acquired_at),
            format_agent_timestamp(&expires_at),
            project.path.display()
        );

        let done_task_contents_before = completed_task_contents(&project.path).unwrap_or_default();
        let blocked_task_snapshots_before =
            blocked_task_snapshots(&project.path).unwrap_or_default();
        pass.runs_started += 1;
        jobs.push(AgentRunJob {
            state_dir: state_dir.to_path_buf(),
            project,
            holder: holder.clone(),
            worker_token: None,
            max_global_jobs,
            task_selection,
            resume_session_id,
            blocked_task_count_before: scan.blocked_task_count(),
            done_task_contents_before,
            blocked_task_snapshots_before,
        });
    }

    Ok(AgentSchedulerStart { pass, jobs })
}

pub(super) fn agent_lease_for_project(
    state_dir: &Path,
    project_id: i64,
) -> Result<Option<agent::AgentLeaseRecord>> {
    with_agent_store_at(state_dir, |store| {
        store.lease_for_project_blocking(project_id)
    })
}

#[cfg(unix)]
pub(super) fn interactive_guardian_child_is_proven_absent(child_pid: Option<u32>) -> bool {
    child_pid
        .is_none_or(|child_pid| automated_agent_process_group_is_running(child_pid) == Some(false))
}

#[cfg(not(unix))]
pub(super) fn interactive_guardian_child_is_proven_absent(child_pid: Option<u32>) -> bool {
    // The non-Unix gate must remain as a parent while Codex runs because this
    // platform has no exec replacement. Its disappearance cannot prove that a
    // spawned target also exited, so only the pre-release NULL phase is safe to
    // recover automatically.
    child_pid.is_none()
}

pub(super) fn reconcile_stale_agent_session_controls(
    state_dir: &Path,
    project_id: i64,
    lease: Option<&agent::AgentLeaseRecord>,
    reclaim_current_process_leases: bool,
    now: u64,
) -> Result<()> {
    let controls = with_agent_store_at(state_dir, |store| {
        store.session_controls_for_project_blocking(project_id)
    })?;
    let lease_is_active = lease.is_some_and(|lease| {
        !agent_lease_is_reclaimable(lease, reclaim_current_process_leases, now)
    });

    for control in controls {
        if control.state == AgentSessionControlState::ReadyInteractive
            && let Some(holder) = control.interactive_holder.as_deref()
            && (holder.starts_with("clt-idle-interactive-")
                || holder.starts_with("clt-stopped-interactive-"))
        {
            let matching_active_lease = lease.is_some_and(|lease| {
                lease.holder == holder
                    && !agent_lease_is_reclaimable(lease, reclaim_current_process_leases, now)
                    && !interactive_lease_holder_is_proven_dead(holder)
            });
            let recently_updated = control.updated_at.parse::<u64>().is_ok_and(|updated_at| {
                now.saturating_sub(updated_at)
                    <= TUI_SESSION_HANDOFF_TIMEOUT_SECONDS.saturating_add(5)
            });
            if !matching_active_lease
                && (interactive_lease_holder_is_proven_dead(holder) || !recently_updated)
            {
                with_agent_store_at(state_dir, |store| {
                    store
                        .cancel_idle_session_interactive_blocking(
                            project_id,
                            &control.codex_session_id,
                            holder,
                        )
                        .map(|_| ())
                })?;
                continue;
            }
        }
        if matches!(
            control.state,
            AgentSessionControlState::Interactive | AgentSessionControlState::StopRequested
        ) && let Some(holder) = control.interactive_holder.as_deref()
            && let Some(disposition) = InteractiveGuardianDisposition::from_guardian_holder(holder)
            && control.interactive_launch_token.as_deref() == Some(holder)
        {
            let matching_active_lease = lease.is_some_and(|lease| {
                lease.holder == holder
                    && !agent_lease_is_reclaimable(lease, reclaim_current_process_leases, now)
                    && !interactive_lease_holder_is_proven_dead(holder)
            });
            let requester_alive = matches!(
                agent_lease_holder_liveness(holder),
                AgentLeaseHolderLiveness::CurrentProcess
                    | AgentLeaseHolderLiveness::Alive
                    | AgentLeaseHolderLiveness::Unknown
            ) && !interactive_lease_holder_is_proven_dead(holder);
            let recently_updated = control.updated_at.parse::<u64>().is_ok_and(|updated_at| {
                now.saturating_sub(updated_at)
                    <= TUI_SESSION_HANDOFF_TIMEOUT_SECONDS.saturating_add(5)
            });
            let guardian_is_proven_dead =
                InteractiveGuardianDisposition::guardian_process_is_proven_dead(holder);
            let handoff_is_abandoned = guardian_is_proven_dead
                || (!matching_active_lease && (!requester_alive || !recently_updated));
            if handoff_is_abandoned {
                // The interactive exec gate makes a NULL PID proof that Codex was
                // never released. Once a PID is registered, recovery remains
                // fail-closed until that exact PGID is observed absent; it never
                // signals a numeric PID that could have been reused.
                let child_is_proven_absent =
                    interactive_guardian_child_is_proven_absent(control.child_pid);
                if child_is_proven_absent {
                    with_agent_store_at(state_dir, |store| {
                        store
                            .recover_stale_interactive_guardian_blocking(
                                project_id,
                                &control.codex_session_id,
                                holder,
                                control.child_pid,
                                disposition,
                            )
                            .map(|_| ())
                    })?;
                }
            }
            // A recognized interactive guardian owns the only race-free Child
            // handle. Even after `s` records stop_requested, automated stale-run
            // recovery must not reinterpret this row or signal its PID.
            continue;
        }
        let recorded_child_is_gone = control.child_pid.is_some_and(|child_pid| {
            automated_agent_process_group_is_running(child_pid) == Some(false)
        });
        let recovery_state = match control.state {
            AgentSessionControlState::Running if !lease_is_active && recorded_child_is_gone => {
                Some(AgentSessionControlState::ResumeRequested)
            }
            AgentSessionControlState::StopRequested
                if !lease_is_active && recorded_child_is_gone =>
            {
                Some(AgentSessionControlState::Stopped)
            }
            AgentSessionControlState::InterruptRequested
                if !lease_is_active && recorded_child_is_gone =>
            {
                Some(AgentSessionControlState::ReadyInteractive)
            }
            AgentSessionControlState::ReadyInteractive | AgentSessionControlState::Interactive => {
                let expected_holder = control.interactive_holder.as_deref();
                let matching_active_lease = expected_holder.is_some_and(|holder| {
                    lease.is_some_and(|lease| {
                        lease.holder == holder
                            && !agent_lease_is_reclaimable(
                                lease,
                                reclaim_current_process_leases,
                                now,
                            )
                            && !interactive_lease_holder_is_proven_dead(holder)
                    })
                });
                let requester_alive = expected_holder.is_some_and(|holder| {
                    matches!(
                        agent_lease_holder_liveness(holder),
                        AgentLeaseHolderLiveness::CurrentProcess
                            | AgentLeaseHolderLiveness::Alive
                            | AgentLeaseHolderLiveness::Unknown
                    ) && !interactive_lease_holder_is_proven_dead(holder)
                });
                let recently_updated = control.updated_at.parse::<u64>().is_ok_and(|updated_at| {
                    now.saturating_sub(updated_at)
                        <= TUI_SESSION_HANDOFF_TIMEOUT_SECONDS.saturating_add(5)
                });
                let handoff_is_abandoned =
                    !matching_active_lease && (!requester_alive || !recently_updated);
                if !handoff_is_abandoned {
                    None
                } else if control.state == AgentSessionControlState::Interactive
                    && expected_holder.is_some_and(|holder| {
                        holder.starts_with("clt-interactive-worker-")
                            || holder.starts_with("clt-idle-interactive-worker-")
                            || holder.starts_with("clt-stopped-interactive-worker-")
                    })
                {
                    // A guardian owns the only race-free Child handle. If it disappears
                    // before finalizing, an expired lease cannot prove that Codex also
                    // exited; leave the session fenced instead of risking a duplicate
                    // `codex exec resume` against an orphaned interactive process.
                    None
                } else {
                    let previous_interactive_process_is_gone = control
                        .child_pid
                        .is_none_or(|child_pid| local_process_is_running(child_pid) == Some(false));
                    previous_interactive_process_is_gone
                        .then_some(AgentSessionControlState::ResumeRequested)
                }
            }
            _ => None,
        };

        if let Some(recovery_state) = recovery_state {
            with_agent_store_at(state_dir, |store| {
                if control.state == AgentSessionControlState::InterruptRequested {
                    let Some(run_token) = control.run_token.as_deref() else {
                        // Legacy/unregistered requested rows have no generation
                        // proof. Keep them fenced instead of manufacturing an
                        // interactive lease for an ambiguous session.
                        return Ok(());
                    };
                    store
                        .finalize_reaped_automated_session_blocking(
                            project_id,
                            control.child_pid.expect("stale recovery checked child PID"),
                            run_token,
                            lease.map_or("", |lease| lease.holder.as_str()),
                            agent_lease_timeout()?.as_secs().max(60),
                        )
                        .map(|_| ())
                } else if matches!(
                    control.state,
                    AgentSessionControlState::Running | AgentSessionControlState::StopRequested
                ) {
                    store
                        .recover_stale_automated_session_control_blocking(
                            project_id,
                            &control.codex_session_id,
                            control.state,
                            recovery_state,
                            control.child_pid.expect("stale recovery checked child PID"),
                            control.run_token.as_deref(),
                        )
                        .map(|_| ())
                } else if matches!(
                    control.state,
                    AgentSessionControlState::ReadyInteractive
                        | AgentSessionControlState::Interactive
                ) {
                    store
                        .recover_stale_interactive_session_control_blocking(
                            project_id,
                            &control.codex_session_id,
                            control.state,
                            recovery_state,
                            control.interactive_holder.as_deref(),
                        )
                        .map(|_| ())
                } else {
                    store
                        .transition_session_control_state_blocking(
                            project_id,
                            &control.codex_session_id,
                            control.state,
                            recovery_state,
                        )
                        .map(|_| ())
                }
            })?;
        }
    }

    Ok(())
}

pub(super) fn task_status_for_codex_session(
    project_root: &Path,
    session_id: &str,
) -> Result<Option<TaskStatus>> {
    task_status_for_codex_session_in_board(&get_tasks_dir(project_root), session_id)
}

pub(super) fn resumable_codex_session_for_project(
    state_dir: &Path,
    project: &agent::AgentProject,
    now: u64,
) -> Result<Option<String>> {
    loop {
        let Some(session_id) = with_agent_store_at(state_dir, |store| {
            store.resume_requested_session_blocking(project.id)
        })?
        else {
            return Ok(None);
        };
        let git_finalization = with_agent_store_at(state_dir, |store| {
            store.git_finalization_blocking(project.id, &session_id)
        })?;
        if git_finalization
            .as_ref()
            .is_some_and(|finalization| finalization.state == GitFinalizationState::Completed)
        {
            let proof_recovery_request = with_agent_store_at(state_dir, |store| {
                Ok(store
                    .session_control_blocking(project.id, &session_id)?
                    .and_then(|control| control.run_token)
                    .is_some_and(|run_token| {
                        run_token.starts_with(AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX)
                    }))
            })?;
            if proof_recovery_request {
                with_agent_store_at(state_dir, |store| {
                    store.acknowledge_completed_git_finalization_session_blocking(
                        project.id,
                        &session_id,
                    )
                })?;
                continue;
            }
            return Ok(Some(session_id));
        }
        if git_finalization
            .as_ref()
            .is_some_and(|finalization| !finalization.state.is_terminal())
        {
            if let Some(finalization) = git_finalization.as_ref()
                && finalization.state == GitFinalizationState::Working
                && task_status_for_codex_session(&project.path, &session_id)?.is_none()
            {
                with_agent_store_at(state_dir, |store| {
                    repair_working_git_task_link(store, &project.path, finalization)
                })?;
            }
            return Ok(Some(session_id));
        }
        if task_status_for_codex_session(&project.path, &session_id)?.is_some() {
            return Ok(Some(session_id));
        }

        let cleared = with_agent_store_at(state_dir, |store| {
            store.clear_orphaned_resume_requested_session_blocking(project.id, &session_id, now)
        })?;
        if !cleared {
            // Another scheduler or worker may have claimed the project after this
            // pass took its initial snapshot. Keep the session fenced and let the
            // normal lease checks settle that race instead of deleting live state.
            return Ok(Some(session_id));
        }
        eprintln!(
            "Project {}: action=orphaned_session_cleared session={} reason=task_marker_missing path={}",
            project.name,
            session_id,
            project.path.display()
        );
    }
}

pub(super) fn session_controls_suspend_project(
    controls: &[agent::AgentSessionControlRecord],
) -> bool {
    // A stopped session keeps its task-to-session link for an explicit resume,
    // but it no longer owns a process or project lease. Other queued work can
    // therefore run without disturbing the stopped task.
    controls
        .iter()
        .any(|control| control.state != AgentSessionControlState::Stopped)
}

pub(super) fn agent_lease_is_reclaimable(
    lease: &agent::AgentLeaseRecord,
    reclaim_current_process_leases: bool,
    now: u64,
) -> bool {
    if lease
        .expires_at
        .parse::<u64>()
        .is_ok_and(|expires_at| expires_at <= now)
    {
        return true;
    }

    match agent_lease_holder_liveness(&lease.holder) {
        AgentLeaseHolderLiveness::Dead => true,
        AgentLeaseHolderLiveness::CurrentProcess => reclaim_current_process_leases,
        AgentLeaseHolderLiveness::Alive | AgentLeaseHolderLiveness::Unknown => false,
    }
}

pub(super) fn try_reclaim_inactive_agent_lease(
    state_dir: &Path,
    project: &agent::AgentProject,
    scan: Option<&AgentProjectScan>,
    lease: &agent::AgentLeaseRecord,
    reclaim_current_process_leases: bool,
) -> Result<bool> {
    let ordinary_liveness = agent_lease_holder_liveness(&lease.holder);
    let interactive_liveness = interactive_lease_holder_liveness(&lease.holder);
    let liveness = interactive_liveness.unwrap_or(ordinary_liveness);
    let ordinarily_reclaimable = agent_lease_is_reclaimable(
        lease,
        reclaim_current_process_leases,
        agent_timestamp_seconds(),
    );
    let orphaned_interactive_lease = if !ordinarily_reclaimable
        && interactive_liveness == Some(AgentLeaseHolderLiveness::Dead)
    {
        with_agent_store_at(state_dir, |store| {
            Ok(!store
                .session_controls_for_project_blocking(project.id)?
                .into_iter()
                .any(|control| control.interactive_holder.as_deref() == Some(&lease.holder)))
        })?
    } else {
        false
    };
    if !ordinarily_reclaimable && !orphaned_interactive_lease {
        return Ok(false);
    }

    let lease_is_worker_fenced = with_agent_store_at(state_dir, |store| {
        Ok(store
            .list_active_workers_blocking()?
            .into_iter()
            .any(|worker| worker.project_id == project.id && worker.lease_holder == lease.holder))
    })?;
    if lease_is_worker_fenced {
        return Ok(false);
    }

    let released = with_agent_store_at(state_dir, |store| {
        store.release_lease_blocking(project.id, &lease.holder)
    })?;
    if released {
        if let Some(scan) = scan {
            println!(
                "Project {}: action=reclaim reason=inactive_lease todo={} scan_status={} lease_holder={} lease_process={} lease_acquired_at={} lease_expires_at={} path={}",
                project.name,
                scan.todo_count,
                scan.status_label(),
                lease.holder,
                liveness.label(),
                format_agent_timestamp(&lease.acquired_at),
                format_agent_timestamp(&lease.expires_at),
                project.path.display()
            );
        } else {
            println!(
                "Project {}: action=reclaim reason=inactive_lease project_state=disabled lease_holder={} lease_process={} lease_acquired_at={} lease_expires_at={} path={}",
                project.name,
                lease.holder,
                liveness.label(),
                format_agent_timestamp(&lease.acquired_at),
                format_agent_timestamp(&lease.expires_at),
                project.path.display()
            );
        }
    }

    Ok(released)
}

pub(super) fn print_active_lease_skip(
    project: &agent::AgentProject,
    scan: &AgentProjectScan,
    lease: Option<&agent::AgentLeaseRecord>,
) {
    if let Some(lease) = lease {
        let liveness = agent_lease_holder_liveness(&lease.holder);
        println!(
            "Project {}: action=skip reason=active_lease todo={} scan_status={} lease_holder={} lease_process={} lease_acquired_at={} lease_expires_at={} path={}",
            project.name,
            scan.todo_count,
            scan.status_label(),
            lease.holder,
            liveness.label(),
            format_agent_timestamp(&lease.acquired_at),
            format_agent_timestamp(&lease.expires_at),
            project.path.display()
        );
    } else {
        println!(
            "Project {}: action=skip reason=active_lease todo={} scan_status={} path={}",
            project.name,
            scan.todo_count,
            scan.status_label(),
            project.path.display()
        );
    }
}

impl AgentLeaseHolderLiveness {
    pub(super) fn label(self) -> &'static str {
        match self {
            AgentLeaseHolderLiveness::CurrentProcess => "current_process",
            AgentLeaseHolderLiveness::Alive => "alive",
            AgentLeaseHolderLiveness::Dead => "dead",
            AgentLeaseHolderLiveness::Unknown => "unknown",
        }
    }
}

pub(super) fn agent_lease_holder_liveness(holder: &str) -> AgentLeaseHolderLiveness {
    let Some(pid) = agent_lease_holder_pid(holder) else {
        return AgentLeaseHolderLiveness::Unknown;
    };

    agent_pid_liveness(pid)
}

pub(super) fn interactive_lease_holder_liveness(holder: &str) -> Option<AgentLeaseHolderLiveness> {
    interactive_lease_holder_pid(holder).map(agent_pid_liveness)
}

pub(super) fn interactive_lease_holder_is_proven_dead(holder: &str) -> bool {
    interactive_lease_holder_liveness(holder) == Some(AgentLeaseHolderLiveness::Dead)
}

pub(super) fn interactive_lease_holder_pid(holder: &str) -> Option<u32> {
    if let Some(pid) = InteractiveGuardianDisposition::guardian_process_id(holder) {
        return Some(pid);
    }

    [
        "clt-stopped-shared-interactive-",
        "clt-shared-interactive-",
        "clt-stopped-readonly-interactive-",
        "clt-readonly-interactive-",
        "clt-stopped-interactive-",
        "clt-idle-interactive-",
        "clt-interactive-",
    ]
    .into_iter()
    .find_map(|prefix| {
        holder
            .strip_prefix(prefix)
            .and_then(|suffix| suffix.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok())
    })
}

pub(super) fn agent_pid_liveness(pid: u32) -> AgentLeaseHolderLiveness {
    if pid == std::process::id() {
        return AgentLeaseHolderLiveness::CurrentProcess;
    }

    match local_process_is_running(pid) {
        Some(true) => AgentLeaseHolderLiveness::Alive,
        Some(false) => AgentLeaseHolderLiveness::Dead,
        None => AgentLeaseHolderLiveness::Unknown,
    }
}

pub(super) fn agent_lease_holder_pid(holder: &str) -> Option<u32> {
    holder
        .strip_prefix("clt-agent-")
        .or_else(|| holder.strip_prefix("clt-scheduler-"))
        .or_else(|| holder.strip_prefix("clt-interactive-"))?
        .parse()
        .ok()
}

pub(super) fn scan_agent_project(project_root: &Path) -> AgentProjectScan {
    if !project_root.exists() {
        return AgentProjectScan::missing();
    }

    match ensure_existing_board(project_root) {
        Ok(true) => {}
        Ok(false) => return AgentProjectScan::uninitialized(),
        Err(err) => return AgentProjectScan::unavailable(err),
    }

    let board_dir = get_tasks_dir(project_root);
    let todo_entries = match read_task_entries(&board_dir, TaskStatus::Todo) {
        Ok(entries) => entries,
        Err(err) => return AgentProjectScan::unavailable(err),
    };
    let doing_entries = match read_task_entries(&board_dir, TaskStatus::Doing) {
        Ok(entries) => entries,
        Err(err) => return AgentProjectScan::unavailable(err),
    };
    let todo_count = todo_entries.len();
    let blocked_todo_count = todo_entries
        .iter()
        .filter(|entry| task_entry_is_blocked(entry))
        .count();
    let doing_count = doing_entries.len();
    let blocked_doing_count = doing_entries
        .iter()
        .filter(|entry| task_entry_is_blocked(entry))
        .count();

    AgentProjectScan::from_counts(
        todo_count,
        blocked_todo_count,
        doing_count,
        blocked_doing_count,
    )
}

#[cfg(test)]
pub(super) fn has_pending_agent_task(project_root: &Path) -> bool {
    scan_agent_project(project_root).has_pending_task()
}

impl AgentProjectScan {
    #[cfg(test)]
    pub(super) fn pending(todo_count: usize) -> Self {
        Self::from_counts(todo_count, 0, 0, 0)
    }

    #[cfg(test)]
    pub(super) fn pending_with_doing(todo_count: usize, doing_count: usize) -> Self {
        Self::from_counts(todo_count, 0, doing_count, 0)
    }

    pub(super) fn from_counts(
        todo_count: usize,
        blocked_todo_count: usize,
        doing_count: usize,
        blocked_doing_count: usize,
    ) -> Self {
        let available_todo_count = todo_count.saturating_sub(blocked_todo_count);
        let task_count = todo_count.saturating_add(doing_count);
        let blocked_task_count = blocked_todo_count.saturating_add(blocked_doing_count);
        let status = if available_todo_count > 0 {
            AgentProjectScanStatus::Pending
        } else if task_count > 0 && blocked_task_count == task_count {
            AgentProjectScanStatus::Blocked
        } else {
            AgentProjectScanStatus::Empty
        };

        Self {
            status,
            todo_count,
            blocked_todo_count,
            doing_count,
            blocked_doing_count,
        }
    }

    #[cfg(test)]
    pub(super) fn empty() -> Self {
        Self::from_counts(0, 0, 0, 0)
    }

    pub(super) fn missing() -> Self {
        Self {
            status: AgentProjectScanStatus::Missing,
            todo_count: 0,
            blocked_todo_count: 0,
            doing_count: 0,
            blocked_doing_count: 0,
        }
    }

    pub(super) fn uninitialized() -> Self {
        Self {
            status: AgentProjectScanStatus::Uninitialized,
            todo_count: 0,
            blocked_todo_count: 0,
            doing_count: 0,
            blocked_doing_count: 0,
        }
    }

    pub(super) fn unavailable(err: anyhow::Error) -> Self {
        Self {
            status: AgentProjectScanStatus::Unavailable(err.to_string()),
            todo_count: 0,
            blocked_todo_count: 0,
            doing_count: 0,
            blocked_doing_count: 0,
        }
    }

    pub(super) fn has_pending_task(&self) -> bool {
        self.status == AgentProjectScanStatus::Pending
    }

    #[cfg(test)]
    pub(super) fn all_actionable_tasks_blocked(&self) -> bool {
        self.status == AgentProjectScanStatus::Blocked
    }

    pub(super) fn has_blocked_task(&self) -> bool {
        self.blocked_task_count() > 0
    }

    pub(super) fn available_todo_count(&self) -> usize {
        self.todo_count.saturating_sub(self.blocked_todo_count)
    }

    pub(super) fn blocked_task_count(&self) -> usize {
        self.blocked_todo_count
            .saturating_add(self.blocked_doing_count)
    }

    pub(super) fn has_schedulable_work(&self) -> bool {
        self.has_pending_task() || self.has_blocked_task()
    }

    pub(super) fn pending_signal(&self) -> &'static str {
        if self.has_schedulable_work() {
            "yes"
        } else {
            "no"
        }
    }

    pub(super) fn status_label(&self) -> &str {
        match &self.status {
            AgentProjectScanStatus::Pending => "pending",
            AgentProjectScanStatus::Blocked => "blocked",
            AgentProjectScanStatus::Empty => "empty",
            AgentProjectScanStatus::Missing => "missing",
            AgentProjectScanStatus::Uninitialized => "uninitialized",
            AgentProjectScanStatus::Unavailable(_) => "unavailable",
        }
    }

    pub(super) fn error_message(&self) -> Option<&str> {
        match &self.status {
            AgentProjectScanStatus::Unavailable(error) => Some(error),
            _ => None,
        }
    }
}

pub(super) fn agent_lease_holder() -> String {
    format!("clt-agent-{}", std::process::id())
}

pub(super) fn agent_scheduler_lease_holder() -> String {
    format!("clt-scheduler-{}", std::process::id())
}

impl AgentDaemonCheckinSource {
    #[cfg(test)]
    pub(super) fn current() -> Self {
        Self::current_with_holder(agent_lease_holder())
    }

    pub(super) fn current_with_holder(holder: String) -> Self {
        Self {
            holder,
            mode: agent_daemon_mode(),
            started_at: agent_timestamp(),
        }
    }
}

pub(super) fn agent_daemon_mode() -> String {
    std::env::var(AGENT_DAEMON_MODE_ENV)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "cli".to_string())
}

pub(super) fn record_agent_daemon_checkin(
    store: &agent::TursoAgentStore,
    checkin: &AgentDaemonCheckinSource,
) -> Result<()> {
    let checked_in_at = agent_timestamp();
    let expires_at = agent_timestamp_after(AGENT_DAEMON_CHECKIN_STALE_SECONDS);
    store.record_daemon_checkin_blocking(
        &checkin.holder,
        &checkin.mode,
        &checkin.started_at,
        &checked_in_at,
        &expires_at,
    )
}

pub(super) async fn clear_agent_daemon_checkin_best_effort(state_dir: &Path, holder: &str) {
    let state_dir = state_dir.to_path_buf();
    let holder = holder.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        with_agent_store_at(&state_dir, |store| {
            store.clear_daemon_checkin_blocking(&holder)?;
            Ok(())
        })
    })
    .await;
}

pub(super) async fn release_agent_dispatch_lease_best_effort(
    state_dir: PathBuf,
    project_id: i64,
    holder: String,
) {
    let release = tokio::task::spawn_blocking(move || {
        with_agent_store_at(&state_dir, |store| {
            store
                .release_lease_blocking(project_id, &holder)
                .map(|_| ())
        })
    })
    .await;
    match release {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!(
                "Failed to release scheduler lease after worker dispatch task failure: {error:#}"
            );
        }
        Err(error) => {
            eprintln!("Agent worker dispatch lease recovery task failed: {error}");
        }
    }
}

pub(super) fn agent_max_global_jobs() -> Result<usize> {
    match std::env::var(AGENT_MAX_GLOBAL_JOBS_ENV) {
        Ok(raw) => parse_agent_positive_usize(AGENT_MAX_GLOBAL_JOBS_ENV, &raw),
        Err(std::env::VarError::NotPresent) => Ok(AGENT_DEFAULT_MAX_GLOBAL_JOBS),
        Err(err) => anyhow::bail!("Failed to read {AGENT_MAX_GLOBAL_JOBS_ENV}: {err}"),
    }
}

pub(super) fn agent_heartbeat_tail_enabled() -> Result<bool> {
    match std::env::var(AGENT_HEARTBEAT_TAIL_ENV) {
        Ok(raw) => parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, &raw),
        Err(std::env::VarError::NotPresent) => Ok(false),
        Err(err) => anyhow::bail!("Failed to read {AGENT_HEARTBEAT_TAIL_ENV}: {err}"),
    }
}

pub(super) fn agent_lease_timeout() -> Result<Duration> {
    agent_timeout_from_env(
        AGENT_LEASE_TIMEOUT_SECONDS_ENV,
        AGENT_DEFAULT_LEASE_TIMEOUT_SECONDS,
    )
}

pub(super) fn agent_lease_renew_interval(lease_timeout: Duration) -> Duration {
    let interval_millis = (lease_timeout.as_millis() / 3)
        .max(250)
        .min(u128::from(AGENT_LEASE_RENEW_MAX_INTERVAL_MILLIS));
    Duration::from_millis(interval_millis as u64)
}

pub(super) fn agent_failure_backoff() -> Result<Duration> {
    agent_timeout_from_env(
        AGENT_FAILURE_BACKOFF_SECONDS_ENV,
        AGENT_DEFAULT_FAILURE_BACKOFF_SECONDS,
    )
}

pub(super) fn agent_poll_interval() -> Result<Duration> {
    agent_timeout_from_env(
        AGENT_POLL_INTERVAL_SECONDS_ENV,
        AGENT_DEFAULT_POLL_INTERVAL_SECONDS,
    )
}

pub(super) fn agent_run_timeout() -> Result<Duration> {
    agent_timeout_from_env(
        AGENT_RUN_TIMEOUT_SECONDS_ENV,
        AGENT_DEFAULT_RUN_TIMEOUT_SECONDS,
    )
}

pub(super) fn agent_success_cooldown() -> Result<Duration> {
    agent_timeout_from_env(
        AGENT_SUCCESS_COOLDOWN_SECONDS_ENV,
        AGENT_DEFAULT_SUCCESS_COOLDOWN_SECONDS,
    )
}

pub(super) fn agent_timeout_from_env(env_name: &str, default_seconds: u64) -> Result<Duration> {
    match std::env::var(env_name) {
        Ok(raw) => parse_agent_timeout_duration(env_name, &raw),
        Err(std::env::VarError::NotPresent) => Ok(default_seconds),
        Err(err) => anyhow::bail!("Failed to read {env_name}: {err}"),
    }
    .map(Duration::from_secs)
}

pub(super) fn parse_agent_timeout_duration(env_name: &str, raw: &str) -> Result<u64> {
    let seconds = raw
        .parse::<u64>()
        .with_context(|| format!("{env_name} must be a positive integer number of seconds"))?;
    if seconds == 0 {
        anyhow::bail!("{env_name} must be greater than zero");
    }

    Ok(seconds)
}

pub(super) fn parse_agent_positive_usize(env_name: &str, raw: &str) -> Result<usize> {
    let value = raw
        .parse::<usize>()
        .with_context(|| format!("{env_name} must be a positive integer"))?;
    if value == 0 {
        anyhow::bail!("{env_name} must be greater than zero");
    }

    Ok(value)
}

pub(super) fn parse_agent_bool(env_name: &str, raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => anyhow::bail!("{env_name} must be one of 1, true, yes, on, 0, false, no, off"),
    }
}

pub(super) fn agent_project_cooldown_reason(
    project: &agent::AgentProject,
    now: u64,
    success_cooldown: Duration,
    failure_backoff: Duration,
) -> Option<String> {
    if project.failure_count > 0
        && let Some(remaining) =
            remaining_agent_delay(project.last_failure_at.as_deref(), now, failure_backoff)
    {
        return Some(format!("failure backoff active for {remaining}s"));
    }

    remaining_agent_delay(project.last_success_at.as_deref(), now, success_cooldown)
        .map(|remaining| format!("success cooldown active for {remaining}s"))
}

pub(super) fn agent_task_cooldown_reason(
    project: &agent::AgentProject,
    task_selection: AgentTaskSelection,
    now: u64,
    success_cooldown: Duration,
    failure_backoff: Duration,
) -> Option<String> {
    if task_selection == AgentTaskSelection::ResumeDoing {
        return None;
    }
    if task_selection == AgentTaskSelection::ResumeSession {
        return (project.failure_count > 0)
            .then(|| {
                remaining_agent_delay(project.last_failure_at.as_deref(), now, failure_backoff)
            })
            .flatten()
            .map(|remaining| format!("failure backoff active for {remaining}s"));
    }

    agent_project_cooldown_reason(project, now, success_cooldown, failure_backoff).or_else(|| {
        (task_selection == AgentTaskSelection::RecoverBlocked)
            .then(|| blocked_recovery_backoff_reason(project, now, failure_backoff))
            .flatten()
    })
}

pub(super) fn blocked_recovery_backoff_reason(
    project: &agent::AgentProject,
    now: u64,
    failure_backoff: Duration,
) -> Option<String> {
    remaining_agent_delay(
        project.last_blocked_recovery_at.as_deref(),
        now,
        failure_backoff,
    )
    .map(|remaining| format!("blocked-task recovery backoff active for {remaining}s"))
}

pub(super) fn remaining_agent_delay(
    last_at: Option<&str>,
    now: u64,
    delay: Duration,
) -> Option<u64> {
    let last_at = last_at?.parse::<u64>().ok()?;
    let ready_at = last_at.saturating_add(delay.as_secs());

    if ready_at > now {
        Some(ready_at - now)
    } else {
        None
    }
}
