use std::{
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::sync::OnceLock;

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item, Table, value};
use turso::transaction::TransactionBehavior;
use turso::{Builder, Connection, Database, Value, params};

use crate::platform::AgentPlatform;

#[cfg(unix)]
use std::os::fd::AsRawFd;

mod repositories;
mod runtime;

use repositories::AgentRepositories;
use runtime::AgentStoreBlockingAdapter;

pub(super) fn open_agent_store() -> Result<TursoAgentStore> {
    let state_dir = ensure_agent_state_dir()?;
    open_agent_store_at(&state_dir)
}

pub(super) fn open_agent_store_at(state_dir: &Path) -> Result<TursoAgentStore> {
    ensure_agent_state_dir_at(state_dir)?;
    TursoAgentStore::open_blocking(state_dir)
}

pub(super) fn with_agent_store_at<T>(
    state_dir: &Path,
    action: impl FnOnce(&TursoAgentStore) -> Result<T>,
) -> Result<T> {
    let store = open_agent_store_at(state_dir)?;
    action(&store)
}

pub(super) fn ensure_agent_state_dir() -> Result<PathBuf> {
    let state_dir = agent_state_dir()?;
    ensure_agent_state_dir_at(&state_dir)?;
    Ok(state_dir)
}

pub(super) fn ensure_agent_state_dir_at(state_dir: &Path) -> Result<()> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("Failed to create agent state directory {:?}", state_dir))
}

