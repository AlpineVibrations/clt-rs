//! Supervision of an already-running Codex generation whose original worker exited.
//!
//! Reattachment never launches Codex or repeats Git preflight. The task's session
//! identifies the conversation; the recorded PID and inherited run context bind
//! it to an OS process. Platform handles retain that process's identity when
//! delivering controls, including after numeric PIDs have been reused.

use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use crate::{
    agent::{AgentSessionControlRecord, AgentSessionControlState, with_agent_store_at},
    application::AgentLeaseHolderLiveness,
    platform::{configure_agent_child_command, orphan::OrphanProcess},
    runner::agent_timestamp_after,
    scheduler::{agent_lease_holder_liveness, try_reclaim_inactive_agent_lease},
    worker::next_agent_worker_token,
};

const RECOVERED_LEASE_SECONDS: u64 = 60;
const RECOVERED_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RECOVERED_STOP_GRACE: Duration = Duration::from_secs(3);
const RECOVERED_HOLDER_PREFIX: &str = "clt-reattached-";

pub(super) fn recovered_supervisor_pid(holder: &str) -> Option<u32> {
    holder
        .strip_prefix(RECOVERED_HOLDER_PREFIX)?
        .split('-')
        .next()?
        .parse()
        .ok()
}

fn recoverable_control(control: &AgentSessionControlRecord) -> bool {
    matches!(
        control.state,
        AgentSessionControlState::Running
            | AgentSessionControlState::StopRequested
            | AgentSessionControlState::InterruptRequested
    ) && control.interactive_launch_token.is_none()
        && control.child_pid.is_some()
        && control.run_token.is_some()
}

fn same_generation(
    expected: &AgentSessionControlRecord,
    current: &AgentSessionControlRecord,
) -> bool {
    expected.project_id == current.project_id
        && expected.codex_session_id == current.codex_session_id
        && expected.child_pid == current.child_pid
        && expected.run_token == current.run_token
        && recoverable_control(current)
}

/// Called both by the scheduler and by a requested TUI takeover. The store claim
/// in the child repeats all ownership checks after inspecting the OS process.
pub(super) fn ensure_orphaned_session_supervision(
    state_dir: &Path,
    project_id: i64,
    session_id: &str,
) -> Result<bool> {
    let (control, lease, project, has_worker) = with_agent_store_at(state_dir, |store| {
        Ok((
            store.session_control_blocking(project_id, session_id)?,
            store.lease_for_project_blocking(project_id)?,
            store
                .list_projects_blocking()?
                .into_iter()
                .find(|p| p.id == project_id),
            store
                .list_active_workers_blocking()?
                .iter()
                .any(|w| w.project_id == project_id),
        ))
    })?;
    let Some(control) = control.filter(recoverable_control) else {
        return Ok(false);
    };
    if has_worker {
        return Ok(false);
    }
    let Some(project) = project else {
        return Ok(false);
    };
    if lease.as_ref().is_some_and(|lease| {
        agent_lease_holder_liveness(&lease.holder) != AgentLeaseHolderLiveness::Dead
    }) {
        return Ok(false);
    }
    // Validate before spawning so unverifiable legacy records remain fenced
    // with a useful error, rather than spawning a failing helper every tick.
    let Some(_process) = OrphanProcess::attach(
        control.child_pid.context("Missing orphan process ID")?,
        project_id,
        control
            .run_token
            .as_deref()
            .context("Missing orphan run token")?,
    )?
    else {
        return Ok(false);
    };
    if let Some(lease) = lease
        && !try_reclaim_inactive_agent_lease(state_dir, &project, None, &lease, false)?
    {
        return Ok(false);
    }
    spawn_orphaned_session_supervisor(state_dir, &control)?;
    Ok(true)
}

fn spawn_orphaned_session_supervisor(
    state_dir: &Path,
    control: &AgentSessionControlRecord,
) -> Result<()> {
    let log_dir = state_dir.join("session-supervisors");
    fs::create_dir_all(&log_dir)?;
    // Use an internally generated filename; session IDs from task text never
    // become filesystem paths.
    let log_path = log_dir.join(format!(
        "{}.log",
        next_agent_worker_token(control.project_id)
    ));
    let log = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&log_path)?;
    let mut command = orphaned_supervisor_command(state_dir, control)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    configure_agent_child_command(&mut command);
    let mut child = command
        .spawn()
        .context("Failed to start replacement Codex supervisor")?;
    thread::Builder::new()
        .name(format!("clt-reattached-{}", control.project_id))
        .spawn(move || {
            let _ = child.wait();
        })
        .context("Failed to start replacement-supervisor reaper")?;
    eprintln!(
        "Project {}: action=supervisor_reattaching session={} log={}",
        control.project_id,
        control.codex_session_id,
        log_path.display()
    );
    Ok(())
}

