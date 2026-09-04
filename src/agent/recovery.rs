//! Durable recovery material and exclusive maintenance for the rebuildable registry.
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde_json::{Map, Value as Json, json};
use turso::{Database, Value, params_from_iter};

use super::{TursoAgentStore, configure_agent_connection};

pub(crate) const SNAPSHOT_FILE: &str = "registry.json";
const DIRTY_FILE: &str = "registry-dirty";
const REQUIRED_FILE: &str = "recovery-required";
const RECOVERING_REASON: &str = "Exclusive registry recovery in progress";
const BUNDLE_FILES: [&str; 4] = ["agent.db", "agent.db-wal", "agent.db-tshm", "agent.db-shm"];

// Retain identity, user choices and immutable Git boundaries, not run history or leases.
const TABLES: &[(&str, &str)] = &[
    (
        "projects",
        "id,path,name,enabled,registered_at,updated_at,git_mode,codex_provider,codex_model,codex_reasoning_effort,codex_fast_enabled",
    ),
    ("model_providers", "*"),
    ("model_targets", "*"),
    ("agent_settings", "*"),
    ("agent_workers", "*"),
    ("session_controls", "*"),
    ("agent_git_launch_states", "*"),
    ("git_finalizations", "*"),
];

pub(super) struct RegistryAccess {
    // Keep this field until every Turso handle and checkpoint pin has been dropped.
    _file: File,
}

impl RegistryAccess {
    pub(super) fn shared(state_dir: &Path) -> Result<Self> {
        let file = lock_file(state_dir, "agent-access.lock")?;
        file.try_lock_shared()
            .context("Agent registry recovery is in progress; retry after it finishes")?;
        Ok(Self { _file: file })
    }

    fn exclusive(state_dir: &Path) -> Result<Self> {
        let file = lock_file(state_dir, "agent-access.lock")?;
        file.try_lock().context("Agent registry is still in use. Close CLT TUIs and stop foreground agents and workers, then retry clt agent recover")?;
        Ok(Self { _file: file })
    }
}

fn lock_file(state_dir: &Path, name: &str) -> Result<File> {
    fs::create_dir_all(state_dir)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state_dir.join(name))
        .with_context(|| format!("Failed to open registry lock {name}"))
}

pub(super) fn write_lock(state_dir: &Path) -> Result<File> {
    let file = lock_file(state_dir, "agent-write.lock")?;
    file.lock()
        .context("Failed to serialize durable registry updates")?;
    Ok(file)
}

pub(crate) fn check_required(state_dir: &Path) -> Result<()> {
    if state_dir.join(REQUIRED_FILE).exists()
        || state_dir.join("recovery-in-progress.json").exists()
    {
        anyhow::bail!(
            "Agent registry recovery required. Stop agents, close CLT TUIs, and run clt agent recover (state: {})",
            state_dir.display()
        );
    }
    Ok(())
}

pub(super) fn check_clean(state_dir: &Path) -> Result<()> {
    check_required(state_dir)?;
    if state_dir.join(DIRTY_FILE).exists() {
        mark_required(
            state_dir,
            "An agent registry update was interrupted before its external snapshot was durable",
        )?;
        check_required(state_dir)?;
    }
    Ok(())
}

pub(super) fn mark_required(state_dir: &Path, reason: &str) -> Result<()> {
    atomic_write(&state_dir.join(REQUIRED_FILE), reason.as_bytes())
}

pub(super) fn begin_update(state_dir: &Path) -> Result<()> {
    check_clean(state_dir)?;
    atomic_write(
        &state_dir.join(DIRTY_FILE),
        b"Registry update in progress; snapshot may lag the database",
    )
}

pub(super) fn finish_update(state_dir: &Path) -> Result<()> {
    remove_if_exists(&state_dir.join(DIRTY_FILE))?;
    sync_directory(state_dir)
}

pub(super) fn shared_wal_failure(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    (text.contains("shared wal")
        && (text.contains("frame") || text.contains("owner") || text.contains("assertion")))
        || [
            "process-local writer released by non-owner",
            "process-local checkpoint released by non-owner",
            "process-local reader slot released by non-owner",
            "shared owner slot released by non-owner",
            "reader slot updated by non-owner",
            "reader slot released by non-owner",
        ]
        .iter()
        .any(|signature| text.contains(signature))
}