#[cfg(not(test))]
pub(super) fn agent_state_dir() -> Result<PathBuf> {
    resolve_agent_state_dir(
        current_agent_platform(),
        std::env::var_os(AGENT_STATE_DIR_ENV).map(PathBuf::from),
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

#[cfg(test)]
pub(super) fn agent_state_dir() -> Result<PathBuf> {
    Ok(isolated_unit_test_agent_state_dir())
}

#[cfg(test)]
pub(super) fn isolated_unit_test_agent_state_dir() -> PathBuf {
    static PROCESS_STATE_DIR: OnceLock<PathBuf> = OnceLock::new();

    let process_state_dir = PROCESS_STATE_DIR
        .get_or_init(|| {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            std::env::temp_dir().join(format!(
                "clt-unit-test-agent-state-{}-{nonce}",
                std::process::id()
            ))
        })
        .clone();
    let Some(test_name) = thread::current().name().map(str::to_owned) else {
        return process_state_dir;
    };
    let test_name = test_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    process_state_dir.join(test_name)
}

pub(super) fn current_agent_platform() -> AgentPlatform {
    if cfg!(target_os = "macos") {
        AgentPlatform::Macos
    } else if cfg!(target_os = "linux") {
        AgentPlatform::Linux
    } else {
        AgentPlatform::Other
    }
}

pub(super) fn resolve_agent_state_dir(
    platform: AgentPlatform,
    override_dir: Option<PathBuf>,
    xdg_state_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = override_dir {
        return Ok(path);
    }

    match platform {
        AgentPlatform::Macos => home
            .map(|path| path.join("Library/Application Support/clt"))
            .ok_or_else(|| {
                anyhow::anyhow!("HOME is required to resolve the agent state directory")
            }),
        AgentPlatform::Linux => {
            if let Some(path) = xdg_state_home {
                Ok(path.join("clt"))
            } else {
                home.map(|path| path.join(".local/state/clt"))
                    .ok_or_else(|| {
                        anyhow::anyhow!("HOME is required to resolve the agent state directory")
                    })
            }
        }
        AgentPlatform::Other => home
            .map(|path| path.join(".local/state/clt"))
            .ok_or_else(|| {
                anyhow::anyhow!("HOME is required to resolve the agent state directory")
            }),
    }
}
pub(super) const CODEX_HOME_ENV: &str = "CODEX_HOME";
pub(super) const AGENT_CODEX_REASONING_EFFORTS: [&str; 7] =
    ["", "low", "medium", "high", "xhigh", "max", "ultra"];
pub(super) const AGENT_STATE_DIR_ENV: &str = "CLT_AGENT_STATE_DIR";
pub(super) const AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX: &str = "clt-git-finalization:";
pub(super) const AGENT_DB_FILE: &str = "agent.db";
const AGENT_DATABASE_OPEN_RETRY_ATTEMPTS: usize = 100;
const AGENT_DATABASE_OPEN_RETRY_MILLIS: u64 = 10;
#[cfg(unix)]
pub(super) const TURSO_SHARED_WAL_HEADER_MIN_BYTES: usize = 48;
#[cfg(unix)]
pub(super) const TURSO_SHARED_WAL_INDEX_OVERFLOW_OFFSET: usize = 44;
#[cfg(unix)]
pub(super) const TURSO_SHARED_WAL_MAGIC: &[u8; 8] = b"TSHMWAL\0";
#[cfg(unix)]
pub(super) const TURSO_SHARED_WAL_VERSION: u32 = 1;
pub(super) const AGENT_WORKERS_ACTIVE_PROJECT_INDEX: &str = "agent_workers_active_project_unique";
const AGENT_WORKERS_ACTIVE_PROJECT_INDEX_SQL: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS agent_workers_active_project_unique
        ON agent_workers(project_id)
        WHERE state IN ('dispatching', 'running', 'finalizing')";
// Any future migration above this version is deferred while a pinned worker is
// active. Store access remains available for cross-generation control and
// recovery; the scheduler waits in compatibility mode until it can migrate.
pub(super) const AGENT_WORKER_SHARED_SCHEMA_VERSION: i64 = 16;
#[cfg(all(unix, test))]
pub(super) const TEST_AGENT_SHARED_WAL_REBUILD_MARKER_ENV: &str =
    "CLT_TEST_AGENT_SHARED_WAL_REBUILD_MARKER";

#[derive(Clone, Copy)]
pub(super) struct AgentProviderPreset {
    pub(super) id: &'static str,
    pub(super) name: &'static str,
    pub(super) base_url: Option<&'static str>,
    pub(super) env_key: Option<&'static str>,
    pub(super) built_in: bool,
}

pub(super) const AGENT_PROVIDER_PRESETS: [AgentProviderPreset; 4] = [
    AgentProviderPreset {
        id: "openai",
        name: "OpenAI",
        base_url: None,
        env_key: Some("OPENAI_API_KEY"),
        built_in: true,
    },
    AgentProviderPreset {
        id: "openrouter",
        name: "OpenRouter",
        base_url: Some("https://openrouter.ai/api/v1"),
        env_key: Some("OPENROUTER_API_KEY"),
        built_in: false,
    },
    AgentProviderPreset {
        id: "ollama",
        name: "Ollama",
        base_url: Some("http://localhost:11434/v1"),
        env_key: None,
        built_in: false,
    },
    AgentProviderPreset {
        id: "lmstudio",
        name: "LM Studio",
        base_url: Some("http://localhost:1234/v1"),
        env_key: None,
        built_in: false,
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentGitMode {
    Off,
    Commit,
    CommitAndPush,
}

impl AgentGitMode {
    pub(super) fn next(self) -> Self {
        match self {
            Self::Off => Self::Commit,
            Self::Commit => Self::CommitAndPush,
            Self::CommitAndPush => Self::Off,
        }
    }

    pub(super) fn database_value(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Commit => "commit",
            Self::CommitAndPush => "commit-and-push",
        }
    }

    pub(super) fn from_database(value: &str) -> Result<Self> {
        match value {
            "off" => Ok(Self::Off),
            "commit" => Ok(Self::Commit),
            "commit-and-push" => Ok(Self::CommitAndPush),
            _ => anyhow::bail!("Unknown agent Git mode: {value}"),
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Commit => "commit",
            Self::CommitAndPush => "commit & push",
        }
    }

    pub(super) fn tui_label(self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Commit => "COM",
            Self::CommitAndPush => "PUSH",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GitFinalizationState {
    Working,
    Tracking,
    CommitPending,
    PushPending,
    Completed,
    Cancelled,
}

impl GitFinalizationState {
    pub(super) fn database_value(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Tracking => "tracking",
            Self::CommitPending => "commit_pending",
            Self::PushPending => "push_pending",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(super) fn from_database(value: &str) -> Result<Self> {
        match value {
            "working" => Ok(Self::Working),
            "tracking" => Ok(Self::Tracking),
            "commit_pending" => Ok(Self::CommitPending),
            "push_pending" => Ok(Self::PushPending),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            _ => anyhow::bail!("Unknown Git finalization state: {value}"),
        }
    }

    pub(super) fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }

    pub(super) fn is_finalizing(self) -> bool {
        matches!(
            self,
            Self::Tracking | Self::CommitPending | Self::PushPending
        )
    }

    pub(super) fn status_label(self) -> &'static str {
        match self {
            Self::Working => "WORKING",
            Self::Tracking | Self::CommitPending => "FINALIZING",
            Self::PushPending => "PUSH-PENDING",
            Self::Completed => "DONE",
            Self::Cancelled => "CANCELLED",
        }
    }

    pub(super) fn can_transition_to(self, next: Self) -> bool {
        match self {
            Self::Working => matches!(next, Self::Working | Self::Tracking | Self::Cancelled),
            Self::Tracking => matches!(next, Self::Tracking | Self::CommitPending),
            Self::CommitPending => matches!(
                next,
                Self::CommitPending | Self::PushPending | Self::Completed
            ),
            Self::PushPending => matches!(next, Self::PushPending | Self::Completed),
            Self::Completed | Self::Cancelled => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentSessionControlAction {
    Stop,
    Interrupt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentSessionControlState {
    Running,
    StopRequested,
    Stopped,
    InterruptRequested,
    ReadyInteractive,
    Interactive,
    ResumeRequested,
}

impl AgentSessionControlState {
    pub(super) fn database_value(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::StopRequested => "stop_requested",
            Self::Stopped => "stopped",
            Self::InterruptRequested => "interrupt_requested",
            Self::ReadyInteractive => "ready_interactive",
            Self::Interactive => "interactive",
            Self::ResumeRequested => "resume_requested",
        }
    }

    pub(super) fn from_database(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "stop_requested" => Ok(Self::StopRequested),
            "stopped" => Ok(Self::Stopped),
            "interrupt_requested" => Ok(Self::InterruptRequested),
            "ready_interactive" => Ok(Self::ReadyInteractive),
            "interactive" => Ok(Self::Interactive),
            "resume_requested" => Ok(Self::ResumeRequested),
            _ => anyhow::bail!("Unknown Codex session control state: {value}"),
        }
    }

    pub(super) fn requested_action(self) -> Option<AgentSessionControlAction> {
        match self {
            Self::StopRequested => Some(AgentSessionControlAction::Stop),
            Self::InterruptRequested => Some(AgentSessionControlAction::Interrupt),
            _ => None,
        }
    }
}

pub(super) fn codex_config_path() -> Result<PathBuf> {
    let codex_home = std::env::var_os(CODEX_HOME_ENV)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or_else(|| {
            anyhow::anyhow!("HOME or {CODEX_HOME_ENV} is required to find Codex config")
        })?;
    Ok(codex_home.join("config.toml"))
}

pub(super) fn valid_codex_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

pub(super) fn valid_environment_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

pub(super) fn mutate_codex_config_at(
    path: &Path,
    mutate: impl FnOnce(&mut DocumentMut) -> Result<()>,
) -> Result<()> {
    let original = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("Failed to read Codex config {path:?}"))?
    } else {
        String::new()
    };
    let mut document = if original.trim().is_empty() {
        DocumentMut::new()
    } else {
        original
            .parse::<DocumentMut>()
            .with_context(|| format!("Codex config is not valid TOML: {path:?}"))?
    };

    mutate(&mut document)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Codex config path has no parent: {path:?}"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create Codex config directory {parent:?}"))?;

    if path.exists() {
        let backup = parent.join("config.toml.clt.bak");
        if !backup.exists() {
            fs::copy(path, &backup)
                .with_context(|| format!("Failed to back up Codex config to {backup:?}"))?;
        }
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_path = parent.join(format!(
        ".config.toml.clt-{}-{nonce}.tmp",
        std::process::id()
    ));
    fs::write(&temp_path, document.to_string())
        .with_context(|| format!("Failed to write temporary Codex config {temp_path:?}"))?;
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error).with_context(|| format!("Failed to replace Codex config {path:?}"));
    }
    Ok(())
}

pub(super) fn upsert_codex_provider_config_at(
    path: &Path,
    provider_id: &str,
    name: &str,
    base_url: &str,
    env_key: Option<&str>,
) -> Result<()> {
    if !valid_codex_provider_id(provider_id) {
        anyhow::bail!("Provider ID must use only ASCII letters, numbers, hyphens, or underscores");
    }
    if base_url.trim().is_empty() {
        anyhow::bail!("A custom provider requires a base URL");
    }

    mutate_codex_config_at(path, |document| {
        if !document.as_table().contains_key("model_providers") {
            document["model_providers"] = Item::Table(Table::new());
        }
        let providers = document["model_providers"]
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("model_providers must be a TOML table"))?;
        if !providers.contains_key(provider_id) {
            providers[provider_id] = Item::Table(Table::new());
        }
        let provider = providers[provider_id]
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("model_providers.{provider_id} must be a TOML table"))?;
        provider["name"] = value(name.trim());
        provider["base_url"] = value(base_url.trim());
        provider["wire_api"] = value("responses");
        if let Some(env_key) = env_key.filter(|key| !key.trim().is_empty()) {
            provider["env_key"] = value(env_key.trim());
        } else {
            provider.remove("env_key");
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests;

pub(super) fn remove_codex_provider_config_at(path: &Path, provider_id: &str) -> Result<bool> {
    if !valid_codex_provider_id(provider_id) {
        anyhow::bail!("Provider ID must use only ASCII letters, numbers, hyphens, or underscores");
    }
    if !path.exists() {
        return Ok(false);
    }

    let mut changed = false;
    mutate_codex_config_at(path, |document| {
        let remove_empty_providers_table = match document.get_mut("model_providers") {
            Some(item) => {
                let providers = item
                    .as_table_mut()
                    .ok_or_else(|| anyhow::anyhow!("model_providers must be a TOML table"))?;
                changed |= providers.remove(provider_id).is_some();
                providers.is_empty()
            }
            None => false,
        };
        if remove_empty_providers_table {
            document.as_table_mut().remove("model_providers");
        }
        if document
            .get("model_provider")
            .and_then(Item::as_str)
            .is_some_and(|configured| configured == provider_id)
        {
            document.as_table_mut().remove("model_provider");
            document.as_table_mut().remove("model");
            document.as_table_mut().remove("model_reasoning_effort");
            changed = true;
        }
        Ok(())
    })?;
    Ok(changed)
}

pub(super) fn set_codex_default_config_at(
    path: &Path,
    provider_id: &str,
    model_id: &str,
    reasoning_effort: Option<&str>,
) -> Result<()> {
    if !valid_codex_provider_id(provider_id) || model_id.trim().is_empty() {
        anyhow::bail!("A valid provider and model are required");
    }
    mutate_codex_config_at(path, |document| {
        document["model_provider"] = value(provider_id);
        document["model"] = value(model_id.trim());
        if let Some(reasoning_effort) = reasoning_effort.filter(|effort| !effort.trim().is_empty())
        {
            document["model_reasoning_effort"] = value(reasoning_effort.trim());
        } else {
            document.as_table_mut().remove("model_reasoning_effort");
        }
        Ok(())
    })
}

pub(super) fn read_codex_default_config_at(
    path: &Path,
) -> Result<(Option<String>, Option<String>)> {
    if !path.exists() {
        return Ok((None, None));
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("Failed to read Codex config {path:?}"))?;
    let document = contents
        .parse::<DocumentMut>()
        .with_context(|| format!("Codex config is not valid TOML: {path:?}"))?;
    Ok((
        document
            .get("model_provider")
            .and_then(Item::as_str)
            .map(str::to_string),
        document
            .get("model")
            .and_then(Item::as_str)
            .map(str::to_string),
    ))
}

pub(super) fn set_codex_model_reasoning_if_default_at(
    path: &Path,
    provider_id: &str,
    model_id: &str,
    reasoning_effort: Option<&str>,
) -> Result<bool> {
    let (configured_provider, configured_model) = read_codex_default_config_at(path)?;
    if configured_provider.as_deref().unwrap_or("openai") != provider_id
        || configured_model.as_deref() != Some(model_id)
    {
        return Ok(false);
    }

    mutate_codex_config_at(path, |document| {
        if let Some(reasoning_effort) = reasoning_effort.filter(|effort| !effort.trim().is_empty())
        {
            document["model_reasoning_effort"] = value(reasoning_effort.trim());
        } else {
            document.as_table_mut().remove("model_reasoning_effort");
        }
        Ok(())
    })?;
    Ok(true)
}
// This is a contention ceiling, not a delay for every query: Turso only
// sleeps and retries while a statement reports that the database is busy.
const AGENT_DB_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

fn shared_wal_path(db_path: &Path) -> PathBuf {
    let mut path = db_path.as_os_str().to_os_string();
    path.push("-tshm");
    PathBuf::from(path)
}

fn error_indicates_stale_shared_wal_index(error: &turso::Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(source) = current {
        let message = source.to_string();
        if message.contains("Invalid page type:") || message.contains("non-index page") {
            return true;
        }
        current = source.source();
    }
    false
}

#[cfg(unix)]
fn request_shared_wal_index_rebuild(db_path: &Path) -> Result<bool> {
    let shared_wal_path = shared_wal_path(db_path);
    let file = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&shared_wal_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "Failed to open Turso shared WAL coordination file {:?}",
                    shared_wal_path
                )
            });
        }
    };
    let file_len = file
        .metadata()
        .with_context(|| {
            format!(
                "Failed to inspect Turso shared WAL coordination file {:?}",
                shared_wal_path
            )
        })?
        .len() as usize;
    if file_len < TURSO_SHARED_WAL_HEADER_MIN_BYTES {
        return Ok(false);
    }

    // The overflow fallback is safe for future opens, but an already-open
    // Turso process may not have a local WAL scan to fall back to. Only
    // request the rebuild after taking Turso's byte-0 lifetime lock
    // exclusively, which proves no peer process is still using this map.
    let mut lifetime_lock: libc::flock = unsafe { std::mem::zeroed() };
    lifetime_lock.l_type = libc::F_WRLCK as _;
    lifetime_lock.l_whence = libc::SEEK_SET as _;
    lifetime_lock.l_start = 0;
    lifetime_lock.l_len = 1;
    let lock_result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lifetime_lock) };
    if lock_result == -1 {
        let error = io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|code| code == libc::EACCES || code == libc::EAGAIN)
        {
            return Ok(false);
        }
        return Err(error).with_context(|| {
            format!(
                "Failed to lock Turso shared WAL coordination file {:?}",
                shared_wal_path
            )
        });
    }

    let mapping_len = file_len.min(4096);
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            mapping_len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    if mapping == libc::MAP_FAILED {
        return Err(io::Error::last_os_error()).with_context(|| {
            format!(
                "Failed to map Turso shared WAL coordination file {:?}",
                shared_wal_path
            )
        });
    }

    let result = (|| {
        let bytes = unsafe { std::slice::from_raw_parts(mapping.cast::<u8>(), mapping_len) };
        if bytes.get(..TURSO_SHARED_WAL_MAGIC.len()) != Some(TURSO_SHARED_WAL_MAGIC) {
            return Ok(false);
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if version != TURSO_SHARED_WAL_VERSION {
            return Ok(false);
        }

        // Turso treats this bit as a correctness fallback: every process
        // stops trusting the persisted page-to-frame index, scans the WAL,
        // and the first idle opener republishes a rebuilt index. The field
        // is a naturally aligned AtomicU32 in the version-1 tshm layout.
        let overflow = unsafe {
            &*mapping
                .cast::<u8>()
                .add(TURSO_SHARED_WAL_INDEX_OVERFLOW_OFFSET)
                .cast::<std::sync::atomic::AtomicU32>()
        };
        overflow.store(1, Ordering::Release);
        #[cfg(test)]
        if let Some(marker_path) = std::env::var_os(TEST_AGENT_SHARED_WAL_REBUILD_MARKER_ENV) {
            fs::write(&marker_path, b"requested").with_context(|| {
                format!("Failed to write shared WAL rebuild marker {marker_path:?}")
            })?;
        }
        Ok(true)
    })();

    let unmap_result = unsafe { libc::munmap(mapping, mapping_len) };
    if unmap_result != 0 {
        return Err(io::Error::last_os_error()).with_context(|| {
            format!(
                "Failed to unmap Turso shared WAL coordination file {:?}",
                shared_wal_path
            )
        });
    }
    result
}

