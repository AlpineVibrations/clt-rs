use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use turso::Database;

use super::recovery;

/// Owns the single Tokio runtime used at the synchronous agent-store boundary.
pub(super) struct AgentStoreBlockingAdapter {
    runtime: tokio::runtime::Runtime,
    state_dir: PathBuf,
    database: Option<Database>,
    recovering: bool,
}

impl AgentStoreBlockingAdapter {
    pub(super) fn new(state_dir: &Path, recovering: bool) -> Result<Self> {
        Ok(Self {
            runtime: tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?,
            state_dir: state_dir.to_path_buf(),
            database: None,
            recovering,
        })
    }

    pub(super) fn attach(&mut self, database: &Database) {
        self.database = Some(database.clone());
    }

    pub(super) fn block_on<T>(&self, future: impl Future<Output = Result<T>>) -> Result<T> {
        if !self.recovering {
            recovery::check_required(&self.state_dir)?;
        }
        self.block_on_recovery(future)
    }

    pub(super) fn block_on_recovery<T>(
        &self,
        future: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        match catch_unwind(AssertUnwindSafe(|| self.runtime.block_on(future))) {
            Ok(result) => {
                if let Err(error) = &result
                    && recovery::shared_wal_failure(&format!("{error:#}"))
                {
                    recovery::mark_required(&self.state_dir, &format!("{error:#}"))?;
                    anyhow::bail!(
                        "Agent registry recovery required after a shared-WAL failure. Run clt agent recover: {error:#}"
                    );
                }
                result
            }
            Err(payload) => {
                let message = payload
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| payload.downcast_ref::<&str>().copied())
                    .unwrap_or("");
                if !recovery::shared_wal_failure(message) {
                    resume_unwind(payload);
                }
                recovery::mark_required(&self.state_dir, message)?;
                anyhow::bail!(
                    "Agent registry recovery required after a Turso shared-WAL panic. Run clt agent recover: {message}"
                )
            }
        }
    }

    pub(super) fn block_on_persist<T>(&self, future: impl Future<Output = Result<T>>) -> Result<T> {
        let _writer = recovery::write_lock(&self.state_dir)?;
        recovery::begin_update(&self.state_dir)?;
        let outcome = self.block_on(future);
        // A panic invalidates the handle; never issue another query on it.
        recovery::check_required(&self.state_dir)?;
        if let Some(db) = &self.database
            && let Err(error) = self.block_on(recovery::snapshot(db, &self.state_dir))
        {
            recovery::mark_required(
                &self.state_dir,
                &format!("External registry snapshot failed: {error:#}"),
            )?;
            return Err(error)
                .context("Agent registry recovery required: external snapshot was not durable");
        }
        recovery::finish_update(&self.state_dir)?;
        outcome
    }

    pub(super) fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }
}