pub(super) fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("Durable registry file has no directory")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}-{}.partial", std::process::id(), nonce()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("Failed to durably publish {}", path.display()))
}

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?
        .sync_all()
        .with_context(|| format!("Failed to sync {}", path.display()))
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn snapshot(db: &Database, state_dir: &Path) -> Result<()> {
    let mut conn = db.connect()?;
    configure_agent_connection(&conn).await?;
    let transaction = conn.transaction().await?;
    let mut launch_owners = HashSet::new();
    {
        let mut rows = transaction
            .query("SELECT run_token FROM agent_git_launch_states", ())
            .await?;
        while let Some(row) = rows.next().await? {
            launch_owners.insert(row.get::<String>(0)?);
        }
    }
    let mut tables = Map::new();
    for (name, columns) in TABLES {
        let mut rows = transaction
            .query(&format!("SELECT {columns} FROM {name}"), ())
            .await?;
        let names = rows.column_names();
        let mut records = Vec::new();
        while let Some(row) = rows.next().await? {
            let mut record = Map::new();
            for (index, name) in names.iter().enumerate() {
                let value = match row.get_value(index)? {
                    Value::Null => Json::Null,
                    Value::Integer(value) => json!(value),
                    Value::Text(value) => json!(value),
                    _ => anyhow::bail!("Unsupported value in durable registry {name}"),
                };
                record.insert(name.clone(), value);
            }
            if *name != "agent_workers"
                || matches!(
                    record.get("state").and_then(Json::as_str),
                    Some("dispatching" | "running" | "finalizing")
                )
                || record
                    .get("worker_token")
                    .and_then(Json::as_str)
                    .is_some_and(|token| launch_owners.contains(token))
            {
                records.push(Json::Object(record));
            }
        }
        tables.insert((*name).to_string(), json!(records));
    }
    transaction.commit().await?;
    let bytes = serde_json::to_vec_pretty(&json!({"version": 1, "tables": tables}))?;
    let path = state_dir.join(SNAPSHOT_FILE);
    if fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
        atomic_write(&path, &bytes)?;
    }
    Ok(())
}

pub(crate) fn read_snapshot(state_dir: &Path) -> Result<Option<Json>> {
    let path = state_dir.join(SNAPSHOT_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let snapshot: Json = serde_json::from_slice(&fs::read(&path)?)
        .with_context(|| format!("Invalid external registry snapshot {}", path.display()))?;
    anyhow::ensure!(
        snapshot["version"] == 1,
        "Unsupported registry snapshot version"
    );
    for (table, _) in TABLES {
        anyhow::ensure!(
            snapshot["tables"][table].is_array(),
            "Registry snapshot is missing {table}"
        );
    }
    Ok(Some(snapshot))
}

async fn integrity_check(db: &Database) -> Result<()> {
    let conn = db.connect()?;
    let mut rows = conn.query("PRAGMA integrity_check", ()).await?;
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        let result = row.get::<String>(0)?;
        anyhow::ensure!(result == "ok", "Agent registry integrity check: {result}");
        count += 1;
    }
    anyhow::ensure!(
        count == 1,
        "Agent registry integrity check returned no definitive result"
    );
    let mut foreign = conn.query("PRAGMA foreign_key_check", ()).await?;
    anyhow::ensure!(
        foreign.next().await?.is_none(),
        "Agent registry has broken foreign keys"
    );
    Ok(())
}