#[cfg(not(unix))]
fn request_shared_wal_index_rebuild(_db_path: &Path) -> Result<bool> {
    Ok(false)
}

struct AgentMigration<'a> {
    version: i64,
    statements: &'a [&'static str],
}

const AGENT_MIGRATIONS: &[AgentMigration<'static>] = &[
    AgentMigration {
        version: 1,
        statements: &[
            "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        )",
            "CREATE TABLE IF NOT EXISTS projects (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            registered_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            last_scan_at TEXT,
            last_run_at TEXT,
            last_success_at TEXT,
            last_failure_at TEXT,
            failure_count INTEGER NOT NULL DEFAULT 0
        )",
            "CREATE TABLE IF NOT EXISTS runs (
            id INTEGER PRIMARY KEY,
            project_id INTEGER NOT NULL REFERENCES projects(id),
            status TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            exit_code INTEGER,
            log_dir TEXT,
            stdout_path TEXT,
            stderr_path TEXT,
            summary TEXT
        )",
            "CREATE TABLE IF NOT EXISTS leases (
            project_id INTEGER PRIMARY KEY REFERENCES projects(id),
            holder TEXT NOT NULL,
            acquired_at TEXT NOT NULL,
            expires_at TEXT NOT NULL
        )",
        ],
    },
    AgentMigration {
        version: 2,
        statements: &["CREATE TABLE IF NOT EXISTS daemon_checkins (
            holder TEXT PRIMARY KEY,
            mode TEXT NOT NULL,
            started_at TEXT NOT NULL,
            checked_in_at TEXT NOT NULL,
            expires_at TEXT NOT NULL
        )"],
    },
    AgentMigration {
        version: 3,
        statements: &[
            "ALTER TABLE projects ADD COLUMN git_commit_enabled INTEGER NOT NULL DEFAULT 0",
        ],
    },
    AgentMigration {
        version: 4,
        statements: &[
            "ALTER TABLE projects ADD COLUMN codex_model TEXT",
            "ALTER TABLE projects ADD COLUMN codex_reasoning_effort TEXT",
            "ALTER TABLE projects ADD COLUMN codex_fast_enabled INTEGER NOT NULL DEFAULT 0",
        ],
    },
    AgentMigration {
        version: 5,
        statements: &[
            "ALTER TABLE projects ADD COLUMN git_mode TEXT NOT NULL DEFAULT 'off'",
            "UPDATE projects SET git_mode = 'commit' WHERE git_commit_enabled != 0",
        ],
    },
    AgentMigration {
        version: 6,
        statements: &["ALTER TABLE projects ADD COLUMN last_blocked_recovery_at TEXT"],
    },
    AgentMigration {
        version: 7,
        statements: &[
            "ALTER TABLE runs ADD COLUMN codex_session_id TEXT",
            "ALTER TABLE runs ADD COLUMN task_content TEXT",
        ],
    },
    AgentMigration {
        version: 8,
        statements: &[
            "ALTER TABLE projects ADD COLUMN codex_provider TEXT",
            "UPDATE projects SET codex_provider = 'openai' WHERE codex_model IS NOT NULL",
            "CREATE TABLE IF NOT EXISTS model_providers (
                provider_id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                base_url TEXT,
                env_key TEXT,
                built_in INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS model_targets (
                provider_id TEXT NOT NULL REFERENCES model_providers(provider_id),
                model_id TEXT NOT NULL,
                label TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                favorite INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (provider_id, model_id)
            )",
            "CREATE TABLE IF NOT EXISTS agent_settings (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                default_provider TEXT,
                default_model TEXT,
                updated_at TEXT NOT NULL
            )",
            "INSERT OR IGNORE INTO agent_settings (id, updated_at) VALUES (1, datetime('now'))",
            "INSERT OR IGNORE INTO model_providers (
                provider_id, name, env_key, built_in, enabled, created_at, updated_at
             ) VALUES ('openai', 'OpenAI', 'OPENAI_API_KEY', 1, 1, datetime('now'), datetime('now'))",
            "INSERT OR IGNORE INTO model_targets (
                provider_id, model_id, label, enabled, favorite, created_at, updated_at
             ) VALUES ('openai', 'gpt-5.6', 'GPT-5.6', 1, 1, datetime('now'), datetime('now'))",
            "INSERT OR IGNORE INTO model_targets (
                provider_id, model_id, label, enabled, favorite, created_at, updated_at
             ) VALUES ('openai', 'gpt-5.6-terra', 'GPT-5.6 Terra', 1, 1, datetime('now'), datetime('now'))",
            "INSERT OR IGNORE INTO model_targets (
                provider_id, model_id, label, enabled, favorite, created_at, updated_at
             ) VALUES ('openai', 'gpt-5.6-luna', 'GPT-5.6 Luna', 1, 0, datetime('now'), datetime('now'))",
            "INSERT OR IGNORE INTO model_targets (
                provider_id, model_id, label, enabled, favorite, created_at, updated_at
             ) VALUES ('openai', 'gpt-5.5', 'GPT-5.5', 1, 0, datetime('now'), datetime('now'))",
            "INSERT OR IGNORE INTO model_targets (
                provider_id, model_id, label, enabled, favorite, created_at, updated_at
             ) VALUES ('openai', 'gpt-5.4', 'GPT-5.4', 1, 0, datetime('now'), datetime('now'))",
            "INSERT OR IGNORE INTO model_targets (
                provider_id, model_id, label, enabled, favorite, created_at, updated_at
            ) VALUES ('openai', 'gpt-5.3-codex-spark', 'GPT-5.3 Codex Spark', 1, 0, datetime('now'), datetime('now'))",
        ],
    },
    AgentMigration {
        version: 9,
        statements: &[
            "INSERT OR IGNORE INTO model_targets (
                provider_id, model_id, label, enabled, favorite, created_at, updated_at
             ) SELECT provider_id, 'gpt-5.6-sol', 'GPT-5.6 Sol', enabled, favorite,
                      created_at, datetime('now')
               FROM model_targets
              WHERE provider_id = 'openai' AND model_id = 'gpt-5.6'",
            "INSERT OR IGNORE INTO model_targets (
                provider_id, model_id, label, enabled, favorite, created_at, updated_at
             ) VALUES ('openai', 'gpt-5.6-sol', 'GPT-5.6 Sol', 1, 1, datetime('now'), datetime('now'))",
            "UPDATE model_targets
                SET enabled = MAX(enabled, COALESCE((
                        SELECT enabled FROM model_targets AS alias
                         WHERE alias.provider_id = 'openai'
                           AND alias.model_id = 'gpt-5.6'
                    ), 0)),
                    favorite = MAX(favorite, COALESCE((
                        SELECT favorite FROM model_targets AS alias
                         WHERE alias.provider_id = 'openai'
                           AND alias.model_id = 'gpt-5.6'
                    ), 0)),
                    label = 'GPT-5.6 Sol',
                    updated_at = datetime('now')
              WHERE provider_id = 'openai' AND model_id = 'gpt-5.6-sol'",
            "UPDATE projects
                SET codex_model = 'gpt-5.6-sol', updated_at = datetime('now')
              WHERE COALESCE(codex_provider, 'openai') = 'openai'
                AND codex_model = 'gpt-5.6'",
            "UPDATE agent_settings
                SET default_model = 'gpt-5.6-sol', updated_at = datetime('now')
              WHERE COALESCE(default_provider, 'openai') = 'openai'
                AND default_model = 'gpt-5.6'",
            "DELETE FROM model_targets
              WHERE provider_id = 'openai' AND model_id = 'gpt-5.6'",
        ],
    },
    AgentMigration {
        version: 10,
        statements: &["ALTER TABLE model_targets ADD COLUMN reasoning_effort TEXT"],
    },
    AgentMigration {
        version: 11,
        statements: &["ALTER TABLE runs DROP COLUMN task_content"],
    },
    AgentMigration {
        version: 12,
        statements: &["CREATE TABLE IF NOT EXISTS session_controls (
            project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            codex_session_id TEXT NOT NULL,
            state TEXT NOT NULL,
            child_pid INTEGER,
            interactive_holder TEXT,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (project_id, codex_session_id)
        )"],
    },
    AgentMigration {
        version: 13,
        statements: &[
            "ALTER TABLE session_controls ADD COLUMN run_token TEXT",
            "ALTER TABLE session_controls ADD COLUMN stdout_path TEXT",
            "ALTER TABLE session_controls ADD COLUMN stderr_path TEXT",
        ],
    },
    AgentMigration {
        version: 14,
        statements: &["ALTER TABLE session_controls ADD COLUMN interactive_launch_token TEXT"],
    },
    AgentMigration {
        version: 15,
        statements: &[
            "ALTER TABLE runs ADD COLUMN worker_token TEXT",
            "CREATE UNIQUE INDEX IF NOT EXISTS runs_worker_token_unique
                ON runs(worker_token)
                WHERE worker_token IS NOT NULL",
            "CREATE TABLE IF NOT EXISTS agent_workers (
                worker_token TEXT PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                state TEXT NOT NULL,
                protocol_version INTEGER NOT NULL,
                lease_holder TEXT NOT NULL UNIQUE,
                service_label TEXT NOT NULL UNIQUE,
                binary_path TEXT NOT NULL,
                command_arguments TEXT NOT NULL,
                path_env TEXT NOT NULL,
                codex_path TEXT,
                task_selection TEXT NOT NULL,
                resume_session_id TEXT,
                worker_pid INTEGER,
                created_at TEXT NOT NULL,
                started_at TEXT,
                heartbeat_at TEXT,
                finished_at TEXT,
                run_id INTEGER,
                error TEXT,
                service_cleaned_at TEXT
            )",
            AGENT_WORKERS_ACTIVE_PROJECT_INDEX_SQL,
        ],
    },
    AgentMigration {
        version: 16,
        statements: &[
            "ALTER TABLE projects ADD COLUMN last_daemon_scan_status TEXT",
            "ALTER TABLE projects ADD COLUMN last_daemon_scan_error TEXT",
        ],
    },
    AgentMigration {
        version: 17,
        statements: &[
            "CREATE TABLE IF NOT EXISTS git_finalizations (
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                codex_session_id TEXT NOT NULL,
                state TEXT NOT NULL,
                git_mode TEXT NOT NULL,
                starting_head TEXT,
                branch_ref TEXT,
                upstream_ref TEXT,
                worktree_baseline TEXT NOT NULL,
                task_identity TEXT,
                owner_run_token TEXT,
                commit_oid TEXT,
                generation INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                completed_at TEXT,
                acknowledged_at TEXT,
                acknowledged_run_id INTEGER REFERENCES runs(id) ON DELETE SET NULL,
                PRIMARY KEY (project_id, codex_session_id)
            )",
            "CREATE UNIQUE INDEX IF NOT EXISTS git_finalizations_pending_project_unique
                ON git_finalizations(project_id)
                WHERE state IN ('tracking', 'commit_pending', 'push_pending')",
            "CREATE TABLE IF NOT EXISTS agent_git_launch_states (
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                run_token TEXT NOT NULL,
                git_mode TEXT NOT NULL,
                starting_head TEXT NOT NULL,
                branch_ref TEXT,
                upstream_ref TEXT,
                worktree_baseline TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (project_id, run_token)
            )",
        ],
    },
];

