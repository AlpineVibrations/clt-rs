use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::{
    agent::TursoAgentStore, runner::agent_timestamp_seconds, scheduler::agent_lease_renew_interval,
};

/// Retry contention only within the last confirmed reservation. In particular,
/// a busy writer must never move the expiry forward or hide a recovery marker.
pub(super) struct InteractiveGuardianLease {
    expires_at: u64,
    timeout_seconds: u64,
    renew_interval: Duration,
    next_renewal: Instant,
}

impl InteractiveGuardianLease {
    /// Construct before child registration, which publishes an expiry at least
    /// this far in the future. Include time spent registering and releasing it.
    pub(super) fn new(timeout: Duration) -> Self {
        let timeout_seconds = timeout.as_secs().max(60);
        let renew_interval = agent_lease_renew_interval(timeout);
        Self {
            expires_at: agent_timestamp_seconds().saturating_add(timeout_seconds),
            timeout_seconds,
            renew_interval,
            next_renewal: Instant::now() + renew_interval,
        }
    }

    pub(super) fn renew_if_due(
        &mut self,
        store: &TursoAgentStore,
        project_id: i64,
        holder: &str,
    ) -> Result<()> {
        let now = agent_timestamp_seconds();
        if now >= self.expires_at {
            anyhow::bail!(
                "Interactive guardian project lease expired before renewal could be confirmed"
            );
        }
        if Instant::now() < self.next_renewal {
            return Ok(());
        }
        let expires_at = now.saturating_add(self.timeout_seconds);
        match store
            .try_renew_lease_blocking(project_id, holder, &expires_at.to_string())
            .context("Failed to renew interactive guardian lease")?
        {
            Some(true) => {
                self.expires_at = expires_at;
                self.next_renewal = Instant::now() + self.renew_interval;
            }
            Some(false) => anyhow::bail!("Interactive guardian no longer holds its project lease"),
            None => {
                // Continue polling the child and parent lifeline between tries.
                // Contention is expected during updates and service restarts.
                self.next_renewal = Instant::now() + Duration::from_millis(250);
            }
        }
        if agent_timestamp_seconds() >= self.expires_at {
            anyhow::bail!(
                "Interactive guardian project lease expired before renewal could be confirmed"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
