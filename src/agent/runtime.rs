use std::future::Future;

use anyhow::{Context, Result};

/// Owns the single Tokio runtime used at the synchronous agent-store boundary.
pub(super) struct AgentStoreBlockingAdapter {
    runtime: tokio::runtime::Runtime,
}

impl AgentStoreBlockingAdapter {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            runtime: tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?,
        })
    }

    pub(super) fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    pub(super) fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }
}
