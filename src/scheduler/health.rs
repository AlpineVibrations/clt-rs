use std::{future::Future, time::Duration};

use anyhow::{Context, Result};
use tokio::{task::JoinHandle, time::Instant};

pub(super) enum DaemonHealthOutcome {
    Stopped(Result<()>),
    Unresponsive(anyhow::Error),
}

/// Run the service heartbeat independently of scans, dispatch and poll sleeps.
/// Permit only one database heartbeat at a time, and bound how long the service
/// can stay alive without completing one. The caller restarts only the scheduler.
pub(super) async fn supervise_daemon(
    daemon: impl Future<Output = Result<()>> + Send + 'static,
    mut start_heartbeat: impl FnMut() -> JoinHandle<Result<()>>,
    interval: Duration,
    timeout: Duration,
) -> DaemonHealthOutcome {
    // Keep the supervisor off the task executing scheduler code: a synchronous
    // stall there must not stop the heartbeat timer or its watchdog.
    let mut daemon = tokio::spawn(daemon);
    let mut ticker = tokio::time::interval_at(Instant::now() + interval, interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut deadline = Instant::now() + timeout;
    let mut in_flight: Option<JoinHandle<Result<()>>> = None;
    let mut last_error = None;
    loop {
        tokio::select! {
            result = &mut daemon => {
                let result = result.context("Agent scheduler task failed").and_then(|result| result);
                // Finish the last write before the caller clears its check-in,
                // otherwise a late heartbeat could resurrect a stopped daemon.
                if let Some(handle) = in_flight.take()
                    && tokio::time::timeout_at(deadline, handle).await.is_err()
                {
                    return unresponsive(timeout, last_error);
                }
                return DaemonHealthOutcome::Stopped(result);
            }
            _ = tokio::time::sleep_until(deadline) => {
                daemon.abort();
                if let Some(handle) = in_flight.take() {
                    handle.abort();
                }
                return unresponsive(timeout, last_error);
            }
            _ = ticker.tick(), if in_flight.is_none() => {
                in_flight = Some(start_heartbeat());
            }
            result = async { in_flight.as_mut().unwrap().await }, if in_flight.is_some() => {
                in_flight = None;
                match result.context("Daemon heartbeat task failed").and_then(|result| result) {
                    Ok(()) => {
                        deadline = Instant::now() + timeout;
                        last_error = None;
                    }
                    Err(error) => last_error = Some(error),
                }
            }
        }
    }
}

fn unresponsive(timeout: Duration, last_error: Option<anyhow::Error>) -> DaemonHealthOutcome {
    let message = format!(
        "Agent service could not refresh its registry heartbeat for {} seconds",
        timeout.as_secs()
    );
    DaemonHealthOutcome::Unresponsive(match last_error {
        Some(error) => error.context(message),
        None => anyhow::anyhow!(message),
    })
}

#[cfg(test)]
mod tests;