fn orphaned_supervisor_command(
    state_dir: &Path,
    control: &AgentSessionControlRecord,
) -> Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    #[cfg(not(test))]
    command
        .arg("--local")
        .arg("agent")
        .arg("supervise-session")
        .arg("--state-dir")
        .arg(state_dir)
        .arg("--project-id")
        .arg(control.project_id.to_string())
        .arg("--session-id")
        .arg(&control.codex_session_id)
        .arg("--child-pid")
        .arg(
            control
                .child_pid
                .context("Missing orphan process ID")?
                .to_string(),
        )
        .arg("--run-token")
        .arg(
            control
                .run_token
                .as_deref()
                .context("Missing orphan run token")?,
        );
    #[cfg(test)]
    command
        .arg("--exact")
        .arg("session_recovery::tests::supervisor_process_entry")
        .arg("--nocapture")
        .env("CLT_TEST_RECOVERED_STATE_DIR", state_dir)
        .env("CLT_TEST_RECOVERED_PROJECT", control.project_id.to_string())
        .env("CLT_TEST_RECOVERED_SESSION", &control.codex_session_id)
        .env(
            "CLT_TEST_RECOVERED_PID",
            control
                .child_pid
                .context("Missing orphan process ID")?
                .to_string(),
        )
        .env(
            "CLT_TEST_RECOVERED_TOKEN",
            control
                .run_token
                .as_deref()
                .context("Missing orphan run token")?,
        );
    Ok(command)
}

pub(super) fn run_orphaned_session_supervisor(
    state_dir: &Path,
    project_id: i64,
    session_id: &str,
    child_pid: u32,
    run_token: &str,
) -> Result<()> {
    let Some(expected) = with_agent_store_at(state_dir, |store| {
        store.session_control_blocking(project_id, session_id)
    })?
    else {
        return Ok(());
    };
    if !recoverable_control(&expected)
        || expected.child_pid != Some(child_pid)
        || expected.run_token.as_deref() != Some(run_token)
    {
        return Ok(());
    }
    let Some(mut process) = OrphanProcess::attach(child_pid, project_id, run_token)? else {
        return Ok(());
    };
    let holder = format!(
        "{RECOVERED_HOLDER_PREFIX}{}-{}",
        std::process::id(),
        next_agent_worker_token(project_id)
    );
    if !with_agent_store_at(state_dir, |store| {
        store.claim_orphaned_session_supervision_blocking(
            &expected,
            &holder,
            RECOVERED_LEASE_SECONDS,
        )
    })? {
        return Ok(());
    }
    println!(
        "Reattached supervision to Codex session {session_id}, existing process {child_pid}; preserving its run and output."
    );
    let result = supervise_attached_process(state_dir, &expected, &holder, &mut process);
    // Releasing only our own lease is safe even on an inspection error: the
    // unchanged session control continues fencing the still-running process.
    let release = with_agent_store_at(state_dir, |store| {
        store.release_lease_blocking(project_id, &holder)
    });
    result?;
    release?;
    Ok(())
}

fn supervise_attached_process(
    state_dir: &Path,
    expected: &AgentSessionControlRecord,
    holder: &str,
    process: &mut OrphanProcess,
) -> Result<()> {
    let mut stop_started = None;
    let mut last_renewal = Instant::now();
    let mut last_warning: Option<Instant> = None;
    loop {
        // A failed DB read grants no authority to signal. A successor may hold
        // the lease after recovery; every iteration checks exact ownership again.
        let step = (|| -> Result<bool> {
            let current = with_agent_store_at(state_dir, |store| {
                if !store
                    .lease_for_project_blocking(expected.project_id)?
                    .is_some_and(|lease| lease.holder == holder)
                {
                    return Ok(None);
                }
                if last_renewal.elapsed() >= Duration::from_secs(10) {
                    if !store.renew_lease_blocking(
                        expected.project_id,
                        holder,
                        &agent_timestamp_after(RECOVERED_LEASE_SECONDS),
                    )? {
                        return Ok(None);
                    }
                    last_renewal = Instant::now();
                }
                store.session_control_blocking(expected.project_id, &expected.codex_session_id)
            })?;
            let Some(current) = current.filter(|current| same_generation(expected, current)) else {
                return Ok(true);
            };
            if !process.is_running()? {
                if with_agent_store_at(state_dir, |store| {
                    store.finalize_reattached_automated_session_blocking(
                        expected,
                        holder,
                        RECOVERED_LEASE_SECONDS,
                    )
                })? {
                    println!(
                        "Recovered Codex process group exited; its exit status is unavailable. Recorded session state determines stop, interactive handoff, or exact-session recovery."
                    );
                    return Ok(true);
                }
                return Ok(false);
            }
            if current.state.requested_action().is_some() {
                let started = stop_started.get_or_insert_with(Instant::now);
                if started.elapsed() >= RECOVERED_STOP_GRACE {
                    process.kill()?;
                } else {
                    process.stop()?;
                }
            }
            Ok(false)
        })();
        match step {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => {
                if crate::agent::recovery::check_required(state_dir).is_err() {
                    return Err(error);
                }
                if last_warning.is_none_or(|last| last.elapsed() >= Duration::from_secs(5)) {
                    eprintln!(
                        "Replacement supervisor is preserving the session fence while retrying: {error:#}"
                    );
                    last_warning = Some(Instant::now());
                }
            }
        }
        thread::sleep(RECOVERED_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests;