pub(super) struct TursoAgentStore {
    #[cfg_attr(not(test), allow(dead_code))]
    db_path: PathBuf,
    repositories: AgentRepositories,
    pending_migration_version: Option<i64>,
    checkpoint_pin: Option<Connection>,
    blocking: AgentStoreBlockingAdapter,
}

struct OpenedAgentDatabase {
    db_path: PathBuf,
    db: Database,
    pending_migration_version: Option<i64>,
    checkpoint_pin: Connection,
}

#[derive(Clone, Debug)]
pub(super) struct AgentProject {
    pub(crate) id: i64,
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) git_mode: AgentGitMode,
    pub(crate) codex_provider: Option<String>,
    pub(crate) codex_model: Option<String>,
    pub(crate) codex_reasoning_effort: Option<String>,
    pub(crate) codex_fast_enabled: bool,
    pub(crate) last_scan_at: Option<String>,
    pub(crate) last_daemon_scan_status: Option<String>,
    pub(crate) last_daemon_scan_error: Option<String>,
    pub(crate) last_run_at: Option<String>,
    pub(crate) last_success_at: Option<String>,
    pub(crate) last_failure_at: Option<String>,
    pub(crate) last_blocked_recovery_at: Option<String>,
    pub(crate) failure_count: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentModelProvider {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) base_url: Option<String>,
    pub(crate) env_key: Option<String>,
    pub(crate) built_in: bool,
    pub(crate) enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentModelTarget {
    pub(crate) provider_id: String,
    pub(crate) model_id: String,
    pub(crate) label: String,
    pub(crate) enabled: bool,
    pub(crate) favorite: bool,
    pub(crate) reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct AgentModelDefaults {
    pub(crate) provider_id: Option<String>,
    pub(crate) model_id: Option<String>,
}

pub(super) struct AgentRunOutcome<'a> {
    pub(crate) project_id: i64,
    pub(crate) status: &'a str,
    pub(crate) started_at: &'a str,
    pub(crate) finished_at: Option<&'a str>,
    pub(crate) exit_code: Option<i64>,
    pub(crate) log_dir: Option<&'a str>,
    pub(crate) stdout_path: Option<&'a str>,
    pub(crate) stderr_path: Option<&'a str>,
    pub(crate) summary: Option<&'a str>,
    pub(crate) codex_session_id: Option<&'a str>,
}