async fn restore_snapshot(db: &Database, snapshot: &Json) -> Result<()> {
    let mut conn = db.connect()?;
    configure_agent_connection(&conn).await?;
    let transaction = conn.transaction().await?;
    for (table, _) in TABLES.iter().rev() {
        transaction
            .execute(&format!("DELETE FROM {table}"), ())
            .await?;
    }
    for (table, _) in TABLES {
        let columns = transaction
            .query(&format!("SELECT * FROM {table} LIMIT 0"), ())
            .await?
            .column_names();
        for record in snapshot["tables"][table]
            .as_array()
            .context("Missing snapshot records")?
        {
            let mut record = record
                .as_object()
                .context("Invalid snapshot record")?
                .clone();
            if *table == "agent_workers" {
                // Retain terminal identity for immutable prelaunch boundaries:
                // their reclamation must prove the exact worker is gone.
                record.insert("state".into(), json!("abandoned"));
                record.insert(
                    "finished_at".into(),
                    json!(crate::runner::agent_timestamp()),
                );
                record.insert("run_id".into(), Json::Null);
                record.insert(
                    "error".into(),
                    json!("Worker stopped and reconciled by exclusive registry recovery"),
                );
            }
            if *table == "git_finalizations" {
                // The run table is intentionally disposable. Keep proof and acknowledgement time.
                record.insert("acknowledged_run_id".into(), Json::Null);
                record.insert("owner_run_token".into(), Json::Null);
            }
            if *table == "session_controls" {
                record.insert("child_pid".into(), Json::Null);
                record.insert("interactive_holder".into(), Json::Null);
                record.insert("interactive_launch_token".into(), Json::Null);
                // Never restart an unjournaled session merely because its old worker died.
                record.insert("state".into(), json!("stopped"));
                record.insert("run_token".into(), Json::Null);
            }
            anyhow::ensure!(
                record.keys().all(|key| columns.contains(key)),
                "Snapshot contains unknown {table} columns"
            );
            let names = record.keys().map(String::as_str).collect::<Vec<_>>();
            let parameters = record
                .values()
                .map(|value| match value {
                    Json::Null => Ok(Value::Null),
                    Json::String(text) => Ok(Value::Text(text.clone())),
                    Json::Number(number) => number
                        .as_i64()
                        .map(Value::Integer)
                        .context("Invalid registry integer"),
                    _ => anyhow::bail!("Invalid registry value"),
                })
                .collect::<Result<Vec<_>>>()?;
            let placeholders = vec!["?"; names.len()].join(",");
            transaction
                .execute(
                    &format!(
                        "INSERT INTO {table} ({}) VALUES ({placeholders})",
                        names.join(",")
                    ),
                    params_from_iter(parameters),
                )
                .await?;
        }
    }
    // Journals retain exact identity and generation. Pending non-push work needs a
    // generation-bound recovery request, not the dead worker's control token.
    transaction.execute("DELETE FROM session_controls WHERE EXISTS (SELECT 1 FROM git_finalizations g WHERE g.project_id = session_controls.project_id AND g.codex_session_id = session_controls.codex_session_id AND g.state IN ('working','tracking','commit_pending','push_pending'))", ()).await?;
    transaction.execute("INSERT INTO session_controls (project_id,codex_session_id,state,run_token,updated_at) SELECT project_id,codex_session_id,'resume_requested','clt-git-finalization:' || generation,strftime('%s','now') FROM git_finalizations WHERE state IN ('working','tracking','commit_pending')", ()).await?;
    transaction.commit().await?;
    integrity_check(db).await
}

// CLT's lifetime lock covers new clients. POSIX locks additionally reject a
// still-open pre-refactor Turso/SQLite client before touching its files.
#[cfg(unix)]
fn lock_legacy_users(state_dir: &Path) -> Result<Vec<File>> {
    use std::os::fd::AsRawFd;
    let mut locks = Vec::new();
    for name in BUNDLE_FILES {
        let path = state_dir.join(name);
        if !path.exists() {
            continue;
        }
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_len = 0;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) } == -1 {
            return Err(std::io::Error::last_os_error())
                .context("A legacy database user is still active; stop it before recovery");
        }
        locks.push(file);
    }
    Ok(locks)
}

#[cfg(not(unix))]
fn lock_legacy_users(_state_dir: &Path) -> Result<Vec<File>> {
    anyhow::bail!("Exclusive Turso registry recovery is supported only on Unix")
}

fn quarantine_bundle(state_dir: &Path) -> Result<PathBuf> {
    let root = state_dir.join("quarantine");
    fs::create_dir_all(&root)?;
    let name = format!("agent-{}-{}", nonce(), std::process::id());
    let staging = root.join(format!(".{name}.partial"));
    let destination = root.join(name);
    fs::create_dir(&staging)?;
    for name in BUNDLE_FILES
        .into_iter()
        .chain([SNAPSHOT_FILE, DIRTY_FILE, REQUIRED_FILE])
    {
        let source = state_dir.join(name);
        if source.exists() {
            fs::copy(source, staging.join(name))?;
            File::open(staging.join(name))?.sync_all()?;
        }
    }
    sync_directory(&staging)?;
    fs::rename(staging, &destination)?;
    sync_directory(&root)?;
    Ok(destination)
}

fn restore_bundle(state_dir: &Path, archive: &Path) -> Result<()> {
    for name in BUNDLE_FILES {
        let source = archive.join(name);
        if source.exists() {
            atomic_write(&state_dir.join(name), &fs::read(source)?)?;
        } else {
            remove_if_exists(&state_dir.join(name))?;
        }
    }
    sync_directory(state_dir)
}

pub(crate) struct RecoveryReport {
    pub(crate) quarantine: PathBuf,
    pub(crate) rebuilt_registry: bool,
}

