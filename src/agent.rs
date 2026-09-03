use super::*;
use turso::transaction::TransactionBehavior;
use turso::{Builder, Connection, Database, Value, params};

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
pub(crate) enum GitFinalizationState {
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

pub(crate) fn shared_wal_path(db_path: &Path) -> PathBuf {
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
pub(crate) fn request_shared_wal_index_rebuild(db_path: &Path) -> Result<bool> {
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

pub(crate) struct TursoAgentStore {
    #[cfg_attr(not(test), allow(dead_code))]
    db_path: PathBuf,
    db: Database,
    pending_migration_version: Option<i64>,
    checkpoint_pin: Option<Connection>,
}

#[derive(Clone, Debug)]
pub(crate) struct AgentProject {
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
pub(crate) struct AgentModelProvider {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) base_url: Option<String>,
    pub(crate) env_key: Option<String>,
    pub(crate) built_in: bool,
    pub(crate) enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentModelTarget {
    pub(crate) provider_id: String,
    pub(crate) model_id: String,
    pub(crate) label: String,
    pub(crate) enabled: bool,
    pub(crate) favorite: bool,
    pub(crate) reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AgentModelDefaults {
    pub(crate) provider_id: Option<String>,
    pub(crate) model_id: Option<String>,
}

pub(crate) struct AgentRunOutcome<'a> {
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

pub(crate) struct NewGitFinalization<'a> {
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
pub(crate) struct GitFinalizationRecord {
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

pub(crate) struct AgentWorkerReservation<'a> {
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

pub(crate) struct AgentWorkerAbandonment<'a> {
    pub(crate) worker_token: &'a str,
    pub(crate) expected_state: &'a str,
    pub(crate) expected_worker_pid: Option<u32>,
    pub(crate) expected_heartbeat_at: Option<&'a str>,
    pub(crate) finished_at: &'a str,
    pub(crate) error: &'a str,
    pub(crate) permitted_successor_holder: Option<&'a str>,
}

pub(crate) struct AgentWorkerFinalization<'a> {
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
pub(crate) struct AgentWorkerRecord {
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

pub(crate) fn worker_lease_holder(worker_token: &str) -> String {
    format!("clt-worker-{worker_token}")
}

pub(crate) struct AgentKnownSessionRegistration<'a> {
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

pub(crate) struct AgentLeaseRecord {
    pub(crate) project_id: i64,
    pub(crate) project_name: String,
    pub(crate) project_path: PathBuf,
    pub(crate) holder: String,
    pub(crate) acquired_at: String,
    pub(crate) expires_at: String,
}

pub(crate) struct AgentRunRecord {
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
pub(crate) struct AgentSessionControlRecord {
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
pub(crate) struct AgentDaemonCheckin {
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
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(Self::open(state_dir))
    }

    pub(crate) async fn open(state_dir: &Path) -> Result<Self> {
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

        Ok(Self {
            db_path,
            db,
            pending_migration_version,
            checkpoint_pin: Some(checkpoint_pin),
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
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
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
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut store = Self::open(state_dir).await?;
                let mut conn = store.connect().await?;
                let statements = [statement];
                let migration = AgentMigration {
                    version,
                    statements: &statements,
                };
                store.pending_migration_version =
                    apply_migrations(&mut conn, std::slice::from_ref(&migration)).await?;
                Ok(store)
            })
    }

    pub(crate) fn rebuild_active_worker_project_index_blocking(&self) -> Result<()> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.rebuild_active_worker_project_index())
    }

    async fn rebuild_active_worker_project_index(&self) -> Result<()> {
        let mut conn = self.connect().await?;
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
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
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
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                conn.execute(
                    &format!("DROP INDEX {AGENT_WORKERS_ACTIVE_PROJECT_INDEX}"),
                    (),
                )
                .await
                .context("Failed to drop the active-worker project index for a test")?;
                Ok(())
            })
    }

    async fn connect(&self) -> Result<Connection> {
        let conn = self
            .db
            .connect()
            .context("Failed to connect to agent database")?;
        configure_agent_connection(&conn).await?;
        Ok(conn)
    }

    pub(crate) fn register_project_blocking(
        &self,
        project_root: &Path,
        name: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.register_project(project_root, name))
    }

    async fn register_project(&self, project_root: &Path, name: &str) -> Result<bool> {
        let conn = self.connect().await?;
        let path = project_root.display().to_string();
        let exists = query_count(
            &conn,
            "SELECT COUNT(*) FROM projects WHERE path = ?1",
            [path.as_str()],
        )
        .await?
            > 0;

        if exists {
            conn.execute(
                "UPDATE projects
                 SET name = ?1, enabled = 1, updated_at = datetime('now')
                 WHERE path = ?2",
                params![name, path.as_str()],
            )
            .await
            .with_context(|| format!("Failed to update registered project {}", path))?;
        } else {
            conn.execute(
                "INSERT INTO projects (path, name, registered_at, updated_at)
                 VALUES (?1, ?2, datetime('now'), datetime('now'))",
                params![path.as_str(), name],
            )
            .await
            .with_context(|| format!("Failed to register project {}", path))?;
        }

        Ok(!exists)
    }

    pub(crate) fn unregister_project_blocking(&self, project_root: &Path) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.unregister_project(project_root))
    }

    async fn unregister_project(&self, project_root: &Path) -> Result<bool> {
        let mut conn = self.connect().await?;
        let path = project_root.display().to_string();
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin unregistering project {}", path))?;
        let active_workers = query_count(
            &transaction,
            "SELECT COUNT(*)
               FROM agent_workers
              WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                AND state IN ('dispatching', 'running', 'finalizing')",
            [path.as_str()],
        )
        .await?;
        if active_workers > 0 {
            anyhow::bail!(
                "Cannot unregister project {path} while {active_workers} independent worker(s) are active"
            );
        }
        let lease = {
            let mut rows = transaction
                .query(
                    "SELECT holder, expires_at
                       FROM leases
                      WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
                    [path.as_str()],
                )
                .await
                .with_context(|| format!("Failed to read agent lease for project {path}"))?;
            match rows
                .next()
                .await
                .context("Failed to read agent lease row while unregistering project")?
            {
                Some(row) => Some((
                    row_text(&row, 0, "holder")?,
                    row_text(&row, 1, "expires_at")?,
                )),
                None => None,
            }
        };
        if let Some((holder, expires_at)) = lease {
            let reclaimable = expires_at
                .parse::<u64>()
                .is_ok_and(|expires_at| expires_at <= agent_timestamp_seconds())
                || agent_lease_holder_liveness(&holder) == AgentLeaseHolderLiveness::Dead;
            if !reclaimable {
                anyhow::bail!("Cannot unregister project {path} while its agent lease is active");
            }
            transaction
                .execute(
                    "DELETE FROM leases
                      WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                        AND holder = ?2",
                    params![path.as_str(), holder],
                )
                .await
                .with_context(|| {
                    format!("Failed to reclaim stale agent lease for project {path}")
                })?;
        }
        let pending_git_finalizations = query_count(
            &transaction,
            "SELECT COUNT(*) FROM git_finalizations
              WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                AND state IN ('working', 'tracking', 'commit_pending', 'push_pending')",
            [path.as_str()],
        )
        .await?;
        if pending_git_finalizations > 0 {
            anyhow::bail!(
                "Cannot unregister project {path} while {pending_git_finalizations} Git finalization(s) are nonterminal"
            );
        }
        let unconsumed_git_launches = query_count(
            &transaction,
            "SELECT COUNT(*) FROM agent_git_launch_states
              WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
            [path.as_str()],
        )
        .await?;
        if unconsumed_git_launches > 0 {
            anyhow::bail!(
                "Cannot unregister project {path} while {unconsumed_git_launches} Git launch boundary record(s) remain unconsumed"
            );
        }
        transaction
            .execute(
                "DELETE FROM agent_workers
                 WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
                [path.as_str()],
            )
            .await
            .with_context(|| format!("Failed to remove worker history for project {path}"))?;
        transaction
            .execute(
                "DELETE FROM git_finalizations
                 WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
                [path.as_str()],
            )
            .await
            .with_context(|| {
                format!("Failed to remove Git finalization history for project {path}")
            })?;
        transaction
            .execute(
                "DELETE FROM runs
                 WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
                [path.as_str()],
            )
            .await
            .with_context(|| format!("Failed to remove run history for project {}", path))?;
        let removed = transaction
            .execute("DELETE FROM projects WHERE path = ?1", [path.as_str()])
            .await
            .with_context(|| format!("Failed to unregister project {}", path))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit unregistering project {}", path))?;

        Ok(removed > 0)
    }