pub(super) struct NewGitFinalization<'a> {
    pub(crate) project_id: i64,
    pub(crate) codex_session_id: &'a str,
    pub(crate) git_mode: AgentGitMode,
    pub(crate) starting_head: Option<&'a str>,
    pub(crate) branch_ref: Option<&'a str>,
    pub(crate) upstream_ref: Option<&'a str>,
    pub(crate) worktree_baseline: &'a str,
    pub(crate) task_identity: Option<&'a str>,
    pub(crate) owner_run_token: Option<&'a str>,
    pub(crate) created_at: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GitFinalizationRecord {
    pub(crate) project_id: i64,
    pub(crate) codex_session_id: String,
    pub(crate) state: GitFinalizationState,
    pub(crate) git_mode: AgentGitMode,
    pub(crate) starting_head: Option<String>,
    pub(crate) branch_ref: Option<String>,
    pub(crate) upstream_ref: Option<String>,
    pub(crate) worktree_baseline: String,
    pub(crate) task_identity: Option<String>,
    pub(crate) owner_run_token: Option<String>,
    pub(crate) commit_oid: Option<String>,
    pub(crate) generation: i64,
    pub(crate) last_error: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) completed_at: Option<String>,
    pub(crate) acknowledged_at: Option<String>,
    pub(crate) acknowledged_run_id: Option<i64>,
}

pub(super) struct AgentWorkerReservation<'a> {
    pub(crate) project_id: i64,
    pub(crate) worker_token: &'a str,
    pub(crate) expected_lease_holder: &'a str,
    pub(crate) max_active_workers: usize,
    pub(crate) protocol_version: i64,
    pub(crate) service_label: &'a str,
    pub(crate) binary_path: &'a Path,
    pub(crate) command_arguments: &'a str,
    pub(crate) path_env: &'a OsStr,
    pub(crate) codex_path: Option<&'a Path>,
    pub(crate) task_selection: &'a str,
    pub(crate) resume_session_id: Option<&'a str>,
    pub(crate) created_at: &'a str,
}

pub(super) struct AgentWorkerAbandonment<'a> {
    pub(crate) worker_token: &'a str,
    pub(crate) expected_state: &'a str,
    pub(crate) expected_worker_pid: Option<u32>,
    pub(crate) expected_heartbeat_at: Option<&'a str>,
    pub(crate) finished_at: &'a str,
    pub(crate) error: &'a str,
    pub(crate) permitted_successor_holder: Option<&'a str>,
}

