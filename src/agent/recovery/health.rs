use std::{
    fmt, fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use turso::Database;

use super::{atomic_write, mark_required};

const CHECKED_AT: &str = "integrity-checked-at";
const CHECK_INTERVAL_SECONDS: u64 = 60;
const WORKER_INDEXES: &[&str] = &[
    "agent_workers_active_project_unique",
    "sqlite_autoindex_agent_workers_1",
    "sqlite_autoindex_agent_workers_2",
    "sqlite_autoindex_agent_workers_3",
];

#[derive(Debug)]
struct IntegrityFailure(Vec<String>);

impl fmt::Display for IntegrityFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Agent registry integrity check: {}", self.0.join("; "))
    }
}

impl std::error::Error for IntegrityFailure {}

pub(super) async fn integrity_check(db: &Database) -> Result<()> {
    let conn = db.connect()?;
    let mut rows = conn.query("PRAGMA integrity_check", ()).await?;
    let mut failures = Vec::new();
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        let result = row.get::<String>(0)?;
        if result != "ok" {
            failures.push(result);
        }
        count += 1;
    }
    if !failures.is_empty() {
        return Err(IntegrityFailure(failures).into());
    }
    anyhow::ensure!(
        count == 1,
        "Agent registry integrity check returned no definitive result"
    );
    let mut foreign = conn.query("PRAGMA foreign_key_check", ()).await?;
    if foreign.next().await?.is_some() {
        return Err(IntegrityFailure(vec!["broken foreign keys".into()]).into());
    }
    Ok(())
}

/// The caller holds the registry writer lock. Failed SQL (for example Busy) is
/// not proof of corruption; only a definitive integrity result requests repair.
pub(in crate::agent) async fn check_health_if_due(db: &Database, state_dir: &Path) -> Result<()> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let last = fs::read_to_string(state_dir.join(CHECKED_AT))
        .ok()
        .and_then(|text| text.parse::<u64>().ok());
    if last.is_some_and(|last| now >= last && now - last < CHECK_INTERVAL_SECONDS) {
        return Ok(());
    }
    if let Err(error) = integrity_check(db).await {
        if error.is::<IntegrityFailure>() {
            mark_required(state_dir, &error.to_string())?;
        }
        return Err(error);
    }
    atomic_write(&state_dir.join(CHECKED_AT), now.to_string().as_bytes())
}

fn worker_index_failure(message: &str) -> bool {
    let name = message
        .strip_prefix("wrong # of entries in index ")
        .or_else(|| {
            let (row, name) = message
                .strip_prefix("row ")?
                .split_once(" missing from index ")?;
            row.parse::<u64>().ok()?;
            Some(name)
        });
    name.is_some_and(|name| WORKER_INDEXES.contains(&name))
}

/// Runs only inside the existing exclusive recovery fence, after quarantine and
/// coordination rebuild. Refill derived worker indexes from authoritative rows;
/// never reconstruct tables or discard history during automatic recovery.
pub(super) async fn repair_worker_indexes_if_needed(db: &Database) -> Result<()> {
    let error = match integrity_check(db).await {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let repairable = error
        .downcast_ref::<IntegrityFailure>()
        .is_some_and(|failure| {
            failure
                .0
                .iter()
                .all(|message| worker_index_failure(message))
        });
    if !repairable {
        return Err(error);
    }
    let conn = db.connect()?;
    conn.execute("REINDEX agent_workers", ())
        .await
        .context("Failed to rebuild damaged worker indexes")?;
    integrity_check(db)
        .await
        .context("Worker index repair did not restore registry integrity")
}

#[cfg(test)]
mod tests;
