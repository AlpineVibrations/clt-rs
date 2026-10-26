//! Refresh only the external snapshot after a writer exited between commit and export.
//! Unlike coordination/index repair, this keeps the live database and its readers intact.
use std::{fs, path::Path};

use anyhow::Result;
use serde_json::Value as Json;

use super::{
    DIRTY_FILE, INTERRUPTED_UPDATE_REASON, REQUIRED_FILE, RegistryAccess, SNAPSHOT_FILE,
    atomic_write, finish_update, integrity_check, marker_lock, read_database_snapshot,
    read_snapshot, remove_if_exists, sync_directory, write_lock,
};
use crate::agent::{AGENT_DB_FILE, open_checkpoint_pin, runtime::AgentStoreBlockingAdapter};
use clt_database::turso::Builder;

fn eligible(state_dir: &Path) -> bool {
    state_dir.join(DIRTY_FILE).is_file()
        && !state_dir.join("recovery-in-progress.json").exists()
        && fs::read(state_dir.join(REQUIRED_FILE)).ok().as_deref()
            == Some(INTERRUPTED_UPDATE_REASON.as_bytes())
}

pub(super) fn try_repair(state_dir: &Path) -> Result<bool> {
    if !eligible(state_dir) {
        return Ok(false);
    }
    let _access = RegistryAccess::shared(state_dir)?;
    let _writer = write_lock(state_dir)?;
    if !eligible(state_dir) {
        return Ok(false);
    }
    anyhow::ensure!(
        read_snapshot(state_dir)?.is_some(),
        "Registry snapshot is missing"
    );
    let path = state_dir.join(AGENT_DB_FILE);
    anyhow::ensure!(
        fs::metadata(&path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0),
        "The original agent database is missing or empty"
    );

    // Do not migrate, checkpoint, reindex, replace coordination files, or restore
    // from the old snapshot. The writer fence makes committed rows authoritative
    // even when the previous snapshot omitted a newly registered live worker.
    let blocking = AgentStoreBlockingAdapter::new(state_dir, true)?;
    let snapshot = blocking.block_on_recovery(async {
        let db = Builder::new_local(path.to_string_lossy().as_ref())
            .experimental_multiprocess_wal(true)
            .build()
            .await?;
        let pin = open_checkpoint_pin(&db, &path).await?;
        integrity_check(&db).await?;
        let snapshot = read_database_snapshot(&db).await?;
        pin.execute("ROLLBACK", ()).await?;
        Ok(snapshot)
    })?;

    publish_verified_snapshot(state_dir, &snapshot)?;
    Ok(true)
}

fn publish_verified_snapshot(state_dir: &Path, snapshot: &Json) -> Result<()> {
    // A concurrent reader may have discovered an actual storage failure. Its
    // marker must win over this snapshot-only repair, including during teardown.
    let _marker = marker_lock(state_dir)?;
    anyhow::ensure!(
        eligible(state_dir),
        "Registry recovery reason changed during snapshot verification"
    );
    atomic_write(
        &state_dir.join(SNAPSHOT_FILE),
        &serde_json::to_vec_pretty(snapshot)?,
    )?;
    remove_if_exists(&state_dir.join(REQUIRED_FILE))?;
    // A crash here leaves a dirty marker that can safely take this same path again.
    sync_directory(state_dir)?;
    finish_update(state_dir)
}

#[cfg(test)]
#[path = "interrupted_snapshot_tests.rs"]
mod tests;