pub(super) struct AgentWorkerFinalization<'a> {
    pub(crate) worker_token: &'a str,
    pub(crate) expected_worker_pid: Option<u32>,
    pub(crate) expected_lease_holder: &'a str,
    pub(crate) status: &'a str,
    pub(crate) finished_at: &'a str,
    pub(crate) exit_code: Option<i64>,
    pub(crate) log_dir: Option<&'a str>,
    pub(crate) stdout_path: Option<&'a str>,
    pub(crate) stderr_path: Option<&'a str>,
    pub(crate) summary: Option<&'a str>,
    pub(crate) codex_session_id: Option<&'a str>,
    pub(crate) error: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentWorkerRecord {
    pub(crate) worker_token: String,
    pub(crate) project_id: i64,
    pub(crate) project_name: String,
    pub(crate) project_path: PathBuf,
    pub(crate) state: String,
    pub(crate) protocol_version: i64,
    pub(crate) lease_holder: String,
    pub(crate) service_label: String,
    pub(crate) binary_path: PathBuf,
    pub(crate) command_arguments: String,
    pub(crate) path_env: OsString,
    pub(crate) codex_path: Option<PathBuf>,
    pub(crate) task_selection: String,
    pub(crate) resume_session_id: Option<String>,
    pub(crate) worker_pid: Option<u32>,
    pub(crate) created_at: String,
    pub(crate) started_at: Option<String>,
    pub(crate) heartbeat_at: Option<String>,
    pub(crate) finished_at: Option<String>,
    pub(crate) run_id: Option<i64>,
    pub(crate) error: Option<String>,
    pub(crate) service_cleaned_at: Option<String>,
}

pub(super) fn worker_lease_holder(worker_token: &str) -> String {
    format!("clt-worker-{worker_token}")
}

pub(super) struct AgentKnownSessionRegistration<'a> {
    pub(crate) project_id: i64,
    pub(crate) codex_session_id: &'a str,
    pub(crate) child_pid: u32,
    pub(crate) run_token: &'a str,
    pub(crate) stdout_path: &'a Path,
    pub(crate) stderr_path: &'a Path,
    pub(crate) lease_holder: &'a str,
    pub(crate) lease_timeout_seconds: u64,
    pub(crate) claim_requested_resume: bool,
}

pub(super) struct AgentLeaseRecord {
    pub(crate) project_id: i64,
    pub(crate) project_name: String,
    pub(crate) project_path: PathBuf,
    pub(crate) holder: String,
    pub(crate) acquired_at: String,
    pub(crate) expires_at: String,
}

pub(super) struct AgentRunRecord {
    pub(crate) id: i64,
    pub(crate) project_id: i64,
    pub(crate) project_name: String,
    pub(crate) project_path: PathBuf,
    pub(crate) status: String,
    pub(crate) started_at: String,
    pub(crate) finished_at: Option<String>,
    pub(crate) exit_code: Option<i64>,
    pub(crate) stdout_path: Option<String>,
    pub(crate) stderr_path: Option<String>,
    pub(crate) summary: Option<String>,
    pub(crate) codex_session_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentSessionControlRecord {
    pub(crate) project_id: i64,
    pub(crate) codex_session_id: String,
    pub(crate) state: AgentSessionControlState,
    pub(crate) child_pid: Option<u32>,
    pub(crate) run_token: Option<String>,
    pub(crate) interactive_holder: Option<String>,
    pub(crate) interactive_launch_token: Option<String>,
    pub(crate) stdout_path: Option<String>,
    pub(crate) stderr_path: Option<String>,
    pub(crate) updated_at: String,
}

#[derive(Clone, Debug)]
pub(super) struct AgentDaemonCheckin {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) holder: String,
    pub(crate) mode: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) started_at: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) checked_in_at: String,
    pub(crate) expires_at: String,
}

impl TursoAgentStore {
    pub(crate) fn open_blocking(state_dir: &Path) -> Result<Self> {
        let blocking = AgentStoreBlockingAdapter::new()?;
        let opened = blocking.block_on(Self::open_database(state_dir))?;
        Ok(Self {
            db_path: opened.db_path,
            repositories: AgentRepositories::new(&opened.db),
            pending_migration_version: opened.pending_migration_version,
            checkpoint_pin: Some(opened.checkpoint_pin),
            blocking,
        })
    }

    async fn open_database(state_dir: &Path) -> Result<OpenedAgentDatabase> {
        fs::create_dir_all(state_dir)
            .with_context(|| format!("Failed to create agent state directory {:?}", state_dir))?;
        let db_path = state_dir.join(AGENT_DB_FILE);
        let mut open_attempt = 0;
        let mut shared_wal_rebuild_requested = false;
        let db = loop {
            match Builder::new_local(db_path.to_string_lossy().as_ref())
                .experimental_multiprocess_wal(true)
                .build()
                .await
            {
                Ok(db) => break db,
                Err(error)
                    if error
                        .to_string()
                        .contains("shared WAL coordination map magic mismatch")
                        && open_attempt < AGENT_DATABASE_OPEN_RETRY_ATTEMPTS =>
                {
                    open_attempt += 1;
                    tokio::time::sleep(Duration::from_millis(AGENT_DATABASE_OPEN_RETRY_MILLIS))
                        .await;
                }
                Err(error) => {
                    if !shared_wal_rebuild_requested
                        && error_indicates_stale_shared_wal_index(&error)
                    {
                        match request_shared_wal_index_rebuild(&db_path) {
                            Ok(true) => {
                                shared_wal_rebuild_requested = true;
                                tokio::time::sleep(Duration::from_millis(
                                    AGENT_DATABASE_OPEN_RETRY_MILLIS,
                                ))
                                .await;
                                continue;
                            }
                            Ok(false) => {}
                            Err(rebuild_error) => {
                                return Err(error).with_context(|| {
                                    format!(
                                        "Failed to open agent database {:?}; also failed to request a shared WAL index rebuild: {rebuild_error:#}",
                                        db_path
                                    )
                                });
                            }
                        }
                    }
                    return Err(error)
                        .with_context(|| format!("Failed to open agent database {:?}", db_path));
                }
            }
        };
        let mut conn = db
            .connect()
            .with_context(|| format!("Failed to connect to agent database {:?}", db_path))?;
        configure_agent_connection(&conn).await?;

        let pending_migration_version = apply_migrations(&mut conn, AGENT_MIGRATIONS).await?;

        let checkpoint_pin = open_checkpoint_pin(&db, &db_path).await?;

        Ok(OpenedAgentDatabase {
            db_path,
            db,
            pending_migration_version,
            checkpoint_pin,
        })
    }

    pub(crate) fn pending_migration_version(&self) -> Option<i64> {
        self.pending_migration_version
    }