/// Call after stopping service-managed processes. A held TUI/worker access lock
/// or legacy database lock refuses maintenance without changing the bundle.
#[cfg(test)]
pub(crate) fn recover_registry(state_dir: &Path) -> Result<RecoveryReport> {
    recover_registry_with(state_dir, |_| Ok(()))
}

pub(crate) fn recover_registry_with(
    state_dir: &Path,
    stop_services: impl Fn(&Json) -> Result<()>,
) -> Result<RecoveryReport> {
    if let Some(manifest) = read_snapshot(state_dir)? {
        stop_services(&manifest)?;
    }
    let _access = RegistryAccess::exclusive(state_dir)?;
    let _writer = write_lock(state_dir)?;
    let legacy = lock_legacy_users(state_dir)?;
    let snapshot = read_snapshot(state_dir)?;
    if let Some(manifest) = &snapshot {
        // Reload under the exclusive fence: a worker may have registered during
        // the initial drain, and no new DB user can now race this final check.
        stop_services(manifest)?;
    }
    let dirty = state_dir.join(DIRTY_FILE).exists();
    let progress = state_dir.join("recovery-in-progress.json");
    if progress.exists() {
        let archive: PathBuf = serde_json::from_slice(&fs::read(&progress)?)?;
        anyhow::ensure!(
            archive.parent() == Some(state_dir.join("quarantine").as_path()),
            "Invalid recovery quarantine path"
        );
        restore_bundle(state_dir, &archive)?;
    }
    let has_database =
        fs::metadata(state_dir.join(super::AGENT_DB_FILE)).is_ok_and(|metadata| metadata.len() > 0);
    let quarantine = quarantine_bundle(state_dir)?;
    atomic_write(&progress, &serde_json::to_vec(&quarantine)?)?;
    mark_required(state_dir, RECOVERING_REASON)?;
    for name in ["agent.db-tshm", "agent.db-shm"] {
        remove_if_exists(&state_dir.join(name))?;
    }
    sync_directory(state_dir)?;
    drop(legacy);

    let attempt = || -> Result<()> {
        anyhow::ensure!(
            has_database,
            "The original agent database is missing or empty"
        );
        {
            let store = TursoAgentStore::open_for_recovery(state_dir)?;
            store.blocking.block_on_recovery(async {
                integrity_check(&store.recovery_db).await?;
                snapshot_db(&store.recovery_db, state_dir).await
            })?;
        }
        verify_recovery_teardown(state_dir)
    };
    let mut rebuilt_registry = false;
    if let Err(coordination_error) = attempt() {
        restore_bundle(state_dir, &quarantine)?;
        anyhow::ensure!(
            !dirty,
            "Coordination rebuild failed ({coordination_error:#}). The last external snapshot may predate an interrupted Git transition; refusing to reconstruct finalization. Original DB/WAL retained at {}",
            quarantine.display()
        );
        let snapshot = snapshot.context(format!("Coordination rebuild failed ({coordination_error:#}); no external registry snapshot is available. Original DB/WAL retained at {}", quarantine.display()))?;
        for name in BUNDLE_FILES {
            remove_if_exists(&state_dir.join(name))?;
        }
        mark_required(state_dir, RECOVERING_REASON)?;
        let result = (|| -> Result<()> {
            {
                let store = TursoAgentStore::open_for_recovery(state_dir)?;
                store.blocking.block_on_recovery(async {
                    restore_snapshot(&store.recovery_db, &snapshot).await?;
                    snapshot_db(&store.recovery_db, state_dir).await
                })?;
            }
            verify_recovery_teardown(state_dir)
        })();
        if let Err(error) = result {
            restore_bundle(state_dir, &quarantine)?;
            return Err(error).context(format!(
                "Registry reconstruction refused; original bundle retained at {}",
                quarantine.display()
            ));
        }
        rebuilt_registry = true;
    }
    finish_update(state_dir)?;
    remove_if_exists(&progress)?;
    remove_if_exists(&state_dir.join(REQUIRED_FILE))?;
    sync_directory(state_dir)?;
    Ok(RecoveryReport {
        quarantine,
        rebuilt_registry,
    })
}

fn verify_recovery_teardown(state_dir: &Path) -> Result<()> {
    anyhow::ensure!(
        fs::read(state_dir.join(REQUIRED_FILE))? == RECOVERING_REASON.as_bytes(),
        "Turso failed while closing the recovered registry; recovery is still required"
    );
    Ok(())
}

// A distinct name avoids shadowing the externally loaded snapshot in recovery.
async fn snapshot_db(db: &Database, state_dir: &Path) -> Result<()> {
    snapshot(db, state_dir).await
}

#[cfg(test)]
mod tests;