    pub(crate) fn list_projects_blocking(&self) -> Result<Vec<AgentProject>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.list_projects())
    }

    async fn list_projects(&self) -> Result<Vec<AgentProject>> {
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT id, path, name, enabled, git_mode, codex_provider, codex_model,
                        codex_reasoning_effort, codex_fast_enabled, last_scan_at,
                        last_daemon_scan_status, last_daemon_scan_error, last_run_at,
                        last_success_at, last_failure_at, last_blocked_recovery_at, failure_count
                 FROM projects
                 ORDER BY name COLLATE NOCASE, path COLLATE NOCASE",
                (),
            )
            .await
            .context("Failed to list registered projects")?;
        let mut projects = Vec::new();

        while let Some(row) = rows
            .next()
            .await
            .context("Failed to read registered project row")?
        {
            let id = row_integer(&row, 0, "id")?;
            let path = PathBuf::from(row_text(&row, 1, "path")?);
            let name = row_text(&row, 2, "name")?;
            let enabled = row_integer(&row, 3, "enabled")? != 0;
            let git_mode = AgentGitMode::from_database(&row_text(&row, 4, "git_mode")?)?;
            let codex_provider = row_optional_text(&row, 5, "codex_provider")?;
            let codex_model = row_optional_text(&row, 6, "codex_model")?;
            let codex_reasoning_effort = row_optional_text(&row, 7, "codex_reasoning_effort")?;
            let codex_fast_enabled = row_integer(&row, 8, "codex_fast_enabled")? != 0;
            let last_scan_at = row_optional_text(&row, 9, "last_scan_at")?;
            let last_daemon_scan_status = row_optional_text(&row, 10, "last_daemon_scan_status")?;
            let last_daemon_scan_error = row_optional_text(&row, 11, "last_daemon_scan_error")?;
            let last_run_at = row_optional_text(&row, 12, "last_run_at")?;
            let last_success_at = row_optional_text(&row, 13, "last_success_at")?;
            let last_failure_at = row_optional_text(&row, 14, "last_failure_at")?;
            let last_blocked_recovery_at = row_optional_text(&row, 15, "last_blocked_recovery_at")?;
            let failure_count = row_integer(&row, 16, "failure_count")?;

            projects.push(AgentProject {
                id,
                path,
                name,
                enabled,
                git_mode,
                codex_provider,
                codex_model,
                codex_reasoning_effort,
                codex_fast_enabled,
                last_scan_at,
                last_daemon_scan_status,
                last_daemon_scan_error,
                last_run_at,
                last_success_at,
                last_failure_at,
                last_blocked_recovery_at,
                failure_count,
            });
        }

        Ok(projects)
    }

    pub(crate) fn record_git_launch_state_blocking(
        &self,
        project_id: i64,
        run_token: &str,
        git_mode: AgentGitMode,
        start: &AgentGitStartState,
        created_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                if git_mode == AgentGitMode::Off {
                    anyhow::bail!("A Git launch state cannot use Git mode off");
                }
                let mut conn = self.connect().await?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .context("Failed to begin recording the prelaunch Git state")?;
                if query_count(
                    &transaction,
                    "SELECT COUNT(*) FROM agent_git_launch_states
                      WHERE project_id = ?1 AND run_token <> ?2",
                    params![project_id, run_token],
                )
                .await?
                    != 0
                {
                    anyhow::bail!(
                        "A prior automated run has an unconsumed Git launch boundary for project {project_id}; refusing to replace it"
                    );
                }
                let inserted = transaction
                    .execute(
                        "INSERT OR IGNORE INTO agent_git_launch_states (
                            project_id, run_token, git_mode, starting_head, branch_ref,
                            upstream_ref, worktree_baseline, created_at
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            project_id,
                            run_token,
                            git_mode.database_value(),
                            start.starting_head.as_str(),
                            start.branch_ref.as_deref(),
                            start.upstream_ref.as_deref(),
                            start.worktree_baseline.as_str(),
                            created_at,
                        ],
                    )
                    .await
                    .context("Failed to persist the prelaunch Git state")?;
                if inserted == 0
                    && query_count(
                        &transaction,
                        "SELECT COUNT(*) FROM agent_git_launch_states
                          WHERE project_id = ?1 AND run_token = ?2
                            AND git_mode = ?3 AND starting_head = ?4
                            AND branch_ref IS ?5 AND upstream_ref IS ?6
                            AND worktree_baseline = ?7",
                        params![
                            project_id,
                            run_token,
                            git_mode.database_value(),
                            start.starting_head.as_str(),
                            start.branch_ref.as_deref(),
                            start.upstream_ref.as_deref(),
                            start.worktree_baseline.as_str(),
                        ],
                    )
                    .await?
                        != 1
                {
                    anyhow::bail!(
                        "Automated run {run_token} already has a different immutable Git launch boundary"
                    );
                }
                transaction
                    .commit()
                    .await
                    .context("Failed to commit the prelaunch Git state")?;
                Ok(inserted == 1)
            })
    }

    pub(crate) fn has_other_git_launch_state_blocking(
        &self,
        project_id: i64,
        run_token: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                Ok(query_count(
                    &conn,
                    "SELECT COUNT(*) FROM agent_git_launch_states
                      WHERE project_id = ?1 AND run_token <> ?2",
                    params![project_id, run_token],
                )
                .await?
                    != 0)
            })
    }

    pub(crate) fn git_launch_state_for_project_blocking(
        &self,
        project_id: i64,
    ) -> Result<Option<(String, AgentGitMode, AgentGitStartState)>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT run_token, git_mode, starting_head, branch_ref,
                                upstream_ref, worktree_baseline
                           FROM agent_git_launch_states
                          WHERE project_id = ?1
                          ORDER BY created_at, run_token",
                        [project_id],
                    )
                    .await
                    .context("Failed to read project Git launch states")?;
                let Some(row) = rows
                    .next()
                    .await
                    .context("Failed to read project Git launch-state row")?
                else {
                    return Ok(None);
                };
                let launch = (
                    row_text(&row, 0, "run_token")?,
                    AgentGitMode::from_database(&row_text(&row, 1, "git_mode")?)?,
                    AgentGitStartState {
                        starting_head: row_text(&row, 2, "starting_head")?,
                        branch_ref: row_optional_text(&row, 3, "branch_ref")?,
                        upstream_ref: row_optional_text(&row, 4, "upstream_ref")?,
                        worktree_baseline: row_text(&row, 5, "worktree_baseline")?,
                    },
                );
                if rows
                    .next()
                    .await
                    .context("Failed to check for duplicate project Git launch states")?
                    .is_some()
                {
                    anyhow::bail!(
                        "Project {project_id} has more than one unconsumed Git launch boundary"
                    );
                }
                Ok(Some(launch))
            })
    }

    pub(crate) fn reclaim_unchanged_git_launch_state_blocking(
        &self,
        project_id: i64,
        run_token: &str,
        git_mode: AgentGitMode,
        start: &AgentGitStartState,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .context("Failed to begin reclaiming an unchanged Git launch state")?;
                let terminal_worker = query_count(
                    &transaction,
                    "SELECT COUNT(*) FROM agent_workers
                      WHERE worker_token = ?1 AND project_id = ?2
                        AND state IN ('completed', 'abandoned', 'superseded')",
                    params![run_token, project_id],
                )
                .await?
                    == 1;
                let any_session = query_count(
                    &transaction,
                    "SELECT COUNT(*) FROM session_controls
                      WHERE project_id = ?1 AND run_token = ?2",
                    params![project_id, run_token],
                )
                .await?
                    != 0;
                if !terminal_worker || any_session {
                    transaction
                        .commit()
                        .await
                        .context("Failed to finish checking an unreclaimable Git launch state")?;
                    return Ok(false);
                }
                let deleted = transaction
                    .execute(
                        "DELETE FROM agent_git_launch_states
                          WHERE project_id = ?1 AND run_token = ?2
                            AND git_mode = ?3 AND starting_head = ?4
                            AND branch_ref IS ?5 AND upstream_ref IS ?6
                            AND worktree_baseline = ?7",
                        params![
                            project_id,
                            run_token,
                            git_mode.database_value(),
                            start.starting_head.as_str(),
                            start.branch_ref.as_deref(),
                            start.upstream_ref.as_deref(),
                            start.worktree_baseline.as_str(),
                        ],
                    )
                    .await
                    .context("Failed to delete the proven-unchanged Git launch state")?;
                transaction
                    .commit()
                    .await
                    .context("Failed to commit Git launch-state reclamation")?;
                Ok(deleted == 1)
            })
    }

    pub(crate) fn git_launch_state_blocking(
        &self,
        project_id: i64,
        run_token: &str,
    ) -> Result<Option<(AgentGitMode, AgentGitStartState)>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT git_mode, starting_head, branch_ref, upstream_ref,
                                worktree_baseline
                           FROM agent_git_launch_states
                          WHERE project_id = ?1 AND run_token = ?2",
                        params![project_id, run_token],
                    )
                    .await
                    .context("Failed to read the prelaunch Git state")?;
                let Some(row) = rows
                    .next()
                    .await
                    .context("Failed to read the prelaunch Git state row")?
                else {
                    return Ok(None);
                };
                Ok(Some((
                    AgentGitMode::from_database(&row_text(&row, 0, "git_mode")?)?,
                    AgentGitStartState {
                        starting_head: row_text(&row, 1, "starting_head")?,
                        branch_ref: row_optional_text(&row, 2, "branch_ref")?,
                        upstream_ref: row_optional_text(&row, 3, "upstream_ref")?,
                        worktree_baseline: row_text(&row, 4, "worktree_baseline")?,
                    },
                )))
            })
    }

    #[cfg_attr(unix, allow(dead_code))]
    pub(crate) fn delete_git_launch_state_blocking(
        &self,
        project_id: i64,
        run_token: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                Ok(conn
                    .execute(
                        "DELETE FROM agent_git_launch_states
                          WHERE project_id = ?1 AND run_token = ?2",
                        params![project_id, run_token],
                    )
                    .await
                    .context("Failed to delete the prelaunch Git state")?
                    == 1)
            })
    }

    pub(crate) fn create_git_finalization_blocking(
        &self,
        finalization: NewGitFinalization<'_>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.create_git_finalization(finalization))
    }

    async fn create_git_finalization(&self, finalization: NewGitFinalization<'_>) -> Result<bool> {
        if finalization.codex_session_id.is_empty() {
            anyhow::bail!("Git finalization requires a Codex session ID");
        }
        if finalization.git_mode == AgentGitMode::Off {
            anyhow::bail!("Git finalization cannot be created when Git automation is off");
        }
        let mut conn = self.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!(
                    "Failed to begin creating Git finalization for project {} and Codex session {}",
                    finalization.project_id, finalization.codex_session_id
                )
            })?;
        let inserted = if let Some(owner_run_token) = finalization.owner_run_token {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO git_finalizations (
                        project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation,
                        last_error, created_at, updated_at, completed_at
                     ) SELECT ?1, ?2, 'working', ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, 0,
                              NULL, ?10, ?10, NULL
                       WHERE EXISTS (
                           SELECT 1 FROM session_controls
                            WHERE project_id = ?1 AND codex_session_id = ?2
                              AND state = 'running' AND run_token = ?9
                       )",
                    params![
                        finalization.project_id,
                        finalization.codex_session_id,
                        finalization.git_mode.database_value(),
                        finalization.starting_head,
                        finalization.branch_ref,
                        finalization.upstream_ref,
                        finalization.worktree_baseline,
                        finalization.task_identity,
                        owner_run_token,
                        finalization.created_at,
                    ],
                )
                .await
        } else {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO git_finalizations (
                        project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation,
                        last_error, created_at, updated_at, completed_at
                     ) VALUES (?1, ?2, 'working', ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, 0,
                               NULL, ?9, ?9, NULL)",
                    params![
                        finalization.project_id,
                        finalization.codex_session_id,
                        finalization.git_mode.database_value(),
                        finalization.starting_head,
                        finalization.branch_ref,
                        finalization.upstream_ref,
                        finalization.worktree_baseline,
                        finalization.task_identity,
                        finalization.created_at,
                    ],
                )
                .await
        }
        .with_context(|| {
            format!(
                "Failed to create Git finalization for project {} and Codex session {}",
                finalization.project_id, finalization.codex_session_id
            )
        })?;
        transaction.commit().await.with_context(|| {
            format!(
                "Failed to commit Git finalization creation for project {} and Codex session {}",
                finalization.project_id, finalization.codex_session_id
            )
        })?;
        Ok(inserted == 1)
    }

    pub(crate) fn git_finalization_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<GitFinalizationRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.git_finalization(project_id, codex_session_id))
    }

    async fn git_finalization(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<GitFinalizationRecord>> {
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation, last_error, created_at,
                        updated_at, completed_at, acknowledged_at, acknowledged_run_id
                   FROM git_finalizations
                  WHERE project_id = ?1 AND codex_session_id = ?2",
                params![project_id, codex_session_id],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to read Git finalization for project {project_id} and Codex session {codex_session_id}"
                )
            })?;
        rows.next()
            .await
            .context("Failed to read Git finalization row")?
            .map(|row| git_finalization_record_from_row(&row))
            .transpose()
    }

    pub(crate) fn list_pending_git_finalizations_blocking(
        &self,
        project_id: Option<i64>,
    ) -> Result<Vec<GitFinalizationRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.list_pending_git_finalizations(project_id))
    }

    async fn list_pending_git_finalizations(
        &self,
        project_id: Option<i64>,
    ) -> Result<Vec<GitFinalizationRecord>> {
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation, last_error, created_at,
                        updated_at, completed_at, acknowledged_at, acknowledged_run_id
                   FROM git_finalizations
                  WHERE state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                    AND (?1 IS NULL OR project_id = ?1)
                  ORDER BY CAST(updated_at AS INTEGER), project_id, codex_session_id",
                params![project_id],
            )
            .await
            .context("Failed to list pending Git finalizations")?;
        let mut finalizations = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .context("Failed to read pending Git finalization row")?
        {
            finalizations.push(git_finalization_record_from_row(&row)?);
        }
        Ok(finalizations)
    }

    pub(crate) fn list_unacknowledged_completed_git_finalizations_blocking(
        &self,
        project_id: Option<i64>,
    ) -> Result<Vec<GitFinalizationRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.list_unacknowledged_completed_git_finalizations(project_id))
    }

    async fn list_unacknowledged_completed_git_finalizations(
        &self,
        project_id: Option<i64>,
    ) -> Result<Vec<GitFinalizationRecord>> {
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation, last_error, created_at,
                        updated_at, completed_at, acknowledged_at, acknowledged_run_id
                   FROM git_finalizations
                  WHERE state = 'completed' AND acknowledged_at IS NULL
                    AND (?1 IS NULL OR project_id = ?1)
                  ORDER BY CAST(completed_at AS INTEGER), project_id, codex_session_id",
                params![project_id],
            )
            .await
            .context("Failed to list unacknowledged completed Git finalizations")?;
        let mut finalizations = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .context("Failed to read an unacknowledged Git finalization row")?
        {
            finalizations.push(git_finalization_record_from_row(&row)?);
        }
        Ok(finalizations)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compare_and_set_git_finalization_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        next_state: GitFinalizationState,
        owner_run_token: Option<&str>,
        commit_oid: Option<&str>,
        last_error: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                next_state,
                None,
                None,
                false,
                owner_run_token,
                commit_oid,
                last_error,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compare_and_set_owned_git_finalization_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        next_state: GitFinalizationState,
        owner_run_token: &str,
        commit_oid: Option<&str>,
        last_error: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                next_state,
                None,
                None,
                true,
                Some(owner_run_token),
                commit_oid,
                last_error,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compare_and_set_git_finalization_with_identity_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        next_state: GitFinalizationState,
        task_identity: &str,
        owner_run_token: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                next_state,
                Some(task_identity),
                None,
                true,
                owner_run_token,
                None,
                None,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn track_git_finalization_with_manifest_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        task_identity: &str,
        worktree_baseline: &str,
        owner_run_token: &str,
        updated_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                GitFinalizationState::Tracking,
                Some(task_identity),
                Some(worktree_baseline),
                true,
                Some(owner_run_token),
                None,
                None,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reseal_git_finalization_manifest_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        task_identity: &str,
        worktree_baseline: &str,
        owner_run_token: &str,
        updated_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                GitFinalizationState::CommitPending,
                Some(task_identity),
                Some(worktree_baseline),
                true,
                Some(owner_run_token),
                None,
                None,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn recover_git_finalization_intent_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        task_identity: &str,
        owner_run_token: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.compare_and_set_git_finalization(
                project_id,
                codex_session_id,
                expected_generation,
                GitFinalizationState::Tracking,
                Some(task_identity),
                None,
                false,
                owner_run_token,
                None,
                None,
                updated_at,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn compare_and_set_git_finalization(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_generation: i64,
        next_state: GitFinalizationState,
        task_identity: Option<&str>,
        worktree_baseline: Option<&str>,
        require_running_owner: bool,
        owner_run_token: Option<&str>,
        commit_oid: Option<&str>,
        last_error: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        let mut conn = self.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!(
                    "Failed to begin updating Git finalization for project {project_id} and Codex session {codex_session_id}"
                )
            })?;
        let current = {
            let mut rows = transaction
                .query(
                    "SELECT project_id, codex_session_id, state, git_mode, starting_head,
                            branch_ref, upstream_ref, worktree_baseline, task_identity,
                            owner_run_token, commit_oid, generation, last_error, created_at,
                            updated_at, completed_at, acknowledged_at, acknowledged_run_id
                       FROM git_finalizations
                      WHERE project_id = ?1 AND codex_session_id = ?2",
                    params![project_id, codex_session_id],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to inspect Git finalization for project {project_id} and Codex session {codex_session_id}"
                    )
                })?;
            rows.next()
                .await
                .context("Failed to read Git finalization compare-and-set row")?
                .map(|row| git_finalization_record_from_row(&row))
                .transpose()?
        };
        let Some(current) = current else {
            transaction
                .commit()
                .await
                .context("Failed to finish compare-and-set for a missing Git finalization")?;
            return Ok(false);
        };
        if current.generation != expected_generation {
            transaction
                .commit()
                .await
                .context("Failed to finish compare-and-set for a changed Git finalization")?;
            return Ok(false);
        }
        if require_running_owner {
            let Some(owner_run_token) = owner_run_token else {
                anyhow::bail!("Git completion intent requires a running owner token");
            };
            if query_count(
                &transaction,
                "SELECT COUNT(*) FROM session_controls
                  WHERE project_id = ?1 AND codex_session_id = ?2
                    AND state = 'running' AND run_token = ?3",
                params![project_id, codex_session_id, owner_run_token],
            )
            .await?
                != 1
            {
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to finish fenced Git completion intent for project {project_id} and Codex session {codex_session_id}"
                    )
                })?;
                return Ok(false);
            }
        }
        if !current.state.can_transition_to(next_state) {
            anyhow::bail!(
                "Invalid Git finalization transition from {} to {}",
                current.state.database_value(),
                next_state.database_value()
            );
        }
        if next_state == GitFinalizationState::PushPending
            && current.git_mode != AgentGitMode::CommitAndPush
        {
            anyhow::bail!("Only commit-and-push finalizations may enter push_pending");
        }
        if let (Some(current_identity), Some(next_identity)) =
            (current.task_identity.as_deref(), task_identity)
            && current_identity != next_identity
        {
            anyhow::bail!(
                "Git finalization task identity cannot change after completion intent is recorded"
            );
        }
        let effective_task_identity = task_identity.or(current.task_identity.as_deref());
        if next_state.is_finalizing() && effective_task_identity.is_none() {
            anyhow::bail!(
                "Git finalization cannot enter {} without a task identity",
                next_state.database_value()
            );
        }
        let effective_commit_oid = commit_oid.or(current.commit_oid.as_deref());
        if let (Some(current_oid), Some(next_oid)) = (current.commit_oid.as_deref(), commit_oid)
            && current_oid != next_oid
        {
            anyhow::bail!("Git finalization commit OID cannot change once recorded");
        }
        if matches!(
            next_state,
            GitFinalizationState::PushPending | GitFinalizationState::Completed
        ) && effective_commit_oid.is_none()
        {
            anyhow::bail!(
                "Git finalization cannot enter {} without a commit OID",
                next_state.database_value()
            );
        }
        if next_state == GitFinalizationState::Completed
            && ((current.git_mode == AgentGitMode::Commit
                && current.state != GitFinalizationState::CommitPending)
                || (current.git_mode == AgentGitMode::CommitAndPush
                    && current.state != GitFinalizationState::PushPending))
        {
            anyhow::bail!(
                "Git finalization cannot complete before its configured commit or push step"
            );
        }

        let completed_at = next_state.is_terminal().then_some(updated_at);
        let changed = transaction
            .execute(
                "UPDATE git_finalizations
                    SET state = ?1,
                        task_identity = COALESCE(task_identity, ?2),
                        worktree_baseline = COALESCE(?3, worktree_baseline),
                        owner_run_token = ?4,
                        commit_oid = CASE WHEN ?5 IS NULL THEN commit_oid ELSE ?5 END,
                        generation = generation + 1,
                        last_error = ?6,
                        updated_at = ?7,
                        completed_at = ?8
                  WHERE project_id = ?9 AND codex_session_id = ?10 AND generation = ?11",
                params![
                    next_state.database_value(),
                    task_identity,
                    worktree_baseline,
                    owner_run_token,
                    commit_oid,
                    last_error,
                    updated_at,
                    completed_at,
                    project_id,
                    codex_session_id,
                    expected_generation,
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to update Git finalization for project {project_id} and Codex session {codex_session_id}"
                )
            })?;
        if changed == 1 {
            let next_generation = expected_generation
                .checked_add(1)
                .context("Git finalization generation overflowed")?;
            if next_state.is_terminal() {
                transaction
                    .execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state = 'resume_requested' AND child_pid IS NULL
                            AND interactive_holder IS NULL AND interactive_launch_token IS NULL
                            AND run_token = ?3 || CAST(?4 AS TEXT)",
                        params![
                            project_id,
                            codex_session_id,
                            AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX,
                            expected_generation
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to clear the terminal Git finalization recovery fence for session {codex_session_id}"
                        )
                    })?;
            } else {
                transaction
                    .execute(
                    "UPDATE session_controls
                        SET run_token = ?1 || CAST(?2 AS TEXT), updated_at = ?3
                      WHERE project_id = ?4 AND codex_session_id = ?5
                        AND state = 'resume_requested' AND child_pid IS NULL
                        AND interactive_holder IS NULL AND interactive_launch_token IS NULL
                        AND run_token = ?1 || CAST(?6 AS TEXT)",
                    params![
                        AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX,
                        next_generation,
                        updated_at,
                        project_id,
                        codex_session_id,
                        expected_generation
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to advance the Git finalization recovery fence for session {codex_session_id}"
                    )
                })?;
            }
        }
        transaction.commit().await.with_context(|| {
            format!(
                "Failed to commit Git finalization update for project {project_id} and Codex session {codex_session_id}"
            )
        })?;
        Ok(changed == 1)
    }

    #[cfg(test)]
    pub(crate) fn delete_terminal_git_finalization_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let removed = conn
                    .execute(
                        "DELETE FROM git_finalizations
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state IN ('completed', 'cancelled')",
                        params![project_id, codex_session_id],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to delete terminal Git finalization for project {project_id} and Codex session {codex_session_id}"
                        )
                    })?;
                Ok(removed == 1)
            })
    }

    pub(crate) fn acknowledge_completed_git_finalization_session_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let acknowledged_at = agent_timestamp();
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to begin acknowledging completed Git finalization for project {project_id} and Codex session {codex_session_id}"
                        )
                    })?;
                let completed = {
                    let mut rows = transaction
                        .query(
                            "SELECT completed_at, commit_oid, acknowledged_at,
                                    acknowledged_run_id
                               FROM git_finalizations
                              WHERE project_id = ?1 AND codex_session_id = ?2
                                AND state = 'completed'",
                            params![project_id, codex_session_id],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to inspect completed Git finalization for project {project_id} and Codex session {codex_session_id}"
                            )
                        })?;
                    rows
                        .next()
                        .await
                        .context("Failed to read completed Git finalization acknowledgement")?
                        .map(|row| {
                            Ok::<_, anyhow::Error>((
                                row_text(&row, 0, "completed_at")?,
                                row_optional_text(&row, 1, "commit_oid")?,
                                row_optional_text(&row, 2, "acknowledged_at")?,
                                row_optional_integer(&row, 3, "acknowledged_run_id")?,
                            ))
                        })
                        .transpose()?
                };
                let Some((completed_at, commit_oid, prior_acknowledgement, _)) = completed
                else {
                    transaction.commit().await.with_context(|| {
                        format!(
                            "Failed to finish acknowledging absent Git finalization for project {project_id} and Codex session {codex_session_id}"
                        )
                    })?;
                    return Ok(false);
                };

                if prior_acknowledgement.is_some() {
                    transaction
                        .execute(
                            "DELETE FROM session_controls
                              WHERE project_id = ?1 AND codex_session_id = ?2
                                AND state = 'resume_requested' AND child_pid IS NULL
                                AND interactive_holder IS NULL
                                AND run_token LIKE ?3",
                            params![
                                project_id,
                                codex_session_id,
                                format!("{AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX}%")
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to clear a late resume request for completed Git finalization {codex_session_id}"
                            )
                        })?;
                    transaction.commit().await.with_context(|| {
                        format!(
                            "Failed to commit idempotent Git finalization acknowledgement for {codex_session_id}"
                        )
                    })?;
                    return Ok(true);
                }

                let latest_session_run = {
                    let mut rows = transaction
                        .query(
                            "SELECT id, status, finished_at
                               FROM runs
                              WHERE project_id = ?1 AND codex_session_id = ?2
                              ORDER BY id DESC
                              LIMIT 1",
                            params![project_id, codex_session_id],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to find an existing successful run for Git finalization {codex_session_id}"
                            )
                        })?;
                    rows
                        .next()
                        .await
                        .context("Failed to read an existing Git finalization run")?
                        .map(|row| {
                            Ok::<_, anyhow::Error>((
                                row_integer(&row, 0, "id")?,
                                row_text(&row, 1, "status")?,
                                row_optional_text(&row, 2, "finished_at")?,
                            ))
                        })
                        .transpose()?
                };
                let short_commit = commit_oid
                    .as_deref()
                    .map(|oid| &oid[..oid.len().min(12)])
                    .unwrap_or("unknown");
                let summary = format!(
                    "CLT recovered the proven Git finalization at commit {short_commit} after an interrupted run acknowledgement."
                );
                let acknowledged_run_id = match latest_session_run {
                    Some((run_id, status, finished_at))
                        if matches!(status.as_str(), "success" | "idle")
                            && finished_at
                                .as_deref()
                                .and_then(|value| value.parse::<u64>().ok())
                                >= completed_at.parse::<u64>().ok() =>
                    {
                        run_id
                    }
                    _ => {
                    transaction
                        .execute(
                            "INSERT INTO runs (
                                project_id, status, started_at, finished_at, summary,
                                codex_session_id
                             ) VALUES (?1, 'success', ?2, ?2, ?3, ?4)",
                            params![
                                project_id,
                                completed_at.as_str(),
                                summary.as_str(),
                                codex_session_id,
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to record recovered success for Git finalization {codex_session_id}"
                            )
                        })?;
                    query_count(&transaction, "SELECT last_insert_rowid()", ()).await?
                    }
                };

                let latest_project_run_id =
                    query_count(&transaction, "SELECT COALESCE(MAX(id), 0) FROM runs WHERE project_id = ?1", [project_id]).await?;
                if latest_project_run_id == acknowledged_run_id {
                    update_project_after_run(
                        &transaction,
                        &AgentRunOutcome {
                            project_id,
                            status: "success",
                            started_at: &completed_at,
                            finished_at: Some(&completed_at),
                            exit_code: None,
                            log_dir: None,
                            stdout_path: None,
                            stderr_path: None,
                            summary: Some(&summary),
                            codex_session_id: Some(codex_session_id),
                        },
                    )
                    .await?;
                }

                let marked = transaction
                    .execute(
                        "UPDATE git_finalizations
                            SET acknowledged_at = ?1, acknowledged_run_id = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'completed' AND acknowledged_at IS NULL",
                        params![
                            acknowledged_at.as_str(),
                            acknowledged_run_id,
                            project_id,
                            codex_session_id,
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to mark Git finalization {codex_session_id} acknowledged"
                        )
                    })?;
                if marked != 1 {
                    anyhow::bail!(
                        "Git finalization acknowledgement for {codex_session_id} changed inside its exclusive transaction"
                    );
                }
                transaction
                    .execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state = 'resume_requested' AND child_pid IS NULL
                            AND interactive_holder IS NULL
                            AND run_token LIKE ?3
                            AND EXISTS (
                                SELECT 1 FROM git_finalizations
                                 WHERE project_id = ?1 AND codex_session_id = ?2
                                   AND state = 'completed'
                            )",
                        params![
                            project_id,
                            codex_session_id,
                            format!("{AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX}%")
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to acknowledge completed Git finalization for project {project_id} and Codex session {codex_session_id}"
                        )
                    })?;
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit completed Git finalization acknowledgement for project {project_id} and Codex session {codex_session_id}"
                    )
                })?;
                Ok(true)
            })
    }

    pub(crate) fn record_project_scan_blocking(&self, project_id: i64) -> Result<String> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.record_project_scan(project_id))
    }

    async fn record_project_scan(&self, project_id: i64) -> Result<String> {
        let conn = self.connect().await?;
        let scanned_at = agent_timestamp();

        conn.execute(
            "UPDATE projects
             SET last_scan_at = ?1, updated_at = ?1
             WHERE id = ?2",
            params![scanned_at.as_str(), project_id],
        )
        .await
        .with_context(|| format!("Failed to record agent project scan {}", project_id))?;

        Ok(scanned_at)
    }

    pub(crate) fn record_project_daemon_scan_blocking(
        &self,
        project_id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<String> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.record_project_daemon_scan(project_id, status, error))
    }

    async fn record_project_daemon_scan(
        &self,
        project_id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<String> {
        let conn = self.connect().await?;
        let scanned_at = agent_timestamp();

        conn.execute(
            "UPDATE projects
             SET last_scan_at = ?1,
                 last_daemon_scan_status = ?2,
                 last_daemon_scan_error = ?3,
                 updated_at = ?1
             WHERE id = ?4",
            params![scanned_at.as_str(), status, error, project_id],
        )
        .await
        .with_context(|| format!("Failed to record daemon project scan {project_id}"))?;

        Ok(scanned_at)
    }

    pub(crate) fn try_acquire_lease_blocking(
        &self,
        project_id: i64,
        holder: &str,
        acquired_at: &str,
        expires_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.try_acquire_lease(project_id, holder, acquired_at, expires_at))
    }

    async fn try_acquire_lease(
        &self,
        project_id: i64,
        holder: &str,
        acquired_at: &str,
        expires_at: &str,
    ) -> Result<bool> {
        let conn = self.connect().await?;

        conn.execute(
            "DELETE FROM leases
              WHERE project_id = ?1 AND expires_at <= ?2
                AND NOT EXISTS (
                    SELECT 1 FROM agent_workers w
                     WHERE w.project_id = leases.project_id
                       AND w.lease_holder = leases.holder
                       AND w.state IN ('dispatching', 'running', 'finalizing')
                )",
            params![project_id, acquired_at],
        )
        .await
        .with_context(|| format!("Failed to clear expired lease for project {}", project_id))?;

        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO leases (project_id, holder, acquired_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![project_id, holder, acquired_at, expires_at],
            )
            .await
            .with_context(|| format!("Failed to acquire lease for project {}", project_id))?;

        Ok(inserted > 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_acquire_git_finalization_lease_blocking(
        &self,
        project_id: i64,
        holder: &str,
        acquired_at: &str,
        expires_at: &str,
        reclaim_holder: Option<&str>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.try_acquire_git_finalization_lease(
                project_id,
                holder,
                acquired_at,
                expires_at,
                reclaim_holder,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn try_acquire_git_finalization_lease(
        &self,
        project_id: i64,
        holder: &str,
        acquired_at: &str,
        expires_at: &str,
        reclaim_holder: Option<&str>,
    ) -> Result<bool> {
        let mut conn = self.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!(
                    "Failed to begin acquiring the Git finalization lease for project {project_id}"
                )
            })?;
        transaction
            .execute(
                "UPDATE session_controls
                    SET run_token = ?1 || (
                            SELECT CAST(g.generation AS TEXT)
                              FROM git_finalizations g
                             WHERE g.project_id = session_controls.project_id
                               AND g.codex_session_id = session_controls.codex_session_id
                               AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                        ),
                        updated_at = ?2
                  WHERE project_id = ?3 AND state = 'resume_requested'
                    AND child_pid IS NULL AND interactive_holder IS NULL
                    AND interactive_launch_token IS NULL
                    AND run_token LIKE ?1 || '%'
                    AND EXISTS (
                        SELECT 1 FROM git_finalizations g
                         WHERE g.project_id = session_controls.project_id
                           AND g.codex_session_id = session_controls.codex_session_id
                           AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                    )",
                params![
                    AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX,
                    acquired_at,
                    project_id
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to repair stale Git finalization recovery fences for project {project_id}"
                )
            })?;
        let controls_allow_finalization = "NOT EXISTS (
            SELECT 1 FROM session_controls sc
             WHERE sc.project_id = ?1
               AND NOT (
                   sc.state = 'stopped'
                   OR (sc.state = 'resume_requested'
                       AND sc.child_pid IS NULL
                       AND sc.interactive_holder IS NULL
                       AND sc.interactive_launch_token IS NULL
                       AND EXISTS (
                           SELECT 1 FROM git_finalizations g
                            WHERE g.project_id = sc.project_id
                              AND g.codex_session_id = sc.codex_session_id
                              AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                              AND sc.run_token = ?4 || CAST(g.generation AS TEXT)
                       ))
               )
        )";
        transaction
            .execute(
                &format!(
                    "DELETE FROM leases
                      WHERE project_id = ?1
                        AND (CAST(expires_at AS INTEGER) <= CAST(?2 AS INTEGER)
                             OR (?3 IS NOT NULL AND holder = ?3))
                        AND NOT EXISTS (
                            SELECT 1 FROM agent_workers w
                             WHERE w.project_id = leases.project_id
                               AND w.state IN ('dispatching', 'running', 'finalizing')
                        )
                        AND {controls_allow_finalization}"
                ),
                params![
                    project_id,
                    acquired_at,
                    reclaim_holder,
                    AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to clear a reclaimable lease before Git finalization for project {project_id}"
                )
            })?;
        let inserted = transaction
            .execute(
                &format!(
                    "INSERT OR IGNORE INTO leases (project_id, holder, acquired_at, expires_at)
                     SELECT ?1, ?2, ?3, ?5
                      WHERE EXISTS (
                          SELECT 1 FROM git_finalizations g
                           WHERE g.project_id = ?1
                             AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                      )
                        AND NOT EXISTS (
                            SELECT 1 FROM agent_workers w
                             WHERE w.project_id = ?1
                               AND w.state IN ('dispatching', 'running', 'finalizing')
                        )
                        AND {controls_allow_finalization}"
                ),
                params![
                    project_id,
                    holder,
                    acquired_at,
                    AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX,
                    expires_at
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to acquire the guarded Git finalization lease for project {project_id}"
                )
            })?;
        transaction.commit().await.with_context(|| {
            format!("Failed to commit Git finalization lease acquisition for project {project_id}")
        })?;
        Ok(inserted == 1)
    }

    pub(crate) fn renew_git_finalization_lease_blocking(
        &self,
        project_id: i64,
        holder: &str,
        expires_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE leases
                            SET expires_at = ?1
                          WHERE project_id = ?2 AND holder = ?3
                            AND NOT EXISTS (
                                SELECT 1 FROM agent_workers w
                                 WHERE w.project_id = ?2
                                   AND w.state IN ('dispatching', 'running', 'finalizing')
                            )
                            AND NOT EXISTS (
                                SELECT 1 FROM session_controls sc
                                 WHERE sc.project_id = ?2
                                   AND NOT (
                                       sc.state = 'stopped'
                                       OR (sc.state = 'resume_requested'
                                           AND sc.child_pid IS NULL
                                           AND sc.interactive_holder IS NULL
                                           AND sc.interactive_launch_token IS NULL
                                           AND EXISTS (
                                               SELECT 1 FROM git_finalizations g
                                                WHERE g.project_id = sc.project_id
                                                  AND g.codex_session_id = sc.codex_session_id
                                                  AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                                                  AND sc.run_token = ?4 || CAST(g.generation AS TEXT)
                                           ))
                                   )
                            )",
                        params![
                            expires_at,
                            project_id,
                            holder,
                            AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to renew the guarded Git finalization lease for project {project_id}"
                        )
                    })?;
                Ok(changed == 1)
            })
    }

    pub(crate) fn git_finalization_lease_is_owned_blocking(
        &self,
        project_id: i64,
        holder: &str,
        now: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                Ok(query_count(
                    &conn,
                    "SELECT COUNT(*) FROM leases l
                      WHERE l.project_id = ?1 AND l.holder = ?2
                        AND CAST(l.expires_at AS INTEGER) > CAST(?3 AS INTEGER)
                        AND NOT EXISTS (
                            SELECT 1 FROM agent_workers w
                             WHERE w.project_id = ?1
                               AND w.state IN ('dispatching', 'running', 'finalizing')
                        )
                        AND NOT EXISTS (
                            SELECT 1 FROM session_controls sc
                             WHERE sc.project_id = ?1
                               AND NOT (
                                   sc.state = 'stopped'
                                   OR (sc.state = 'resume_requested'
                                       AND sc.child_pid IS NULL
                                       AND sc.interactive_holder IS NULL
                                       AND sc.interactive_launch_token IS NULL
                                       AND EXISTS (
                                           SELECT 1 FROM git_finalizations g
                                            WHERE g.project_id = sc.project_id
                                              AND g.codex_session_id = sc.codex_session_id
                                              AND g.state IN ('working', 'tracking', 'commit_pending', 'push_pending')
                                              AND sc.run_token = ?4 || CAST(g.generation AS TEXT)
                                       ))
                               )
                        )",
                    params![
                        project_id,
                        holder,
                        now,
                        AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX
                    ],
                )
                .await? == 1)
            })
    }

    pub(crate) fn renew_lease_blocking(
        &self,
        project_id: i64,
        holder: &str,
        expires_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.renew_lease(project_id, holder, expires_at))
    }

    async fn renew_lease(&self, project_id: i64, holder: &str, expires_at: &str) -> Result<bool> {
        let conn = self.connect().await?;
        let changed = conn
            .execute(
                "UPDATE leases SET expires_at = ?1 WHERE project_id = ?2 AND holder = ?3",
                params![expires_at, project_id, holder],
            )
            .await
            .with_context(|| format!("Failed to renew lease for project {project_id}"))?;

        if changed > 0 {
            return Ok(true);
        }

        Ok(query_count(
            &conn,
            "SELECT COUNT(*) FROM leases WHERE project_id = ?1 AND holder = ?2",
            params![project_id, holder],
        )
        .await?
            > 0)
    }

    pub(crate) fn release_lease_blocking(&self, project_id: i64, holder: &str) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.release_lease(project_id, holder))
    }

    async fn release_lease(&self, project_id: i64, holder: &str) -> Result<bool> {
        let conn = self.connect().await?;
        let removed = conn
            .execute(
                "DELETE FROM leases WHERE project_id = ?1 AND holder = ?2",
                params![project_id, holder],
            )
            .await
            .with_context(|| format!("Failed to release lease for project {}", project_id))?;

        Ok(removed > 0)
    }

    pub(crate) fn reserve_worker_blocking(
        &self,
        reservation: AgentWorkerReservation<'_>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.reserve_worker(reservation, None))
    }

    pub(crate) fn reserve_and_claim_worker_blocking(
        &self,
        reservation: AgentWorkerReservation<'_>,
        worker_pid: u32,
        started_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.reserve_worker(reservation, Some((worker_pid, started_at))))
    }

    async fn reserve_worker(
        &self,
        reservation: AgentWorkerReservation<'_>,
        initial_claim: Option<(u32, &str)>,
    ) -> Result<bool> {
        let AgentWorkerReservation {
            project_id,
            worker_token,
            expected_lease_holder,
            max_active_workers,
            protocol_version,
            service_label,
            binary_path,
            command_arguments,
            path_env,
            codex_path,
            task_selection,
            resume_session_id,
            created_at,
        } = reservation;
        let lease_holder = worker_lease_holder(worker_token);
        let codex_path = codex_path.map(|path| path.to_string_lossy().into_owned());
        let mut conn = self.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!("Failed to begin worker reservation for project {project_id}")
            })?;

        let max_active_workers = i64::try_from(max_active_workers)
            .context("Maximum active worker count is outside the supported range")?;
        if max_active_workers <= 0
            || query_count(
                &transaction,
                "SELECT COUNT(*) FROM agent_workers
                  WHERE state IN ('dispatching', 'running', 'finalizing')",
                (),
            )
            .await?
                >= max_active_workers
        {
            return Ok(false);
        }

        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO agent_workers (
                    worker_token, project_id, state, protocol_version, lease_holder, service_label,
                    binary_path, command_arguments, path_env, codex_path,
                    task_selection, resume_session_id, worker_pid,
                    created_at, started_at, heartbeat_at, finished_at, run_id, error,
                    service_cleaned_at
                 ) VALUES (?1, ?2, 'dispatching', ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                           ?10, ?11, NULL, ?12, NULL, ?12, NULL, NULL, NULL, NULL)",
                params![
                    worker_token,
                    project_id,
                    protocol_version,
                    lease_holder.as_str(),
                    service_label,
                    binary_path.to_string_lossy().as_ref(),
                    command_arguments,
                    path_env.to_string_lossy().as_ref(),
                    codex_path.as_deref(),
                    task_selection,
                    resume_session_id,
                    created_at,
                ],
            )
            .await
            .with_context(|| {
                format!("Failed to reserve worker {worker_token} for project {project_id}")
            })?;
        if inserted != 1 {
            return Ok(false);
        }

        let transferred = transaction
            .execute(
                "UPDATE leases
                    SET holder = ?1
                  WHERE project_id = ?2 AND holder = ?3",
                params![lease_holder.as_str(), project_id, expected_lease_holder],
            )
            .await
            .with_context(|| {
                format!("Failed to transfer project {project_id} lease to worker {worker_token}")
            })?;
        if transferred != 1 {
            return Ok(false);
        }

        transaction
            .execute(
                "UPDATE agent_workers
                    SET state = 'superseded'
                  WHERE project_id = ?1 AND state = 'abandoned'
                    AND worker_token <> ?2",
                params![project_id, worker_token],
            )
            .await
            .with_context(|| {
                format!("Failed to supersede earlier abandoned workers for project {project_id}")
            })?;

        if let Some((worker_pid, started_at)) = initial_claim {
            let claimed = transaction
                .execute(
                    "UPDATE agent_workers
                        SET state = 'running', worker_pid = ?1, started_at = ?2,
                            heartbeat_at = ?2, error = NULL
                      WHERE worker_token = ?3 AND state = 'dispatching'
                        AND EXISTS (
                            SELECT 1 FROM leases
                             WHERE leases.project_id = agent_workers.project_id
                               AND leases.holder = agent_workers.lease_holder
                        )",
                    params![i64::from(worker_pid), started_at, worker_token],
                )
                .await
                .with_context(|| {
                    format!("Failed to atomically claim inline worker {worker_token}")
                })?;
            if claimed != 1 {
                return Ok(false);
            }
        }

        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit worker {worker_token} reservation"))?;
        Ok(true)
    }

    pub(crate) fn claim_worker_blocking(
        &self,
        worker_token: &str,
        worker_pid: u32,
        started_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.claim_worker(worker_token, worker_pid, started_at))
    }

    async fn claim_worker(
        &self,
        worker_token: &str,
        worker_pid: u32,
        started_at: &str,
    ) -> Result<bool> {
        let mut conn = self.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin worker {worker_token} claim"))?;
        let changed = transaction
            .execute(
                "UPDATE agent_workers
                    SET state = 'running', worker_pid = ?1, started_at = ?2,
                        heartbeat_at = ?2, error = NULL
                  WHERE worker_token = ?3 AND state = 'dispatching'
                    AND EXISTS (
                        SELECT 1 FROM leases
                         WHERE leases.project_id = agent_workers.project_id
                           AND leases.holder = agent_workers.lease_holder
                    )",
                params![i64::from(worker_pid), started_at, worker_token],
            )
            .await
            .with_context(|| format!("Failed to claim worker {worker_token}"))?;
        if changed == 1 {
            transaction
                .commit()
                .await
                .with_context(|| format!("Failed to commit worker {worker_token} claim"))?;
            return Ok(true);
        }

        let already_claimed = query_count(
            &transaction,
            "SELECT COUNT(*)
               FROM agent_workers w
               JOIN leases l
                 ON l.project_id = w.project_id AND l.holder = w.lease_holder
              WHERE w.worker_token = ?1 AND w.state = 'running' AND w.worker_pid = ?2",
            params![worker_token, i64::from(worker_pid)],
        )
        .await?
            == 1;
        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to finish idempotent worker {worker_token} claim"))?;
        Ok(already_claimed)
    }

    pub(crate) fn renew_worker_blocking(
        &self,
        worker_token: &str,
        worker_pid: u32,
        heartbeat_at: &str,
        lease_expires_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.renew_worker(worker_token, worker_pid, heartbeat_at, lease_expires_at))
    }

    pub(crate) fn worker_fence_snapshot_blocking(
        &self,
        worker_token: &str,
        expected_worker_pid: u32,
    ) -> Result<String> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT project_id, state, worker_pid, lease_holder, heartbeat_at
                           FROM agent_workers WHERE worker_token = ?1",
                        [worker_token],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to inspect worker {worker_token} ownership fence")
                    })?;
                let Some(row) = rows
                    .next()
                    .await
                    .context("Failed to read worker ownership fence row")?
                else {
                    return Ok(format!(
                        "worker=missing expected_pid={expected_worker_pid}"
                    ));
                };
                let project_id = row_integer(&row, 0, "project_id")?;
                let state = row_text(&row, 1, "state")?;
                let worker_pid = row_optional_integer(&row, 2, "worker_pid")?;
                let worker_lease_holder = row_text(&row, 3, "lease_holder")?;
                let heartbeat_at = row_optional_text(&row, 4, "heartbeat_at")?;
                drop(rows);
                let mut lease_rows = conn
                    .query(
                        "SELECT holder, expires_at FROM leases WHERE project_id = ?1",
                        [project_id],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to inspect worker {worker_token} project lease")
                    })?;
                let lease = lease_rows
                    .next()
                    .await
                    .context("Failed to read worker project lease row")?
                    .map(|row| {
                        Ok::<String, anyhow::Error>(format!(
                            "{}@{}",
                            row_text(&row, 0, "holder")?,
                            row_text(&row, 1, "expires_at")?
                        ))
                    })
                    .transpose()?
                    .unwrap_or_else(|| "missing".to_string());
                Ok(format!(
                    "state={state} worker_pid={} expected_pid={expected_worker_pid} worker_lease_holder={worker_lease_holder} project_lease={lease} heartbeat_at={}",
                    worker_pid
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "missing".to_string()),
                    heartbeat_at.as_deref().unwrap_or("missing")
                ))
            })
    }

    async fn renew_worker(
        &self,
        worker_token: &str,
        worker_pid: u32,
        heartbeat_at: &str,
        lease_expires_at: &str,
    ) -> Result<bool> {
        let mut conn = self.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin worker {worker_token} heartbeat"))?;
        let (project_id, lease_holder) = {
            let mut rows = transaction
                .query(
                    "SELECT w.project_id, w.lease_holder
                       FROM agent_workers w
                       JOIN leases l
                         ON l.project_id = w.project_id AND l.holder = w.lease_holder
                      WHERE w.worker_token = ?1 AND w.state = 'running'
                        AND w.worker_pid = ?2",
                    params![worker_token, i64::from(worker_pid)],
                )
                .await
                .with_context(|| {
                    format!("Failed to verify worker {worker_token} heartbeat ownership")
                })?;
            let Some(row) = rows
                .next()
                .await
                .context("Failed to read worker heartbeat ownership row")?
            else {
                return Ok(false);
            };
            (
                row_integer(&row, 0, "project_id")?,
                row_text(&row, 1, "lease_holder")?,
            )
        };
        let worker_changed = transaction
            .execute(
                "UPDATE agent_workers
                    SET heartbeat_at = ?1
                  WHERE worker_token = ?2 AND state = 'running' AND worker_pid = ?3",
                params![heartbeat_at, worker_token, i64::from(worker_pid)],
            )
            .await
            .with_context(|| format!("Failed to update worker {worker_token} heartbeat"))?;
        if worker_changed != 1
            && query_count(
                &transaction,
                "SELECT COUNT(*) FROM agent_workers
                  WHERE worker_token = ?1 AND state = 'running' AND worker_pid = ?2
                    AND heartbeat_at = ?3",
                params![worker_token, i64::from(worker_pid), heartbeat_at],
            )
            .await?
                != 1
        {
            return Ok(false);
        }

        let lease_changed = transaction
            .execute(
                "UPDATE leases
                    SET expires_at = ?1
                  WHERE project_id = ?2 AND holder = ?3",
                params![lease_expires_at, project_id, lease_holder.as_str()],
            )
            .await
            .with_context(|| format!("Failed to renew worker {worker_token} lease"))?;
        if lease_changed != 1
            && query_count(
                &transaction,
                "SELECT COUNT(*) FROM leases
                  WHERE project_id = ?1 AND holder = ?2 AND expires_at = ?3",
                params![project_id, lease_holder.as_str(), lease_expires_at],
            )
            .await?
                != 1
        {
            return Ok(false);
        }

        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit worker {worker_token} heartbeat"))?;
        Ok(true)
    }

    pub(crate) fn list_active_workers_blocking(&self) -> Result<Vec<AgentWorkerRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.list_active_workers())
    }

    async fn list_active_workers(&self) -> Result<Vec<AgentWorkerRecord>> {
        self.list_workers_by_terminal_state(false).await
    }

    pub(crate) fn list_terminal_workers_blocking(&self) -> Result<Vec<AgentWorkerRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.list_terminal_workers())
    }

    pub(crate) fn supersede_abandoned_workers_for_lease_blocking(
        &self,
        project_id: i64,
        expected_lease_holder: &str,
    ) -> Result<u64> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                conn.execute(
                    "UPDATE agent_workers
                        SET state = 'superseded'
                      WHERE project_id = ?1 AND state = 'abandoned'
                        AND EXISTS (
                            SELECT 1 FROM leases
                             WHERE leases.project_id = agent_workers.project_id
                               AND leases.holder = ?2
                        )",
                    params![project_id, expected_lease_holder],
                )
                .await
                .with_context(|| {
                    format!("Failed to supersede abandoned workers for project {project_id}")
                })
            })
    }

    async fn list_terminal_workers(&self) -> Result<Vec<AgentWorkerRecord>> {
        self.list_workers_by_terminal_state(true).await
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn mark_worker_service_cleaned_blocking(
        &self,
        worker_token: &str,
        cleaned_at: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE agent_workers
                            SET service_cleaned_at = ?1
                          WHERE worker_token = ?2
                            AND state NOT IN ('dispatching', 'running', 'finalizing')",
                        params![cleaned_at, worker_token],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to mark worker {worker_token} service metadata cleaned")
                    })?;
                Ok(changed == 1)
            })
    }

    async fn list_workers_by_terminal_state(
        &self,
        terminal: bool,
    ) -> Result<Vec<AgentWorkerRecord>> {
        let conn = self.connect().await?;
        let states = if terminal {
            "NOT IN ('dispatching', 'running', 'finalizing')"
        } else {
            "IN ('dispatching', 'running', 'finalizing')"
        };
        let sql = format!(
            "SELECT w.worker_token, w.project_id, p.name, p.path, w.state,
                    w.protocol_version, w.lease_holder, w.service_label, w.binary_path,
                    w.command_arguments, w.path_env, w.codex_path, w.task_selection,
                    w.resume_session_id, w.worker_pid, w.created_at, w.started_at,
                    w.heartbeat_at, w.finished_at, w.run_id, w.error, w.service_cleaned_at
               FROM agent_workers w
               JOIN projects p ON p.id = w.project_id
              WHERE w.state {states}
              ORDER BY CAST(w.created_at AS INTEGER), w.worker_token"
        );
        let mut rows = conn.query(&sql, ()).await.with_context(|| {
            if terminal {
                "Failed to list terminal agent workers"
            } else {
                "Failed to list active agent workers"
            }
        })?;
        let mut workers = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .context("Failed to read agent worker row")?
        {
            let worker_pid = row_optional_integer(&row, 14, "worker_pid")?
                .map(u32::try_from)
                .transpose()
                .context("Agent worker PID is outside the supported range")?;
            workers.push(AgentWorkerRecord {
                worker_token: row_text(&row, 0, "worker_token")?,
                project_id: row_integer(&row, 1, "project_id")?,
                project_name: row_text(&row, 2, "name")?,
                project_path: PathBuf::from(row_text(&row, 3, "path")?),
                state: row_text(&row, 4, "state")?,
                protocol_version: row_integer(&row, 5, "protocol_version")?,
                lease_holder: row_text(&row, 6, "lease_holder")?,
                service_label: row_text(&row, 7, "service_label")?,
                binary_path: PathBuf::from(row_text(&row, 8, "binary_path")?),
                command_arguments: row_text(&row, 9, "command_arguments")?,
                path_env: OsString::from(row_text(&row, 10, "path_env")?),
                codex_path: row_optional_text(&row, 11, "codex_path")?.map(PathBuf::from),
                task_selection: row_text(&row, 12, "task_selection")?,
                resume_session_id: row_optional_text(&row, 13, "resume_session_id")?,
                worker_pid,
                created_at: row_text(&row, 15, "created_at")?,
                started_at: row_optional_text(&row, 16, "started_at")?,
                heartbeat_at: row_optional_text(&row, 17, "heartbeat_at")?,
                finished_at: row_optional_text(&row, 18, "finished_at")?,
                run_id: row_optional_integer(&row, 19, "run_id")?,
                error: row_optional_text(&row, 20, "error")?,
                service_cleaned_at: row_optional_text(&row, 21, "service_cleaned_at")?,
            });
        }
        Ok(workers)
    }

    pub(crate) fn abandon_worker_blocking(
        &self,
        abandonment: AgentWorkerAbandonment<'_>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.abandon_worker(abandonment))
    }

    async fn abandon_worker(&self, abandonment: AgentWorkerAbandonment<'_>) -> Result<bool> {
        let AgentWorkerAbandonment {
            worker_token,
            expected_state,
            expected_worker_pid,
            expected_heartbeat_at,
            finished_at,
            error,
            permitted_successor_holder,
        } = abandonment;
        let mut conn = self.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin abandoning worker {worker_token}"))?;

        let (project_id, created_at, started_at, worker_lease_holder) = {
            let mut rows = transaction
                .query(
                    "SELECT project_id, created_at, started_at, lease_holder
                       FROM agent_workers
                      WHERE worker_token = ?1 AND state = ?2
                        AND (worker_pid = ?3 OR (worker_pid IS NULL AND ?3 IS NULL))
                        AND (heartbeat_at = ?4 OR (heartbeat_at IS NULL AND ?4 IS NULL))",
                    params![
                        worker_token,
                        expected_state,
                        expected_worker_pid.map(i64::from),
                        expected_heartbeat_at,
                    ],
                )
                .await
                .with_context(|| format!("Failed to inspect worker {worker_token}"))?;
            let Some(row) = rows
                .next()
                .await
                .context("Failed to read worker abandonment row")?
            else {
                return Ok(false);
            };
            (
                row_integer(&row, 0, "project_id")?,
                row_text(&row, 1, "created_at")?,
                row_optional_text(&row, 2, "started_at")?,
                row_text(&row, 3, "lease_holder")?,
            )
        };
        let observed_lease_holder = {
            let mut rows = transaction
                .query(
                    "SELECT holder FROM leases WHERE project_id = ?1",
                    [project_id],
                )
                .await
                .with_context(|| {
                    format!("Failed to inspect worker {worker_token} project lease")
                })?;
            rows.next()
                .await
                .context("Failed to read worker project lease")?
                .map(|row| row_text(&row, 0, "holder"))
                .transpose()?
        };
        let preserve_successor_lease = match observed_lease_holder.as_deref() {
            None => false,
            Some(holder) if holder == worker_lease_holder => false,
            Some(holder) if permitted_successor_holder == Some(holder) => true,
            Some(_) => return Ok(false),
        };
        if observed_lease_holder.is_some()
            && observed_lease_holder.as_deref() != Some(worker_lease_holder.as_str())
            && !preserve_successor_lease
        {
            return Ok(false);
        }

        let run_started_at = started_at.as_deref().unwrap_or(created_at.as_str());
        let outcome = AgentRunOutcome {
            project_id,
            status: "failure",
            started_at: run_started_at,
            finished_at: Some(finished_at),
            exit_code: None,
            log_dir: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some(error),
            codex_session_id: None,
        };
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO runs (
                    project_id, status, started_at, finished_at, exit_code,
                    log_dir, stdout_path, stderr_path, summary, codex_session_id,
                    worker_token
                 ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL, NULL, ?5, NULL, ?6)",
                params![
                    project_id,
                    outcome.status,
                    outcome.started_at,
                    outcome.finished_at,
                    error,
                    worker_token,
                ],
            )
            .await
            .with_context(|| format!("Failed to record abandoned worker {worker_token}"))?;
        let run_id = if inserted == 1 {
            let run_id = query_count(&transaction, "SELECT last_insert_rowid()", ()).await?;
            update_project_after_run(&transaction, &outcome).await?;
            run_id
        } else {
            query_count(
                &transaction,
                "SELECT id FROM runs WHERE worker_token = ?1 AND project_id = ?2",
                params![worker_token, project_id],
            )
            .await?
        };
        let changed = transaction
            .execute(
                "UPDATE agent_workers
                    SET state = 'abandoned', finished_at = ?1, error = ?2, run_id = ?3
                  WHERE worker_token = ?4 AND state = ?5
                    AND (worker_pid = ?6 OR (worker_pid IS NULL AND ?6 IS NULL))
                    AND (
                        heartbeat_at = ?7
                        OR (heartbeat_at IS NULL AND ?7 IS NULL)
                    )",
                params![
                    finished_at,
                    error,
                    run_id,
                    worker_token,
                    expected_state,
                    expected_worker_pid.map(i64::from),
                    expected_heartbeat_at,
                ],
            )
            .await
            .with_context(|| format!("Failed to abandon worker {worker_token}"))?;
        if changed != 1 {
            return Ok(false);
        }

        if observed_lease_holder.as_deref() == Some(worker_lease_holder.as_str()) {
            let released = transaction
                .execute(
                    "DELETE FROM leases WHERE project_id = ?1 AND holder = ?2",
                    params![project_id, worker_lease_holder.as_str()],
                )
                .await
                .with_context(|| {
                    format!("Failed to release abandoned worker {worker_token} lease")
                })?;
            if released != 1 {
                return Ok(false);
            }
        }

        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit abandoned worker {worker_token}"))?;
        Ok(true)
    }

    pub(crate) fn finalize_worker_blocking(
        &self,
        finalization: AgentWorkerFinalization<'_>,
    ) -> Result<Option<i64>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.finalize_worker(finalization))
    }

    async fn finalize_worker(
        &self,
        finalization: AgentWorkerFinalization<'_>,
    ) -> Result<Option<i64>> {
        let AgentWorkerFinalization {
            worker_token,
            expected_worker_pid,
            expected_lease_holder,
            status,
            finished_at,
            exit_code,
            log_dir,
            stdout_path,
            stderr_path,
            summary,
            codex_session_id,
            error,
        } = finalization;
        let mut conn = self.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin finalizing worker {worker_token}"))?;
        let (project_id, state, observed_worker_pid, created_at, started_at, existing_run_id) = {
            let mut rows = transaction
                .query(
                    "SELECT project_id, state, worker_pid, created_at, started_at, run_id
                       FROM agent_workers
                      WHERE worker_token = ?1",
                    [worker_token],
                )
                .await
                .with_context(|| {
                    format!("Failed to read worker {worker_token} for finalization")
                })?;
            let Some(row) = rows
                .next()
                .await
                .context("Failed to read worker finalization row")?
            else {
                return Ok(None);
            };
            (
                row_integer(&row, 0, "project_id")?,
                row_text(&row, 1, "state")?,
                row_optional_integer(&row, 2, "worker_pid")?,
                row_text(&row, 3, "created_at")?,
                row_optional_text(&row, 4, "started_at")?,
                row_optional_integer(&row, 5, "run_id")?,
            )
        };
        if state == "completed" {
            transaction.commit().await.with_context(|| {
                format!("Failed to finish idempotent worker {worker_token} finalization")
            })?;
            return existing_run_id
                .map(Some)
                .context("Completed agent worker is missing its run ID");
        }
        if !matches!(state.as_str(), "dispatching" | "running" | "finalizing")
            || observed_worker_pid != expected_worker_pid.map(i64::from)
        {
            return Ok(None);
        }
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM leases WHERE project_id = ?1 AND holder = ?2",
            params![project_id, expected_lease_holder],
        )
        .await?
            != 1
        {
            return Ok(None);
        }

        let claimed = transaction
            .execute(
                "UPDATE agent_workers
                    SET state = 'finalizing'
                  WHERE worker_token = ?1 AND state = ?2
                    AND (worker_pid = ?3 OR (worker_pid IS NULL AND ?3 IS NULL))",
                params![
                    worker_token,
                    state.as_str(),
                    expected_worker_pid.map(i64::from)
                ],
            )
            .await
            .with_context(|| format!("Failed to claim worker {worker_token} finalization"))?;
        if claimed != 1 {
            return Ok(None);
        }

        let run_started_at = started_at.as_deref().unwrap_or(created_at.as_str());
        let outcome = AgentRunOutcome {
            project_id,
            status,
            started_at: run_started_at,
            finished_at: Some(finished_at),
            exit_code,
            log_dir,
            stdout_path,
            stderr_path,
            summary,
            codex_session_id,
        };
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO runs (
                    project_id, status, started_at, finished_at, exit_code,
                    log_dir, stdout_path, stderr_path, summary, codex_session_id,
                    worker_token
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    outcome.project_id,
                    outcome.status,
                    outcome.started_at,
                    outcome.finished_at,
                    outcome.exit_code,
                    outcome.log_dir,
                    outcome.stdout_path,
                    outcome.stderr_path,
                    outcome.summary,
                    outcome.codex_session_id,
                    worker_token,
                ],
            )
            .await
            .with_context(|| format!("Failed to record run for worker {worker_token}"))?;
        let run_id = if inserted == 1 {
            let run_id = query_count(&transaction, "SELECT last_insert_rowid()", ()).await?;
            update_project_after_run(&transaction, &outcome).await?;
            run_id
        } else {
            query_count(
                &transaction,
                "SELECT id FROM runs WHERE worker_token = ?1 AND project_id = ?2",
                params![worker_token, project_id],
            )
            .await
            .with_context(|| {
                format!("Failed to reuse the existing run for worker {worker_token}")
            })?
        };

        let completed = transaction
            .execute(
                "UPDATE agent_workers
                    SET state = 'completed', finished_at = ?1, run_id = ?2, error = ?3
                  WHERE worker_token = ?4 AND state = 'finalizing'",
                params![finished_at, run_id, error, worker_token],
            )
            .await
            .with_context(|| format!("Failed to complete worker {worker_token}"))?;
        if completed != 1 {
            return Ok(None);
        }
        transaction
            .execute(
                "DELETE FROM leases
                  WHERE project_id = ?1
                    AND holder = (
                            SELECT lease_holder FROM agent_workers WHERE worker_token = ?2
                        )",
                params![project_id, worker_token],
            )
            .await
            .with_context(|| format!("Failed to release completed worker {worker_token} lease"))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit worker {worker_token} finalization"))?;
        Ok(Some(run_id))
    }

    pub(crate) fn mark_session_running_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        child_pid: u32,
        run_token: &str,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> Result<()> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.mark_session_running(
                project_id,
                codex_session_id,
                child_pid,
                run_token,
                stdout_path,
                stderr_path,
                None,
            ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mark_session_running_with_git_finalization_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        child_pid: u32,
        run_token: &str,
        stdout_path: &Path,
        stderr_path: &Path,
        git_mode: AgentGitMode,
    ) -> Result<()> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.mark_session_running(
                project_id,
                codex_session_id,
                child_pid,
                run_token,
                stdout_path,
                stderr_path,
                Some(git_mode),
            ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn mark_session_running(
        &self,
        project_id: i64,
        codex_session_id: &str,
        child_pid: u32,
        run_token: &str,
        stdout_path: &Path,
        stderr_path: &Path,
        git_mode: Option<AgentGitMode>,
    ) -> Result<()> {
        let mut conn = self.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| {
                format!(
                    "Failed to begin registering Codex session {codex_session_id} for project {project_id}"
                )
            })?;
        let known_worker = query_count(
            &transaction,
            "SELECT COUNT(*) FROM agent_workers WHERE worker_token = ?1",
            [run_token],
        )
        .await?
            == 1;
        let fenced_worker = query_count(
            &transaction,
            "SELECT COUNT(*)
               FROM agent_workers w
               JOIN leases l
                 ON l.project_id = w.project_id AND l.holder = w.lease_holder
              WHERE w.worker_token = ?1 AND w.project_id = ?2
                AND w.state IN ('dispatching', 'running', 'finalizing')",
            params![run_token, project_id],
        )
        .await?
            == 1;
        if known_worker && !fenced_worker {
            anyhow::bail!(
                "Codex session {codex_session_id} worker generation no longer owns its lease"
            );
        }
        if fenced_worker
            && query_count(
                &transaction,
                "SELECT COUNT(*) FROM session_controls
                  WHERE project_id = ?1 AND codex_session_id = ?2
                    AND run_token IS NOT NULL AND run_token <> ?3",
                params![project_id, codex_session_id, run_token],
            )
            .await?
                > 0
        {
            anyhow::bail!(
                "Codex session {codex_session_id} belongs to a different active run generation"
            );
        }
        let changed = transaction.execute(
            "INSERT INTO session_controls (
                project_id, codex_session_id, state, child_pid, run_token,
                interactive_holder, stdout_path, stderr_path, updated_at
             ) VALUES (?1, ?2, 'running', ?3, ?4, NULL, ?5, ?6, ?7)
             ON CONFLICT(project_id, codex_session_id) DO UPDATE SET
                state = CASE
                    WHEN session_controls.run_token = excluded.run_token
                     AND session_controls.state IN ('stop_requested', 'interrupt_requested')
                        THEN session_controls.state
                    ELSE 'running'
                END,
                child_pid = excluded.child_pid,
                run_token = excluded.run_token,
                interactive_holder = CASE
                    WHEN session_controls.run_token = excluded.run_token
                     AND session_controls.state = 'interrupt_requested'
                        THEN session_controls.interactive_holder
                    ELSE NULL
                END,
                stdout_path = excluded.stdout_path,
                stderr_path = excluded.stderr_path,
                updated_at = excluded.updated_at",
            params![
                project_id,
                codex_session_id,
                i64::from(child_pid),
                run_token,
                stdout_path.to_string_lossy().as_ref(),
                stderr_path.to_string_lossy().as_ref(),
                agent_timestamp()
            ],
        )
        .await
        .with_context(|| {
            format!(
                "Failed to mark Codex session {codex_session_id} running for project {project_id}"
            )
        })?;
        if changed != 1 {
            anyhow::bail!(
                "Codex session {codex_session_id} belongs to a different active run generation"
            );
        }
        if let Some(git_mode) = git_mode {
            if git_mode == AgentGitMode::Off {
                anyhow::bail!("An atomic Git session registration cannot use Git mode off");
            }
            let created_at = agent_timestamp();
            let inserted = transaction
                .execute(
                    "INSERT OR IGNORE INTO git_finalizations (
                        project_id, codex_session_id, state, git_mode, starting_head,
                        branch_ref, upstream_ref, worktree_baseline, task_identity,
                        owner_run_token, commit_oid, generation, last_error,
                        created_at, updated_at, completed_at
                     )
                     SELECT launch.project_id, ?2, 'working', launch.git_mode,
                            launch.starting_head, launch.branch_ref, launch.upstream_ref,
                            launch.worktree_baseline, NULL, launch.run_token, NULL, 0, NULL,
                            ?5, ?5, NULL
                       FROM agent_git_launch_states launch
                      WHERE launch.project_id = ?1 AND launch.run_token = ?3
                        AND launch.git_mode = ?4",
                    params![
                        project_id,
                        codex_session_id,
                        run_token,
                        git_mode.database_value(),
                        created_at.as_str(),
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to atomically create the Git journal for Codex session {codex_session_id}"
                    )
                })?;
            let compatible_journal = query_count(
                &transaction,
                "SELECT COUNT(*)
                   FROM git_finalizations
                  WHERE project_id = ?1 AND codex_session_id = ?2
                    AND state = 'working' AND git_mode = ?3
                    AND owner_run_token = ?4",
                params![
                    project_id,
                    codex_session_id,
                    git_mode.database_value(),
                    run_token,
                ],
            )
            .await?
                == 1;
            if !compatible_journal
                || (inserted == 0
                    && query_count(
                        &transaction,
                        "SELECT COUNT(*)
                       FROM git_finalizations finalization
                       LEFT JOIN agent_git_launch_states launch
                         ON launch.project_id = finalization.project_id
                        AND launch.run_token = finalization.owner_run_token
                      WHERE finalization.project_id = ?1
                        AND finalization.codex_session_id = ?2
                        AND finalization.state = 'working'
                        AND finalization.git_mode = ?3
                        AND finalization.owner_run_token = ?4
                        AND (launch.run_token IS NULL OR (
                            finalization.git_mode = launch.git_mode
                            AND finalization.starting_head = launch.starting_head
                            AND finalization.branch_ref IS launch.branch_ref
                            AND finalization.upstream_ref IS launch.upstream_ref
                            AND finalization.worktree_baseline = launch.worktree_baseline
                        ))",
                        params![
                            project_id,
                            codex_session_id,
                            git_mode.database_value(),
                            run_token,
                        ],
                    )
                    .await?
                        != 1)
            {
                anyhow::bail!(
                    "Automated run {run_token} has no compatible scheduler-owned Git launch state for Codex session {codex_session_id}"
                );
            }
            if inserted == 1 {
                let deleted = transaction
                    .execute(
                        "DELETE FROM agent_git_launch_states
                          WHERE project_id = ?1 AND run_token = ?2",
                        params![project_id, run_token],
                    )
                    .await
                    .context("Failed to consume the scheduler-owned Git launch state")?;
                if deleted != 1 {
                    anyhow::bail!(
                        "Automated run {run_token} lost its Git launch state while registering Codex session {codex_session_id}"
                    );
                }
            }
        }
        transaction.commit().await.with_context(|| {
            format!(
                "Failed to commit Codex session {codex_session_id} registration for project {project_id}"
            )
        })?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_session_control_state_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        state: AgentSessionControlState,
    ) -> Result<()> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.set_session_control_state(project_id, codex_session_id, state))
    }

    #[cfg(test)]
    async fn set_session_control_state(
        &self,
        project_id: i64,
        codex_session_id: &str,
        state: AgentSessionControlState,
    ) -> Result<()> {
        let conn = self.connect().await?;
        conn.execute(
            "INSERT INTO session_controls (
                project_id, codex_session_id, state, child_pid, updated_at
             ) VALUES (?1, ?2, ?3, NULL, ?4)
             ON CONFLICT(project_id, codex_session_id) DO UPDATE SET
                state = excluded.state,
                updated_at = excluded.updated_at",
            params![
                project_id,
                codex_session_id,
                state.database_value(),
                agent_timestamp()
            ],
        )
        .await
        .with_context(|| {
            format!(
                "Failed to set Codex session {codex_session_id} state to {} for project {project_id}",
                state.database_value()
            )
        })?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_session_control_recovery_token_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        run_token: &str,
    ) -> Result<()> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                conn.execute(
                    "INSERT INTO session_controls (
                        project_id, codex_session_id, state, child_pid, run_token,
                        interactive_holder, interactive_launch_token, updated_at
                     ) VALUES (?1, ?2, 'resume_requested', NULL, ?3, NULL, NULL, ?4)
                     ON CONFLICT(project_id, codex_session_id) DO UPDATE SET
                        state = 'resume_requested', child_pid = NULL, run_token = excluded.run_token,
                        interactive_holder = NULL, interactive_launch_token = NULL,
                        updated_at = excluded.updated_at",
                    params![project_id, codex_session_id, run_token, agent_timestamp()],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to set the test recovery token for Codex session {codex_session_id}"
                    )
                })?;
                Ok(())
            })
    }

    pub(crate) fn request_session_interrupt_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_child_pid: u32,
        expected_run_token: &str,
        interactive_holder: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
        let conn = self.connect().await?;
                let changed = conn.execute(
                    "UPDATE session_controls
                        SET state = 'interrupt_requested',
                            interactive_holder = ?1,
                            updated_at = ?2
                      WHERE project_id = ?3 AND codex_session_id = ?4
                        AND state = 'running' AND child_pid = ?5
                        AND run_token = ?6",
                    params![
                        interactive_holder,
                        agent_timestamp(),
                        project_id,
                        codex_session_id,
                        i64::from(expected_child_pid),
                        expected_run_token
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to request interactive interruption for Codex session {codex_session_id}"
                    )
                })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn cancel_session_interrupt_handoff_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        interactive_holder: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
        let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE session_controls
                            SET state = CASE
                                    WHEN state = 'interrupt_requested' THEN 'running'
                                    ELSE 'resume_requested'
                                END,
                                child_pid = CASE
                                    WHEN state = 'interrupt_requested' THEN child_pid
                                    ELSE NULL
                                END,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL,
                                updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state IN ('interrupt_requested', 'ready_interactive')
                            AND interactive_holder = ?4",
                        params![
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            interactive_holder
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to cancel interactive handoff for Codex session {codex_session_id}"
                        )
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn reserve_idle_session_interactive_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        interactive_holder: &str,
        expected_stopped_run_token: Option<&str>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let restore_stopped =
                    interactive_holder.starts_with("clt-stopped-interactive-");
                let changed = if restore_stopped {
                    conn.execute(
                        "UPDATE session_controls
                            SET state = 'ready_interactive', child_pid = NULL,
                                interactive_holder = ?1,
                                interactive_launch_token = NULL, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'stopped'
                            AND (
                                run_token = ?5
                                OR (run_token IS NULL AND ?5 IS NULL)
                            )
                            AND NOT EXISTS (
                                SELECT 1 FROM session_controls
                                 WHERE project_id = ?3
                                   AND codex_session_id <> ?4
                                   AND state <> 'stopped'
                            )",
                        params![
                            interactive_holder,
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            expected_stopped_run_token
                        ],
                    )
                    .await
                } else {
                    conn.execute(
                        "INSERT INTO session_controls (
                            project_id, codex_session_id, state, child_pid,
                            interactive_holder, updated_at
                         )
                         SELECT ?1, ?2, 'ready_interactive', NULL, ?3, ?4
                          WHERE NOT EXISTS (
                            SELECT 1 FROM session_controls
                             WHERE project_id = ?1 AND codex_session_id = ?2
                          )
                            AND NOT EXISTS (
                            SELECT 1 FROM session_controls
                             WHERE project_id = ?1 AND state <> 'stopped'
                          )
                         ON CONFLICT(project_id, codex_session_id) DO NOTHING",
                        params![
                            project_id,
                            codex_session_id,
                            interactive_holder,
                            agent_timestamp()
                        ],
                    )
                    .await
                }
                    .with_context(|| {
                        format!(
                            "Failed to reserve idle Codex session {codex_session_id} for interactive use"
                        )
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn reserve_shared_session_interactive_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        interactive_holder: &str,
        expected_stopped_run_token: Option<&str>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to begin reserving shared interactive session {codex_session_id}"
                        )
                    })?;
                let git_boundary_conflict = query_count(
                    &transaction,
                    "SELECT COUNT(*)
                       FROM projects p
                      WHERE p.id = ?1
                        AND (
                            EXISTS (
                                SELECT 1 FROM agent_git_launch_states launch
                                 WHERE launch.project_id = p.id
                            )
                            OR EXISTS (
                                SELECT 1 FROM git_finalizations finalization
                                 WHERE finalization.project_id = p.id
                                   AND finalization.state NOT IN ('completed', 'cancelled')
                            )
                            OR (
                                p.git_mode <> 'off'
                                AND EXISTS (
                                    SELECT 1 FROM leases
                                     WHERE leases.project_id = p.id
                                )
                            )
                        )",
                    [project_id],
                )
                .await?
                    != 0;
                if git_boundary_conflict {
                    transaction.commit().await.with_context(|| {
                        format!(
                            "Failed to finish rejecting unsafe shared interactive session {codex_session_id}"
                        )
                    })?;
                    return Ok(false);
                }
                let restore_stopped =
                    is_stopped_shared_interactive_holder(interactive_holder);
                let changed = if restore_stopped {
                    transaction.execute(
                        "UPDATE session_controls
                            SET state = 'ready_interactive', child_pid = NULL,
                                interactive_holder = ?1,
                                interactive_launch_token = NULL, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'stopped'
                            AND (
                                run_token = ?5
                                OR (run_token IS NULL AND ?5 IS NULL)
                            )
                            AND (
                                EXISTS (
                                    SELECT 1 FROM leases WHERE project_id = ?3
                                )
                                OR EXISTS (
                                    SELECT 1 FROM session_controls
                                     WHERE project_id = ?3
                                       AND codex_session_id <> ?4
                                       AND state <> 'stopped'
                                )
                            )",
                        params![
                            interactive_holder,
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            expected_stopped_run_token
                        ],
                    )
                    .await
                } else {
                    transaction.execute(
                        "INSERT INTO session_controls (
                            project_id, codex_session_id, state, child_pid,
                            interactive_holder, updated_at
                         )
                         SELECT ?1, ?2, 'ready_interactive', NULL, ?3, ?4
                          WHERE NOT EXISTS (
                            SELECT 1 FROM session_controls
                             WHERE project_id = ?1 AND codex_session_id = ?2
                          )
                            AND (
                                EXISTS (
                                    SELECT 1 FROM leases WHERE project_id = ?1
                                )
                                OR EXISTS (
                                    SELECT 1 FROM session_controls
                                     WHERE project_id = ?1
                                       AND codex_session_id <> ?2
                                       AND state <> 'stopped'
                                )
                            )
                         ON CONFLICT(project_id, codex_session_id) DO NOTHING",
                        params![
                            project_id,
                            codex_session_id,
                            interactive_holder,
                            agent_timestamp()
                        ],
                    )
                    .await
                }
                .with_context(|| {
                    format!(
                        "Failed to reserve Codex session {codex_session_id} for shared interactive use"
                    )
                })?;
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit shared interactive session {codex_session_id} reservation"
                    )
                })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn cancel_idle_session_interactive_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        interactive_holder: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let restore_stopped = interactive_holder
                    .starts_with("clt-stopped-interactive-")
                    || is_stopped_shared_interactive_holder(interactive_holder);
                let changed = if restore_stopped {
                    conn.execute(
                        "UPDATE session_controls
                            SET state = 'stopped', child_pid = NULL,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL, updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'ready_interactive'
                            AND interactive_holder = ?4",
                        params![
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            interactive_holder
                        ],
                    )
                    .await
                } else {
                    conn.execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state = 'ready_interactive'
                            AND interactive_holder = ?3",
                        params![project_id, codex_session_id, interactive_holder],
                    )
                    .await
                }
                    .with_context(|| {
                        format!(
                            "Failed to cancel idle interactive reservation for Codex session {codex_session_id}"
                        )
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn request_session_stop_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_child_pid: u32,
        expected_run_token: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE session_controls
                            SET state = 'stop_requested', updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'running' AND child_pid = ?4
                            AND run_token = ?5",
                        params![
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            i64::from(expected_child_pid),
                            expected_run_token
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to request stop for Codex session {codex_session_id}")
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn request_interactive_session_stop_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_child_pid: u32,
        expected_interactive_holder: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE session_controls
                            SET state = 'stop_requested', updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'interactive' AND child_pid = ?4
                            AND interactive_holder = ?5
                            AND interactive_launch_token = ?5",
                        params![
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            i64::from(expected_child_pid),
                            expected_interactive_holder
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to request stop for interactive Codex session {codex_session_id}"
                        )
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn request_stopped_session_resume_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        expected_run_token: Option<&str>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE session_controls
                            SET state = 'resume_requested', child_pid = NULL,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL, updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'stopped'
                            AND (
                                run_token = ?4
                                OR (run_token IS NULL AND ?4 IS NULL)
                            )",
                        params![
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            expected_run_token
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to request resume for stopped Codex session {codex_session_id}"
                        )
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn transition_session_control_state_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        from: AgentSessionControlState,
        to: AgentSessionControlState,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE session_controls
                            SET state = ?1,
                                child_pid = CASE WHEN ?1 = 'running' THEN child_pid ELSE NULL END,
                                interactive_launch_token = NULL,
                                updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4 AND state = ?5",
                        params![
                            to.database_value(),
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            from.database_value()
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to transition Codex session {codex_session_id} from {} to {}",
                            from.database_value(),
                            to.database_value()
                        )
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn recover_stale_automated_session_control_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        from: AgentSessionControlState,
        to: AgentSessionControlState,
        expected_child_pid: u32,
        expected_run_token: Option<&str>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE session_controls
                            SET state = ?1, child_pid = NULL,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = ?5 AND child_pid = ?6
                            AND (
                                run_token = ?7
                                OR (run_token IS NULL AND ?7 IS NULL)
                            )",
                        params![
                            to.database_value(),
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            from.database_value(),
                            i64::from(expected_child_pid),
                            expected_run_token
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to recover stale Codex session {codex_session_id} from {} to {}",
                            from.database_value(),
                            to.database_value()
                        )
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn finalize_reaped_automated_session_blocking(
        &self,
        project_id: i64,
        expected_child_pid: u32,
        expected_run_token: &str,
        lease_holder: &str,
        lease_timeout_seconds: u64,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let transaction = conn.transaction().await.with_context(|| {
                    format!(
                        "Failed to begin reaped automated-session finalization for project {project_id}"
                    )
                })?;
                let (codex_session_id, state, interactive_holder) = {
                    let mut rows = transaction
                        .query(
                            "SELECT codex_session_id, state, interactive_holder
                               FROM session_controls
                              WHERE project_id = ?1 AND child_pid = ?2 AND run_token = ?3",
                            params![
                                project_id,
                                i64::from(expected_child_pid),
                                expected_run_token
                            ],
                        )
                        .await
                        .context("Failed to read the reaped automated session generation")?;
                    let Some(row) = rows
                        .next()
                        .await
                        .context("Failed to read the reaped automated session row")?
                    else {
                        return Ok(false);
                    };
                    (
                        row_text(&row, 0, "codex_session_id")?,
                        AgentSessionControlState::from_database(&row_text(&row, 1, "state")?)?,
                        row_optional_text(&row, 2, "interactive_holder")?,
                    )
                };

                let terminal_state = match state {
                    AgentSessionControlState::Running => {
                        AgentSessionControlState::ResumeRequested
                    }
                    AgentSessionControlState::StopRequested => {
                        AgentSessionControlState::Stopped
                    }
                    AgentSessionControlState::InterruptRequested => {
                        AgentSessionControlState::ReadyInteractive
                    }
                    _ => return Ok(false),
                };

                if terminal_state == AgentSessionControlState::ReadyInteractive {
                    let Some(interactive_holder) = interactive_holder.as_deref() else {
                        return Ok(false);
                    };
                    let acquired_at = agent_timestamp();
                    let expires_at = agent_timestamp_after(lease_timeout_seconds);
                    let transferred = transaction
                        .execute(
                            "UPDATE leases
                                SET holder = ?1, acquired_at = ?2, expires_at = ?3
                              WHERE project_id = ?4 AND holder = ?5",
                            params![
                                interactive_holder,
                                acquired_at.as_str(),
                                expires_at.as_str(),
                                project_id,
                                lease_holder
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to transfer the reaped project {project_id} lease for interactive handoff"
                            )
                        })?;
                    if transferred == 0 {
                        let inserted = transaction
                            .execute(
                                "INSERT OR IGNORE INTO leases (
                                    project_id, holder, acquired_at, expires_at
                                 ) VALUES (?1, ?2, ?3, ?4)",
                                params![
                                    project_id,
                                    interactive_holder,
                                    acquired_at.as_str(),
                                    expires_at.as_str()
                                ],
                            )
                            .await
                            .with_context(|| {
                                format!(
                                    "Failed to acquire the reaped project {project_id} lease for interactive handoff"
                                )
                            })?;
                        if inserted == 0 {
                            let existing_holder = {
                                let mut rows = transaction
                                    .query(
                                        "SELECT holder FROM leases WHERE project_id = ?1",
                                        [project_id],
                                    )
                                    .await
                                    .with_context(|| {
                                        format!(
                                            "Failed to inspect the reaped project {project_id} lease"
                                        )
                                    })?;
                                rows.next()
                                    .await
                                    .context("Failed to read the reaped project lease")?
                                    .map(|row| row_text(&row, 0, "holder"))
                                    .transpose()?
                            };
                            if existing_holder.as_deref() != Some(interactive_holder) {
                                return Ok(false);
                            }
                        }
                    }
                }

                let changed = transaction
                    .execute(
                        "UPDATE session_controls
                            SET state = ?1, child_pid = NULL,
                                interactive_holder = CASE
                                    WHEN ?1 = 'ready_interactive' THEN interactive_holder
                                    ELSE NULL
                                END,
                                updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = ?5 AND child_pid = ?6 AND run_token = ?7",
                        params![
                            terminal_state.database_value(),
                            agent_timestamp(),
                            project_id,
                            codex_session_id.as_str(),
                            state.database_value(),
                            i64::from(expected_child_pid),
                            expected_run_token
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to finalize reaped Codex session {codex_session_id}"
                        )
                    })?;
                if changed != 1 {
                    return Ok(false);
                }
                if terminal_state != AgentSessionControlState::ReadyInteractive {
                    transaction
                        .execute(
                            "DELETE FROM leases WHERE project_id = ?1 AND holder = ?2",
                            params![project_id, lease_holder],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to release the reaped project {project_id} lease"
                            )
                        })?;
                }
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit reaped automated-session finalization for project {project_id}"
                    )
                })?;
                Ok(true)
            })
    }

    pub(crate) fn recover_stale_interactive_session_control_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        from: AgentSessionControlState,
        to: AgentSessionControlState,
        expected_interactive_holder: Option<&str>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE session_controls
                            SET state = ?1, child_pid = NULL,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = ?5
                            AND (
                                interactive_holder = ?6
                                OR (interactive_holder IS NULL AND ?6 IS NULL)
                            )",
                        params![
                            to.database_value(),
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            from.database_value(),
                            expected_interactive_holder
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to recover abandoned Codex interactive handoff {codex_session_id} from {} to {}",
                            from.database_value(),
                            to.database_value()
                        )
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn begin_stopped_session_interactive_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        interactive_holder: &str,
        expected_run_token: Option<&str>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE session_controls
                            SET state = 'ready_interactive', child_pid = NULL,
                                interactive_holder = ?1,
                                interactive_launch_token = NULL, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'stopped'
                            AND (
                                run_token = ?5
                                OR (run_token IS NULL AND ?5 IS NULL)
                            )",
                        params![
                            interactive_holder,
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                            expected_run_token
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to open stopped Codex session {codex_session_id} interactively"
                        )
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn adopt_interactive_guardian_blocking(
        &self,
        project_id: i64,
        codex_session_id: Option<&str>,
        from_holder: &str,
        guardian_holder: &str,
        lease_timeout_seconds: u64,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let disposition = InteractiveGuardianDisposition::from_guardian_holder(
                    guardian_holder,
                )
                .context("Invalid interactive guardian holder")?;
                let mut conn = self.connect().await?;
                let transaction = conn.transaction().await.with_context(|| {
                    format!("Failed to begin interactive guardian for project {project_id}")
                })?;
                if disposition.holds_project_lease() {
                    let acquired_at = agent_timestamp();
                    let expires_at = agent_timestamp_after(lease_timeout_seconds);
                    let transferred = transaction
                        .execute(
                            "UPDATE leases
                                SET holder = ?1, acquired_at = ?2, expires_at = ?3
                              WHERE project_id = ?4 AND holder = ?5",
                            params![
                                guardian_holder,
                                acquired_at,
                                expires_at,
                                project_id,
                                from_holder
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to transfer project {project_id} lease to its interactive guardian"
                            )
                        })?;
                    if transferred == 0 {
                        return Ok(false);
                    }
                }
                if let Some(codex_session_id) = codex_session_id {
                    let changed = transaction
                        .execute(
                            "UPDATE session_controls
                                SET state = 'interactive', interactive_holder = ?1,
                                    interactive_launch_token = ?1, child_pid = NULL,
                                    updated_at = ?2
                              WHERE project_id = ?3 AND codex_session_id = ?4
                                AND state = 'ready_interactive'
                                AND interactive_holder = ?5",
                            params![
                                guardian_holder,
                                agent_timestamp(),
                                project_id,
                                codex_session_id,
                                from_holder
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to transfer Codex session {codex_session_id} to its interactive guardian"
                            )
                        })?;
                    if changed == 0 {
                        return Ok(false);
                    }
                }
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit project {project_id} interactive guardian transfer"
                    )
                })?;
                Ok(true)
            })
    }

    pub(crate) fn register_interactive_guardian_child_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        guardian_holder: &str,
        child_pid: u32,
        lease_timeout_seconds: u64,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let disposition = InteractiveGuardianDisposition::from_guardian_holder(
                    guardian_holder,
                )
                .context("Invalid interactive guardian holder")?;
                let mut conn = self.connect().await?;
                let now = agent_timestamp();
                let transaction = conn.transaction().await.with_context(|| {
                    format!(
                        "Failed to begin interactive child registration for project {project_id}"
                    )
                })?;
                if disposition.holds_project_lease() {
                    let fresh_expiry = agent_timestamp_after(lease_timeout_seconds);
                    let lease_changed = transaction
                        .execute(
                            "UPDATE leases SET expires_at = ?1
                              WHERE project_id = ?2 AND holder = ?3
                                AND expires_at > ?4",
                            params![
                                fresh_expiry.as_str(),
                                project_id,
                                guardian_holder,
                                now.as_str()
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to renew interactive guardian lease for project {project_id}"
                            )
                        })?;
                    if lease_changed != 1 {
                        return Ok(false);
                    }
                }
                let control_changed = transaction
                    .execute(
                        "UPDATE session_controls
                            SET child_pid = ?1, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'interactive'
                            AND interactive_holder = ?5
                            AND interactive_launch_token = ?5
                            AND child_pid IS NULL",
                        params![
                            i64::from(child_pid),
                            now.as_str(),
                            project_id,
                            codex_session_id,
                            guardian_holder
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to register interactive Codex child {child_pid} for session {codex_session_id}"
                        )
                    })?;
                if control_changed != 1 {
                    return Ok(false);
                }
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit interactive Codex child registration for project {project_id}"
                    )
                })?;
                Ok(true)
            })
    }

    pub(crate) fn finish_interactive_guardian_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        guardian_holder: &str,
        disposition: InteractiveGuardianDisposition,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let transaction = conn.transaction().await.with_context(|| {
                    format!("Failed to finish interactive guardian for project {project_id}")
                })?;
                let changed = match disposition {
                    InteractiveGuardianDisposition::ResumeExec => transaction
                        .execute(
                            "UPDATE session_controls
                                SET state = CASE
                                        WHEN state = 'stop_requested' THEN 'stopped'
                                        ELSE 'resume_requested'
                                    END,
                                    child_pid = NULL,
                                    interactive_holder = NULL,
                                    interactive_launch_token = NULL, updated_at = ?1
                              WHERE project_id = ?2 AND codex_session_id = ?3
                                AND state IN ('interactive', 'stop_requested')
                                AND interactive_holder = ?4",
                            params![
                                agent_timestamp(),
                                project_id,
                                codex_session_id,
                                guardian_holder
                            ],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to hand Codex session {codex_session_id} back to exec mode"
                            )
                        })?,
                    InteractiveGuardianDisposition::PreserveIdleSession
                    | InteractiveGuardianDisposition::PreserveSharedSession
                    | InteractiveGuardianDisposition::RestoreStopped
                    | InteractiveGuardianDisposition::RestoreStoppedShared => {
                        transaction.execute(
                            "UPDATE session_controls
                                SET state = 'stopped', child_pid = NULL,
                                    interactive_holder = NULL,
                                    interactive_launch_token = NULL, updated_at = ?1
                              WHERE project_id = ?2 AND codex_session_id = ?3
                                AND state IN ('interactive', 'stop_requested')
                                AND interactive_holder = ?4",
                            params![
                                agent_timestamp(),
                                project_id,
                                codex_session_id,
                                guardian_holder
                            ],
                        )
                        .await.with_context(|| {
                            format!(
                                "Failed to preserve Codex session {codex_session_id} after interactive use"
                            )
                        })?
                    }
                };
                if changed == 0 {
                    return Ok(false);
                }
                if disposition.holds_project_lease() {
                    let _released = transaction
                        .execute(
                            "DELETE FROM leases WHERE project_id = ?1 AND holder = ?2",
                            params![project_id, guardian_holder],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to release project {project_id} interactive guardian lease"
                            )
                        })?;
                }
                // The child is already reaped before this transaction begins. A
                // missing exact-holder lease can only mean it expired or was
                // independently cleared; the generation-scoped control CAS above
                // is the authority for both an `i` handback and a completed `c`
                // reservation. Releasing an already-gone lease is complete.
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit project {project_id} interactive guardian completion"
                    )
                })?;
                Ok(true)
            })
    }

    pub(crate) fn recover_stale_interactive_guardian_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        guardian_holder: &str,
        expected_child_pid: Option<u32>,
        disposition: InteractiveGuardianDisposition,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let transaction = conn.transaction().await.with_context(|| {
                    format!(
                        "Failed to begin stale interactive guardian recovery for project {project_id}"
                    )
                })?;
                let changed = match disposition {
                    InteractiveGuardianDisposition::ResumeExec => transaction
                        .execute(
                            "UPDATE session_controls
                                SET state = CASE
                                        WHEN state = 'stop_requested' THEN 'stopped'
                                        ELSE 'resume_requested'
                                    END,
                                    child_pid = NULL,
                                    interactive_holder = NULL,
                                    interactive_launch_token = NULL, updated_at = ?1
                              WHERE project_id = ?2 AND codex_session_id = ?3
                                AND state IN ('interactive', 'stop_requested')
                                AND interactive_holder = ?4
                                AND interactive_launch_token = ?4
                                AND (
                                    child_pid = ?5
                                    OR (child_pid IS NULL AND ?5 IS NULL)
                                )",
                            params![
                                agent_timestamp(),
                                project_id,
                                codex_session_id,
                                guardian_holder,
                                expected_child_pid.map(i64::from)
                            ],
                        )
                        .await,
                    InteractiveGuardianDisposition::PreserveIdleSession
                    | InteractiveGuardianDisposition::PreserveSharedSession
                    | InteractiveGuardianDisposition::RestoreStopped
                    | InteractiveGuardianDisposition::RestoreStoppedShared => {
                        transaction.execute(
                            "UPDATE session_controls
                                SET state = 'stopped', child_pid = NULL,
                                    interactive_holder = NULL,
                                    interactive_launch_token = NULL, updated_at = ?1
                              WHERE project_id = ?2 AND codex_session_id = ?3
                                AND state IN ('interactive', 'stop_requested')
                                AND interactive_holder = ?4
                                AND interactive_launch_token = ?4
                                AND (
                                    child_pid = ?5
                                    OR (child_pid IS NULL AND ?5 IS NULL)
                                )",
                            params![
                                agent_timestamp(),
                                project_id,
                                codex_session_id,
                                guardian_holder,
                                expected_child_pid.map(i64::from)
                            ],
                        )
                        .await
                    }
                }
                .with_context(|| {
                    format!(
                        "Failed to recover stale interactive guardian for Codex session {codex_session_id}"
                    )
                })?;
                if changed != 1 {
                    return Ok(false);
                }
                if disposition.holds_project_lease() {
                    let _ = transaction
                        .execute(
                            "DELETE FROM leases WHERE project_id = ?1 AND holder = ?2",
                            params![project_id, guardian_holder],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to release stale interactive guardian lease for project {project_id}"
                            )
                        })?;
                }
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit stale interactive guardian recovery for project {project_id}"
                    )
                })?;
                Ok(true)
            })
    }

    pub(crate) fn complete_session_stop_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        run_token: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE session_controls
                            SET state = 'stopped', child_pid = NULL,
                                interactive_holder = NULL,
                                interactive_launch_token = NULL, updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'stop_requested' AND run_token = ?4
                            AND (
                                NOT EXISTS (
                                    SELECT 1 FROM agent_workers WHERE worker_token = ?4
                                )
                                OR EXISTS (
                                    SELECT 1 FROM agent_workers w
                                    JOIN leases l ON l.project_id = w.project_id
                                                 AND l.holder = w.lease_holder
                                    WHERE w.worker_token = ?4 AND w.project_id = ?2
                                      AND w.state IN ('dispatching', 'running', 'finalizing')
                                )
                            )",
                        params![agent_timestamp(), project_id, codex_session_id, run_token],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to finish stopping Codex session {codex_session_id}")
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn complete_session_interrupt_handoff_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        run_token: &str,
        from_holder: &str,
        lease_timeout_seconds: u64,
    ) -> Result<Option<String>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let transaction = conn.transaction().await.with_context(|| {
                    format!(
                        "Failed to begin interactive handoff for Codex session {codex_session_id}"
                    )
                })?;
                let interactive_holder = {
                    let mut rows = transaction
                        .query(
                            "SELECT interactive_holder
                               FROM session_controls
                              WHERE project_id = ?1 AND codex_session_id = ?2
                                AND state = 'interrupt_requested' AND run_token = ?3",
                            params![project_id, codex_session_id, run_token],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to read interactive handoff for Codex session {codex_session_id}"
                            )
                        })?;
                    let Some(row) = rows
                        .next()
                        .await
                        .context("Failed to read interactive handoff row")?
                    else {
                        return Ok(None);
                    };
                    row_optional_text(&row, 0, "interactive_holder")?
                };
                let Some(interactive_holder) = interactive_holder else {
                    return Ok(None);
                };
                let acquired_at = agent_timestamp();
                let expires_at = agent_timestamp_after(lease_timeout_seconds);
                let transferred = transaction
                    .execute(
                        "UPDATE leases
                            SET holder = ?1, acquired_at = ?2, expires_at = ?3
                          WHERE project_id = ?4 AND holder = ?5",
                        params![
                            interactive_holder.as_str(),
                            acquired_at,
                            expires_at,
                            project_id,
                            from_holder
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to transfer project {project_id} lease for interactive handoff"
                        )
                    })?;
                if transferred == 0 {
                    return Ok(None);
                }
                let changed = transaction
                    .execute(
                        "UPDATE session_controls
                            SET state = 'ready_interactive', child_pid = NULL,
                                interactive_launch_token = NULL, updated_at = ?1
                          WHERE project_id = ?2 AND codex_session_id = ?3
                            AND state = 'interrupt_requested' AND run_token = ?4",
                        params![agent_timestamp(), project_id, codex_session_id, run_token],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to mark Codex session {codex_session_id} ready for interactive handoff"
                        )
                    })?;
                if changed == 0 {
                    anyhow::bail!(
                        "Codex session {codex_session_id} changed during interactive handoff"
                    );
                }
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit interactive handoff for Codex session {codex_session_id}"
                    )
                })?;
                Ok(Some(interactive_holder))
            })
    }

    pub(crate) fn session_control_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<AgentSessionControlRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.session_control(project_id, codex_session_id))
    }

    async fn session_control(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<AgentSessionControlRecord>> {
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT project_id, codex_session_id, state, child_pid, run_token,
                        interactive_holder, interactive_launch_token,
                        stdout_path, stderr_path, updated_at
                   FROM session_controls
                  WHERE project_id = ?1 AND codex_session_id = ?2",
                params![project_id, codex_session_id],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to read Codex session {codex_session_id} control for project {project_id}"
                )
            })?;
        let Some(row) = rows
            .next()
            .await
            .context("Failed to read Codex session control row")?
        else {
            return Ok(None);
        };
        let child_pid = row_optional_integer(&row, 3, "child_pid")?
            .map(u32::try_from)
            .transpose()
            .context("Codex session child PID is outside the supported range")?;
        Ok(Some(AgentSessionControlRecord {
            project_id: row_integer(&row, 0, "project_id")?,
            codex_session_id: row_text(&row, 1, "codex_session_id")?,
            state: AgentSessionControlState::from_database(&row_text(&row, 2, "state")?)?,
            child_pid,
            run_token: row_optional_text(&row, 4, "run_token")?,
            interactive_holder: row_optional_text(&row, 5, "interactive_holder")?,
            interactive_launch_token: row_optional_text(&row, 6, "interactive_launch_token")?,
            stdout_path: row_optional_text(&row, 7, "stdout_path")?,
            stderr_path: row_optional_text(&row, 8, "stderr_path")?,
            updated_at: row_text(&row, 9, "updated_at")?,
        }))
    }

    pub(crate) fn session_controls_for_project_blocking(
        &self,
        project_id: i64,
    ) -> Result<Vec<AgentSessionControlRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT project_id, codex_session_id, state, child_pid, run_token,
                                interactive_holder, interactive_launch_token,
                                stdout_path, stderr_path, updated_at
                           FROM session_controls
                          WHERE project_id = ?1
                          ORDER BY updated_at, codex_session_id",
                        [project_id],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to list Codex session controls for project {project_id}")
                    })?;
                let mut controls = Vec::new();
                while let Some(row) = rows
                    .next()
                    .await
                    .context("Failed to read Codex session control row")?
                {
                    let child_pid = row_optional_integer(&row, 3, "child_pid")?
                        .map(u32::try_from)
                        .transpose()
                        .context("Codex session child PID is outside the supported range")?;
                    controls.push(AgentSessionControlRecord {
                        project_id: row_integer(&row, 0, "project_id")?,
                        codex_session_id: row_text(&row, 1, "codex_session_id")?,
                        state: AgentSessionControlState::from_database(&row_text(
                            &row, 2, "state",
                        )?)?,
                        child_pid,
                        run_token: row_optional_text(&row, 4, "run_token")?,
                        interactive_holder: row_optional_text(&row, 5, "interactive_holder")?,
                        interactive_launch_token: row_optional_text(
                            &row,
                            6,
                            "interactive_launch_token",
                        )?,
                        stdout_path: row_optional_text(&row, 7, "stdout_path")?,
                        stderr_path: row_optional_text(&row, 8, "stderr_path")?,
                        updated_at: row_text(&row, 9, "updated_at")?,
                    });
                }
                Ok(controls)
            })
    }

    pub(crate) fn suspending_session_project_ids_blocking(&self) -> Result<HashSet<i64>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT DISTINCT project_id
                           FROM session_controls
                          WHERE state != 'stopped'",
                        (),
                    )
                    .await
                    .context("Failed to list projects suspended by Codex session controls")?;
                let mut project_ids = HashSet::new();
                while let Some(row) = rows
                    .next()
                    .await
                    .context("Failed to read suspended Codex session project")?
                {
                    project_ids.insert(row_integer(&row, 0, "project_id")?);
                }
                Ok(project_ids)
            })
    }

    pub(crate) fn resume_requested_session_blocking(
        &self,
        project_id: i64,
    ) -> Result<Option<String>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
        let conn = self.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT codex_session_id
                           FROM session_controls
                          WHERE project_id = ?1 AND state = 'resume_requested'
                          ORDER BY updated_at, codex_session_id
                          LIMIT 1",
                        [project_id],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to find a requested Codex session resume for project {project_id}"
                        )
                    })?;
                let Some(row) = rows
                    .next()
                    .await
                    .context("Failed to read requested Codex session resume")?
                else {
                    return Ok(None);
                };
                Ok(Some(row_text(&row, 0, "codex_session_id")?))
            })
    }

    pub(crate) fn ensure_pending_git_finalization_resume_requested_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to begin restoring the exact Git finalization session {codex_session_id}"
                        )
                    })?;
                let generation = {
                    let mut rows = transaction
                        .query(
                            "SELECT generation FROM git_finalizations
                              WHERE project_id = ?1 AND codex_session_id = ?2
                                AND state IN ('working', 'tracking', 'commit_pending', 'push_pending')",
                            params![project_id, codex_session_id],
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to inspect pending Git finalization {codex_session_id}"
                            )
                        })?;
                    rows.next()
                        .await
                        .context("Failed to read pending Git finalization generation")?
                        .map(|row| row_integer(&row, 0, "generation"))
                        .transpose()?
                };
                let Some(generation) = generation else {
                    transaction.commit().await.with_context(|| {
                        format!(
                            "Failed to finish restoring absent Git finalization session {codex_session_id}"
                        )
                    })?;
                    return Ok(false);
                };
                let recovery_token = format!(
                    "{AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX}{generation}"
                );
                transaction
                    .execute(
                        "UPDATE session_controls
                            SET run_token = ?1, updated_at = ?2
                          WHERE project_id = ?3 AND codex_session_id = ?4
                            AND state = 'resume_requested' AND child_pid IS NULL
                            AND interactive_holder IS NULL",
                        params![
                            recovery_token.as_str(),
                            agent_timestamp(),
                            project_id,
                            codex_session_id,
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to tag Git finalization recovery session {codex_session_id}"
                        )
                    })?;
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO session_controls (
                            project_id, codex_session_id, state, child_pid,
                            run_token, interactive_holder, interactive_launch_token, updated_at
                         ) VALUES (?1, ?2, 'resume_requested', NULL, ?3, NULL, NULL, ?4)",
                        params![
                            project_id,
                            codex_session_id,
                            recovery_token.as_str(),
                            agent_timestamp()
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to restore Git finalization session {codex_session_id}"
                        )
                    })?;
                let ready = query_count(
                    &transaction,
                    "SELECT COUNT(*) FROM session_controls
                      WHERE project_id = ?1 AND codex_session_id = ?2
                        AND state = 'resume_requested' AND child_pid IS NULL
                        AND interactive_holder IS NULL",
                    params![project_id, codex_session_id],
                )
                .await?
                    == 1;
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit restored Git finalization session {codex_session_id}"
                    )
                })?;
                Ok(ready)
            })
    }

    pub(crate) fn clear_orphaned_resume_requested_session_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        now: u64,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to begin clearing orphaned Codex session {codex_session_id} for project {project_id}"
                        )
                    })?;
                if query_count(
                    &transaction,
                    "SELECT COUNT(*) FROM agent_workers
                      WHERE project_id = ?1
                        AND state IN ('dispatching', 'running', 'finalizing')",
                    [project_id],
                )
                .await?
                    > 0
                    || query_count(
                        &transaction,
                        "SELECT COUNT(*) FROM leases
                          WHERE project_id = ?1
                            AND CAST(expires_at AS INTEGER) > CAST(?2 AS INTEGER)",
                        params![project_id, now.to_string()],
                    )
                    .await?
                        > 0
                {
                    return Ok(false);
                }
                let removed = transaction
                    .execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state = 'resume_requested' AND child_pid IS NULL
                            AND interactive_holder IS NULL",
                        params![project_id, codex_session_id],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to clear orphaned Codex session {codex_session_id} for project {project_id}"
                        )
                    })?;
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit clearing orphaned Codex session {codex_session_id} for project {project_id}"
                    )
                })?;
                Ok(removed > 0)
            })
    }

    pub(crate) fn register_known_session_with_child_blocking(
        &self,
        registration: AgentKnownSessionRegistration<'_>,
    ) -> Result<bool> {
        let AgentKnownSessionRegistration {
            project_id,
            codex_session_id,
            child_pid,
            run_token,
            stdout_path,
            stderr_path,
            lease_holder,
            lease_timeout_seconds,
            claim_requested_resume,
        } = registration;
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let now = agent_timestamp();
                let fresh_expiry = agent_timestamp_after(lease_timeout_seconds);
                let transaction = conn.transaction().await.with_context(|| {
                    format!("Failed to begin known-session registration for project {project_id}")
                })?;
                let lease_changed = transaction
                    .execute(
                        "UPDATE leases SET expires_at = ?1
                          WHERE project_id = ?2 AND holder = ?3
                            AND expires_at > ?4",
                        params![
                            fresh_expiry.as_str(),
                            project_id,
                            lease_holder,
                            now.as_str()
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to renew known-session lease for project {project_id}")
                    })?;
                if lease_changed != 1 {
                    return Ok(false);
                }
                let control_changed = if claim_requested_resume {
                    transaction
                        .execute(
                            "UPDATE session_controls
                                SET state = 'running', child_pid = ?1, run_token = ?2,
                                    interactive_holder = NULL,
                                    interactive_launch_token = NULL, stdout_path = ?3,
                                    stderr_path = ?4, updated_at = ?5
                              WHERE project_id = ?6 AND codex_session_id = ?7
                                AND state = 'resume_requested'",
                            params![
                                i64::from(child_pid),
                                run_token,
                                stdout_path.to_string_lossy().as_ref(),
                                stderr_path.to_string_lossy().as_ref(),
                                now.as_str(),
                                project_id,
                                codex_session_id,
                            ],
                        )
                        .await
                } else {
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO session_controls (
                                project_id, codex_session_id, state, child_pid, run_token,
                                interactive_holder, stdout_path, stderr_path, updated_at
                             ) VALUES (?1, ?2, 'running', ?3, ?4, NULL, ?5, ?6, ?7)",
                            params![
                                project_id,
                                codex_session_id,
                                i64::from(child_pid),
                                run_token,
                                stdout_path.to_string_lossy().as_ref(),
                                stderr_path.to_string_lossy().as_ref(),
                                now.as_str(),
                            ],
                        )
                        .await
                }
                .with_context(|| {
                    format!(
                        "Failed to register known Codex session {codex_session_id} before launch"
                    )
                })?;
                if control_changed != 1 {
                    return Ok(false);
                }
                transaction.commit().await.with_context(|| {
                    format!("Failed to commit known-session registration for project {project_id}")
                })?;
                Ok(true)
            })
    }

    pub(crate) fn clear_running_session_control_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
        run_token: Option<&str>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to begin clearing Codex session {codex_session_id} for project {project_id}"
                        )
                    })?;
                if run_token.is_none()
                    && query_count(
                        &transaction,
                        "SELECT COUNT(*) FROM agent_workers
                          WHERE project_id = ?1
                            AND state IN ('dispatching', 'running', 'finalizing')",
                        [project_id],
                    )
                    .await?
                        > 0
                {
                    return Ok(false);
                }
                if let Some(run_token) = run_token {
                    let known_worker = query_count(
                        &transaction,
                        "SELECT COUNT(*) FROM agent_workers WHERE worker_token = ?1",
                        [run_token],
                    )
                    .await?
                        == 1;
                    let fenced_worker = query_count(
                        &transaction,
                        "SELECT COUNT(*) FROM agent_workers w
                          JOIN leases l ON l.project_id = w.project_id
                                       AND l.holder = w.lease_holder
                         WHERE w.worker_token = ?1 AND w.project_id = ?2
                           AND w.state IN ('dispatching', 'running', 'finalizing')",
                        params![run_token, project_id],
                    )
                    .await?
                        == 1;
                    if known_worker && !fenced_worker {
                        return Ok(false);
                    }
                }
                let removed = transaction
                    .execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2 AND state = 'running'
                            AND (?3 IS NULL OR run_token = ?3)",
                        params![project_id, codex_session_id, run_token],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to clear running Codex session {codex_session_id} for project {project_id}"
                        )
                    })?;
                transaction.commit().await.with_context(|| {
                    format!(
                        "Failed to commit clearing Codex session {codex_session_id} for project {project_id}"
                    )
                })?;
                Ok(removed > 0)
            })
    }

    pub(crate) fn clear_autonomous_push_resume_request_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let removed = conn
                    .execute(
                        "DELETE FROM session_controls
                          WHERE project_id = ?1 AND codex_session_id = ?2
                            AND state = 'resume_requested' AND child_pid IS NULL
                            AND EXISTS (
                                SELECT 1 FROM git_finalizations
                                 WHERE project_id = ?1 AND codex_session_id = ?2
                                   AND state = 'push_pending'
                            )
                            AND NOT EXISTS (
                                SELECT 1 FROM agent_workers
                                 WHERE project_id = ?1
                                   AND state IN ('dispatching', 'running', 'finalizing')
                            )",
                        params![project_id, codex_session_id],
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to clear autonomous PushPending resume request for Codex session {codex_session_id}"
                        )
                    })?;
                Ok(removed == 1)
            })
    }

    pub(crate) fn lease_for_project_blocking(
        &self,
        project_id: i64,
    ) -> Result<Option<AgentLeaseRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.lease_for_project(project_id))
    }

    async fn lease_for_project(&self, project_id: i64) -> Result<Option<AgentLeaseRecord>> {
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT l.project_id, p.name, p.path, l.holder, l.acquired_at, l.expires_at
                 FROM leases l
                 JOIN projects p ON p.id = l.project_id
                 WHERE l.project_id = ?1",
                [project_id],
            )
            .await
            .with_context(|| format!("Failed to read lease for project {project_id}"))?;

        let Some(row) = rows
            .next()
            .await
            .context("Failed to read agent lease row")?
        else {
            return Ok(None);
        };

        Ok(Some(AgentLeaseRecord {
            project_id: row_integer(&row, 0, "project_id")?,
            project_name: row_text(&row, 1, "name")?,
            project_path: PathBuf::from(row_text(&row, 2, "path")?),
            holder: row_text(&row, 3, "holder")?,
            acquired_at: row_text(&row, 4, "acquired_at")?,
            expires_at: row_text(&row, 5, "expires_at")?,
        }))
    }

    pub(crate) fn list_active_leases_blocking(&self, now: &str) -> Result<Vec<AgentLeaseRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.list_active_leases(now))
    }

    async fn list_active_leases(&self, now: &str) -> Result<Vec<AgentLeaseRecord>> {
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT l.project_id, p.name, p.path, l.holder, l.acquired_at, l.expires_at
                 FROM leases l
                 JOIN projects p ON p.id = l.project_id
                 WHERE CAST(l.expires_at AS INTEGER) > CAST(?1 AS INTEGER)
                 ORDER BY CAST(l.expires_at AS INTEGER), p.name COLLATE NOCASE",
                [now],
            )
            .await
            .context("Failed to list active agent leases")?;
        let mut leases = Vec::new();

        while let Some(row) = rows.next().await.context("Failed to read lease row")? {
            leases.push(AgentLeaseRecord {
                project_id: row_integer(&row, 0, "project_id")?,
                project_name: row_text(&row, 1, "name")?,
                project_path: PathBuf::from(row_text(&row, 2, "path")?),
                holder: row_text(&row, 3, "holder")?,
                acquired_at: row_text(&row, 4, "acquired_at")?,
                expires_at: row_text(&row, 5, "expires_at")?,
            });
        }

        Ok(leases)
    }

    pub(crate) fn record_run_outcome_blocking(&self, outcome: AgentRunOutcome<'_>) -> Result<i64> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.record_run_outcome(outcome))
    }

    async fn record_run_outcome(&self, outcome: AgentRunOutcome<'_>) -> Result<i64> {
        let conn = self.connect().await?;

        conn.execute(
            "INSERT INTO runs (
                project_id, status, started_at, finished_at, exit_code,
                log_dir, stdout_path, stderr_path, summary, codex_session_id
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                outcome.project_id,
                outcome.status,
                outcome.started_at,
                outcome.finished_at,
                outcome.exit_code,
                outcome.log_dir,
                outcome.stdout_path,
                outcome.stderr_path,
                outcome.summary,
                outcome.codex_session_id
            ],
        )
        .await
        .with_context(|| format!("Failed to record run for project {}", outcome.project_id))?;

        let run_id = query_count(&conn, "SELECT last_insert_rowid()", ()).await?;
        update_project_after_run(&conn, &outcome).await?;

        Ok(run_id)
    }

    pub(crate) fn list_recent_runs_blocking(&self, limit: i64) -> Result<Vec<AgentRunRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.list_recent_runs(limit))
    }

    async fn list_recent_runs(&self, limit: i64) -> Result<Vec<AgentRunRecord>> {
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT r.id, r.project_id, p.name, p.path, r.status, r.started_at,
                        r.finished_at, r.exit_code, r.stdout_path, r.stderr_path, r.summary,
                        r.codex_session_id
                 FROM runs r
                 JOIN projects p ON p.id = r.project_id
                 ORDER BY r.id DESC
                 LIMIT ?1",
                params![limit],
            )
            .await
            .context("Failed to list recent agent runs")?;
        let mut runs = Vec::new();

        while let Some(row) = rows.next().await.context("Failed to read run row")? {
            runs.push(AgentRunRecord {
                id: row_integer(&row, 0, "id")?,
                project_id: row_integer(&row, 1, "project_id")?,
                project_name: row_text(&row, 2, "name")?,
                project_path: PathBuf::from(row_text(&row, 3, "path")?),
                status: row_text(&row, 4, "status")?,
                started_at: row_text(&row, 5, "started_at")?,
                finished_at: row_optional_text(&row, 6, "finished_at")?,
                exit_code: row_optional_integer(&row, 7, "exit_code")?,
                stdout_path: row_optional_text(&row, 8, "stdout_path")?,
                stderr_path: row_optional_text(&row, 9, "stderr_path")?,
                summary: row_optional_text(&row, 10, "summary")?,
                codex_session_id: row_optional_text(&row, 11, "codex_session_id")?,
            });
        }

        Ok(runs)
    }

    pub(crate) fn latest_run_for_project_blocking(
        &self,
        project_id: i64,
    ) -> Result<Option<AgentRunRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.latest_run_for_project(project_id))
    }

    async fn latest_run_for_project(&self, project_id: i64) -> Result<Option<AgentRunRecord>> {
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT r.id, r.project_id, p.name, p.path, r.status, r.started_at,
                        r.finished_at, r.exit_code, r.stdout_path, r.stderr_path, r.summary,
                        r.codex_session_id
                 FROM runs r
                 JOIN projects p ON p.id = r.project_id
                 WHERE r.project_id = ?1
                 ORDER BY r.id DESC
                 LIMIT 1",
                [project_id],
            )
            .await
            .with_context(|| format!("Failed to find latest run for project {project_id}"))?;

        let Some(row) = rows
            .next()
            .await
            .context("Failed to read latest agent run row")?
        else {
            return Ok(None);
        };

        Ok(Some(AgentRunRecord {
            id: row_integer(&row, 0, "id")?,
            project_id: row_integer(&row, 1, "project_id")?,
            project_name: row_text(&row, 2, "name")?,
            project_path: PathBuf::from(row_text(&row, 3, "path")?),
            status: row_text(&row, 4, "status")?,
            started_at: row_text(&row, 5, "started_at")?,
            finished_at: row_optional_text(&row, 6, "finished_at")?,
            exit_code: row_optional_integer(&row, 7, "exit_code")?,
            stdout_path: row_optional_text(&row, 8, "stdout_path")?,
            stderr_path: row_optional_text(&row, 9, "stderr_path")?,
            summary: row_optional_text(&row, 10, "summary")?,
            codex_session_id: row_optional_text(&row, 11, "codex_session_id")?,
        }))
    }

    pub(crate) fn latest_run_for_codex_session_blocking(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<AgentRunRecord>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.latest_run_for_codex_session(project_id, codex_session_id))
    }

    async fn latest_run_for_codex_session(
        &self,
        project_id: i64,
        codex_session_id: &str,
    ) -> Result<Option<AgentRunRecord>> {
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT r.id, r.project_id, p.name, p.path, r.status, r.started_at,
                        r.finished_at, r.exit_code, r.stdout_path, r.stderr_path, r.summary,
                        r.codex_session_id
                 FROM runs r
                 JOIN projects p ON p.id = r.project_id
                 WHERE r.project_id = ?1 AND r.codex_session_id = ?2
                 ORDER BY r.id DESC
                 LIMIT 1",
                params![project_id, codex_session_id],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to find run for project {project_id} and Codex session {codex_session_id}"
                )
            })?;

        let Some(row) = rows
            .next()
            .await
            .context("Failed to read Codex session run row")?
        else {
            return Ok(None);
        };

        Ok(Some(AgentRunRecord {
            id: row_integer(&row, 0, "id")?,
            project_id: row_integer(&row, 1, "project_id")?,
            project_name: row_text(&row, 2, "name")?,
            project_path: PathBuf::from(row_text(&row, 3, "path")?),
            status: row_text(&row, 4, "status")?,
            started_at: row_text(&row, 5, "started_at")?,
            finished_at: row_optional_text(&row, 6, "finished_at")?,
            exit_code: row_optional_integer(&row, 7, "exit_code")?,
            stdout_path: row_optional_text(&row, 8, "stdout_path")?,
            stderr_path: row_optional_text(&row, 9, "stderr_path")?,
            summary: row_optional_text(&row, 10, "summary")?,
            codex_session_id: row_optional_text(&row, 11, "codex_session_id")?,
        }))
    }

    pub(crate) fn record_daemon_checkin_blocking(
        &self,
        holder: &str,
        mode: &str,
        started_at: &str,
        checked_in_at: &str,
        expires_at: &str,
    ) -> Result<()> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.record_daemon_checkin(
                holder,
                mode,
                started_at,
                checked_in_at,
                expires_at,
            ))
    }

    async fn record_daemon_checkin(
        &self,
        holder: &str,
        mode: &str,
        started_at: &str,
        checked_in_at: &str,
        expires_at: &str,
    ) -> Result<()> {
        let conn = self.connect().await?;

        conn.execute(
            "INSERT INTO daemon_checkins (holder, mode, started_at, checked_in_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(holder) DO UPDATE SET
                mode = excluded.mode,
                started_at = excluded.started_at,
                checked_in_at = excluded.checked_in_at,
                expires_at = excluded.expires_at",
            params![holder, mode, started_at, checked_in_at, expires_at],
        )
        .await
        .with_context(|| format!("Failed to record daemon check-in for {holder}"))?;

        Ok(())
    }

    pub(crate) fn clear_daemon_checkin_blocking(&self, holder: &str) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.clear_daemon_checkin(holder))
    }

    async fn clear_daemon_checkin(&self, holder: &str) -> Result<bool> {
        let conn = self.connect().await?;
        let removed = conn
            .execute("DELETE FROM daemon_checkins WHERE holder = ?1", [holder])
            .await
            .with_context(|| format!("Failed to clear daemon check-in for {holder}"))?;

        Ok(removed > 0)
    }

    pub(crate) fn list_daemon_checkins_blocking(&self) -> Result<Vec<AgentDaemonCheckin>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.list_daemon_checkins())
    }

    async fn list_daemon_checkins(&self) -> Result<Vec<AgentDaemonCheckin>> {
        let conn = self.connect().await?;
        let mut rows = conn
            .query(
                "SELECT holder, mode, started_at, checked_in_at, expires_at
                 FROM daemon_checkins
                 ORDER BY CAST(checked_in_at AS INTEGER) DESC, holder",
                (),
            )
            .await
            .context("Failed to list daemon check-ins")?;
        let mut checkins = Vec::new();

        while let Some(row) = rows
            .next()
            .await
            .context("Failed to read daemon check-in row")?
        {
            checkins.push(AgentDaemonCheckin {
                holder: row_text(&row, 0, "holder")?,
                mode: row_text(&row, 1, "mode")?,
                started_at: row_text(&row, 2, "started_at")?,
                checked_in_at: row_text(&row, 3, "checked_in_at")?,
                expires_at: row_text(&row, 4, "expires_at")?,
            });
        }

        Ok(checkins)
    }

    pub(crate) fn clean_agent_history_blocking(
        &self,
        cleaned_at: &str,
    ) -> Result<AgentCleanSummary> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.clean_agent_history(cleaned_at))
    }

    async fn clean_agent_history(&self, cleaned_at: &str) -> Result<AgentCleanSummary> {
        let mut conn = self.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .context("Failed to begin cleaning agent history")?;
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM agent_workers
              WHERE state IN ('dispatching', 'running', 'finalizing')",
            (),
        )
        .await?
            > 0
        {
            anyhow::bail!("Cannot clean agent history while independent workers are active");
        }
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM git_finalizations
              WHERE state IN ('working', 'tracking', 'commit_pending', 'push_pending')",
            (),
        )
        .await?
            > 0
        {
            anyhow::bail!("Cannot clean agent history while Git finalization is pending");
        }
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM agent_git_launch_states",
            (),
        )
        .await?
            > 0
        {
            anyhow::bail!(
                "Cannot clean agent history while an unconsumed Git launch boundary remains"
            );
        }
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM leases
              WHERE CAST(expires_at AS INTEGER) > CAST(?1 AS INTEGER)",
            [cleaned_at],
        )
        .await?
            > 0
        {
            anyhow::bail!("Cannot clean agent history while project leases are active");
        }

        let projects_reset = transaction
            .execute(
                "UPDATE projects
                 SET failure_count = 0,
                     last_failure_at = NULL,
                     last_blocked_recovery_at = NULL,
                     updated_at = ?1
                 WHERE failure_count <> 0
                    OR last_failure_at IS NOT NULL
                    OR last_blocked_recovery_at IS NOT NULL",
                [cleaned_at],
            )
            .await
            .context("Failed to reset agent project failure state")?;
        transaction
            .execute("DELETE FROM agent_workers", ())
            .await
            .context("Failed to delete terminal agent worker records")?;
        transaction
            .execute("DELETE FROM git_finalizations", ())
            .await
            .context("Failed to delete terminal Git finalization records")?;
        transaction
            .execute("DELETE FROM agent_git_launch_states", ())
            .await
            .context("Failed to delete stale prelaunch Git states")?;
        let runs_deleted = transaction
            .execute("DELETE FROM runs", ())
            .await
            .context("Failed to delete agent run records")?;
        let leases_deleted = transaction
            .execute("DELETE FROM leases", ())
            .await
            .context("Failed to delete agent leases")?;
        let daemon_checkins_deleted = transaction
            .execute("DELETE FROM daemon_checkins", ())
            .await
            .context("Failed to delete agent daemon check-ins")?;
        transaction
            .commit()
            .await
            .context("Failed to commit cleaned agent history")?;

        Ok(AgentCleanSummary {
            projects_reset,
            runs_deleted,
            leases_deleted,
            daemon_checkins_deleted,
            run_log_dirs_removed: 0,
            service_logs_truncated: 0,
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
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                worker_schema_migration_is_deferred(&conn, migration_version).await
            })
    }

    #[cfg(test)]
    pub(crate) fn table_exists_blocking(&self, table_name: &str) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.table_exists(table_name))
    }

    #[cfg(test)]
    async fn table_exists(&self, table_name: &str) -> Result<bool> {
        let conn = self.connect().await?;
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
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
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
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
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
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                query_count(&conn, "SELECT COUNT(*) FROM runs", ()).await
            })
    }

    #[cfg(test)]
    pub(crate) fn lease_count_blocking(&self) -> Result<i64> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                query_count(&conn, "SELECT COUNT(*) FROM leases", ()).await
            })
    }

    pub(crate) fn set_project_enabled_blocking(
        &self,
        project_id: i64,
        enabled: bool,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.set_project_enabled(project_id, enabled))
    }

    async fn set_project_enabled(&self, project_id: i64, enabled: bool) -> Result<bool> {
        let conn = self.connect().await?;
        let changed = conn
            .execute(
                "UPDATE projects SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
                params![
                    if enabled { 1_i64 } else { 0_i64 },
                    agent_timestamp(),
                    project_id
                ],
            )
            .await
            .with_context(|| format!("Failed to set project {} enabled state", project_id))?;

        Ok(changed > 0)
    }

    pub(crate) fn set_project_enabled_for_path_blocking(
        &self,
        project_root: &Path,
        enabled: bool,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.set_project_enabled_for_path(project_root, enabled))
    }

    async fn set_project_enabled_for_path(
        &self,
        project_root: &Path,
        enabled: bool,
    ) -> Result<bool> {
        let conn = self.connect().await?;
        let path = project_root.display().to_string();
        let changed = conn
            .execute(
                "UPDATE projects SET enabled = ?1, updated_at = ?2 WHERE path = ?3",
                params![
                    if enabled { 1_i64 } else { 0_i64 },
                    agent_timestamp(),
                    path.as_str()
                ],
            )
            .await
            .with_context(|| format!("Failed to set project {} enabled state", path))?;

        Ok(changed > 0)
    }

    pub(crate) fn clear_project_failure_backoff_for_path_blocking(
        &self,
        project_root: &Path,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let path = project_root.display().to_string();
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await
                    .with_context(|| {
                        format!("Failed to begin retrying registered project {path}")
                    })?;
                if query_count(
                    &transaction,
                    "SELECT COUNT(*) FROM agent_workers
                      WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                        AND state IN ('dispatching', 'running', 'finalizing')",
                    [path.as_str()],
                )
                .await?
                    > 0
                    || query_count(
                        &transaction,
                        "SELECT COUNT(*) FROM leases
                          WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                            AND CAST(expires_at AS INTEGER) > CAST(?2 AS INTEGER)",
                        params![path.as_str(), agent_timestamp()],
                    )
                    .await?
                        > 0
                {
                    anyhow::bail!(
                        "Cannot retry project {path} while its agent worker or lease is active"
                    );
                }
                let changed = transaction
                    .execute(
                        "UPDATE projects SET failure_count = 0, updated_at = ?1 WHERE path = ?2",
                        params![agent_timestamp(), path.as_str()],
                    )
                    .await
                    .with_context(|| format!("Failed to clear project {path} failure cooldown"))?;
                transaction
                    .commit()
                    .await
                    .with_context(|| format!("Failed to commit project {path} immediate retry"))?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn set_project_git_mode_blocking(
        &self,
        project_id: i64,
        mode: AgentGitMode,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.set_project_git_mode(project_id, mode))
    }

    async fn set_project_git_mode(&self, project_id: i64, mode: AgentGitMode) -> Result<bool> {
        let mut conn = self.connect().await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin setting project {project_id} Git mode"))?;
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM git_finalizations
              WHERE project_id = ?1
                AND state IN ('working', 'tracking', 'commit_pending', 'push_pending')",
            [project_id],
        )
        .await?
            > 0
            || query_count(
                &transaction,
                "SELECT COUNT(*) FROM agent_workers
                  WHERE project_id = ?1
                    AND state IN ('dispatching', 'running', 'finalizing')",
                [project_id],
            )
            .await?
                > 0
            || query_count(
                &transaction,
                "SELECT COUNT(*) FROM leases WHERE project_id = ?1",
                [project_id],
            )
            .await?
                > 0
        {
            anyhow::bail!(
                "Cannot change project {project_id} Git mode while an agent run or Git journal is active"
            );
        }
        let changed = transaction
            .execute(
                "UPDATE projects SET git_mode = ?1, updated_at = ?2 WHERE id = ?3",
                params![mode.database_value(), agent_timestamp(), project_id],
            )
            .await
            .with_context(|| format!("Failed to set project {} Git mode", project_id))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit project {project_id} Git mode"))?;

        Ok(changed > 0)
    }

    pub(crate) fn set_project_git_mode_for_path_blocking(
        &self,
        project_root: &Path,
        mode: AgentGitMode,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.set_project_git_mode_for_path(project_root, mode))
    }

    async fn set_project_git_mode_for_path(
        &self,
        project_root: &Path,
        mode: AgentGitMode,
    ) -> Result<bool> {
        let mut conn = self.connect().await?;
        let path = project_root.display().to_string();
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .with_context(|| format!("Failed to begin setting project {path} Git mode"))?;
        if query_count(
            &transaction,
            "SELECT COUNT(*) FROM git_finalizations
              WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                AND state IN ('working', 'tracking', 'commit_pending', 'push_pending')",
            [path.as_str()],
        )
        .await?
            > 0
            || query_count(
                &transaction,
                "SELECT COUNT(*) FROM agent_workers
                  WHERE project_id = (SELECT id FROM projects WHERE path = ?1)
                    AND state IN ('dispatching', 'running', 'finalizing')",
                [path.as_str()],
            )
            .await?
                > 0
            || query_count(
                &transaction,
                "SELECT COUNT(*) FROM leases
                  WHERE project_id = (SELECT id FROM projects WHERE path = ?1)",
                [path.as_str()],
            )
            .await?
                > 0
        {
            anyhow::bail!(
                "Cannot change project {path} Git mode while an agent run or Git journal is active"
            );
        }
        let changed = transaction
            .execute(
                "UPDATE projects SET git_mode = ?1, updated_at = ?2 WHERE path = ?3",
                params![mode.database_value(), agent_timestamp(), path.as_str()],
            )
            .await
            .with_context(|| format!("Failed to set project {} Git mode", path))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("Failed to commit project {path} Git mode"))?;

        Ok(changed > 0)
    }

    pub(crate) fn set_project_codex_settings_blocking(
        &self,
        project_id: i64,
        provider: Option<&str>,
        model: Option<&str>,
        reasoning_effort: Option<&str>,
        fast_enabled: bool,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(self.set_project_codex_settings(
                project_id,
                provider,
                model,
                reasoning_effort,
                fast_enabled,
            ))
    }

    async fn set_project_codex_settings(
        &self,
        project_id: i64,
        provider: Option<&str>,
        model: Option<&str>,
        reasoning_effort: Option<&str>,
        fast_enabled: bool,
    ) -> Result<bool> {
        let conn = self.connect().await?;
        let changed = conn
            .execute(
                "UPDATE projects
                 SET codex_provider = ?1,
                     codex_model = ?2,
                     codex_reasoning_effort = ?3,
                     codex_fast_enabled = ?4,
                     updated_at = ?5
                 WHERE id = ?6",
                params![
                    provider,
                    model,
                    reasoning_effort,
                    if fast_enabled { 1_i64 } else { 0_i64 },
                    agent_timestamp(),
                    project_id
                ],
            )
            .await
            .with_context(|| format!("Failed to set project {} Codex settings", project_id))?;

        Ok(changed > 0)
    }

    pub(crate) fn list_model_providers_blocking(&self) -> Result<Vec<AgentModelProvider>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT provider_id, name, base_url, env_key, built_in, enabled
                         FROM model_providers
                         ORDER BY built_in DESC, name COLLATE NOCASE, provider_id COLLATE NOCASE",
                        (),
                    )
                    .await
                    .context("Failed to list model providers")?;
                let mut providers = Vec::new();
                while let Some(row) = rows.next().await.context("Failed to read model provider")? {
                    providers.push(AgentModelProvider {
                        id: row_text(&row, 0, "provider_id")?,
                        name: row_text(&row, 1, "name")?,
                        base_url: row_optional_text(&row, 2, "base_url")?,
                        env_key: row_optional_text(&row, 3, "env_key")?,
                        built_in: row_integer(&row, 4, "built_in")? != 0,
                        enabled: row_integer(&row, 5, "enabled")? != 0,
                    });
                }
                Ok(providers)
            })
    }

    pub(crate) fn list_model_targets_blocking(
        &self,
        provider_id: Option<&str>,
    ) -> Result<Vec<AgentModelTarget>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT provider_id, model_id, label, enabled, favorite, reasoning_effort
                         FROM model_targets
                         WHERE (?1 IS NULL OR provider_id = ?1)
                         ORDER BY favorite DESC, label COLLATE NOCASE, model_id COLLATE NOCASE",
                        [provider_id],
                    )
                    .await
                    .context("Failed to list model targets")?;
                let mut targets = Vec::new();
                while let Some(row) = rows.next().await.context("Failed to read model target")? {
                    targets.push(AgentModelTarget {
                        provider_id: row_text(&row, 0, "provider_id")?,
                        model_id: row_text(&row, 1, "model_id")?,
                        label: row_text(&row, 2, "label")?,
                        enabled: row_integer(&row, 3, "enabled")? != 0,
                        favorite: row_integer(&row, 4, "favorite")? != 0,
                        reasoning_effort: row_optional_text(&row, 5, "reasoning_effort")?,
                    });
                }
                Ok(targets)
            })
    }

    pub(crate) fn list_enabled_model_targets_blocking(&self) -> Result<Vec<AgentModelTarget>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT t.provider_id, t.model_id, t.label, t.enabled, t.favorite,
                                t.reasoning_effort
                         FROM model_targets t
                         JOIN model_providers p ON p.provider_id = t.provider_id
                         WHERE p.enabled != 0 AND t.enabled != 0
                         ORDER BY t.favorite DESC, p.name COLLATE NOCASE,
                                  t.label COLLATE NOCASE, t.model_id COLLATE NOCASE",
                        (),
                    )
                    .await
                    .context("Failed to list enabled model targets")?;
                let mut targets = Vec::new();
                while let Some(row) = rows.next().await.context("Failed to read model target")? {
                    targets.push(AgentModelTarget {
                        provider_id: row_text(&row, 0, "provider_id")?,
                        model_id: row_text(&row, 1, "model_id")?,
                        label: row_text(&row, 2, "label")?,
                        enabled: row_integer(&row, 3, "enabled")? != 0,
                        favorite: row_integer(&row, 4, "favorite")? != 0,
                        reasoning_effort: row_optional_text(&row, 5, "reasoning_effort")?,
                    });
                }
                Ok(targets)
            })
    }

    pub(crate) fn model_defaults_blocking(&self) -> Result<AgentModelDefaults> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT default_provider, default_model FROM agent_settings WHERE id = 1",
                        (),
                    )
                    .await
                    .context("Failed to read model defaults")?;
                let Some(row) = rows.next().await.context("Failed to read model defaults")? else {
                    return Ok(AgentModelDefaults::default());
                };
                Ok(AgentModelDefaults {
                    provider_id: row_optional_text(&row, 0, "default_provider")?,
                    model_id: row_optional_text(&row, 1, "default_model")?,
                })
            })
    }

    pub(crate) fn resolve_model_target_blocking(
        &self,
        project: &AgentProject,
    ) -> Result<AgentModelDefaults> {
        if let Some(model_id) = project.codex_model.as_ref() {
            return Ok(AgentModelDefaults {
                provider_id: Some(
                    project
                        .codex_provider
                        .clone()
                        .unwrap_or_else(|| "openai".to_string()),
                ),
                model_id: Some(model_id.clone()),
            });
        }
        self.model_defaults_blocking()
    }

    pub(crate) fn model_target_reasoning_blocking(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<Option<String>> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let mut rows = conn
                    .query(
                        "SELECT reasoning_effort
                         FROM model_targets
                         WHERE provider_id = ?1 AND model_id = ?2",
                        params![provider_id, model_id],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to read model {provider_id}/{model_id} reasoning")
                    })?;
                let Some(row) = rows
                    .next()
                    .await
                    .context("Failed to read model reasoning")?
                else {
                    return Ok(None);
                };
                row_optional_text(&row, 0, "reasoning_effort")
            })
    }

    pub(crate) fn upsert_model_provider_blocking(
        &self,
        provider: &AgentModelProvider,
    ) -> Result<()> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                conn.execute(
                    "INSERT INTO model_providers (
                        provider_id, name, base_url, env_key, built_in, enabled,
                        created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), datetime('now'))
                     ON CONFLICT(provider_id) DO UPDATE SET
                        name = excluded.name,
                        base_url = excluded.base_url,
                        env_key = excluded.env_key,
                        built_in = excluded.built_in,
                        enabled = excluded.enabled,
                        updated_at = datetime('now')",
                    params![
                        provider.id.as_str(),
                        provider.name.as_str(),
                        provider.base_url.as_deref(),
                        provider.env_key.as_deref(),
                        if provider.built_in { 1_i64 } else { 0_i64 },
                        if provider.enabled { 1_i64 } else { 0_i64 },
                    ],
                )
                .await
                .with_context(|| format!("Failed to save model provider {}", provider.id))?;
                Ok(())
            })
    }

    pub(crate) fn delete_model_provider_blocking(&self, provider_id: &str) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let mut conn = self.connect().await?;
                let transaction = conn
                    .transaction()
                    .await
                    .with_context(|| format!("Failed to begin deleting provider {provider_id}"))?;
                transaction
                    .execute(
                        "UPDATE projects
                         SET codex_provider = NULL, codex_model = NULL,
                             updated_at = datetime('now')
                         WHERE codex_provider = ?1",
                        [provider_id],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to clear project settings for provider {provider_id}")
                    })?;
                transaction
                    .execute(
                        "UPDATE agent_settings
                         SET default_provider = NULL, default_model = NULL,
                             updated_at = datetime('now')
                         WHERE default_provider = ?1",
                        [provider_id],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to clear the default for provider {provider_id}")
                    })?;
                transaction
                    .execute(
                        "DELETE FROM model_targets WHERE provider_id = ?1",
                        [provider_id],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to delete models for provider {provider_id}")
                    })?;
                let deleted = transaction
                    .execute(
                        "DELETE FROM model_providers WHERE provider_id = ?1",
                        [provider_id],
                    )
                    .await
                    .with_context(|| format!("Failed to delete provider {provider_id}"))?;
                transaction
                    .commit()
                    .await
                    .with_context(|| format!("Failed to commit deleting provider {provider_id}"))?;
                Ok(deleted > 0)
            })
    }

    pub(crate) fn upsert_model_target_blocking(&self, target: &AgentModelTarget) -> Result<()> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                conn.execute(
                    "INSERT INTO model_targets (
                        provider_id, model_id, label, enabled, favorite, reasoning_effort,
                        created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), datetime('now'))
                     ON CONFLICT(provider_id, model_id) DO UPDATE SET
                        label = excluded.label,
                        enabled = excluded.enabled,
                        favorite = excluded.favorite,
                        reasoning_effort = excluded.reasoning_effort,
                        updated_at = datetime('now')",
                    params![
                        target.provider_id.as_str(),
                        target.model_id.as_str(),
                        target.label.as_str(),
                        if target.enabled { 1_i64 } else { 0_i64 },
                        if target.favorite { 1_i64 } else { 0_i64 },
                        target.reasoning_effort.as_deref(),
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to save model target {}/{}",
                        target.provider_id, target.model_id
                    )
                })?;
                Ok(())
            })
    }

    pub(crate) fn set_model_provider_enabled_blocking(
        &self,
        provider_id: &str,
        enabled: bool,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE model_providers SET enabled = ?1, updated_at = datetime('now')
                         WHERE provider_id = ?2",
                        params![if enabled { 1_i64 } else { 0_i64 }, provider_id],
                    )
                    .await
                    .with_context(|| format!("Failed to update provider {provider_id}"))?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn set_model_target_flags_blocking(
        &self,
        provider_id: &str,
        model_id: &str,
        enabled: bool,
        favorite: bool,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE model_targets
                         SET enabled = ?1, favorite = ?2, updated_at = datetime('now')
                         WHERE provider_id = ?3 AND model_id = ?4",
                        params![
                            if enabled { 1_i64 } else { 0_i64 },
                            if favorite { 1_i64 } else { 0_i64 },
                            provider_id,
                            model_id,
                        ],
                    )
                    .await
                    .with_context(|| format!("Failed to update model {provider_id}/{model_id}"))?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn set_model_target_reasoning_blocking(
        &self,
        provider_id: &str,
        model_id: &str,
        reasoning_effort: Option<&str>,
    ) -> Result<bool> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                let changed = conn
                    .execute(
                        "UPDATE model_targets
                         SET reasoning_effort = ?1, updated_at = datetime('now')
                         WHERE provider_id = ?2 AND model_id = ?3",
                        params![reasoning_effort, provider_id, model_id],
                    )
                    .await
                    .with_context(|| {
                        format!("Failed to update model {provider_id}/{model_id} reasoning")
                    })?;
                Ok(changed > 0)
            })
    }

    pub(crate) fn set_model_default_blocking(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<()> {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime for agent store")?
            .block_on(async {
                let conn = self.connect().await?;
                conn.execute(
                    "UPDATE agent_settings
                     SET default_provider = ?1, default_model = ?2, updated_at = datetime('now')
                     WHERE id = 1",
                    params![provider_id, model_id],
                )
                .await
                .context("Failed to set CLT default model")?;
                Ok(())
            })
    }
}

impl Drop for TursoAgentStore {
    fn drop(&mut self) {
        let Some(checkpoint_pin) = self.checkpoint_pin.take() else {
            return;
        };
        // Drop may run from inside a Tokio runtime. Roll the pin back on a
        // short-lived helper thread so we can drive Turso's async state
        // machine without nesting runtimes or leaving a shared read mark.
        let rollback = thread::Builder::new()
            .name("clt-agent-wal-pin-release".to_string())
            .spawn(move || {
                tokio::runtime::Runtime::new().ok().and_then(|runtime| {
                    runtime
                        .block_on(checkpoint_pin.execute("ROLLBACK", ()))
                        .ok()
                })
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