    #[cfg(test)]
    pub(crate) fn write_checkpoint_pressure_blocking(
        &self,
        project_id: i64,
        writes: usize,
    ) -> Result<()> {
        self.blocking.block_on(async {
            let conn = self.repositories.projects_models.connect().await?;
            for value in 0..writes {
                conn.execute(
                    "UPDATE projects SET failure_count = ?1 WHERE id = ?2",
                    params![value as i64, project_id],
                )
                .await
                .context("Failed to create agent database checkpoint pressure")?;
            }
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn open_blocking_with_test_migration(
        state_dir: &Path,
        version: i64,
        statement: &'static str,
    ) -> Result<Self> {
        let mut store = Self::open_blocking(state_dir)?;
        let pending_migration_version = store.blocking.block_on(async {
            let mut conn = store.repositories.projects_models.connect().await?;
            let statements = [statement];
            let migration = AgentMigration {
                version,
                statements: &statements,
            };
            apply_migrations(&mut conn, std::slice::from_ref(&migration)).await
        })?;
        store.pending_migration_version = pending_migration_version;
        Ok(store)
    }

    pub(crate) fn rebuild_active_worker_project_index_blocking(&self) -> Result<()> {
        self.blocking
            .block_on(self.rebuild_active_worker_project_index())
    }

    async fn rebuild_active_worker_project_index(&self) -> Result<()> {
        let mut conn = self.repositories.workers_leases.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!("Failed to begin rebuilding index {AGENT_WORKERS_ACTIVE_PROJECT_INDEX}")
            })?;
        transaction
            .execute(
                &format!("DROP INDEX IF EXISTS {AGENT_WORKERS_ACTIVE_PROJECT_INDEX}"),
                (),
            )
            .await
            .with_context(|| {
                format!("Failed to drop damaged index {AGENT_WORKERS_ACTIVE_PROJECT_INDEX}")
            })?;
        transaction
            .execute(AGENT_WORKERS_ACTIVE_PROJECT_INDEX_SQL, ())
            .await
            .with_context(|| {
                format!("Failed to recreate index {AGENT_WORKERS_ACTIVE_PROJECT_INDEX}")
            })?;
        transaction.commit().await.with_context(|| {
            format!("Failed to commit rebuilt index {AGENT_WORKERS_ACTIVE_PROJECT_INDEX}")
        })?;

        let mut rows = conn
            .query("PRAGMA integrity_check", ())
            .await
            .context("Failed to verify the agent database after rebuilding its worker index")?;
        let mut integrity_failures = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .context("Failed to read the agent database integrity check")?
        {
            let message = row_text(&row, 0, "integrity_check")?;
            if message != "ok" && integrity_failures.len() < 8 {
                integrity_failures.push(message);
            }
        }
        if !integrity_failures.is_empty() {
            anyhow::bail!(
                "Agent database integrity check still fails after rebuilding {AGENT_WORKERS_ACTIVE_PROJECT_INDEX}: {}",
                integrity_failures.join("; ")
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn active_worker_project_index_exists_blocking(&self) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.workers_leases.connect().await?;
            Ok(query_count(
                &conn,
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name = ?1",
                [AGENT_WORKERS_ACTIVE_PROJECT_INDEX],
            )
            .await?
                == 1)
        })
    }

    #[cfg(test)]
    pub(crate) fn drop_active_worker_project_index_blocking(&self) -> Result<()> {
        self.blocking.block_on(async {
            let conn = self.repositories.workers_leases.connect().await?;
            conn.execute(
                &format!("DROP INDEX {AGENT_WORKERS_ACTIVE_PROJECT_INDEX}"),
                (),
            )
            .await
            .context("Failed to drop the active-worker project index for a test")?;
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn db_path(&self) -> &Path {
        &self.db_path
    }

    #[cfg(test)]
    pub(crate) fn worker_schema_migration_deferred_blocking(
        &self,
        migration_version: i64,
    ) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.workers_leases.connect().await?;
            worker_schema_migration_is_deferred(&conn, migration_version).await
        })
    }

    #[cfg(test)]
    pub(crate) fn table_exists_blocking(&self, table_name: &str) -> Result<bool> {
        self.blocking.block_on(self.table_exists(table_name))
    }

    #[cfg(test)]
    async fn table_exists(&self, table_name: &str) -> Result<bool> {
        let conn = self.repositories.projects_models.connect().await?;
        let count = query_count(
            &conn,
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table_name],
        )
        .await?;

        Ok(count == 1)
    }

    #[cfg(test)]
    pub(crate) fn runs_has_task_content_column_blocking(&self) -> Result<bool> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let count = query_count(
                &conn,
                "SELECT COUNT(*)
                       FROM pragma_table_info('runs')
                      WHERE name = 'task_content'",
                (),
            )
            .await?;
            Ok(count == 1)
        })
    }

    #[cfg(test)]
    pub(crate) fn latest_codex_session_id_blocking(&self) -> Result<Option<String>> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            let mut rows = conn
                .query(
                    "SELECT codex_session_id FROM runs ORDER BY id DESC LIMIT 1",
                    (),
                )
                .await
                .context("Failed to read the latest Codex session ID")?;
            let Some(row) = rows
                .next()
                .await
                .context("Failed to read the latest Codex session ID row")?
            else {
                return Ok(None);
            };
            row_optional_text(&row, 0, "codex_session_id")
        })
    }

    #[cfg(test)]
    pub(crate) fn run_count_blocking(&self) -> Result<i64> {
        self.blocking.block_on(async {
            let conn = self.repositories.sessions_runs.connect().await?;
            query_count(&conn, "SELECT COUNT(*) FROM runs", ()).await
        })
    }

    #[cfg(test)]
    pub(crate) fn lease_count_blocking(&self) -> Result<i64> {
        self.blocking.block_on(async {
            let conn = self.repositories.workers_leases.connect().await?;
            query_count(&conn, "SELECT COUNT(*) FROM leases", ()).await
        })
    }
}

impl Drop for TursoAgentStore {
    fn drop(&mut self) {
        let Some(checkpoint_pin) = self.checkpoint_pin.take() else {
            return;
        };
        // Drop may run from inside a Tokio runtime. Roll the pin back on a
        // short-lived helper thread through the store-owned runtime so we can
        // drive Turso's async state machine without nesting runtimes or leaving
        // a shared read mark.
        let runtime = self.blocking.handle();
        let rollback = thread::Builder::new()
            .name("clt-agent-wal-pin-release".to_string())
            .spawn(move || {
                runtime
                    .block_on(checkpoint_pin.execute("ROLLBACK", ()))
                    .ok()
            });
        if let Ok(handle) = rollback {
            let _ = handle.join();
        }
    }
}

async fn open_checkpoint_pin(db: &Database, db_path: &Path) -> Result<Connection> {
    let pin = db.connect().with_context(|| {
        format!(
            "Failed to connect the agent database checkpoint pin {:?}",
            db_path
        )
    })?;
    configure_agent_connection(&pin).await?;
    pin.execute("BEGIN DEFERRED", ())
        .await
        .context("Failed to begin the agent database checkpoint pin")?;
    let mut rows = pin
        .query("SELECT version FROM schema_migrations LIMIT 1", ())
        .await
        .context("Failed to establish the agent database checkpoint pin")?;
    rows.next()
        .await
        .context("Failed to read the agent database checkpoint pin")?
        .ok_or_else(|| anyhow::anyhow!("Agent database has no applied migrations"))?;
    drop(rows);
    Ok(pin)
}

async fn configure_agent_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(AGENT_DB_BUSY_TIMEOUT)
        .context("Failed to configure agent database busy timeout")?;
    conn.pragma_update("foreign_keys", "ON")
        .await
        .context("Failed to enable agent database foreign keys")?;
    Ok(())
}

async fn update_project_after_run(conn: &Connection, outcome: &AgentRunOutcome<'_>) -> Result<()> {
    let finished_at = outcome.finished_at.unwrap_or(outcome.started_at);

    match outcome.status {
        "success" | "idle" => {
            conn.execute(
                "UPDATE projects
                 SET last_run_at = ?1,
                     last_success_at = ?1,
                     last_failure_at = NULL,
                     last_blocked_recovery_at = NULL,
                     failure_count = 0,
                     updated_at = ?1
                 WHERE id = ?2",
                params![finished_at, outcome.project_id],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to update project {} after successful run",
                    outcome.project_id
                )
            })?;
        }
        "blocked" => {
            conn.execute(
                "UPDATE projects
                 SET last_run_at = ?1,
                     last_blocked_recovery_at = ?1,
                     updated_at = ?1
                 WHERE id = ?2",
                params![finished_at, outcome.project_id],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to update project {} after blocked-task recovery",
                    outcome.project_id
                )
            })?;
        }
        "failure" | "timeout" => {
            conn.execute(
                "UPDATE projects
                 SET last_run_at = ?1,
                     last_failure_at = ?1,
                     failure_count = failure_count + 1,
                     updated_at = ?1
                 WHERE id = ?2",
                params![finished_at, outcome.project_id],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to update project {} after failed run",
                    outcome.project_id
                )
            })?;
        }
        _ => {
            conn.execute(
                "UPDATE projects
                 SET last_run_at = ?1, updated_at = ?1
                 WHERE id = ?2",
                params![finished_at, outcome.project_id],
            )
            .await
            .with_context(|| {
                format!("Failed to update project {} after run", outcome.project_id)
            })?;
        }
    }

    Ok(())
}

async fn apply_migrations(
    conn: &mut Connection,
    migrations: &[AgentMigration<'_>],
) -> Result<Option<i64>> {
    conn.execute("PRAGMA foreign_keys = ON", ())
        .await
        .context("Failed to enable agent database foreign keys")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        )",
        (),
    )
    .await
    .context("Failed to initialize agent schema migrations table")?;

    for migration in migrations {
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!(
                    "Failed to begin agent migration {} transaction",
                    migration.version
                )
            })?;
        if migration_applied(&transaction, migration.version).await? {
            transaction.commit().await.with_context(|| {
                format!(
                    "Failed to finish already-applied agent migration {}",
                    migration.version
                )
            })?;
            continue;
        }

        if worker_schema_migration_is_deferred(&transaction, migration.version).await? {
            transaction.commit().await.with_context(|| {
                format!(
                    "Failed to defer agent migration {} while workers are active",
                    migration.version
                )
            })?;
            return Ok(Some(migration.version));
        }

        for statement in migration.statements {
            transaction.execute(statement, ()).await.with_context(|| {
                format!("Failed to apply agent migration {}", migration.version)
            })?;
        }

        transaction
            .execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, datetime('now'))",
                [migration.version],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to record applied agent migration {}",
                    migration.version
                )
            })?;
        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit agent migration {}", migration.version))?;
    }

    Ok(None)
}

async fn worker_schema_migration_is_deferred(
    conn: &Connection,
    migration_version: i64,
) -> Result<bool> {
    if migration_version <= AGENT_WORKER_SHARED_SCHEMA_VERSION {
        return Ok(false);
    }
    let worker_table_exists = query_count(
        conn,
        "SELECT COUNT(*) FROM sqlite_schema
          WHERE type = 'table' AND name = 'agent_workers'",
        (),
    )
    .await?
        > 0;
    Ok(worker_table_exists
        && query_count(
            conn,
            "SELECT COUNT(*) FROM agent_workers
              WHERE state IN ('dispatching', 'running', 'finalizing')",
            (),
        )
        .await?
            > 0)
}

async fn migration_applied(conn: &Connection, version: i64) -> Result<bool> {
    let count = query_count(
        conn,
        "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
        [version],
    )
    .await?;

    Ok(count > 0)
}

async fn query_count<P>(conn: &Connection, sql: &str, params: P) -> Result<i64>
where
    P: turso::IntoParams,
{
    let mut rows = conn
        .query(sql, params)
        .await
        .with_context(|| format!("Failed to query agent database: {}", sql))?;
    let row = rows
        .next()
        .await
        .with_context(|| format!("Failed to read agent database query result: {}", sql))?
        .ok_or_else(|| anyhow::anyhow!("Agent database query returned no rows: {}", sql))?;
    let value = row
        .get_value(0)
        .with_context(|| format!("Failed to read agent database count: {}", sql))?;
    let count = value.as_integer().copied().ok_or_else(|| {
        anyhow::anyhow!("Agent database count was not an integer for query: {}", sql)
    })?;

    Ok(count)
}

fn row_text(row: &turso::Row, idx: usize, column: &str) -> Result<String> {
    row.get_value(idx)
        .with_context(|| format!("Failed to read agent database column {}", column))?
        .as_text()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Agent database column {} was not text", column))
}

fn row_optional_text(row: &turso::Row, idx: usize, column: &str) -> Result<Option<String>> {
    let value = row
        .get_value(idx)
        .with_context(|| format!("Failed to read agent database column {}", column))?;
    match value {
        Value::Null => Ok(None),
        Value::Text(text) => Ok(Some(text)),
        _ => anyhow::bail!("Agent database column {} was not nullable text", column),
    }
}

fn row_integer(row: &turso::Row, idx: usize, column: &str) -> Result<i64> {
    row.get_value(idx)
        .with_context(|| format!("Failed to read agent database column {}", column))?
        .as_integer()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("Agent database column {} was not an integer", column))
}

fn row_optional_integer(row: &turso::Row, idx: usize, column: &str) -> Result<Option<i64>> {
    let value = row
        .get_value(idx)
        .with_context(|| format!("Failed to read agent database column {}", column))?;
    match value {
        Value::Null => Ok(None),
        Value::Integer(value) => Ok(Some(value)),
        _ => anyhow::bail!("Agent database column {} was not nullable integer", column),
    }
}

fn git_finalization_record_from_row(row: &turso::Row) -> Result<GitFinalizationRecord> {
    Ok(GitFinalizationRecord {
        project_id: row_integer(row, 0, "project_id")?,
        codex_session_id: row_text(row, 1, "codex_session_id")?,
        state: GitFinalizationState::from_database(&row_text(row, 2, "state")?)?,
        git_mode: AgentGitMode::from_database(&row_text(row, 3, "git_mode")?)?,
        starting_head: row_optional_text(row, 4, "starting_head")?,
        branch_ref: row_optional_text(row, 5, "branch_ref")?,
        upstream_ref: row_optional_text(row, 6, "upstream_ref")?,
        worktree_baseline: row_text(row, 7, "worktree_baseline")?,
        task_identity: row_optional_text(row, 8, "task_identity")?,
        owner_run_token: row_optional_text(row, 9, "owner_run_token")?,
        commit_oid: row_optional_text(row, 10, "commit_oid")?,
        generation: row_integer(row, 11, "generation")?,
        last_error: row_optional_text(row, 12, "last_error")?,
        created_at: row_text(row, 13, "created_at")?,
        updated_at: row_text(row, 14, "updated_at")?,
        completed_at: row_optional_text(row, 15, "completed_at")?,
        acknowledged_at: row_optional_text(row, 16, "acknowledged_at")?,
        acknowledged_run_id: row_optional_integer(row, 17, "acknowledged_run_id")?,
    })
}
