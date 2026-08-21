use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use ratatui::layout::{Alignment, Position, Rect};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, BufReader, Write, stdout};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::{
    ExecutableCommand,
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, ListItem, ListState, Paragraph},
};
use toml_edit::{DocumentMut, Item, Table, value};
use tui_input::{Input, InputRequest};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const TASK_STATUSES: [&str; 4] = ["todo", "doing", "done", "backlog"];
const TODO_BOARD_INDEX: usize = 0;
const DONE_BOARD_INDEX: usize = 2;
const BACKLOG_BOARD_INDEX: usize = 3;
const DEFAULT_TUI_BOARD_INDICES: [usize; 3] = [0, 1, 2];
const TUI_BOARD_INDICES_WITH_BACKLOG: [usize; 4] = [3, 0, 1, 2];
const TASK_DETAIL_FILES: [&str; 3] = ["task.md", "README.md", "index.md"];
const ARCHIVE_STATUS_CANDIDATES: [&str; 2] = ["archived", "archive"];
const AGENT_STATE_DIR_ENV: &str = "CLT_AGENT_STATE_DIR";
const AGENT_DB_FILE: &str = "agent.db";
const AGENT_FAILURE_BACKOFF_SECONDS_ENV: &str = "CLT_AGENT_FAILURE_BACKOFF_SECONDS";
const AGENT_HEARTBEAT_TAIL_ENV: &str = "CLT_AGENT_HEARTBEAT_TAIL";
const AGENT_LEASE_TIMEOUT_SECONDS_ENV: &str = "CLT_AGENT_LEASE_TIMEOUT_SECONDS";
const AGENT_MAX_GLOBAL_JOBS_ENV: &str = "CLT_AGENT_MAX_GLOBAL_JOBS";
const AGENT_POLL_INTERVAL_SECONDS_ENV: &str = "CLT_AGENT_POLL_INTERVAL_SECONDS";
const AGENT_RUN_TIMEOUT_SECONDS_ENV: &str = "CLT_AGENT_RUN_TIMEOUT_SECONDS";
const AGENT_DAEMON_MODE_ENV: &str = "CLT_AGENT_DAEMON_MODE";
const AGENT_CODEX_PATH_ENV: &str = "CLT_AGENT_CODEX_PATH";
const CODEX_HOME_ENV: &str = "CODEX_HOME";
const AGENT_SUCCESS_COOLDOWN_SECONDS_ENV: &str = "CLT_AGENT_SUCCESS_COOLDOWN_SECONDS";
const XDG_RUNTIME_DIR_ENV: &str = "XDG_RUNTIME_DIR";
const AGENT_DEFAULT_MAX_GLOBAL_JOBS: usize = 12;
const AGENT_DEFAULT_FAILURE_BACKOFF_SECONDS: u64 = 5 * 60;
const AGENT_DEFAULT_LEASE_TIMEOUT_SECONDS: u64 = 60 * 60;
const AGENT_DEFAULT_POLL_INTERVAL_SECONDS: u64 = 15;
const AGENT_EMPTY_REGISTRY_POLL_INTERVAL_SECONDS: u64 = 5;
const AGENT_DAEMON_DATABASE_LOCK_RETRY_ATTEMPTS: usize = 20;
const AGENT_DAEMON_DATABASE_LOCK_RETRY_MILLIS: u64 = 5;
const AGENT_DEFAULT_RUN_TIMEOUT_SECONDS: u64 = 45 * 60;
const AGENT_DEFAULT_SUCCESS_COOLDOWN_SECONDS: u64 = 5;
const AGENT_DAEMON_CHECKIN_STALE_SECONDS: u64 = 45;
const AGENT_NO_TASKS_LEFT_MARKER: &str = "NO_TASKS_LEFT";
const CLT_TASK_MANAGEMENT_SKILL_NAME: &str = "clt-task-management";
const GIT_COMMIT_SKILL_NAME: &str = "git-commit";
const EMBEDDED_CLT_TASK_MANAGEMENT_SKILL: &str =
    include_str!("../skills/clt-task-management/SKILL.md");
const EMBEDDED_GIT_COMMIT_SKILL: &str = include_str!("../skills/git-commit/SKILL.md");
const AGENT_CODEX_REASONING_EFFORTS: [&str; 7] =
    ["", "low", "medium", "high", "xhigh", "max", "ultra"];
const TUI_AGENT_PANEL_REFRESH_SECONDS: u64 = 2;
const TUI_AGENT_LOG_REFRESH_MILLIS: u64 = 500;
const TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP: &str = "   ";
const TUI_AGENT_TABLE_CODEX_MAX_WIDTH: usize = 20;
const TUI_MODEL_DISCOVERY_TIMEOUT_SECONDS: u64 = 5;
const TUI_NO_ACTIVE_BOARD_MESSAGE: &str =
    "No active board. Open a project from Agent Projects, or press M for Models.";

#[derive(Clone, Copy)]
struct AgentProviderPreset {
    id: &'static str,
    name: &'static str,
    base_url: Option<&'static str>,
    env_key: Option<&'static str>,
    built_in: bool,
}

const AGENT_PROVIDER_PRESETS: [AgentProviderPreset; 4] = [
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
const AGENT_LAUNCHD_LABEL: &str = "com.alpinevibrations.clt.agent";
const AGENT_SYSTEMD_UNIT: &str = "clt-agent.service";
const AGENT_CODEX_PROMPT_BASE: &str = r#"You are working in this repo.

Use the existing task-management CLI tooling: clt.

Your job for this run:

1. Inspect the task board using the task CLI.
2. Pick the next available unblocked TODO / ready task.
3. If there are no available tasks, say exactly: NO_TASKS_LEFT
4. If there is a task:
   - move it to doing
   - complete that task
   - run the relevant checks/tests
   - update the task using the task CLI
   - mark it done if completed
   - include a concise note with what changed and what commands/tests ran
5. Stop after that one task.
6. Do not start another task.
7. Exit when finished.

Safety rules:
- Do not overwrite unrelated user changes.
- Before making edits, inspect the current repo state.
- During normal TODO selection, skip tasks whose latest dated state note is `BLOCKED YYYY-MM-DD:`.
- Inspect task details when needed; a folder-backed task's list summary may not show its blocker notes.
- If the task is blocked or cannot be completed safely, update it with a concise `BLOCKED YYYY-MM-DD:` note instead of forcing it.
"#;
const AGENT_GIT_COMMIT_PROMPT_APPENDIX: &str = r#"

Git commit:
- After completing and verifying the task, use the $git-commit skill to create one git commit for the completed work.
- Include the code changes and related task-board updates in the commit when they are part of the same logical change.
- Do not commit when there are no tasks left, the task is blocked, checks fail, or the work cannot be completed safely.
"#;
const AGENT_GIT_PUSH_PROMPT_APPENDIX: &str = r#"

Git push:
- This project is configured for commit and push. After creating the verified commit, use the $git-commit skill to pull first with the locally configured merge/rebase strategy, then push the current branch.
- Do not push when no commit was created or when synchronization, hooks, checks, or the commit fail.
- Never force-push.
"#;
const AGENT_RESUME_DOING_PROMPT_APPENDIX: &str = r#"

Interrupted task recovery:
- A previous agent run was interrupted after moving a task to doing.
- Resume and finish exactly one existing doing task.
- Do not pick or move a TODO task; this recovery instruction replaces steps 2-4 above.
- If there is no doing task to resume, say exactly: NO_TASKS_LEFT
"#;
const AGENT_RECOVER_BLOCKED_PROMPT_APPENDIX: &str = r#"

Blocked-task monitor:
- The scheduler found that every task across todo and doing is currently blocked; Todo does not have to be empty.
- Review the existing blocker notes and choose exactly one blocked task from todo or doing that can be advanced.
- If the selected task is in todo, move it to doing before working on it.
- Try to resolve that task's blocker and finish the task, including the relevant checks.
- Update the existing task; do not create a replacement task.
- If the task is completed, add its completion note and move it to done.
- If its blocker is resolved but the task should be retried through the normal workflow, add a newer `UNBLOCKED YYYY-MM-DD:` note and move that same task back to todo.
- If it still cannot be completed safely, update its blocked note with what you tried and what is still needed, and leave it in doing.
- Do not select backlog work. Stop after handling that one blocked task.
- These recovery instructions replace steps 2-4 above.
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentGitMode {
    Off,
    Commit,
    CommitAndPush,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ShellKind {
    Bash,
    Zsh,
}

struct InitializationPromptRawMode;

impl InitializationPromptRawMode {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for InitializationPromptRawMode {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn initialization_prompt_choice(key: &KeyEvent) -> Option<bool> {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        || key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }

    match key.code {
        KeyCode::Char('y' | 'Y') => Some(true),
        KeyCode::Char('n' | 'N') => Some(false),
        _ => None,
    }
}

fn prompt_to_initialize_tasks() -> Result<bool> {
    print!("Tasks not initialized. Would you like to initialize now? (y/n): ");
    io::stdout().flush()?;

    let choice = {
        let _raw_mode = InitializationPromptRawMode::enter()?;
        loop {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                {
                    break None;
                }
                if let Some(choice) = initialization_prompt_choice(&key) {
                    break Some(choice);
                }
            }
        }
    };

    match choice {
        Some(choice) => {
            println!("{}", if choice { 'y' } else { 'n' });
            Ok(choice)
        }
        None => {
            println!();
            anyhow::bail!("Initialization cancelled")
        }
    }
}

impl AgentGitMode {
    fn next(self) -> Self {
        match self {
            Self::Off => Self::Commit,
            Self::Commit => Self::CommitAndPush,
            Self::CommitAndPush => Self::Off,
        }
    }

    fn database_value(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Commit => "commit",
            Self::CommitAndPush => "commit-and-push",
        }
    }

    fn from_database(value: &str) -> Result<Self> {
        match value {
            "off" => Ok(Self::Off),
            "commit" => Ok(Self::Commit),
            "commit-and-push" => Ok(Self::CommitAndPush),
            _ => anyhow::bail!("Unknown agent Git mode: {value}"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Commit => "commit",
            Self::CommitAndPush => "commit & push",
        }
    }

    fn tui_label(self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Commit => "COM",
            Self::CommitAndPush => "PUSH",
        }
    }
}

#[derive(Clone, Debug)]
struct TaskEntry {
    source: TaskSource,
    summary: String,
    content: String,
    metadata: Option<String>,
    has_subtasks: bool,
}

#[derive(Clone, Debug)]
enum TaskSource {
    MarkdownLine { line_index: usize },
    Path { path: PathBuf, is_dir: bool },
}

#[derive(Clone, Debug)]
enum StatusStore {
    MarkdownFile(PathBuf),
    Directory(PathBuf),
}

enum ExpansionSummary {
    AlreadyDirectory {
        status: &'static str,
        dir: PathBuf,
    },
    Expanded {
        status: &'static str,
        dir: PathBuf,
        backup: PathBuf,
        task_count: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentPlatform {
    Macos,
    Linux,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentServiceAction {
    Start,
    Stop,
}

#[derive(Clone, Debug)]
struct AgentServiceEnvironment {
    codex_path_override: Option<PathBuf>,
    path: OsString,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentCleanSummary {
    projects_reset: u64,
    runs_deleted: u64,
    leases_deleted: u64,
    daemon_checkins_deleted: u64,
    run_log_dirs_removed: u64,
    service_logs_truncated: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentProjectScan {
    status: AgentProjectScanStatus,
    todo_count: usize,
    blocked_todo_count: usize,
    doing_count: usize,
    blocked_doing_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentSchedulerPass {
    scanned_projects: usize,
    pending_projects: usize,
    active_agent_jobs: usize,
    skipped_active_lease: usize,
    deferred_projects: usize,
    runs_started: usize,
    runs_recorded: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentRunResult {
    status: &'static str,
    exit_code: Option<i64>,
    log_dir: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    summary: String,
    codex_session_id: Option<String>,
}

struct AgentRunJob {
    state_dir: PathBuf,
    project: agent_store::AgentProject,
    holder: String,
    task_selection: AgentTaskSelection,
    blocked_task_count_before: usize,
    done_task_contents_before: Vec<String>,
    blocked_task_snapshots_before: Vec<BlockedTaskSnapshot>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct BlockedTaskSnapshot {
    status: &'static str,
    content: String,
}

struct AgentRunCompletion {
    run_id: i64,
    project_name: String,
    project_path: PathBuf,
    status: &'static str,
    summary: String,
    stdout_path: Option<String>,
    stderr_path: Option<String>,
}

struct AgentSchedulerStart {
    pass: AgentSchedulerPass,
    jobs: Vec<AgentRunJob>,
}

struct AgentDaemonRun {
    project_id: i64,
    project_name: String,
    project_path: PathBuf,
    handle: tokio::task::JoinHandle<Result<AgentRunCompletion>>,
}

#[derive(Clone)]
struct AgentDaemonCheckinSource {
    holder: String,
    mode: String,
    started_at: String,
}

type AgentShutdownSignal = Arc<AtomicBool>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentTaskSelection {
    NextTodo,
    ResumeDoing,
    RecoverBlocked,
}

impl AgentTaskSelection {
    fn label(self) -> &'static str {
        match self {
            Self::NextTodo => "next_todo",
            Self::ResumeDoing => "resume_doing",
            Self::RecoverBlocked => "recover_blocked",
        }
    }
}

trait AgentRunner: Send + Sync {
    fn run_project(
        &self,
        project: &agent_store::AgentProject,
        task_selection: AgentTaskSelection,
        shutdown: &AgentShutdownSignal,
    ) -> Result<AgentRunResult>;
}

struct CodexAgentRunner {
    state_dir: PathBuf,
    timeout: Duration,
    heartbeat_interval: Duration,
    command: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentLeaseHolderLiveness {
    CurrentProcess,
    Alive,
    Dead,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AgentProjectScanStatus {
    Pending,
    Blocked,
    Empty,
    Missing,
    Uninitialized,
    Unavailable(String),
}

fn new_agent_shutdown_signal() -> AgentShutdownSignal {
    Arc::new(AtomicBool::new(false))
}

#[derive(Parser)]
#[command(name = "lls-cli-task")]
#[command(about = "A simple file-system-backed task management system", long_about = None)]
struct Cli {
    /// Force use of current directory instead of git root
    #[arg(long, default_value_t = false)]
    local: bool,

    /// Write the TUI's final project directory for a shell wrapper
    #[arg(long, global = true, hide = true)]
    cwd_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initializes the tasks directory and status stores
    Init {
        /// Create backlog/todo/doing/done folders instead of markdown files
        #[arg(long, default_value_t = false)]
        folders: bool,
    },
    /// Expands markdown status files into folder-backed task files
    Expand {
        /// Optional status to expand (backlog, todo, doing, done). Expands all if omitted.
        status: Option<String>,
    },
    /// Adds a new task to the todo list
    Add {
        /// The description of the task, optionally followed by tag-like metadata
        #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
        task: Vec<String>,
    },
    /// Changes the status of a task
    Status {
        /// The source status (e.g., "todo")
        from: String,
        /// The index of the task to move
        task_index: String,
        /// The destination status (e.g., "doing")
        to: String,
    },
    /// Marks a task as done
    Done {
        /// The status the task is currently in (backlog, todo, doing)
        status: String,
        /// The index of the task to mark as done
        task_index: String,
    },
    /// Deletes a task
    Delete {
        /// The status the task is currently in (backlog, todo, doing, done)
        status: String,
        /// The index of the task to delete
        task_index: String,
    },
    /// Lists tasks. Optional status to filter by (backlog, todo, doing, done)
    List { status: Option<String> },
    /// Prints shell integration that changes directory after leaving the TUI
    ShellInit {
        /// Shell to generate integration for
        #[arg(value_enum)]
        shell: ShellKind,
    },
    /// Manages Codex automation across registered projects
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },
}

#[derive(Subcommand)]
enum AgentCommands {
    /// Registers a project for agent runs
    Register {
        /// Project path to register. Defaults to the current directory.
        path: Option<PathBuf>,
    },
    /// Unregisters a project from agent runs
    Unregister {
        /// Project path to unregister. Defaults to the current directory.
        path: Option<PathBuf>,
    },
    /// Pauses agent runs for a registered project
    Pause {
        /// Project path to pause. Defaults to the current directory.
        path: Option<PathBuf>,
    },
    /// Resumes agent runs for a paused registered project
    Resume {
        /// Project path to resume. Defaults to the current directory.
        path: Option<PathBuf>,
    },
    /// Configures the git-commit skill for a registered project
    GitCommit {
        #[command(subcommand)]
        command: AgentGitCommitCommands,
    },
    /// Lists registered projects
    Projects,
    /// Runs the scheduler
    Run {
        /// Run one scheduler pass and exit
        #[arg(long, default_value_t = false)]
        once: bool,
    },
    /// Runs the foreground scheduler loop
    Daemon,
    /// Starts the background agent service
    Start,
    /// Stops the background agent service
    Stop,
    /// Shows agent service and project status
    Status,
    /// Shows recent agent logs
    Logs,
    /// Clears stored agent failures, run history, and agent log files
    Clean,
}

#[derive(Subcommand)]
enum AgentGitCommitCommands {
    /// Adds a git-commit skill instruction to this project's agent prompt
    Enable {
        /// Project path to update. Defaults to the current directory.
        path: Option<PathBuf>,
    },
    /// Removes the git-commit skill instruction from this project's agent prompt
    Disable {
        /// Project path to update. Defaults to the current directory.
        path: Option<PathBuf>,
    },
    /// Adds git commit and push instructions to this project's agent prompt
    Push {
        /// Project path to update. Defaults to the current directory.
        path: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Commands::ShellInit { shell }) = cli.command.as_ref() {
        print!("{}", shell_init_script(*shell));
        return Ok(());
    }

    let root = get_task_root(cli.local)?;
    let cwd = std::env::current_dir()?;

    if root != cwd {
        println!("Using tasks at: {:?}", root);
    }

    match cli.command {
        Some(Commands::Init { folders }) => {
            init_tasks(&root, folders)?;
        }
        Some(Commands::Expand { status }) => {
            expand_tasks(&root, status)?;
        }
        Some(Commands::Add { task }) => {
            let (description, metadata) = parse_add_task_args(task)?;
            let msg = add_task(&root, &description, metadata)?;
            println!("{}", msg);
        }
        Some(Commands::Status {
            from,
            task_index,
            to,
        }) => {
            move_task(&root, &from, &to, &task_index)?;
        }
        Some(Commands::Done { status, task_index }) => {
            if status == "done" {
                println!("Task is already done.");
            } else {
                move_task(&root, &status, "done", &task_index)?;
                println!("Task {} from {} marked as done.", task_index, status);
            }
        }
        Some(Commands::Delete { status, task_index }) => {
            delete_task(&root, &status, &task_index)?;
            println!("Task {} from {} deleted successfully.", task_index, status);
        }
        Some(Commands::List { status }) => {
            list_tasks(&root, status)?;
        }
        Some(Commands::ShellInit { .. }) => unreachable!("shell init handled before root lookup"),
        Some(Commands::Agent { command }) => {
            handle_agent_command(command, cli.local, &root)?;
        }
        None => {
            if !ensure_existing_board(&root)? {
                if prompt_to_initialize_tasks()? {
                    init_tasks(&root, false)?;
                } else {
                    let final_root = tui_view_without_active_board(&root)?;
                    write_tui_cwd_file(cli.cwd_file.as_deref(), &final_root)?;
                    return Ok(());
                }
            }
            let final_root = tui_view(&root)?;
            write_tui_cwd_file(cli.cwd_file.as_deref(), &final_root)?;
        }
    }

    Ok(())
}

fn shell_init_script(shell: ShellKind) -> &'static str {
    match shell {
        ShellKind::Bash | ShellKind::Zsh => {
            r#"clt() {
    local cwd_file cwd exit_status
    cwd_file="$(mktemp "${TMPDIR:-/tmp}/clt-cwd.XXXXXX")" || return
    command clt --cwd-file "$cwd_file" "$@"
    exit_status=$?
    if [ -s "$cwd_file" ]; then
        IFS= read -r cwd < "$cwd_file"
        if [ -n "$cwd" ] && [ "$cwd" != "$PWD" ]; then
            builtin cd -- "$cwd" || exit_status=$?
        fi
    fi
    command rm -f -- "$cwd_file"
    return "$exit_status"
}
"#
        }
    }
}

fn write_tui_cwd_file(cwd_file: Option<&Path>, active_root: &Path) -> Result<()> {
    let Some(cwd_file) = cwd_file else {
        return Ok(());
    };

    fs::write(cwd_file, active_root.as_os_str().as_encoded_bytes())
        .with_context(|| format!("Failed to write TUI exit directory to {cwd_file:?}"))
}

fn handle_agent_command(command: AgentCommands, local: bool, default_root: &Path) -> Result<()> {
    match &command {
        AgentCommands::Start => return manage_agent_service(AgentServiceAction::Start),
        AgentCommands::Stop => return manage_agent_service(AgentServiceAction::Stop),
        _ => {}
    }

    match command {
        AgentCommands::Register { path } => {
            let store = open_agent_store()?;
            register_agent_project(&store, path.as_deref(), local, default_root)?;
        }
        AgentCommands::Unregister { path } => {
            let store = open_agent_store()?;
            unregister_agent_project(&store, path.as_deref(), local, default_root)?;
        }
        AgentCommands::Pause { path } => {
            let store = open_agent_store()?;
            set_agent_project_enabled(&store, path.as_deref(), local, default_root, false)?;
        }
        AgentCommands::Resume { path } => {
            let store = open_agent_store()?;
            set_agent_project_enabled(&store, path.as_deref(), local, default_root, true)?;
        }
        AgentCommands::GitCommit { command } => {
            let store = open_agent_store()?;
            match command {
                AgentGitCommitCommands::Enable { path } => {
                    set_agent_project_git_mode(
                        &store,
                        path.as_deref(),
                        local,
                        default_root,
                        AgentGitMode::Commit,
                    )?;
                }
                AgentGitCommitCommands::Disable { path } => {
                    set_agent_project_git_mode(
                        &store,
                        path.as_deref(),
                        local,
                        default_root,
                        AgentGitMode::Off,
                    )?;
                }
                AgentGitCommitCommands::Push { path } => {
                    set_agent_project_git_mode(
                        &store,
                        path.as_deref(),
                        local,
                        default_root,
                        AgentGitMode::CommitAndPush,
                    )?;
                }
            }
        }
        AgentCommands::Projects => {
            let store = open_agent_store()?;
            list_agent_projects(&store)?;
        }
        AgentCommands::Run { once } => {
            if !once {
                anyhow::bail!("clt agent run requires --once for the foreground scheduler pass.");
            }

            let pass = run_agent_once()?;
            print_agent_scheduler_pass(&pass);
        }
        AgentCommands::Daemon => {
            run_agent_daemon()?;
        }
        AgentCommands::Status => {
            let store = open_agent_store()?;
            show_agent_status(&store)?;
        }
        AgentCommands::Logs => {
            let store = open_agent_store()?;
            show_agent_logs(&store)?;
        }
        AgentCommands::Clean => {
            let state_dir = ensure_agent_state_dir()?;
            let store = open_agent_store_at(&state_dir)?;
            clean_agent_state(&store, &state_dir)?;
        }
        AgentCommands::Start | AgentCommands::Stop => unreachable!("handled before store open"),
    }

    Ok(())
}

fn register_agent_project(
    store: &agent_store::TursoAgentStore,
    path: Option<&Path>,
    local: bool,
    default_root: &Path,
) -> Result<()> {
    let project_root = resolve_agent_project_root(path, local, default_root)?;
    if !ensure_existing_board(&project_root)? {
        anyhow::bail!(
            "Project {:?} does not have an initialized tasks board. Run 'clt init' there first.",
            project_root
        );
    }

    let name = project_display_name(&project_root);
    let created = store.register_project_blocking(&project_root, &name)?;
    if created {
        println!("Registered project: {} ({})", name, project_root.display());
    } else {
        println!(
            "Project already registered: {} ({})",
            name,
            project_root.display()
        );
    }

    Ok(())
}

fn unregister_agent_project(
    store: &agent_store::TursoAgentStore,
    path: Option<&Path>,
    local: bool,
    default_root: &Path,
) -> Result<()> {
    let project_root = resolve_agent_project_root(path, local, default_root)?;
    if store.unregister_project_blocking(&project_root)? {
        println!("Unregistered project: {}", project_root.display());
    } else {
        println!("Project was not registered: {}", project_root.display());
    }

    Ok(())
}

fn set_agent_project_enabled(
    store: &agent_store::TursoAgentStore,
    path: Option<&Path>,
    local: bool,
    default_root: &Path,
    enabled: bool,
) -> Result<()> {
    let project_root = resolve_agent_project_root(path, local, default_root)?;
    if store.set_project_enabled_for_path_blocking(&project_root, enabled)? {
        let action = if enabled { "Resumed" } else { "Paused" };
        println!("{} project: {}", action, project_root.display());
    } else {
        println!("Project was not registered: {}", project_root.display());
    }

    Ok(())
}

fn set_agent_project_git_mode(
    store: &agent_store::TursoAgentStore,
    path: Option<&Path>,
    local: bool,
    default_root: &Path,
    mode: AgentGitMode,
) -> Result<()> {
    let project_root = resolve_agent_project_root(path, local, default_root)?;
    if store.set_project_git_mode_for_path_blocking(&project_root, mode)? {
        println!(
            "Set Git mode to {} for project: {}",
            mode.label(),
            project_root.display()
        );
    } else {
        println!("Project was not registered: {}", project_root.display());
    }

    Ok(())
}

fn list_agent_projects(store: &agent_store::TursoAgentStore) -> Result<()> {
    let projects = store.list_projects_blocking()?;
    if projects.is_empty() {
        println!("No registered projects.");
        return Ok(());
    }

    println!("Registered projects ({}):", projects.len());
    for project in &projects {
        let scan = scan_agent_project(&project.path);
        let scanned_at = store.record_project_scan_blocking(project.id)?;
        println!();
        println!(
            "{}",
            format_agent_project_summary(project, &scan, Some(scanned_at.as_str()))
        );
    }

    Ok(())
}

fn show_agent_status(store: &agent_store::TursoAgentStore) -> Result<()> {
    let state_dir = agent_state_dir()?;
    let projects = store.list_projects_blocking()?;
    let active_leases = store.list_active_leases_blocking(&agent_timestamp())?;
    let recent_runs = store.list_recent_runs_blocking(5)?;
    let daemon_checkins = store.list_daemon_checkins_blocking()?;
    let service_status = agent_service_status(&state_dir);
    let daemon_status = format_agent_daemon_runtime_status(
        &service_status,
        &daemon_checkins,
        agent_timestamp_seconds(),
    );
    let enabled_count = projects.iter().filter(|project| project.enabled).count();
    let scans: Vec<_> = projects
        .iter()
        .map(|project| (project.id, scan_agent_project(&project.path)))
        .collect();
    let pending_count = scans
        .iter()
        .filter(|(_, scan)| scan.has_schedulable_work())
        .count();

    println!("Agent status:");
    println!("state_dir={}", state_dir.display());
    println!("database={}", state_dir.join(AGENT_DB_FILE).display());
    println!("service={}", service_status);
    println!("daemon={}", daemon_status);
    println!(
        "registered_projects={} enabled={} pending={} active_leases={}",
        projects.len(),
        enabled_count,
        pending_count,
        active_leases.len()
    );

    if projects.is_empty() {
        println!("No registered projects.");
    } else {
        println!();
        println!("Projects:");
        for project in &projects {
            let scan = scans
                .iter()
                .find(|(project_id, _)| *project_id == project.id)
                .map(|(_, scan)| scan)
                .expect("scan recorded for each project");
            println!();
            println!(
                "{}",
                format_agent_project_summary(project, scan, project.last_scan_at.as_deref())
            );
        }
    }

    if !active_leases.is_empty() {
        println!();
        println!("Active leases:");
        for lease in active_leases {
            println!(
                "project={} {} holder={} process={} acquired_at={} expires_at={} path={}",
                lease.project_id,
                lease.project_name,
                lease.holder,
                agent_lease_holder_liveness(&lease.holder).label(),
                format_agent_timestamp(&lease.acquired_at),
                format_agent_timestamp(&lease.expires_at),
                lease.project_path.display()
            );
        }
    }

    if !recent_runs.is_empty() {
        println!();
        println!("Recent runs:");
        for run in recent_runs {
            println!("{}", format_agent_run_line(&run));
        }
    }

    Ok(())
}

fn format_agent_project_summary(
    project: &agent_store::AgentProject,
    scan: &AgentProjectScan,
    last_scan_at: Option<&str>,
) -> String {
    let state = if project.enabled { "enabled" } else { "paused" };
    let mut lines = vec![
        format!("{}. {} [{}]", project.id, project.name, state),
        format!(
            "   queue     pending: {:<3} todo: {:<3} todo-ready: {:<3} todo-blocked: {:<3} doing: {:<3} doing-blocked: {:<3} scan: {}",
            scan.pending_signal(),
            scan.todo_count,
            scan.available_todo_count(),
            scan.blocked_todo_count,
            scan.doing_count,
            scan.blocked_doing_count,
            scan.status_label()
        ),
    ];

    if let AgentProjectScanStatus::Unavailable(err) = &scan.status {
        lines.push(format!("   scan err  {err}"));
    }

    lines.extend([
        format!(
            "   activity  last scan: {}",
            format_optional_agent_timestamp(last_scan_at)
        ),
        format!(
            "             last run:  {}",
            format_optional_agent_timestamp(project.last_run_at.as_deref())
        ),
        format!(
            "             success:   {}",
            format_optional_agent_timestamp(project.last_success_at.as_deref())
        ),
        format!(
            "             failure:   {}",
            format_optional_agent_timestamp(project.last_failure_at.as_deref())
        ),
        format!(
            "             blocked:   {}",
            format_optional_agent_timestamp(project.last_blocked_recovery_at.as_deref())
        ),
        format!("   settings  git: {}", project.git_mode.label()),
        format!(
            "             target: {}  reasoning: {}  fast: {}",
            project
                .codex_model
                .as_deref()
                .map(|model| format!(
                    "{}/{}",
                    project.codex_provider.as_deref().unwrap_or("openai"),
                    model
                ))
                .unwrap_or_else(|| "CLT default".to_string()),
            project
                .codex_reasoning_effort
                .as_deref()
                .unwrap_or("default"),
            if project.codex_fast_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ),
        format!("   health    failures: {}", project.failure_count),
        format!("   path      {}", project.path.display()),
    ]);

    lines.join("\n")
}

fn show_agent_logs(store: &agent_store::TursoAgentStore) -> Result<()> {
    let recent_runs = store.list_recent_runs_blocking(5)?;
    if recent_runs.is_empty() {
        println!("No agent runs recorded.");
        return Ok(());
    }

    println!("Recent agent logs:");
    for run in recent_runs {
        println!();
        println!("{}", format_agent_run_line(&run));
        print_agent_log_tail("stdout", run.stdout_path.as_deref())?;
        print_agent_log_tail("stderr", run.stderr_path.as_deref())?;
    }

    Ok(())
}

fn clean_agent_state(store: &agent_store::TursoAgentStore, state_dir: &Path) -> Result<()> {
    let active_leases = store.list_active_leases_blocking(&agent_timestamp())?;
    if !active_leases.is_empty() {
        anyhow::bail!(
            "Refusing to clean agent state while {} active lease(s) exist. Wait for active Codex runs to finish or stop the service first.",
            active_leases.len()
        );
    }

    let mut summary = store.clean_agent_history_blocking(&agent_timestamp())?;
    summary.run_log_dirs_removed = remove_agent_run_logs(state_dir)?;
    summary.service_logs_truncated = truncate_agent_service_logs(state_dir)?;

    println!("Cleaned agent state:");
    println!("  projects reset: {}", summary.projects_reset);
    println!("  run records deleted: {}", summary.runs_deleted);
    println!("  stale leases deleted: {}", summary.leases_deleted);
    println!(
        "  daemon check-ins deleted: {}",
        summary.daemon_checkins_deleted
    );
    println!(
        "  run log directories removed: {}",
        summary.run_log_dirs_removed
    );
    println!(
        "  service logs truncated: {}",
        summary.service_logs_truncated
    );

    Ok(())
}

fn remove_agent_run_logs(state_dir: &Path) -> Result<u64> {
    let runs_dir = state_dir.join("runs");
    if !runs_dir.exists() {
        fs::create_dir_all(&runs_dir)
            .with_context(|| format!("Failed to create agent run log directory {:?}", runs_dir))?;
        return Ok(0);
    }

    let removed = fs::read_dir(&runs_dir)
        .with_context(|| format!("Failed to read agent run log directory {:?}", runs_dir))?
        .count() as u64;
    fs::remove_dir_all(&runs_dir)
        .with_context(|| format!("Failed to remove agent run log directory {:?}", runs_dir))?;
    fs::create_dir_all(&runs_dir)
        .with_context(|| format!("Failed to recreate agent run log directory {:?}", runs_dir))?;

    Ok(removed)
}

fn truncate_agent_service_logs(state_dir: &Path) -> Result<u64> {
    let mut truncated = 0;
    for extension in ["out", "err"] {
        let path = PathBuf::from(state_dir_service_log_path(state_dir, extension));
        if path.exists() {
            fs::File::create(&path)
                .with_context(|| format!("Failed to truncate agent service log {:?}", path))?;
            truncated += 1;
        }
    }

    Ok(truncated)
}

fn manage_agent_service(action: AgentServiceAction) -> Result<()> {
    let state_dir = ensure_agent_state_dir()?;
    let executable = std::env::current_exe().context("Failed to resolve current clt executable")?;

    match current_agent_platform() {
        AgentPlatform::Macos => manage_launchd_agent(action, &state_dir, &executable),
        AgentPlatform::Linux => manage_systemd_agent(action, &state_dir, &executable),
        AgentPlatform::Other => anyhow::bail!(
            "clt agent start/stop is only supported on macOS launchd and Linux user systemd."
        ),
    }
}

fn manage_launchd_agent(
    action: AgentServiceAction,
    state_dir: &Path,
    executable: &Path,
) -> Result<()> {
    let domain = launchd_user_domain()?;
    let plist_path = launchd_plist_path(&home_dir()?);
    let service_target = format!("{domain}/{AGENT_LAUNCHD_LABEL}");

    match action {
        AgentServiceAction::Start => {
            let service_env = resolve_agent_service_environment()?;
            if let Some(parent) = plist_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create launchd directory {:?}", parent))?;
            }
            fs::write(
                &plist_path,
                launchd_plist_content(executable, state_dir, &service_env),
            )
            .with_context(|| format!("Failed to write launchd plist {:?}", plist_path))?;

            if run_service_command_optional("launchctl", &["print", &service_target])? {
                run_service_command(
                    "launchctl",
                    &["bootout", &domain, plist_path.to_string_lossy().as_ref()],
                )?;
            }
            run_service_command(
                "launchctl",
                &["bootstrap", &domain, plist_path.to_string_lossy().as_ref()],
            )?;
            run_service_command("launchctl", &["kickstart", "-k", &service_target])?;
            println!(
                "Started clt agent launchd service {} ({})",
                AGENT_LAUNCHD_LABEL,
                plist_path.display()
            );
        }
        AgentServiceAction::Stop => {
            if !plist_path.exists() {
                println!(
                    "No clt agent launchd service is installed at {}",
                    plist_path.display()
                );
                return Ok(());
            }

            if run_service_command_optional("launchctl", &["print", &service_target])? {
                run_service_command(
                    "launchctl",
                    &["bootout", &domain, plist_path.to_string_lossy().as_ref()],
                )?;
                println!("Stopped clt agent launchd service {}", AGENT_LAUNCHD_LABEL);
            } else {
                println!(
                    "clt agent launchd service {} was not running",
                    AGENT_LAUNCHD_LABEL
                );
            }
        }
    }

    Ok(())
}

fn manage_systemd_agent(
    action: AgentServiceAction,
    state_dir: &Path,
    executable: &Path,
) -> Result<()> {
    let unit_path = systemd_user_unit_path(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )?;

    match action {
        AgentServiceAction::Start => {
            let service_env = resolve_agent_service_environment()?;
            if let Some(parent) = unit_path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create systemd user unit directory {:?}", parent)
                })?;
            }
            fs::write(
                &unit_path,
                systemd_unit_content(executable, state_dir, &service_env),
            )
            .with_context(|| format!("Failed to write systemd unit {:?}", unit_path))?;

            for args in systemd_start_command_args() {
                run_service_command("systemctl", args)?;
            }
            println!(
                "Started clt agent systemd user service {} ({})",
                AGENT_SYSTEMD_UNIT,
                unit_path.display()
            );
        }
        AgentServiceAction::Stop => {
            if !unit_path.exists() {
                println!(
                    "No clt agent systemd user service is installed at {}",
                    unit_path.display()
                );
                return Ok(());
            }

            run_service_command("systemctl", &["--user", "stop", AGENT_SYSTEMD_UNIT])?;
            println!(
                "Stopped clt agent systemd user service {}",
                AGENT_SYSTEMD_UNIT
            );
        }
    }

    Ok(())
}

fn agent_service_status(state_dir: &Path) -> String {
    match current_agent_platform() {
        AgentPlatform::Macos => launchd_service_status(),
        AgentPlatform::Linux => systemd_service_status(),
        AgentPlatform::Other => Ok("unsupported".to_string()),
    }
    .unwrap_or_else(|err| format!("unknown ({err}); state_dir={}", state_dir.display()))
}

fn launchd_service_status() -> Result<String> {
    let plist_path = launchd_plist_path(&home_dir()?);
    if !plist_path.exists() {
        return Ok("not-installed".to_string());
    }

    let target = format!("{}/{}", launchd_user_domain()?, AGENT_LAUNCHD_LABEL);
    if run_service_command_optional("launchctl", &["print", &target])? {
        Ok("running".to_string())
    } else {
        Ok("installed".to_string())
    }
}

fn systemd_service_status() -> Result<String> {
    let unit_path = systemd_user_unit_path(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )?;
    if !unit_path.exists() {
        return Ok("not-installed".to_string());
    }

    if run_service_command_optional(
        "systemctl",
        &["--user", "is-active", "--quiet", AGENT_SYSTEMD_UNIT],
    )? {
        Ok("running".to_string())
    } else {
        Ok("installed".to_string())
    }
}

fn restart_running_agent_service() -> Result<()> {
    match current_agent_platform() {
        AgentPlatform::Macos => {
            let target = format!("{}/{}", launchd_user_domain()?, AGENT_LAUNCHD_LABEL);
            run_service_command_quiet("launchctl", &["kickstart", "-k", &target])
        }
        AgentPlatform::Linux => {
            run_service_command_quiet("systemctl", &["--user", "restart", AGENT_SYSTEMD_UNIT])
        }
        AgentPlatform::Other => anyhow::bail!(
            "Automatic agent service recovery is only supported on macOS launchd and Linux user systemd."
        ),
    }
}

fn systemd_start_command_args() -> [&'static [&'static str]; 3] {
    [
        &["--user", "daemon-reload"],
        &["--user", "enable", AGENT_SYSTEMD_UNIT],
        &["--user", "restart", AGENT_SYSTEMD_UNIT],
    ]
}

fn launchd_plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{AGENT_LAUNCHD_LABEL}.plist"))
}

fn launchd_plist_content(
    executable: &Path,
    state_dir: &Path,
    service_env: &AgentServiceEnvironment,
) -> String {
    let executable = xml_escape(&executable.display().to_string());
    let stdout_path = xml_escape(&state_dir_service_log_path(state_dir, "out"));
    let stderr_path = xml_escape(&state_dir_service_log_path(state_dir, "err"));
    let state_dir = xml_escape(&state_dir.display().to_string());
    let codex_path_environment = service_env
        .codex_path_override
        .as_ref()
        .map(|codex_path| {
            let codex_path = xml_escape(&codex_path.display().to_string());
            format!(
                "    <key>{AGENT_CODEX_PATH_ENV}</key>\n\
    <string>{codex_path}</string>\n"
            )
        })
        .unwrap_or_default();
    let path = service_env.path.to_string_lossy();
    let path = xml_escape(path.as_ref());

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{AGENT_LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
    <string>agent</string>
    <string>daemon</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>{AGENT_STATE_DIR_ENV}</key>
    <string>{state_dir}</string>
    <key>{AGENT_DAEMON_MODE_ENV}</key>
    <string>service</string>
{codex_path_environment}    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{stdout_path}</string>
  <key>StandardErrorPath</key>
  <string>{stderr_path}</string>
</dict>
</plist>
"#
    )
}

fn systemd_user_unit_path(
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    let config_home = xdg_config_home
        .or_else(|| home.map(|home| home.join(".config")))
        .ok_or_else(|| anyhow::anyhow!("HOME is required to resolve the systemd user unit path"))?;

    Ok(config_home.join("systemd/user").join(AGENT_SYSTEMD_UNIT))
}

fn systemd_unit_content(
    executable: &Path,
    state_dir: &Path,
    service_env: &AgentServiceEnvironment,
) -> String {
    let codex_path_environment = service_env
        .codex_path_override
        .as_ref()
        .map(|codex_path| {
            format!(
                "Environment={}\n",
                systemd_env_assignment(AGENT_CODEX_PATH_ENV, &codex_path.display().to_string())
            )
        })
        .unwrap_or_default();

    format!(
        "[Unit]\n\
Description=CLT Codex agent\n\
After=default.target\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart={} agent daemon\n\
Environment={}\n\
Environment={}\n\
{codex_path_environment}\
Environment={}\n\
Restart=always\n\
RestartSec=10\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        systemd_quote_arg(&executable.display().to_string()),
        systemd_env_assignment(AGENT_STATE_DIR_ENV, &state_dir.display().to_string()),
        systemd_env_assignment(AGENT_DAEMON_MODE_ENV, "service"),
        systemd_env_assignment("PATH", service_env.path.to_string_lossy().as_ref())
    )
}

fn resolve_agent_service_environment() -> Result<AgentServiceEnvironment> {
    let path = agent_service_path_env();
    let codex_path_override =
        resolve_agent_codex_path_override_for_service(agent_codex_path_env().as_deref(), &path)?;
    let codex_command = codex_path_override
        .as_deref()
        .unwrap_or_else(|| Path::new("codex"));
    validate_agent_codex_path(codex_command, &path)?;

    Ok(AgentServiceEnvironment {
        codex_path_override,
        path,
    })
}

fn agent_service_path_env() -> OsString {
    std::env::var_os("PATH")
        .filter(|path| !os_value_is_blank(path.as_os_str()))
        .unwrap_or_else(|| {
            OsString::from("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        })
}

fn resolve_agent_codex_path_override_for_service(
    configured: Option<&Path>,
    path_env: &OsStr,
) -> Result<Option<PathBuf>> {
    if let Some(configured) = configured {
        let resolved =
            resolve_agent_command_candidate(configured, path_env).with_context(|| {
                format!(
                    "Failed to resolve {}={}",
                    AGENT_CODEX_PATH_ENV,
                    configured.display()
                )
            })?;
        return Ok(Some(prefer_packaged_native_codex_binary(&resolved)));
    }

    find_executable_on_path("codex", path_env).ok_or_else(|| {
        anyhow::anyhow!(
            "Failed to find `codex` in PATH while installing the agent service. Install the Codex CLI, start the service from a shell where `codex --version` works, or set {AGENT_CODEX_PATH_ENV} to the Codex executable path."
        )
    })?;
    Ok(None)
}

fn agent_codex_command() -> PathBuf {
    agent_codex_path_env().unwrap_or_else(|| PathBuf::from("codex"))
}

fn configure_interactive_codex_resume_command(
    command: &mut Command,
    project_root: &Path,
    session_id: &str,
) {
    command
        .arg("resume")
        .arg("--include-non-interactive")
        .arg("--sandbox")
        .arg("workspace-write")
        .arg("--ask-for-approval")
        .arg("on-request")
        .arg("-C")
        .arg(project_root)
        .arg(session_id)
        .current_dir(project_root);
}

fn resume_codex_session_interactively(project_root: &Path, session_id: &str) -> Result<ExitStatus> {
    let codex_command = agent_codex_command();
    let mut command = Command::new(&codex_command);
    configure_interactive_codex_resume_command(&mut command, project_root, session_id);
    command.status().with_context(|| {
        format!(
            "Failed to resume Codex session {session_id} with {} in {}",
            codex_command.display(),
            project_root.display()
        )
    })
}

fn agent_codex_path_env() -> Option<PathBuf> {
    std::env::var_os(AGENT_CODEX_PATH_ENV)
        .filter(|value| !os_value_is_blank(value.as_os_str()))
        .map(PathBuf::from)
}

fn resolve_agent_command_candidate(candidate: &Path, path_env: &OsStr) -> Result<PathBuf> {
    if candidate.is_absolute() || path_has_separator(candidate) {
        if agent_command_is_executable(candidate) {
            return Ok(candidate.to_path_buf());
        }

        anyhow::bail!("{} is not an executable file", candidate.display());
    }

    let program = candidate.to_string_lossy();
    find_executable_on_path(program.as_ref(), path_env)
        .ok_or_else(|| anyhow::anyhow!("{} was not found in PATH", candidate.display()))
}

fn path_has_separator(path: &Path) -> bool {
    path.components().count() > 1
}

fn find_executable_on_path(program: &str, path_env: &OsStr) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }

    std::env::split_paths(path_env)
        .map(|dir| dir.join(program))
        .find(|candidate| agent_command_is_executable(candidate))
}

fn agent_command_is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

fn prefer_packaged_native_codex_binary(command: &Path) -> PathBuf {
    let Ok(canonical_command) = fs::canonicalize(command) else {
        return command.to_path_buf();
    };
    if canonical_command.file_name() != Some(OsStr::new("codex.js")) {
        return command.to_path_buf();
    }

    let Some((platform_package, target_triple, binary_name)) = codex_native_package() else {
        return command.to_path_buf();
    };
    let Some(codex_package_dir) = canonical_command.parent().and_then(Path::parent) else {
        return command.to_path_buf();
    };
    let Some(node_modules_dir) = codex_package_dir.parent().and_then(Path::parent) else {
        return command.to_path_buf();
    };

    let native_binary = node_modules_dir
        .join("@openai")
        .join(platform_package)
        .join("vendor")
        .join(target_triple)
        .join("bin")
        .join(binary_name);
    if agent_command_is_executable(&native_binary) {
        native_binary
    } else {
        command.to_path_buf()
    }
}

fn codex_native_package() -> Option<(&'static str, &'static str, &'static str)> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some(("codex-darwin-arm64", "aarch64-apple-darwin", "codex"))
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some(("codex-darwin-x64", "x86_64-apple-darwin", "codex"))
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some(("codex-linux-arm64", "aarch64-unknown-linux-musl", "codex"))
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some(("codex-linux-x64", "x86_64-unknown-linux-musl", "codex"))
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        Some(("codex-win32-arm64", "aarch64-pc-windows-msvc", "codex.exe"))
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some(("codex-win32-x64", "x86_64-pc-windows-msvc", "codex.exe"))
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64")
    )))]
    {
        None
    }
}

fn validate_agent_codex_path(codex_path: &Path, path_env: &OsStr) -> Result<()> {
    let output = Command::new(codex_path)
        .arg("--version")
        .env("PATH", path_env)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| {
            format!(
                "Failed to validate Codex executable {}",
                codex_path.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let detail = if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        };
        anyhow::bail!(
            "Codex executable {} failed `--version` with status {}{detail}",
            codex_path.display(),
            output.status
        );
    }

    Ok(())
}

fn os_value_is_blank(value: &OsStr) -> bool {
    value.to_string_lossy().trim().is_empty()
}

fn state_dir_service_log_path(state_dir: &Path, extension: &str) -> String {
    state_dir
        .join(format!("agent-service.{extension}"))
        .display()
        .to_string()
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is required for agent service management"))
}

fn launchd_user_domain() -> Result<String> {
    launchd_user_domain_for_uid(&current_user_id()?)
}

fn current_user_id() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("Failed to determine current user id with id -u")?;
    if !output.status.success() {
        anyhow::bail!("id -u failed with status {}", output.status);
    }

    let uid = String::from_utf8(output.stdout)
        .context("id -u produced non-UTF-8 output")?
        .trim()
        .to_string();
    if uid.is_empty() {
        anyhow::bail!("id -u produced an empty user id");
    }

    Ok(uid)
}

fn launchd_user_domain_for_uid(uid: &str) -> Result<String> {
    let uid = uid.trim();
    if uid.is_empty() {
        anyhow::bail!("id -u produced an empty user id");
    }
    if !uid.chars().all(|ch| ch.is_ascii_digit()) {
        anyhow::bail!("id -u produced an invalid user id: {uid}");
    }
    if uid == "0" {
        let sudo_user = std::env::var("SUDO_USER")
            .ok()
            .filter(|user| !user.trim().is_empty())
            .map(|user| format!(" for sudo user {user}"))
            .unwrap_or_default();
        anyhow::bail!(
            "Refusing to manage the macOS launchd user agent as root{sudo_user}. Run `clt agent start` or `clt agent stop` without sudo from the logged-in macOS user session."
        );
    }

    Ok(format!("gui/{uid}"))
}

fn run_service_command(program: &str, args: &[&str]) -> Result<()> {
    let status = service_command(program, args)?
        .status()
        .with_context(|| format!("Failed to run {}", service_command_display(program, args)))?;
    if !status.success() {
        anyhow::bail!(
            "{} failed with status {}",
            service_command_display(program, args),
            status
        );
    }

    Ok(())
}

fn run_service_command_optional(program: &str, args: &[&str]) -> Result<bool> {
    // Status probes are called from the TUI refresh path, so child output must
    // never inherit the terminal and overwrite the alternate screen.
    let status = service_command(program, args)?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("Failed to run {}", service_command_display(program, args)))?;

    Ok(status.success())
}

fn run_service_command_quiet(program: &str, args: &[&str]) -> Result<()> {
    let status = service_command(program, args)?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("Failed to run {}", service_command_display(program, args)))?;
    if !status.success() {
        anyhow::bail!(
            "{} failed with status {}",
            service_command_display(program, args),
            status
        );
    }

    Ok(())
}

fn service_command(program: &str, args: &[&str]) -> Result<Command> {
    let mut command = Command::new(program);
    command.args(args);

    if program == "systemctl" && args.contains(&"--user") {
        configure_systemd_user_command(&mut command)?;
    }

    Ok(command)
}

fn configure_systemd_user_command(command: &mut Command) -> Result<()> {
    let inherited_runtime_dir = std::env::var_os(XDG_RUNTIME_DIR_ENV);
    if inherited_runtime_dir
        .as_deref()
        .is_some_and(|value| !os_value_is_blank(value))
    {
        return Ok(());
    }

    let uid = current_user_id()?;
    let runtime_dir = systemd_user_runtime_dir_for_uid(&uid)?;
    let bus_path = runtime_dir.join("bus");
    if !runtime_dir.is_dir() || !bus_path.exists() {
        anyhow::bail!(
            "Linux systemd user bus is unavailable at {}. Log in through a systemd/PAM user session, or enable lingering with `sudo loginctl enable-linger <user>` before managing the clt agent service.",
            bus_path.display()
        );
    }
    configure_systemd_user_command_with_runtime_dir(command, None, &uid)
}

fn configure_systemd_user_command_with_runtime_dir(
    command: &mut Command,
    inherited_runtime_dir: Option<&OsStr>,
    uid: &str,
) -> Result<()> {
    if inherited_runtime_dir.is_some_and(|value| !os_value_is_blank(value)) {
        return Ok(());
    }

    command.env(XDG_RUNTIME_DIR_ENV, systemd_user_runtime_dir_for_uid(uid)?);
    Ok(())
}

fn systemd_user_runtime_dir_for_uid(uid: &str) -> Result<PathBuf> {
    let uid = uid.trim();
    if uid.is_empty() || !uid.chars().all(|ch| ch.is_ascii_digit()) {
        anyhow::bail!("id -u produced an invalid user id: {uid}");
    }

    Ok(Path::new("/run/user").join(uid))
}

fn service_command_display(program: &str, args: &[&str]) -> String {
    let mut parts = vec![program.to_string()];
    parts.extend(args.iter().map(|arg| (*arg).to_string()));
    parts.join(" ")
}

fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn systemd_env_assignment(key: &str, value: &str) -> String {
    format!(
        "\"{}={}\"",
        systemd_escape_double_quoted(key),
        systemd_escape_double_quoted(value)
    )
}

fn systemd_quote_arg(raw: &str) -> String {
    if raw
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        raw.to_string()
    } else {
        format!("\"{}\"", systemd_escape_double_quoted(raw))
    }
}

fn systemd_escape_double_quoted(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

fn format_agent_run_line(run: &agent_store::AgentRunRecord) -> String {
    format!(
        "run={} project={} {} status={} started_at={} finished_at={} exit_code={} summary={} stdout={} stderr={} path={}",
        run.id,
        run.project_id,
        run.project_name,
        run.status,
        format_agent_timestamp(&run.started_at),
        format_optional_agent_timestamp(run.finished_at.as_deref()),
        run.exit_code
            .map(|exit_code| exit_code.to_string())
            .unwrap_or_else(|| "-".to_string()),
        run.summary.as_deref().unwrap_or("-"),
        run.stdout_path.as_deref().unwrap_or("-"),
        run.stderr_path.as_deref().unwrap_or("-"),
        run.project_path.display()
    )
}

fn print_agent_log_tail(label: &str, path: Option<&str>) -> Result<()> {
    print_agent_log_tail_with_limit(label, path, 20)
}

fn print_agent_log_tail_with_limit(label: &str, path: Option<&str>, limit: usize) -> Result<()> {
    let Some(path) = path else {
        println!("{label}=<not recorded>");
        return Ok(());
    };
    let path = Path::new(path);
    println!("{label}={}", path.display());
    match fs::read_to_string(path) {
        Ok(content) => {
            let tail = tail_lines(&content, limit);
            if tail.is_empty() {
                println!("  <empty>");
            } else {
                for line in tail {
                    println!("  {line}");
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            println!("  <missing>");
        }
        Err(err) => {
            return Err(err).with_context(|| format!("Failed to read agent log {:?}", path));
        }
    }

    Ok(())
}

fn tail_lines(content: &str, limit: usize) -> Vec<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(limit);
    lines[start..].to_vec()
}

fn run_agent_daemon() -> Result<()> {
    let state_dir = ensure_agent_state_dir()?;
    let runner: Arc<dyn AgentRunner> = Arc::new(CodexAgentRunner::new(state_dir.clone())?);
    run_agent_daemon_loop(&state_dir, runner, agent_poll_interval()?, None)
}

fn run_agent_daemon_loop(
    state_dir: &Path,
    runner: Arc<dyn AgentRunner>,
    poll_interval: Duration,
    max_passes: Option<usize>,
) -> Result<()> {
    run_agent_daemon_loop_with_shutdown(
        state_dir,
        runner,
        poll_interval,
        max_passes,
        new_agent_shutdown_signal(),
    )
}

fn run_agent_daemon_loop_with_shutdown(
    state_dir: &Path,
    runner: Arc<dyn AgentRunner>,
    poll_interval: Duration,
    max_passes: Option<usize>,
    shutdown: AgentShutdownSignal,
) -> Result<()> {
    if max_passes == Some(0) {
        return Ok(());
    }

    let max_global_jobs = agent_max_global_jobs()?;
    println!(
        "Starting clt agent daemon with poll_interval_seconds={} empty_registry_poll_interval_seconds={} max_global_jobs={} state_dir={} database={}",
        poll_interval.as_secs(),
        AGENT_EMPTY_REGISTRY_POLL_INTERVAL_SECONDS,
        max_global_jobs,
        state_dir.display(),
        state_dir.join(AGENT_DB_FILE).display()
    );

    let runtime = tokio::runtime::Runtime::new()
        .context("Failed to create async runtime for agent daemon")?;
    runtime.block_on(run_agent_daemon_loop_async(
        state_dir.to_path_buf(),
        runner,
        poll_interval,
        max_passes,
        shutdown,
    ))
}

async fn run_agent_daemon_loop_async(
    state_dir: PathBuf,
    runner: Arc<dyn AgentRunner>,
    poll_interval: Duration,
    max_passes: Option<usize>,
    shutdown: AgentShutdownSignal,
) -> Result<()> {
    let daemon_checkin = AgentDaemonCheckinSource::current();
    let mut scheduled_passes = 0;
    let mut active_passes: Vec<tokio::task::JoinHandle<Result<AgentSchedulerStart>>> = Vec::new();
    let mut active_runs: Vec<AgentDaemonRun> = Vec::new();
    let mut next_sleep = poll_interval;

    loop {
        let mut run_index = 0;
        while run_index < active_runs.len() {
            if !active_runs[run_index].handle.is_finished() {
                run_index += 1;
                continue;
            }

            let run = active_runs.swap_remove(run_index);
            match run.handle.await.context("Agent run task failed")? {
                Ok(completion) => {
                    print_agent_run_completion(&completion)?;
                }
                Err(err) => {
                    println!(
                        "Project {}: action=run_task_failed reason=\"{err:#}\" path={}",
                        run.project_name,
                        run.project_path.display()
                    );
                }
            }
        }

        let mut index = 0;
        while index < active_passes.len() {
            if !active_passes[index].is_finished() {
                index += 1;
                continue;
            }

            let handle = active_passes.swap_remove(index);
            let start = handle.await.context("Agent scheduler pass task failed")??;
            print_agent_scheduler_pass(&start.pass);
            next_sleep = agent_daemon_sleep_interval(&start.pass, poll_interval);
            for job in start.jobs {
                if shutdown.load(Ordering::SeqCst) {
                    release_agent_job_lease_for_shutdown(&job)?;
                } else {
                    active_runs.push(spawn_agent_daemon_run(
                        Arc::clone(&runner),
                        job,
                        Arc::clone(&shutdown),
                    ));
                }
            }
        }

        let scheduling_stopped = shutdown.load(Ordering::SeqCst)
            || max_passes.is_some_and(|max_passes| scheduled_passes >= max_passes);
        if scheduling_stopped && active_passes.is_empty() && active_runs.is_empty() {
            clear_agent_daemon_checkin_best_effort(&state_dir, &daemon_checkin.holder).await;
            return Ok(());
        }

        if !scheduling_stopped && active_passes.is_empty() {
            let task_state_dir = state_dir.clone();
            let active_project_ids = active_runs.iter().map(|run| run.project_id).collect();
            let task_daemon_checkin = daemon_checkin.clone();
            active_passes.push(tokio::task::spawn_blocking(move || {
                run_agent_daemon_scheduler_pass_with_active_and_checkin(
                    &task_state_dir,
                    active_project_ids,
                    Some(&task_daemon_checkin),
                )
            }));
            scheduled_passes += 1;
        }

        wait_for_agent_daemon_sleep_or_shutdown(next_sleep, max_passes, &shutdown).await?;
    }
}

async fn wait_for_agent_daemon_sleep_or_shutdown(
    sleep_duration: Duration,
    max_passes: Option<usize>,
    shutdown: &AgentShutdownSignal,
) -> Result<()> {
    if max_passes.is_some() || shutdown.load(Ordering::SeqCst) {
        tokio::time::sleep(sleep_duration).await;
        return Ok(());
    }

    tokio::select! {
        _ = tokio::time::sleep(sleep_duration) => Ok(()),
        signal = tokio::signal::ctrl_c() => {
            signal.context("Failed to listen for Ctrl-C shutdown signal")?;
            println!("Received Ctrl-C; stopping new agent work and waiting for active runs to clean up.");
            shutdown.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
}

fn agent_daemon_sleep_interval(pass: &AgentSchedulerPass, poll_interval: Duration) -> Duration {
    if pass.scanned_projects == 0 {
        std::cmp::min(
            poll_interval,
            Duration::from_secs(AGENT_EMPTY_REGISTRY_POLL_INTERVAL_SECONDS),
        )
    } else {
        poll_interval
    }
}

fn run_agent_once() -> Result<AgentSchedulerPass> {
    let state_dir = ensure_agent_state_dir()?;
    let runner = CodexAgentRunner::new(state_dir.clone())?;
    run_agent_once_with_runner(&state_dir, &runner)
}

fn print_agent_scheduler_pass(pass: &AgentSchedulerPass) {
    println!(
        "Scheduler pass complete: scanned={} pending_projects={} active_agent_jobs={} skipped_active_lease={} deferred_projects={} runs_started={} runs_recorded={}",
        pass.scanned_projects,
        pass.pending_projects,
        pass.active_agent_jobs,
        pass.skipped_active_lease,
        pass.deferred_projects,
        pass.runs_started,
        pass.runs_recorded
    );
}

fn run_agent_once_with_runner(
    state_dir: &Path,
    runner: &dyn AgentRunner,
) -> Result<AgentSchedulerPass> {
    let shutdown = new_agent_shutdown_signal();
    let mut start = run_agent_scheduler_pass(state_dir, true, &[])?;
    for job in start.jobs {
        let completion = run_agent_job(job, runner, &shutdown)?;
        print_agent_run_completion(&completion)?;
        start.pass.runs_recorded += 1;
    }

    Ok(start.pass)
}

#[cfg(test)]
fn run_agent_daemon_scheduler_pass(state_dir: &Path) -> Result<AgentSchedulerStart> {
    run_agent_daemon_scheduler_pass_with_active(state_dir, Vec::new())
}

#[cfg(test)]
fn run_agent_daemon_scheduler_pass_with_active(
    state_dir: &Path,
    active_project_ids: Vec<i64>,
) -> Result<AgentSchedulerStart> {
    let daemon_checkin = AgentDaemonCheckinSource::current();
    run_agent_daemon_scheduler_pass_with_active_and_checkin(
        state_dir,
        active_project_ids,
        Some(&daemon_checkin),
    )
}

fn run_agent_daemon_scheduler_pass_with_active_and_checkin(
    state_dir: &Path,
    active_project_ids: Vec<i64>,
    daemon_checkin: Option<&AgentDaemonCheckinSource>,
) -> Result<AgentSchedulerStart> {
    let mut attempts = 0;
    loop {
        match run_agent_scheduler_pass_with_daemon_checkin(
            state_dir,
            false,
            &active_project_ids,
            daemon_checkin,
        ) {
            Ok(start) => return Ok(start),
            Err(err)
                if agent_error_is_database_locked(&err)
                    && attempts < AGENT_DAEMON_DATABASE_LOCK_RETRY_ATTEMPTS =>
            {
                attempts += 1;
                println!(
                    "Scheduler pass retry: reason=database_locked attempt={} max_attempts={}",
                    attempts, AGENT_DAEMON_DATABASE_LOCK_RETRY_ATTEMPTS
                );
                thread::sleep(Duration::from_millis(
                    AGENT_DAEMON_DATABASE_LOCK_RETRY_MILLIS,
                ));
            }
            Err(err) => return Err(err),
        }
    }
}

fn agent_error_is_database_locked(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("database is locked"))
}

fn run_agent_scheduler_pass(
    state_dir: &Path,
    reclaim_current_process_leases: bool,
    active_project_ids: &[i64],
) -> Result<AgentSchedulerStart> {
    run_agent_scheduler_pass_with_daemon_checkin(
        state_dir,
        reclaim_current_process_leases,
        active_project_ids,
        None,
    )
}

fn run_agent_scheduler_pass_with_daemon_checkin(
    state_dir: &Path,
    reclaim_current_process_leases: bool,
    active_project_ids: &[i64],
    daemon_checkin: Option<&AgentDaemonCheckinSource>,
) -> Result<AgentSchedulerStart> {
    run_agent_scheduler_pass_with_max_global_jobs(
        state_dir,
        reclaim_current_process_leases,
        active_project_ids,
        agent_max_global_jobs()?,
        daemon_checkin,
    )
}

fn run_agent_scheduler_pass_with_max_global_jobs(
    state_dir: &Path,
    reclaim_current_process_leases: bool,
    active_project_ids: &[i64],
    max_global_jobs: usize,
    daemon_checkin: Option<&AgentDaemonCheckinSource>,
) -> Result<AgentSchedulerStart> {
    if max_global_jobs == 0 {
        anyhow::bail!("{AGENT_MAX_GLOBAL_JOBS_ENV} must be greater than zero");
    }

    let holder = agent_lease_holder();
    let lease_timeout = agent_lease_timeout()?;
    let success_cooldown = agent_success_cooldown()?;
    let failure_backoff = agent_failure_backoff()?;
    let now = agent_timestamp_seconds();
    let mut pass = AgentSchedulerPass {
        scanned_projects: 0,
        pending_projects: 0,
        active_agent_jobs: active_project_ids.len(),
        skipped_active_lease: 0,
        deferred_projects: 0,
        runs_started: 0,
        runs_recorded: 0,
    };
    let mut jobs = Vec::new();

    let projects = with_agent_store_at(state_dir, |store| {
        if let Some(checkin) = daemon_checkin {
            record_agent_daemon_checkin(store, checkin)?;
        }
        store.list_projects_blocking()
    })?;

    for project in projects {
        if !project.enabled {
            continue;
        }

        pass.scanned_projects += 1;
        if active_project_ids.contains(&project.id) {
            continue;
        }

        let scan = scan_agent_project(&project.path);
        with_agent_store_at(state_dir, |store| {
            store.record_project_scan_blocking(project.id)
        })?;

        let existing_lease = agent_lease_for_project(state_dir, project.id)?;
        let resume_interrupted_task = scan.doing_count > 0
            && existing_lease.as_ref().is_some_and(|lease| {
                agent_lease_is_reclaimable(lease, reclaim_current_process_leases, now)
            });
        let task_selection = if resume_interrupted_task {
            Some(AgentTaskSelection::ResumeDoing)
        } else if scan.has_pending_task() {
            Some(AgentTaskSelection::NextTodo)
        } else if scan.all_actionable_tasks_blocked() {
            Some(AgentTaskSelection::RecoverBlocked)
        } else {
            None
        };

        let Some(task_selection) = task_selection else {
            println!(
                "Project {}: action=idle reason=no_pending_tasks todo={} ready_todo={} blocked_todo={} doing={} blocked_doing={} scan_status={} path={}",
                project.name,
                scan.todo_count,
                scan.available_todo_count(),
                scan.blocked_todo_count,
                scan.doing_count,
                scan.blocked_doing_count,
                scan.status_label(),
                project.path.display()
            );
            continue;
        };

        if let Some(reason) = agent_task_cooldown_reason(
            &project,
            task_selection,
            now,
            success_cooldown,
            failure_backoff,
        ) {
            println!(
                "Project {}: action=skip reason=\"{}\" work={} todo={} ready_todo={} blocked_todo={} doing={} blocked_doing={} scan_status={} path={}",
                project.name,
                reason,
                task_selection.label(),
                scan.todo_count,
                scan.available_todo_count(),
                scan.blocked_todo_count,
                scan.doing_count,
                scan.blocked_doing_count,
                scan.status_label(),
                project.path.display()
            );
            continue;
        }

        pass.pending_projects += 1;
        if pass.active_agent_jobs + jobs.len() >= max_global_jobs {
            pass.deferred_projects += 1;
            println!(
                "Project {}: action=defer reason=max_global_jobs_reached max_global_jobs={} active_agent_jobs={} work={} todo={} ready_todo={} blocked_todo={} doing={} blocked_doing={} scan_status={} path={}",
                project.name,
                max_global_jobs,
                pass.active_agent_jobs,
                task_selection.label(),
                scan.todo_count,
                scan.available_todo_count(),
                scan.blocked_todo_count,
                scan.doing_count,
                scan.blocked_doing_count,
                scan.status_label(),
                project.path.display()
            );
            continue;
        }

        let mut acquired_at = agent_timestamp();
        let mut expires_at = agent_timestamp_after(lease_timeout.as_secs());
        let mut acquired = with_agent_store_at(state_dir, |store| {
            store.try_acquire_lease_blocking(project.id, &holder, &acquired_at, &expires_at)
        })?;
        if !acquired {
            let lease = agent_lease_for_project(state_dir, project.id)?;
            if let Some(lease) = lease.as_ref()
                && try_reclaim_inactive_agent_lease(
                    state_dir,
                    &project,
                    &scan,
                    lease,
                    reclaim_current_process_leases,
                )?
            {
                let reacquired_at = agent_timestamp();
                let reexpires_at = agent_timestamp_after(lease_timeout.as_secs());
                acquired_at = reacquired_at;
                expires_at = reexpires_at;
                acquired = with_agent_store_at(state_dir, |store| {
                    store.try_acquire_lease_blocking(project.id, &holder, &acquired_at, &expires_at)
                })?;
            }

            if !acquired {
                pass.skipped_active_lease += 1;
                let lease = agent_lease_for_project(state_dir, project.id)?;
                print_active_lease_skip(&project, &scan, lease.as_ref());
                continue;
            }
        }

        println!(
            "Project {}: action=running work={} todo={} ready_todo={} blocked_todo={} doing={} blocked_doing={} scan_status={} lease_holder={} lease_acquired_at={} lease_expires_at={} path={}",
            project.name,
            task_selection.label(),
            scan.todo_count,
            scan.available_todo_count(),
            scan.blocked_todo_count,
            scan.doing_count,
            scan.blocked_doing_count,
            scan.status_label(),
            holder,
            format_agent_timestamp(&acquired_at),
            format_agent_timestamp(&expires_at),
            project.path.display()
        );

        let done_task_contents_before = completed_task_contents(&project.path).unwrap_or_default();
        let blocked_task_snapshots_before =
            blocked_task_snapshots(&project.path).unwrap_or_default();
        pass.runs_started += 1;
        jobs.push(AgentRunJob {
            state_dir: state_dir.to_path_buf(),
            project,
            holder: holder.clone(),
            task_selection,
            blocked_task_count_before: scan.blocked_task_count(),
            done_task_contents_before,
            blocked_task_snapshots_before,
        });
    }

    Ok(AgentSchedulerStart { pass, jobs })
}

fn spawn_agent_daemon_run(
    runner: Arc<dyn AgentRunner>,
    job: AgentRunJob,
    shutdown: AgentShutdownSignal,
) -> AgentDaemonRun {
    let project_id = job.project.id;
    let project_name = job.project.name.clone();
    let project_path = job.project.path.clone();
    let handle =
        tokio::task::spawn_blocking(move || run_agent_job(job, runner.as_ref(), &shutdown));

    AgentDaemonRun {
        project_id,
        project_name,
        project_path,
        handle,
    }
}

fn run_agent_job(
    job: AgentRunJob,
    runner: &dyn AgentRunner,
    shutdown: &AgentShutdownSignal,
) -> Result<AgentRunCompletion> {
    let started_at = agent_timestamp();
    let run_result = runner.run_project(&job.project, job.task_selection, shutdown);
    let finished_at = agent_timestamp();

    let (
        status,
        exit_code,
        log_dir,
        stdout_path,
        stderr_path,
        summary,
        codex_session_id,
        task_content,
    ) = match run_result {
        Ok(mut result) => {
            if matches!(result.status, "success" | "idle")
                && blocked_recovery_made_no_progress(&job)
            {
                result.status = "blocked";
                result.summary = format!(
                    "Blocked-task recovery left all {} task(s) blocked across todo and doing; retry after the recovery backoff. Runner result: {}",
                    job.blocked_task_count_before, result.summary
                );
            }

            let task_content = result.codex_session_id.as_ref().and_then(|_| {
                let completed =
                    newly_completed_task(&job.project.path, &job.done_task_contents_before)
                        .ok()
                        .flatten()
                        .map(|entry| entry.content.trim_end().to_string());

                completed.or_else(|| {
                    blocked_task_content_after_run(
                        &job.project.path,
                        &job.blocked_task_snapshots_before,
                        job.task_selection != AgentTaskSelection::NextTodo,
                    )
                    .ok()
                    .flatten()
                })
            });

            (
                result.status,
                result.exit_code,
                Some(result.log_dir.display().to_string()),
                Some(result.stdout_path.display().to_string()),
                Some(result.stderr_path.display().to_string()),
                result.summary,
                result.codex_session_id,
                task_content,
            )
        }
        Err(err) => (
            "failure",
            None,
            None,
            None,
            None,
            format!("Codex runner failed before completion: {err:#}"),
            None,
            None,
        ),
    };

    let release_result = with_agent_store_at(&job.state_dir, |store| {
        store.release_lease_blocking(job.project.id, &job.holder)
    });

    let run_id = with_agent_store_at(&job.state_dir, |store| {
        store.record_run_outcome_blocking(agent_store::AgentRunOutcome {
            project_id: job.project.id,
            status,
            started_at: &started_at,
            finished_at: Some(&finished_at),
            exit_code,
            log_dir: log_dir.as_deref(),
            stdout_path: stdout_path.as_deref(),
            stderr_path: stderr_path.as_deref(),
            summary: Some(&summary),
            codex_session_id: codex_session_id.as_deref(),
            task_content: task_content.as_deref(),
        })
    })?;
    release_result?;

    Ok(AgentRunCompletion {
        run_id,
        project_name: job.project.name,
        project_path: job.project.path,
        status,
        summary,
        stdout_path,
        stderr_path,
    })
}

fn completed_task_contents(project_root: &Path) -> Result<Vec<String>> {
    Ok(read_task_entries(&get_tasks_dir(project_root), "done")?
        .into_iter()
        .map(|entry| entry.content)
        .collect())
}

fn blocked_task_snapshots(project_root: &Path) -> Result<Vec<BlockedTaskSnapshot>> {
    let board_dir = get_tasks_dir(project_root);
    let mut snapshots = Vec::new();

    for status in ["todo", "doing"] {
        snapshots.extend(
            read_task_entries(&board_dir, status)?
                .into_iter()
                .filter(task_entry_is_blocked)
                .map(|entry| BlockedTaskSnapshot {
                    status,
                    content: entry.content.trim_end().to_string(),
                }),
        );
    }

    Ok(snapshots)
}

fn blocked_task_content_after_run(
    project_root: &Path,
    snapshots_before: &[BlockedTaskSnapshot],
    allow_unchanged_single_task: bool,
) -> Result<Option<String>> {
    let snapshots_after = blocked_task_snapshots(project_root)?;
    let mut remaining = std::collections::HashMap::<&BlockedTaskSnapshot, usize>::new();
    for snapshot in snapshots_before {
        *remaining.entry(snapshot).or_default() += 1;
    }

    let changed_task = snapshots_after.iter().find(|snapshot| {
        let Some(count) = remaining.get_mut(snapshot) else {
            return true;
        };
        if *count == 0 {
            true
        } else {
            *count -= 1;
            false
        }
    });

    Ok(changed_task
        .or_else(|| {
            (allow_unchanged_single_task && snapshots_after.len() == 1)
                .then(|| snapshots_after.first())
                .flatten()
        })
        .map(|snapshot| snapshot.content.clone()))
}

fn newly_completed_task(
    project_root: &Path,
    contents_before: &[String],
) -> Result<Option<TaskEntry>> {
    let entries_after = read_task_entries(&get_tasks_dir(project_root), "done")?;
    Ok(newly_added_task_entry(contents_before, &entries_after).cloned())
}

fn newly_added_task_entry<'a>(
    contents_before: &[String],
    entries_after: &'a [TaskEntry],
) -> Option<&'a TaskEntry> {
    if entries_after.len() <= contents_before.len() {
        return None;
    }

    if entries_after.len() == contents_before.len() + 1 {
        for skipped in 0..entries_after.len() {
            let matches_before = entries_after
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != skipped)
                .map(|(_, entry)| entry.content.as_str())
                .eq(contents_before.iter().map(String::as_str));
            if matches_before {
                return entries_after.get(skipped);
            }
        }
    }

    let mut remaining = std::collections::HashMap::<&str, usize>::new();
    for content in contents_before {
        *remaining.entry(content.as_str()).or_default() += 1;
    }

    entries_after.iter().find(|entry| {
        let Some(count) = remaining.get_mut(entry.content.as_str()) else {
            return true;
        };
        if *count == 0 {
            true
        } else {
            *count -= 1;
            false
        }
    })
}

fn blocked_recovery_made_no_progress(job: &AgentRunJob) -> bool {
    if job.task_selection != AgentTaskSelection::RecoverBlocked {
        return false;
    }

    let scan = scan_agent_project(&job.project.path);
    scan.all_actionable_tasks_blocked()
        && scan.blocked_task_count() >= job.blocked_task_count_before
}

fn release_agent_job_lease_for_shutdown(job: &AgentRunJob) -> Result<()> {
    let released = with_agent_store_at(&job.state_dir, |store| {
        store.release_lease_blocking(job.project.id, &job.holder)
    })?;

    if released {
        println!(
            "Project {}: action=release reason=shutdown_before_run lease_holder={} path={}",
            job.project.name,
            job.holder,
            job.project.path.display()
        );
    }

    Ok(())
}

fn print_agent_run_completion(completion: &AgentRunCompletion) -> Result<()> {
    println!(
        "Recorded scheduler run {} for {} ({}) status={}",
        completion.run_id,
        completion.project_name,
        completion.project_path.display(),
        completion.status
    );
    print_agent_run_failure_details(
        completion.status,
        &completion.summary,
        completion.stdout_path.as_deref(),
        completion.stderr_path.as_deref(),
    )
}

fn print_agent_run_failure_details(
    status: &str,
    summary: &str,
    stdout_path: Option<&str>,
    stderr_path: Option<&str>,
) -> Result<()> {
    let summary_label = match status {
        "failure" | "timeout" => "run_failure_summary",
        "blocked" => "run_blocked_summary",
        _ => return Ok(()),
    };

    println!("{summary_label}={summary}");
    println!("run_stdout={}", stdout_path.unwrap_or("<not recorded>"));
    println!("run_stderr={}", stderr_path.unwrap_or("<not recorded>"));
    print_agent_log_tail_with_limit("stderr_tail", stderr_path, 20)
}

fn print_agent_run_heartbeat(
    project: &agent_store::AgentProject,
    elapsed: Duration,
    timeout: Duration,
    stdout_path: &Path,
    stderr_path: &Path,
    last_stderr_bytes: &mut u64,
) -> Result<()> {
    let stdout_bytes = file_size(stdout_path);
    let stderr_bytes = file_size(stderr_path);
    let print_tail = agent_heartbeat_tail_enabled()?;

    println!(
        "Project {}: action=still_running elapsed_seconds={} timeout_seconds={} stdout_bytes={} stderr_bytes={} stdout={} stderr={} path={}",
        project.name,
        elapsed.as_secs(),
        timeout.as_secs(),
        format_optional_u64(stdout_bytes),
        format_optional_u64(stderr_bytes),
        stdout_path.display(),
        stderr_path.display(),
        project.path.display()
    );

    if let Some(stderr_bytes) = stderr_bytes {
        if print_tail && stderr_bytes > *last_stderr_bytes {
            print_agent_log_tail_if_nonempty("stderr_tail", stderr_path, 5)?;
        }
        *last_stderr_bytes = stderr_bytes;
    }

    Ok(())
}

fn print_agent_log_tail_if_nonempty(label: &str, path: &Path, limit: usize) -> Result<()> {
    match fs::read_to_string(path) {
        Ok(content) => {
            let tail = tail_lines(&content, limit);
            if !tail.is_empty() {
                println!("{label}={}", path.display());
                for line in tail {
                    println!("  {line}");
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            println!("{label}=<unreadable: {}>", err);
        }
    }

    Ok(())
}

fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|metadata| metadata.len())
}

fn format_optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn agent_lease_for_project(
    state_dir: &Path,
    project_id: i64,
) -> Result<Option<agent_store::AgentLeaseRecord>> {
    with_agent_store_at(state_dir, |store| {
        store.lease_for_project_blocking(project_id)
    })
}

fn agent_lease_is_reclaimable(
    lease: &agent_store::AgentLeaseRecord,
    reclaim_current_process_leases: bool,
    now: u64,
) -> bool {
    if lease
        .expires_at
        .parse::<u64>()
        .is_ok_and(|expires_at| expires_at <= now)
    {
        return true;
    }

    match agent_lease_holder_liveness(&lease.holder) {
        AgentLeaseHolderLiveness::Dead => true,
        AgentLeaseHolderLiveness::CurrentProcess => reclaim_current_process_leases,
        AgentLeaseHolderLiveness::Alive | AgentLeaseHolderLiveness::Unknown => false,
    }
}

fn try_reclaim_inactive_agent_lease(
    state_dir: &Path,
    project: &agent_store::AgentProject,
    scan: &AgentProjectScan,
    lease: &agent_store::AgentLeaseRecord,
    reclaim_current_process_leases: bool,
) -> Result<bool> {
    let liveness = agent_lease_holder_liveness(&lease.holder);
    if !agent_lease_is_reclaimable(
        lease,
        reclaim_current_process_leases,
        agent_timestamp_seconds(),
    ) {
        return Ok(false);
    }

    let released = with_agent_store_at(state_dir, |store| {
        store.release_lease_blocking(project.id, &lease.holder)
    })?;
    if released {
        println!(
            "Project {}: action=reclaim reason=inactive_lease todo={} scan_status={} lease_holder={} lease_process={} lease_acquired_at={} lease_expires_at={} path={}",
            project.name,
            scan.todo_count,
            scan.status_label(),
            lease.holder,
            liveness.label(),
            format_agent_timestamp(&lease.acquired_at),
            format_agent_timestamp(&lease.expires_at),
            project.path.display()
        );
    }

    Ok(released)
}

fn print_active_lease_skip(
    project: &agent_store::AgentProject,
    scan: &AgentProjectScan,
    lease: Option<&agent_store::AgentLeaseRecord>,
) {
    if let Some(lease) = lease {
        let liveness = agent_lease_holder_liveness(&lease.holder);
        println!(
            "Project {}: action=skip reason=active_lease todo={} scan_status={} lease_holder={} lease_process={} lease_acquired_at={} lease_expires_at={} path={}",
            project.name,
            scan.todo_count,
            scan.status_label(),
            lease.holder,
            liveness.label(),
            format_agent_timestamp(&lease.acquired_at),
            format_agent_timestamp(&lease.expires_at),
            project.path.display()
        );
    } else {
        println!(
            "Project {}: action=skip reason=active_lease todo={} scan_status={} path={}",
            project.name,
            scan.todo_count,
            scan.status_label(),
            project.path.display()
        );
    }
}

impl AgentLeaseHolderLiveness {
    fn label(self) -> &'static str {
        match self {
            AgentLeaseHolderLiveness::CurrentProcess => "current_process",
            AgentLeaseHolderLiveness::Alive => "alive",
            AgentLeaseHolderLiveness::Dead => "dead",
            AgentLeaseHolderLiveness::Unknown => "unknown",
        }
    }
}

fn agent_lease_holder_liveness(holder: &str) -> AgentLeaseHolderLiveness {
    let Some(pid) = agent_lease_holder_pid(holder) else {
        return AgentLeaseHolderLiveness::Unknown;
    };

    agent_pid_liveness(pid)
}

fn agent_pid_liveness(pid: u32) -> AgentLeaseHolderLiveness {
    if pid == std::process::id() {
        return AgentLeaseHolderLiveness::CurrentProcess;
    }

    match local_process_is_running(pid) {
        Some(true) => AgentLeaseHolderLiveness::Alive,
        Some(false) => AgentLeaseHolderLiveness::Dead,
        None => AgentLeaseHolderLiveness::Unknown,
    }
}

fn agent_lease_holder_pid(holder: &str) -> Option<u32> {
    holder.strip_prefix("clt-agent-")?.parse().ok()
}

#[cfg(unix)]
fn local_process_is_running(pid: u32) -> Option<bool> {
    let output = Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .arg("-o")
        .arg("pid=")
        .output()
        .ok()?;

    Some(output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

#[cfg(not(unix))]
fn local_process_is_running(_pid: u32) -> Option<bool> {
    None
}

impl CodexAgentRunner {
    fn new(state_dir: PathBuf) -> Result<Self> {
        Ok(Self {
            state_dir,
            timeout: agent_run_timeout()?,
            heartbeat_interval: agent_poll_interval()?,
            command: agent_codex_command(),
        })
    }

    #[cfg(test)]
    fn with_command(state_dir: PathBuf, timeout: Duration, command: PathBuf) -> Self {
        Self {
            state_dir,
            timeout,
            heartbeat_interval: Duration::from_secs(AGENT_DEFAULT_POLL_INTERVAL_SECONDS),
            command,
        }
    }
}

fn agent_codex_prompt(
    project: &agent_store::AgentProject,
    task_selection: AgentTaskSelection,
) -> String {
    let clt_skill_available =
        agent_skill_is_available(&project.path, CLT_TASK_MANAGEMENT_SKILL_NAME);
    let git_skill_available = project.git_mode == AgentGitMode::Off
        || agent_skill_is_available(&project.path, GIT_COMMIT_SKILL_NAME);
    build_agent_codex_prompt(
        project,
        task_selection,
        clt_skill_available,
        git_skill_available,
    )
}

fn build_agent_codex_prompt(
    project: &agent_store::AgentProject,
    task_selection: AgentTaskSelection,
    clt_skill_available: bool,
    git_skill_available: bool,
) -> String {
    let mut prompt = AGENT_CODEX_PROMPT_BASE.to_string();
    if clt_skill_available {
        prompt.push_str(
            "\nTask workflow:\n- Use the $clt-task-management skill for the task-board workflow.\n",
        );
    }
    match task_selection {
        AgentTaskSelection::NextTodo => {}
        AgentTaskSelection::ResumeDoing => {
            prompt.push_str(AGENT_RESUME_DOING_PROMPT_APPENDIX);
        }
        AgentTaskSelection::RecoverBlocked => {
            prompt.push_str(AGENT_RECOVER_BLOCKED_PROMPT_APPENDIX);
        }
    }
    match project.git_mode {
        AgentGitMode::Off => {}
        AgentGitMode::Commit => prompt.push_str(AGENT_GIT_COMMIT_PROMPT_APPENDIX),
        AgentGitMode::CommitAndPush => {
            prompt.push_str(AGENT_GIT_COMMIT_PROMPT_APPENDIX);
            prompt.push_str(AGENT_GIT_PUSH_PROMPT_APPENDIX);
        }
    }
    if !clt_skill_available {
        append_embedded_agent_skill(
            &mut prompt,
            CLT_TASK_MANAGEMENT_SKILL_NAME,
            EMBEDDED_CLT_TASK_MANAGEMENT_SKILL,
        );
    }
    if project.git_mode != AgentGitMode::Off && !git_skill_available {
        append_embedded_agent_skill(
            &mut prompt,
            GIT_COMMIT_SKILL_NAME,
            EMBEDDED_GIT_COMMIT_SKILL,
        );
    }
    prompt
}

fn append_embedded_agent_skill(prompt: &mut String, name: &str, contents: &str) {
    prompt.push_str("\n\nEmbedded skill fallback:\n");
    prompt.push_str("- The $");
    prompt.push_str(name);
    prompt.push_str(
        " skill was not found in a standard Codex skill directory. Follow this bundled version for this run.\n\n<skill>\n<name>",
    );
    prompt.push_str(name);
    prompt.push_str("</name>\n<source>embedded in clt</source>\n");
    prompt.push_str(contents);
    if !contents.ends_with('\n') {
        prompt.push('\n');
    }
    prompt.push_str("</skill>");
}

fn agent_skill_is_available(project_root: &Path, skill_name: &str) -> bool {
    agent_skill_search_roots(project_root)
        .iter()
        .any(|root| agent_skill_root_contains_name(root, skill_name))
}

fn agent_skill_search_roots(project_root: &Path) -> Vec<PathBuf> {
    let repository_root =
        get_task_root_at(project_root, false).unwrap_or_else(|_| project_root.to_path_buf());
    let mut roots = Vec::new();
    let mut directory = project_root.to_path_buf();

    loop {
        roots.push(directory.join(".agents/skills"));
        if directory == repository_root
            || !directory.pop()
            || !directory.starts_with(&repository_root)
        {
            break;
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".agents/skills"));
    }
    roots.push(PathBuf::from("/etc/codex/skills"));
    roots
}

fn agent_skill_root_contains_name(root: &Path, skill_name: &str) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };

    entries.filter_map(Result::ok).any(|entry| {
        fs::read_to_string(entry.path().join("SKILL.md"))
            .ok()
            .and_then(|contents| skill_frontmatter_name(&contents).map(str::to_string))
            .is_some_and(|name| name == skill_name)
    })
}

fn skill_frontmatter_name(contents: &str) -> Option<&str> {
    let mut lines = contents.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }

    for line in lines {
        let line = line.trim();
        if line == "---" {
            break;
        }
        if let Some(name) = line.strip_prefix("name:") {
            return Some(name.trim().trim_matches(['\"', '\'']));
        }
    }
    None
}

impl AgentRunner for CodexAgentRunner {
    fn run_project(
        &self,
        project: &agent_store::AgentProject,
        task_selection: AgentTaskSelection,
        shutdown: &AgentShutdownSignal,
    ) -> Result<AgentRunResult> {
        let log_dir = agent_project_run_log_dir(&self.state_dir, project)?;
        fs::create_dir_all(&log_dir)
            .with_context(|| format!("Failed to create agent run log directory {:?}", log_dir))?;

        let run_file_stem = agent_log_file_stem(project.id);
        let stdout_path = log_dir.join(format!("{run_file_stem}.out"));
        let stderr_path = log_dir.join(format!("{run_file_stem}.err"));
        let stdout_file = fs::File::create(&stdout_path)
            .with_context(|| format!("Failed to create stdout log {:?}", stdout_path))?;
        let stderr_file = fs::File::create(&stderr_path)
            .with_context(|| format!("Failed to create stderr log {:?}", stderr_path))?;

        let mut command = Command::new(&self.command);
        command
            .arg("--sandbox")
            .arg("danger-full-access")
            .arg("--ask-for-approval")
            .arg("never");
        let store = open_agent_store_at(&self.state_dir)?;
        let model_target = if let Some(model_id) = project.codex_model.as_ref() {
            agent_store::AgentModelDefaults {
                provider_id: Some(
                    project
                        .codex_provider
                        .clone()
                        .unwrap_or_else(|| "openai".to_string()),
                ),
                model_id: Some(model_id.clone()),
            }
        } else {
            store.resolve_model_target_blocking(project)?
        };
        if let (Some(provider), Some(model)) = (
            model_target.provider_id.as_deref(),
            model_target.model_id.as_deref(),
        ) {
            command
                .arg("--config")
                .arg(format!("model_provider={provider:?}"));
            command.arg("--model").arg(model);
        }
        let model_reasoning_effort = if project.codex_reasoning_effort.is_none() {
            match (
                model_target.provider_id.as_deref(),
                model_target.model_id.as_deref(),
            ) {
                (Some(provider), Some(model)) => {
                    store.model_target_reasoning_blocking(provider, model)?
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(reasoning_effort) = project
            .codex_reasoning_effort
            .as_deref()
            .or(model_reasoning_effort.as_deref())
        {
            command
                .arg("--config")
                .arg(format!("model_reasoning_effort=\"{reasoning_effort}\""));
        }
        if project.codex_fast_enabled {
            command
                .arg("--enable")
                .arg("fast_mode")
                .arg("--config")
                .arg("service_tier=\"fast\"");
        } else {
            command.arg("--disable").arg("fast_mode");
        }
        command
            .arg("exec")
            .arg("-C")
            .arg(&project.path)
            .arg(agent_codex_prompt(project, task_selection))
            .current_dir(&project.path)
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));
        configure_agent_child_command(&mut command);

        let spawn_result = command.spawn();

        let mut child = match spawn_result {
            Ok(child) => child,
            Err(err) => {
                let summary = format!(
                    "Failed to start Codex command {} in {}: {err}",
                    self.command.display(),
                    project.path.display()
                );
                append_agent_log_line(&stderr_path, &summary)?;
                return Ok(AgentRunResult {
                    status: "failure",
                    exit_code: None,
                    log_dir,
                    stdout_path,
                    stderr_path,
                    summary,
                    codex_session_id: None,
                });
            }
        };

        let mut last_heartbeat_stderr_bytes = 0;
        let wait_result = wait_for_child_with_timeout_and_heartbeat(
            &mut child,
            self.timeout,
            self.heartbeat_interval,
            |elapsed| {
                print_agent_run_heartbeat(
                    project,
                    elapsed,
                    self.timeout,
                    &stdout_path,
                    &stderr_path,
                    &mut last_heartbeat_stderr_bytes,
                )
            },
            || shutdown.load(Ordering::SeqCst),
        )?;
        let stdout = fs::read_to_string(&stdout_path).unwrap_or_default();
        let codex_session_id = agent_codex_session_id_from_log(&stderr_path)?;
        let (status, exit_code, summary) = match wait_result {
            AgentProcessWait::Exited(exit_status) => {
                let exit_code = exit_status.code().map(i64::from);
                if stdout.contains(AGENT_NO_TASKS_LEFT_MARKER) {
                    (
                        "idle",
                        exit_code,
                        "Codex reported no available tasks.".to_string(),
                    )
                } else if exit_status.success() {
                    (
                        "success",
                        exit_code,
                        "Codex run completed successfully.".to_string(),
                    )
                } else {
                    (
                        "failure",
                        exit_code,
                        format!("Codex exited with status {exit_status}."),
                    )
                }
            }
            AgentProcessWait::TimedOut(exit_status) => {
                append_agent_log_line(
                    &stderr_path,
                    &format!("Codex timed out after {} seconds.", self.timeout.as_secs()),
                )?;
                (
                    "timeout",
                    exit_status.and_then(|status| status.code().map(i64::from)),
                    format!("Codex timed out after {} seconds.", self.timeout.as_secs()),
                )
            }
            AgentProcessWait::Interrupted(exit_status) => {
                append_agent_log_line(
                    &stderr_path,
                    "Codex stopped because the agent is shutting down.",
                )?;
                (
                    "interrupted",
                    exit_status.and_then(|status| status.code().map(i64::from)),
                    "Codex stopped because the agent is shutting down.".to_string(),
                )
            }
        };

        Ok(AgentRunResult {
            status,
            exit_code,
            log_dir,
            stdout_path,
            stderr_path,
            summary,
            codex_session_id,
        })
    }
}

enum AgentProcessWait {
    Exited(ExitStatus),
    TimedOut(Option<ExitStatus>),
    Interrupted(Option<ExitStatus>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskBlockingState {
    Blocked,
    Unblocked,
}

fn task_entry_is_blocked(entry: &TaskEntry) -> bool {
    let mut state = None;

    for line in entry.content.lines() {
        let heading = line.trim().trim_start_matches(['#', '-', '*', ' ']).trim();
        if heading.eq_ignore_ascii_case("blocked note:") {
            state = Some(TaskBlockingState::Blocked);
        }
        if let Some(line_state) = latest_task_blocking_state_on_line(line) {
            state = Some(line_state);
        }
    }

    state == Some(TaskBlockingState::Blocked)
}

fn latest_task_blocking_state_on_line(line: &str) -> Option<TaskBlockingState> {
    let uppercase = line.to_ascii_uppercase();
    let mut latest = None;

    for (marker, state) in [
        ("BLOCKED ", TaskBlockingState::Blocked),
        ("UNBLOCKED ", TaskBlockingState::Unblocked),
        ("COMPLETED ", TaskBlockingState::Unblocked),
    ] {
        for (index, matched) in uppercase.match_indices(marker) {
            let has_word_boundary = uppercase[..index]
                .chars()
                .next_back()
                .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
            let remainder = &uppercase.as_bytes()[index + matched.len()..];

            if has_word_boundary
                && starts_with_task_note_date(remainder)
                && latest.is_none_or(|(latest_index, _)| index > latest_index)
            {
                latest = Some((index, state));
            }
        }
    }

    latest.map(|(_, state)| state)
}

fn starts_with_task_note_date(value: &[u8]) -> bool {
    value.len() >= 11
        && value[0..4].iter().all(u8::is_ascii_digit)
        && value[4] == b'-'
        && value[5..7].iter().all(u8::is_ascii_digit)
        && value[7] == b'-'
        && value[8..10].iter().all(u8::is_ascii_digit)
        && value[10] == b':'
}

fn scan_agent_project(project_root: &Path) -> AgentProjectScan {
    if !project_root.exists() {
        return AgentProjectScan::missing();
    }

    match ensure_existing_board(project_root) {
        Ok(true) => {}
        Ok(false) => return AgentProjectScan::uninitialized(),
        Err(err) => return AgentProjectScan::unavailable(err),
    }

    let board_dir = get_tasks_dir(project_root);
    let todo_entries = match read_task_entries(&board_dir, "todo") {
        Ok(entries) => entries,
        Err(err) => return AgentProjectScan::unavailable(err),
    };
    let doing_entries = match read_task_entries(&board_dir, "doing") {
        Ok(entries) => entries,
        Err(err) => return AgentProjectScan::unavailable(err),
    };
    let todo_count = todo_entries.len();
    let blocked_todo_count = todo_entries
        .iter()
        .filter(|entry| task_entry_is_blocked(entry))
        .count();
    let doing_count = doing_entries.len();
    let blocked_doing_count = doing_entries
        .iter()
        .filter(|entry| task_entry_is_blocked(entry))
        .count();

    AgentProjectScan::from_counts(
        todo_count,
        blocked_todo_count,
        doing_count,
        blocked_doing_count,
    )
}

#[cfg(test)]
fn has_pending_agent_task(project_root: &Path) -> bool {
    scan_agent_project(project_root).has_pending_task()
}

impl AgentProjectScan {
    #[cfg(test)]
    fn pending(todo_count: usize) -> Self {
        Self::from_counts(todo_count, 0, 0, 0)
    }

    #[cfg(test)]
    fn pending_with_doing(todo_count: usize, doing_count: usize) -> Self {
        Self::from_counts(todo_count, 0, doing_count, 0)
    }

    fn from_counts(
        todo_count: usize,
        blocked_todo_count: usize,
        doing_count: usize,
        blocked_doing_count: usize,
    ) -> Self {
        let available_todo_count = todo_count.saturating_sub(blocked_todo_count);
        let task_count = todo_count.saturating_add(doing_count);
        let blocked_task_count = blocked_todo_count.saturating_add(blocked_doing_count);
        let status = if available_todo_count > 0 {
            AgentProjectScanStatus::Pending
        } else if task_count > 0 && blocked_task_count == task_count {
            AgentProjectScanStatus::Blocked
        } else {
            AgentProjectScanStatus::Empty
        };

        Self {
            status,
            todo_count,
            blocked_todo_count,
            doing_count,
            blocked_doing_count,
        }
    }

    #[cfg(test)]
    fn empty() -> Self {
        Self::from_counts(0, 0, 0, 0)
    }

    fn missing() -> Self {
        Self {
            status: AgentProjectScanStatus::Missing,
            todo_count: 0,
            blocked_todo_count: 0,
            doing_count: 0,
            blocked_doing_count: 0,
        }
    }

    fn uninitialized() -> Self {
        Self {
            status: AgentProjectScanStatus::Uninitialized,
            todo_count: 0,
            blocked_todo_count: 0,
            doing_count: 0,
            blocked_doing_count: 0,
        }
    }

    fn unavailable(err: anyhow::Error) -> Self {
        Self {
            status: AgentProjectScanStatus::Unavailable(err.to_string()),
            todo_count: 0,
            blocked_todo_count: 0,
            doing_count: 0,
            blocked_doing_count: 0,
        }
    }

    fn has_pending_task(&self) -> bool {
        self.status == AgentProjectScanStatus::Pending
    }

    fn all_actionable_tasks_blocked(&self) -> bool {
        self.status == AgentProjectScanStatus::Blocked
    }

    fn available_todo_count(&self) -> usize {
        self.todo_count.saturating_sub(self.blocked_todo_count)
    }

    fn blocked_task_count(&self) -> usize {
        self.blocked_todo_count
            .saturating_add(self.blocked_doing_count)
    }

    fn has_schedulable_work(&self) -> bool {
        self.has_pending_task() || self.all_actionable_tasks_blocked()
    }

    fn pending_signal(&self) -> &'static str {
        if self.has_schedulable_work() {
            "yes"
        } else {
            "no"
        }
    }

    fn status_label(&self) -> &str {
        match &self.status {
            AgentProjectScanStatus::Pending => "pending",
            AgentProjectScanStatus::Blocked => "blocked",
            AgentProjectScanStatus::Empty => "empty",
            AgentProjectScanStatus::Missing => "missing",
            AgentProjectScanStatus::Uninitialized => "uninitialized",
            AgentProjectScanStatus::Unavailable(_) => "unavailable",
        }
    }
}

fn resolve_agent_project_root(
    path: Option<&Path>,
    local: bool,
    default_root: &Path,
) -> Result<PathBuf> {
    match path {
        Some(path) => get_task_root_at(path, local),
        None => canonicalize_existing_path(default_root),
    }
}

fn canonicalize_existing_path(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).with_context(|| format!("Failed to resolve project path {:?}", path))
}

fn open_agent_store() -> Result<agent_store::TursoAgentStore> {
    let state_dir = ensure_agent_state_dir()?;
    open_agent_store_at(&state_dir)
}

fn open_agent_store_at(state_dir: &Path) -> Result<agent_store::TursoAgentStore> {
    ensure_agent_state_dir_at(state_dir)?;
    agent_store::TursoAgentStore::open_blocking(state_dir)
}

fn with_agent_store_at<T>(
    state_dir: &Path,
    action: impl FnOnce(&agent_store::TursoAgentStore) -> Result<T>,
) -> Result<T> {
    let store = open_agent_store_at(state_dir)?;
    action(&store)
}

fn ensure_agent_state_dir() -> Result<PathBuf> {
    let state_dir = agent_state_dir()?;
    ensure_agent_state_dir_at(&state_dir)?;
    Ok(state_dir)
}

fn ensure_agent_state_dir_at(state_dir: &Path) -> Result<()> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("Failed to create agent state directory {:?}", state_dir))
}

fn agent_state_dir() -> Result<PathBuf> {
    resolve_agent_state_dir(
        current_agent_platform(),
        std::env::var_os(AGENT_STATE_DIR_ENV).map(PathBuf::from),
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn current_agent_platform() -> AgentPlatform {
    if cfg!(target_os = "macos") {
        AgentPlatform::Macos
    } else if cfg!(target_os = "linux") {
        AgentPlatform::Linux
    } else {
        AgentPlatform::Other
    }
}

fn resolve_agent_state_dir(
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

fn agent_timestamp() -> String {
    agent_timestamp_seconds().to_string()
}

fn agent_timestamp_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn agent_timestamp_after(seconds: u64) -> String {
    std::time::SystemTime::now()
        .checked_add(std::time::Duration::from_secs(seconds))
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(agent_timestamp)
}

fn format_agent_timestamp(raw: &str) -> String {
    let Ok(seconds) = raw.parse::<i64>() else {
        return raw.to_string();
    };

    let Some(utc) = DateTime::<Utc>::from_timestamp(seconds, 0) else {
        return raw.to_string();
    };

    utc.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string()
}

fn format_optional_agent_timestamp(raw: Option<&str>) -> String {
    raw.map(format_agent_timestamp)
        .unwrap_or_else(|| "-".to_string())
}

fn agent_lease_holder() -> String {
    format!("clt-agent-{}", std::process::id())
}

impl AgentDaemonCheckinSource {
    fn current() -> Self {
        Self {
            holder: agent_lease_holder(),
            mode: agent_daemon_mode(),
            started_at: agent_timestamp(),
        }
    }
}

fn agent_daemon_mode() -> String {
    std::env::var(AGENT_DAEMON_MODE_ENV)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "cli".to_string())
}

fn record_agent_daemon_checkin(
    store: &agent_store::TursoAgentStore,
    checkin: &AgentDaemonCheckinSource,
) -> Result<()> {
    let checked_in_at = agent_timestamp();
    let expires_at = agent_timestamp_after(AGENT_DAEMON_CHECKIN_STALE_SECONDS);
    store.record_daemon_checkin_blocking(
        &checkin.holder,
        &checkin.mode,
        &checkin.started_at,
        &checked_in_at,
        &expires_at,
    )
}

async fn clear_agent_daemon_checkin_best_effort(state_dir: &Path, holder: &str) {
    let state_dir = state_dir.to_path_buf();
    let holder = holder.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        with_agent_store_at(&state_dir, |store| {
            store.clear_daemon_checkin_blocking(&holder)?;
            Ok(())
        })
    })
    .await;
}

fn agent_max_global_jobs() -> Result<usize> {
    match std::env::var(AGENT_MAX_GLOBAL_JOBS_ENV) {
        Ok(raw) => parse_agent_positive_usize(AGENT_MAX_GLOBAL_JOBS_ENV, &raw),
        Err(std::env::VarError::NotPresent) => Ok(AGENT_DEFAULT_MAX_GLOBAL_JOBS),
        Err(err) => anyhow::bail!("Failed to read {AGENT_MAX_GLOBAL_JOBS_ENV}: {err}"),
    }
}

fn agent_heartbeat_tail_enabled() -> Result<bool> {
    match std::env::var(AGENT_HEARTBEAT_TAIL_ENV) {
        Ok(raw) => parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, &raw),
        Err(std::env::VarError::NotPresent) => Ok(false),
        Err(err) => anyhow::bail!("Failed to read {AGENT_HEARTBEAT_TAIL_ENV}: {err}"),
    }
}

fn agent_lease_timeout() -> Result<Duration> {
    agent_timeout_from_env(
        AGENT_LEASE_TIMEOUT_SECONDS_ENV,
        AGENT_DEFAULT_LEASE_TIMEOUT_SECONDS,
    )
}

fn agent_failure_backoff() -> Result<Duration> {
    agent_timeout_from_env(
        AGENT_FAILURE_BACKOFF_SECONDS_ENV,
        AGENT_DEFAULT_FAILURE_BACKOFF_SECONDS,
    )
}

fn agent_poll_interval() -> Result<Duration> {
    agent_timeout_from_env(
        AGENT_POLL_INTERVAL_SECONDS_ENV,
        AGENT_DEFAULT_POLL_INTERVAL_SECONDS,
    )
}

fn agent_run_timeout() -> Result<Duration> {
    agent_timeout_from_env(
        AGENT_RUN_TIMEOUT_SECONDS_ENV,
        AGENT_DEFAULT_RUN_TIMEOUT_SECONDS,
    )
}

fn agent_success_cooldown() -> Result<Duration> {
    agent_timeout_from_env(
        AGENT_SUCCESS_COOLDOWN_SECONDS_ENV,
        AGENT_DEFAULT_SUCCESS_COOLDOWN_SECONDS,
    )
}

fn agent_timeout_from_env(env_name: &str, default_seconds: u64) -> Result<Duration> {
    match std::env::var(env_name) {
        Ok(raw) => parse_agent_timeout_duration(env_name, &raw),
        Err(std::env::VarError::NotPresent) => Ok(default_seconds),
        Err(err) => anyhow::bail!("Failed to read {env_name}: {err}"),
    }
    .map(Duration::from_secs)
}

fn parse_agent_timeout_duration(env_name: &str, raw: &str) -> Result<u64> {
    let seconds = raw
        .parse::<u64>()
        .with_context(|| format!("{env_name} must be a positive integer number of seconds"))?;
    if seconds == 0 {
        anyhow::bail!("{env_name} must be greater than zero");
    }

    Ok(seconds)
}

fn parse_agent_positive_usize(env_name: &str, raw: &str) -> Result<usize> {
    let value = raw
        .parse::<usize>()
        .with_context(|| format!("{env_name} must be a positive integer"))?;
    if value == 0 {
        anyhow::bail!("{env_name} must be greater than zero");
    }

    Ok(value)
}

fn parse_agent_bool(env_name: &str, raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => anyhow::bail!("{env_name} must be one of 1, true, yes, on, 0, false, no, off"),
    }
}

fn agent_project_cooldown_reason(
    project: &agent_store::AgentProject,
    now: u64,
    success_cooldown: Duration,
    failure_backoff: Duration,
) -> Option<String> {
    if project.failure_count > 0
        && let Some(remaining) =
            remaining_agent_delay(project.last_failure_at.as_deref(), now, failure_backoff)
    {
        return Some(format!("failure backoff active for {remaining}s"));
    }

    remaining_agent_delay(project.last_success_at.as_deref(), now, success_cooldown)
        .map(|remaining| format!("success cooldown active for {remaining}s"))
}

fn agent_task_cooldown_reason(
    project: &agent_store::AgentProject,
    task_selection: AgentTaskSelection,
    now: u64,
    success_cooldown: Duration,
    failure_backoff: Duration,
) -> Option<String> {
    if task_selection == AgentTaskSelection::ResumeDoing {
        return None;
    }

    agent_project_cooldown_reason(project, now, success_cooldown, failure_backoff).or_else(|| {
        (task_selection == AgentTaskSelection::RecoverBlocked)
            .then(|| {
                remaining_agent_delay(
                    project.last_blocked_recovery_at.as_deref(),
                    now,
                    failure_backoff,
                )
            })
            .flatten()
            .map(|remaining| format!("blocked-task recovery backoff active for {remaining}s"))
    })
}

fn remaining_agent_delay(last_at: Option<&str>, now: u64, delay: Duration) -> Option<u64> {
    let last_at = last_at?.parse::<u64>().ok()?;
    let ready_at = last_at.saturating_add(delay.as_secs());

    if ready_at > now {
        Some(ready_at - now)
    } else {
        None
    }
}

fn agent_project_run_log_dir(
    state_dir: &Path,
    project: &agent_store::AgentProject,
) -> Result<PathBuf> {
    let slug = agent_project_slug(project);
    Ok(state_dir.join("runs").join(slug))
}

fn agent_project_slug(project: &agent_store::AgentProject) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;

    for ch in project.name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_separator = false;
        } else if (ch == '-' || ch == '_' || ch.is_whitespace()) && !last_was_separator {
            slug.push('-');
            last_was_separator = true;
        }
    }

    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("project-{}", project.id)
    } else {
        format!("{}-{}", project.id, slug)
    }
}

fn agent_log_file_stem(project_id: i64) -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "{}-{:03}-p{}-{}",
        duration.as_secs(),
        duration.subsec_millis(),
        project_id,
        std::process::id()
    )
}

fn wait_for_child_with_timeout_and_heartbeat(
    child: &mut Child,
    timeout: Duration,
    heartbeat_interval: Duration,
    mut heartbeat: impl FnMut(Duration) -> Result<()>,
    mut should_shutdown: impl FnMut() -> bool,
) -> Result<AgentProcessWait> {
    let heartbeat_interval = if heartbeat_interval.is_zero() {
        Duration::from_millis(250)
    } else {
        heartbeat_interval
    };
    let started = Instant::now();
    let mut last_heartbeat = started;

    loop {
        if let Some(status) = child.try_wait().context("Failed to poll Codex process")? {
            return Ok(AgentProcessWait::Exited(status));
        }

        if should_shutdown() {
            let status = stop_agent_child_process(child)
                .context("Failed to stop Codex process during agent shutdown")?;
            return Ok(AgentProcessWait::Interrupted(status));
        }

        if started.elapsed() >= timeout {
            let status = stop_agent_child_process(child)
                .context("Failed to stop timed out Codex process")?;
            return Ok(AgentProcessWait::TimedOut(status));
        }

        if last_heartbeat.elapsed() >= heartbeat_interval {
            heartbeat(started.elapsed())?;
            last_heartbeat = Instant::now();
        }

        thread::sleep(std::cmp::min(
            Duration::from_millis(250),
            heartbeat_interval,
        ));
    }
}

fn configure_agent_child_command(command: &mut Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
}

fn stop_agent_child_process(child: &mut Child) -> Result<Option<ExitStatus>> {
    request_agent_child_termination(child)?;

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if let Some(status) = child
            .try_wait()
            .context("Failed to poll Codex process after termination request")?
        {
            return Ok(Some(status));
        }

        thread::sleep(Duration::from_millis(25));
    }

    child.kill().context("Failed to force-stop Codex process")?;
    Ok(child.wait().ok())
}

#[cfg(unix)]
fn request_agent_child_termination(child: &mut Child) -> Result<()> {
    let process_group = format!("-{}", child.id());
    match Command::new("kill")
        .arg("-TERM")
        .arg(&process_group)
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        _ => {
            child
                .kill()
                .context("Failed to stop Codex process directly")?;
            Ok(())
        }
    }
}

#[cfg(not(unix))]
fn request_agent_child_termination(child: &mut Child) -> Result<()> {
    child
        .kill()
        .context("Failed to stop Codex process directly")?;
    Ok(())
}

fn append_agent_log_line(path: &Path, line: &str) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("Failed to append to agent log {:?}", path))?;
    writeln!(file, "{line}").with_context(|| format!("Failed to write agent log {:?}", path))
}

fn codex_config_path() -> Result<PathBuf> {
    let codex_home = std::env::var_os(CODEX_HOME_ENV)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or_else(|| {
            anyhow::anyhow!("HOME or {CODEX_HOME_ENV} is required to find Codex config")
        })?;
    Ok(codex_home.join("config.toml"))
}

fn valid_codex_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn valid_environment_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn mutate_codex_config_at(
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

fn upsert_codex_provider_config_at(
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

fn remove_codex_provider_config_at(path: &Path, provider_id: &str) -> Result<bool> {
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

fn set_codex_default_config_at(
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

fn read_codex_default_config_at(path: &Path) -> Result<(Option<String>, Option<String>)> {
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

fn set_codex_model_reasoning_if_default_at(
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

mod agent_store {
    use super::*;
    use turso::{Builder, Connection, Database, Value, params};

    // This is a contention ceiling, not a delay for every query: Turso only
    // sleeps and retries while a statement reports that the database is busy.
    const AGENT_DB_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

    struct AgentMigration {
        version: i64,
        statements: &'static [&'static str],
    }

    const AGENT_MIGRATIONS: &[AgentMigration] = &[
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
    ];

    pub(crate) struct TursoAgentStore {
        #[cfg_attr(not(test), allow(dead_code))]
        db_path: PathBuf,
        db: Database,
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
        pub(crate) task_content: Option<&'a str>,
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
            fs::create_dir_all(state_dir).with_context(|| {
                format!("Failed to create agent state directory {:?}", state_dir)
            })?;
            let db_path = state_dir.join(AGENT_DB_FILE);
            let db = Builder::new_local(db_path.to_string_lossy().as_ref())
                .experimental_multiprocess_wal(true)
                .build()
                .await
                .with_context(|| format!("Failed to open agent database {:?}", db_path))?;
            let conn = db
                .connect()
                .with_context(|| format!("Failed to connect to agent database {:?}", db_path))?;
            configure_agent_connection(&conn)?;

            apply_migrations(&conn).await?;

            Ok(Self { db_path, db })
        }

        fn connect(&self) -> Result<Connection> {
            let conn = self
                .db
                .connect()
                .context("Failed to connect to agent database")?;
            configure_agent_connection(&conn)?;
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
            let conn = self.connect()?;
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
            let conn = self.connect()?;
            let path = project_root.display().to_string();
            let removed = conn
                .execute("DELETE FROM projects WHERE path = ?1", [path.as_str()])
                .await
                .with_context(|| format!("Failed to unregister project {}", path))?;

            Ok(removed > 0)
        }

        pub(crate) fn list_projects_blocking(&self) -> Result<Vec<AgentProject>> {
            tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?
                .block_on(self.list_projects())
        }

        async fn list_projects(&self) -> Result<Vec<AgentProject>> {
            let conn = self.connect()?;
            let mut rows = conn
                .query(
                    "SELECT id, path, name, enabled, git_mode, codex_provider, codex_model,
                            codex_reasoning_effort, codex_fast_enabled, last_scan_at, last_run_at,
                            last_success_at, last_failure_at, last_blocked_recovery_at,
                            failure_count
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
                let last_run_at = row_optional_text(&row, 10, "last_run_at")?;
                let last_success_at = row_optional_text(&row, 11, "last_success_at")?;
                let last_failure_at = row_optional_text(&row, 12, "last_failure_at")?;
                let last_blocked_recovery_at =
                    row_optional_text(&row, 13, "last_blocked_recovery_at")?;
                let failure_count = row_integer(&row, 14, "failure_count")?;

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
                    last_run_at,
                    last_success_at,
                    last_failure_at,
                    last_blocked_recovery_at,
                    failure_count,
                });
            }

            Ok(projects)
        }

        pub(crate) fn record_project_scan_blocking(&self, project_id: i64) -> Result<String> {
            tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?
                .block_on(self.record_project_scan(project_id))
        }

        async fn record_project_scan(&self, project_id: i64) -> Result<String> {
            let conn = self.connect()?;
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
            let conn = self.connect()?;

            conn.execute(
                "DELETE FROM leases WHERE project_id = ?1 AND expires_at <= ?2",
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

        pub(crate) fn release_lease_blocking(&self, project_id: i64, holder: &str) -> Result<bool> {
            tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?
                .block_on(self.release_lease(project_id, holder))
        }

        async fn release_lease(&self, project_id: i64, holder: &str) -> Result<bool> {
            let conn = self.connect()?;
            let removed = conn
                .execute(
                    "DELETE FROM leases WHERE project_id = ?1 AND holder = ?2",
                    params![project_id, holder],
                )
                .await
                .with_context(|| format!("Failed to release lease for project {}", project_id))?;

            Ok(removed > 0)
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
            let conn = self.connect()?;
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

        pub(crate) fn list_active_leases_blocking(
            &self,
            now: &str,
        ) -> Result<Vec<AgentLeaseRecord>> {
            tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?
                .block_on(self.list_active_leases(now))
        }

        async fn list_active_leases(&self, now: &str) -> Result<Vec<AgentLeaseRecord>> {
            let conn = self.connect()?;
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

        pub(crate) fn record_run_outcome_blocking(
            &self,
            outcome: AgentRunOutcome<'_>,
        ) -> Result<i64> {
            tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?
                .block_on(self.record_run_outcome(outcome))
        }

        async fn record_run_outcome(&self, outcome: AgentRunOutcome<'_>) -> Result<i64> {
            let conn = self.connect()?;

            conn.execute(
                "INSERT INTO runs (
                    project_id, status, started_at, finished_at, exit_code,
                    log_dir, stdout_path, stderr_path, summary, codex_session_id,
                    task_content
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                    outcome.task_content
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
            let conn = self.connect()?;
            let mut rows = conn
                .query(
                    "SELECT r.id, r.project_id, p.name, p.path, r.status, r.started_at,
                            r.finished_at, r.exit_code, r.stdout_path, r.stderr_path, r.summary
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
            let conn = self.connect()?;
            let mut rows = conn
                .query(
                    "SELECT r.id, r.project_id, p.name, p.path, r.status, r.started_at,
                            r.finished_at, r.exit_code, r.stdout_path, r.stderr_path, r.summary
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
            }))
        }

        pub(crate) fn codex_session_for_task_blocking(
            &self,
            project_root: &Path,
            task_content: &str,
        ) -> Result<Option<String>> {
            tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?
                .block_on(self.codex_session_for_task(project_root, task_content))
        }

        async fn codex_session_for_task(
            &self,
            project_root: &Path,
            task_content: &str,
        ) -> Result<Option<String>> {
            let conn = self.connect()?;
            let project_path = project_root.display().to_string();
            let mut rows = conn
                .query(
                    "SELECT r.codex_session_id
                     FROM runs r
                     JOIN projects p ON p.id = r.project_id
                     WHERE p.path = ?1
                       AND r.task_content = ?2
                       AND r.codex_session_id IS NOT NULL
                     ORDER BY r.id DESC
                     LIMIT 1",
                    params![project_path.as_str(), task_content],
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to find a Codex session for a task in {}",
                        project_root.display()
                    )
                })?;

            let Some(row) = rows
                .next()
                .await
                .context("Failed to read task Codex session")?
            else {
                return Ok(None);
            };

            Ok(Some(row_text(&row, 0, "codex_session_id")?))
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
            let conn = self.connect()?;

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
            let conn = self.connect()?;
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
            let conn = self.connect()?;
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
            let conn = self.connect()?;

            let projects_reset = conn
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
            let runs_deleted = conn
                .execute("DELETE FROM runs", ())
                .await
                .context("Failed to delete agent run records")?;
            let leases_deleted = conn
                .execute("DELETE FROM leases", ())
                .await
                .context("Failed to delete agent leases")?;
            let daemon_checkins_deleted = conn
                .execute("DELETE FROM daemon_checkins", ())
                .await
                .context("Failed to delete agent daemon check-ins")?;

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
        pub(crate) fn table_exists_blocking(&self, table_name: &str) -> Result<bool> {
            tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?
                .block_on(self.table_exists(table_name))
        }

        #[cfg(test)]
        async fn table_exists(&self, table_name: &str) -> Result<bool> {
            let conn = self.connect()?;
            let count = query_count(
                &conn,
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [table_name],
            )
            .await?;

            Ok(count == 1)
        }

        #[cfg(test)]
        pub(crate) fn run_count_blocking(&self) -> Result<i64> {
            tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?
                .block_on(async {
                    let conn = self.connect()?;
                    query_count(&conn, "SELECT COUNT(*) FROM runs", ()).await
                })
        }

        #[cfg(test)]
        pub(crate) fn lease_count_blocking(&self) -> Result<i64> {
            tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?
                .block_on(async {
                    let conn = self.connect()?;
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
            let conn = self.connect()?;
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
            let conn = self.connect()?;
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
            let conn = self.connect()?;
            let changed = conn
                .execute(
                    "UPDATE projects SET git_mode = ?1, updated_at = ?2 WHERE id = ?3",
                    params![mode.database_value(), agent_timestamp(), project_id],
                )
                .await
                .with_context(|| format!("Failed to set project {} Git mode", project_id))?;

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
            let conn = self.connect()?;
            let path = project_root.display().to_string();
            let changed = conn
                .execute(
                    "UPDATE projects SET git_mode = ?1, updated_at = ?2 WHERE path = ?3",
                    params![mode.database_value(), agent_timestamp(), path.as_str()],
                )
                .await
                .with_context(|| format!("Failed to set project {} Git mode", path))?;

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
            let conn = self.connect()?;
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
                    let conn = self.connect()?;
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
                    let conn = self.connect()?;
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
                    while let Some(row) =
                        rows.next().await.context("Failed to read model target")?
                    {
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
                    let conn = self.connect()?;
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
                    while let Some(row) =
                        rows.next().await.context("Failed to read model target")?
                    {
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
                    let conn = self.connect()?;
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
                    let conn = self.connect()?;
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
                    let conn = self.connect()?;
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
                    let mut conn = self.connect()?;
                    let transaction = conn.transaction().await.with_context(|| {
                        format!("Failed to begin deleting provider {provider_id}")
                    })?;
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
                    transaction.commit().await.with_context(|| {
                        format!("Failed to commit deleting provider {provider_id}")
                    })?;
                    Ok(deleted > 0)
                })
        }

        pub(crate) fn upsert_model_target_blocking(&self, target: &AgentModelTarget) -> Result<()> {
            tokio::runtime::Runtime::new()
                .context("Failed to create async runtime for agent store")?
                .block_on(async {
                    let conn = self.connect()?;
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
                    let conn = self.connect()?;
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
                    let conn = self.connect()?;
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
                        .with_context(|| {
                            format!("Failed to update model {provider_id}/{model_id}")
                        })?;
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
                    let conn = self.connect()?;
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
                    let conn = self.connect()?;
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

    fn configure_agent_connection(conn: &Connection) -> Result<()> {
        conn.busy_timeout(AGENT_DB_BUSY_TIMEOUT)
            .context("Failed to configure agent database busy timeout")
    }

    async fn update_project_after_run(
        conn: &Connection,
        outcome: &AgentRunOutcome<'_>,
    ) -> Result<()> {
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

    async fn apply_migrations(conn: &Connection) -> Result<()> {
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

        for migration in AGENT_MIGRATIONS {
            if migration_applied(conn, migration.version).await? {
                continue;
            }

            for statement in migration.statements {
                conn.execute(statement, ()).await.with_context(|| {
                    format!("Failed to apply agent migration {}", migration.version)
                })?;
            }

            conn.execute(
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
        }

        Ok(())
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
}

fn get_task_root(local: bool) -> Result<std::path::PathBuf> {
    get_task_root_at(&std::env::current_dir()?, local)
}

fn get_task_root_at(start: &Path, local: bool) -> Result<PathBuf> {
    if local {
        return canonicalize_existing_path(start);
    }

    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let path_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
            canonicalize_existing_path(Path::new(&path_str))
        }
        _ => canonicalize_existing_path(start),
    }
}

fn get_tasks_dir(root: &Path) -> std::path::PathBuf {
    root.join("tasks")
}

fn project_display_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| root.display().to_string())
}

fn app_title(root: &Path) -> String {
    format!("clt | {}", project_display_name(root))
}

fn set_terminal_title(title: &str) -> Result<()> {
    stdout()
        .execute(SetTitle(title))
        .context("Failed to update terminal title")?;
    Ok(())
}

fn ensure_existing_board(root: &Path) -> Result<bool> {
    let tasks_dir = get_tasks_dir(root);
    if !tasks_dir.is_dir() || !board_has_any_status_store(&tasks_dir) {
        return Ok(false);
    }

    ensure_board_store(&tasks_dir)?;
    Ok(true)
}

#[cfg(test)]
fn ensure_task_store(root: &Path) -> Result<()> {
    ensure_board_store(&get_tasks_dir(root))
}

fn status_filename(status: &str) -> Result<&'static str> {
    match status {
        "backlog" => Ok("backlog.md"),
        "todo" => Ok("todo.md"),
        "doing" => Ok("doing.md"),
        "done" => Ok("done.md"),
        _ => anyhow::bail!("Invalid status. Use 'backlog', 'todo', 'doing', or 'done'."),
    }
}

fn normalize_status_arg(status: &str) -> Result<&'static str> {
    match status {
        "0" | "backlog" => Ok("backlog"),
        "1" | "todo" => Ok("todo"),
        "2" | "doing" => Ok("doing"),
        "3" | "done" => Ok("done"),
        _ => anyhow::bail!("Invalid status. Use 'backlog', 'todo', 'doing', or 'done'."),
    }
}

fn status_header(status: &str) -> Result<&'static str> {
    match status {
        "backlog" => Ok("# Backlog Tasks\n"),
        "todo" => Ok("# To Do Tasks\n"),
        "doing" => Ok("# Doing Tasks\n"),
        "done" => Ok("# Done Tasks\n"),
        _ => anyhow::bail!("Invalid status. Use 'backlog', 'todo', 'doing', or 'done'."),
    }
}

fn status_store_exists(board_dir: &Path, status: &str) -> bool {
    board_dir.join(status).is_dir()
        || status_filename(status)
            .map(|filename| board_dir.join(filename).is_file())
            .unwrap_or(false)
}

fn ensure_board_store(board_dir: &Path) -> Result<()> {
    fs::create_dir_all(board_dir).context("Failed to create tasks directory")?;
    let directory_mode = TASK_STATUSES
        .iter()
        .any(|status| board_dir.join(status).is_dir());

    for status in TASK_STATUSES {
        let dir_path = board_dir.join(status);
        let file_path = board_dir.join(status_filename(status)?);
        if dir_path.is_dir() || file_path.exists() {
            continue;
        }

        if directory_mode {
            fs::create_dir_all(&dir_path)
                .context(format!("Failed to create directory {:?}", dir_path))?;
        } else {
            fs::write(&file_path, status_header(status)?)
                .context(format!("Failed to create file {:?}", file_path))?;
        }
    }

    Ok(())
}

fn get_status_store(board_dir: &Path, status: &str) -> Result<StatusStore> {
    status_filename(status)?;
    ensure_board_store(board_dir)?;

    let dir_path = board_dir.join(status);
    if dir_path.is_dir() {
        return Ok(StatusStore::Directory(dir_path));
    }

    Ok(StatusStore::MarkdownFile(
        board_dir.join(status_filename(status)?),
    ))
}

fn get_archive_status_store(board_dir: &Path) -> Option<StatusStore> {
    ARCHIVE_STATUS_CANDIDATES.iter().find_map(|status| {
        let dir_path = board_dir.join(status);
        if dir_path.is_dir() {
            return Some(StatusStore::Directory(dir_path));
        }

        let file_path = board_dir.join(format!("{status}.md"));
        if file_path.is_file() {
            Some(StatusStore::MarkdownFile(file_path))
        } else {
            None
        }
    })
}

fn get_or_create_archive_status_store(board_dir: &Path) -> Result<StatusStore> {
    if let Some(store) = get_archive_status_store(board_dir) {
        return Ok(store);
    }

    let archive_dir = board_dir.join(ARCHIVE_STATUS_CANDIDATES[0]);
    fs::create_dir_all(&archive_dir)
        .with_context(|| format!("Failed to create archive directory {:?}", archive_dir))?;
    Ok(StatusStore::Directory(archive_dir))
}

// find_task_status is no longer needed for index-based referencing
// as the user must specify the source list.

fn read_task_entries(board_dir: &Path, status: &str) -> Result<Vec<TaskEntry>> {
    match get_status_store(board_dir, status)? {
        StatusStore::MarkdownFile(path) => read_markdown_entries(&path),
        StatusStore::Directory(path) => read_directory_entries(&path),
    }
}

fn read_archived_task_entries(board_dir: &Path) -> Result<Vec<TaskEntry>> {
    match get_archive_status_store(board_dir) {
        Some(StatusStore::MarkdownFile(path)) => read_markdown_entries(&path),
        Some(StatusStore::Directory(path)) => read_directory_entries(&path),
        None => Ok(Vec::new()),
    }
}

fn read_markdown_entries(path: &Path) -> Result<Vec<TaskEntry>> {
    let content = fs::read_to_string(path).context(format!("Failed to read {:?}", path))?;
    let entries = content
        .lines()
        .enumerate()
        .filter_map(|(line_index, line)| {
            let task_text = line.strip_prefix("- ")?;
            Some(task_entry_from_text(
                TaskSource::MarkdownLine { line_index },
                task_text,
                task_text,
                false,
            ))
        })
        .collect();

    Ok(entries)
}

fn read_directory_entries(path: &Path) -> Result<Vec<TaskEntry>> {
    let mut paths = directory_task_paths(path)?;
    paths.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });

    paths
        .into_iter()
        .map(|path| {
            let is_dir = path.is_dir();
            let fallback = title_from_path(&path);
            let content = if is_dir {
                read_directory_task_content(&path).unwrap_or_else(|| fallback.clone())
            } else {
                fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read task file {:?}", path))?
            };
            let summary = first_sentence(&content).unwrap_or(fallback);
            let (description, metadata) = split_description_metadata(&summary);
            let has_subtasks = is_dir && board_has_any_status_store(&path);

            Ok(TaskEntry {
                source: TaskSource::Path { path, is_dir },
                summary: description.to_string(),
                content,
                metadata: metadata.map(str::to_string),
                has_subtasks,
            })
        })
        .collect()
}

fn task_entry_from_text(
    source: TaskSource,
    text: &str,
    content: &str,
    has_subtasks: bool,
) -> TaskEntry {
    let summary = first_sentence(text).unwrap_or_else(|| text.trim().to_string());
    let (description, metadata) = split_description_metadata(&summary);

    TaskEntry {
        source,
        summary: description.to_string(),
        content: content.to_string(),
        metadata: metadata.map(str::to_string),
        has_subtasks,
    }
}

fn board_has_any_status_store(board_dir: &Path) -> bool {
    TASK_STATUSES
        .iter()
        .any(|status| status_store_exists(board_dir, status))
}

fn directory_task_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    if !path.exists() {
        return Ok(paths);
    }

    for entry in
        fs::read_dir(path).with_context(|| format!("Failed to read directory {:?}", path))?
    {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if file_name.starts_with('.') {
            continue;
        }

        let file_type = entry.file_type()?;
        if file_type.is_file() || file_type.is_dir() {
            paths.push(path);
        }
    }

    paths.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });

    Ok(paths)
}

fn read_directory_task_content(path: &Path) -> Option<String> {
    TASK_DETAIL_FILES.iter().find_map(|filename| {
        let detail_path = path.join(filename);
        fs::read_to_string(detail_path).ok()
    })
}

fn directory_task_detail_path(path: &Path) -> PathBuf {
    TASK_DETAIL_FILES
        .iter()
        .map(|filename| path.join(filename))
        .find(|path| path.exists())
        .unwrap_or_else(|| path.join(TASK_DETAIL_FILES[0]))
}

fn title_from_path(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("task");
    let name = strip_order_prefix(name);
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    let title = stem.replace(['-', '_'], " ");

    if title.trim().is_empty() {
        "task".to_string()
    } else {
        title
    }
}

fn strip_order_prefix(name: &str) -> &str {
    let bytes = name.as_bytes();
    if bytes.len() > 5 && bytes[..4].iter().all(|byte| byte.is_ascii_digit()) && bytes[4] == b'-' {
        &name[5..]
    } else {
        name
    }
}

fn first_sentence(content: &str) -> Option<String> {
    let normalized = normalize_task_text(content);
    if normalized.is_empty() {
        return None;
    }

    let end_idx = normalized
        .char_indices()
        .find(|(_, ch)| matches!(ch, '.' | '!' | '?'))
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(normalized.len());

    Some(normalized[..end_idx].trim().to_string())
}

fn normalize_task_text(content: &str) -> String {
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.trim_start_matches('#')
                .trim_start()
                .strip_prefix("- ")
                .unwrap_or_else(|| {
                    line.trim_start_matches('#')
                        .trim_start()
                        .strip_prefix("* ")
                        .unwrap_or(line)
                })
                .trim()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn split_description_metadata(value: &str) -> (&str, Option<&str>) {
    if let Some(start) = value.rfind(" (")
        && value.ends_with(')')
    {
        return (&value[..start], Some(&value[start + 2..value.len() - 1]));
    }

    (value, None)
}

fn task_display_text(entry: &TaskEntry) -> String {
    match &entry.metadata {
        Some(metadata) => format!("{} ({})", entry.summary, metadata),
        None => entry.summary.clone(),
    }
}

fn task_full_display_text(entry: &TaskEntry) -> String {
    let content = normalize_task_text(&entry.content);
    if content.is_empty() {
        task_display_text(entry)
    } else {
        content
    }
}

fn task_tui_display_text(entry: &TaskEntry, is_selected: bool) -> String {
    if is_selected {
        task_full_display_text(entry)
    } else {
        task_display_text(entry)
    }
}

fn parse_one_based_task_index(task_index_str: &str) -> Result<usize> {
    let task_index = task_index_str
        .parse::<usize>()
        .context("Invalid task index. Please provide a number.")?;
    if task_index == 0 {
        anyhow::bail!("Task index must be 1 or greater.");
    }

    Ok(task_index)
}

fn task_entry_at(board_dir: &Path, status: &str, task_index: usize) -> Result<TaskEntry> {
    let entries = read_task_entries(board_dir, status)?;
    entries.get(task_index - 1).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "Task index {} out of range. Only {} tasks found in {}.",
            task_index,
            entries.len(),
            status
        )
    })
}

fn write_lines(path: &Path, lines: &[String]) -> Result<()> {
    let updated_content = lines.join("\n");
    let final_content = if updated_content.is_empty() {
        updated_content
    } else {
        format!("{}\n", updated_content)
    };

    fs::write(path, final_content).context("Failed to update file")?;
    Ok(())
}

fn remove_task_entry(board_dir: &Path, status: &str, entry: &TaskEntry) -> Result<()> {
    match &entry.source {
        TaskSource::MarkdownLine { line_index } => {
            let StatusStore::MarkdownFile(path) = get_status_store(board_dir, status)? else {
                anyhow::bail!("Task storage changed while removing task.");
            };
            let content = fs::read_to_string(&path).context("Failed to read file")?;
            let mut lines: Vec<String> = content.lines().map(str::to_string).collect();

            if *line_index >= lines.len() {
                anyhow::bail!("Task storage changed while removing task.");
            }

            lines.remove(*line_index);
            write_lines(&path, &lines)?;
        }
        TaskSource::Path { path, is_dir } => {
            if *is_dir {
                fs::remove_dir_all(path)
                    .with_context(|| format!("Failed to remove task directory {:?}", path))?;
            } else {
                fs::remove_file(path)
                    .with_context(|| format!("Failed to remove task file {:?}", path))?;
            }

            if let Some(parent) = path.parent() {
                normalize_directory_order(parent)?;
            }
        }
    }

    Ok(())
}

fn content_with_metadata(description: &str, metadata: Option<String>) -> String {
    match metadata {
        Some(metadata) => format!("{} ({})", description, metadata),
        None => description.to_string(),
    }
}

fn insert_task_content(
    board_dir: &Path,
    status: &str,
    index: Option<usize>,
    content: &str,
) -> Result<()> {
    match get_status_store(board_dir, status)? {
        StatusStore::MarkdownFile(path) => insert_content_into_markdown(&path, index, content),
        StatusStore::Directory(path) => insert_content_into_directory(&path, index, content),
    }
}

fn insert_content_into_markdown(path: &Path, index: Option<usize>, content: &str) -> Result<()> {
    let task_line = format!("- {}", single_line_content(content));
    let file_content = fs::read_to_string(path).context("Failed to read file")?;
    let mut lines: Vec<String> = file_content.lines().map(str::to_string).collect();

    if let Some(idx) = index {
        let task_lines: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.starts_with("- "))
            .map(|(i, _)| i)
            .collect();

        if idx < task_lines.len() {
            let actual_idx = task_lines[idx];
            lines.insert(actual_idx, task_line);
        } else {
            lines.push(task_line);
        }
    } else {
        lines.push(task_line);
    }

    write_lines(path, &lines)
}

fn insert_content_into_directory(path: &Path, index: Option<usize>, content: &str) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("Failed to create directory {:?}", path))?;
    let preferred_name = format!(
        "{:04}-{}.md",
        directory_task_paths(path)?.len() + 1,
        slugify(&first_sentence(content).unwrap_or_else(|| "task".to_string()))
    );
    let task_path = unique_child_path(path, &preferred_name);
    fs::write(&task_path, format!("{}\n", content.trim_end()))
        .with_context(|| format!("Failed to write task file {:?}", task_path))?;

    if let Some(idx) = index {
        reorder_path_in_directory(path, &task_path, idx)?;
    } else {
        normalize_directory_order(path)?;
    }

    Ok(())
}

fn single_line_content(content: &str) -> String {
    normalize_task_text(content)
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }

        if slug.len() >= 48 {
            break;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

fn unique_child_path(parent: &Path, preferred_name: &str) -> PathBuf {
    let preferred = parent.join(preferred_name);
    if !preferred.exists() {
        return preferred;
    }

    let preferred_path = Path::new(preferred_name);
    let stem = preferred_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(preferred_name);
    let extension = preferred_path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{}", extension))
        .unwrap_or_default();

    for idx in 2.. {
        let candidate = parent.join(format!("{}-{}{}", stem, idx, extension));
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("unique path search is unbounded")
}

fn normalize_directory_order(path: &Path) -> Result<()> {
    let paths = directory_task_paths(path)?;
    if paths.is_empty() {
        return Ok(());
    }

    let mut temp_paths = Vec::new();
    for (idx, task_path) in paths.iter().enumerate() {
        let original_name = task_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("task");
        let temp_path = unique_child_path(path, &format!(".clt-reorder-{}-{}", idx, original_name));
        fs::rename(task_path, &temp_path).with_context(|| {
            format!(
                "Failed to prepare task {:?} for directory order update",
                task_path
            )
        })?;
        temp_paths.push((temp_path, original_name.to_string()));
    }

    for (idx, (temp_path, original_name)) in temp_paths.into_iter().enumerate() {
        let final_name = format!("{:04}-{}", idx + 1, strip_order_prefix(&original_name));
        let final_path = path.join(final_name);
        fs::rename(&temp_path, &final_path).with_context(|| {
            format!(
                "Failed to finish task {:?} directory order update",
                temp_path
            )
        })?;
    }

    Ok(())
}

fn reorder_path_in_directory(path: &Path, task_path: &Path, to_idx: usize) -> Result<()> {
    let mut paths = directory_task_paths(path)?;
    let Some(from_idx) = paths.iter().position(|path| path == task_path) else {
        anyhow::bail!("Task file disappeared while reordering.");
    };
    let task_path = paths.remove(from_idx);
    let to_idx = to_idx.min(paths.len());
    paths.insert(to_idx, task_path);

    rewrite_directory_order(path, paths)
}

fn rewrite_directory_order(path: &Path, ordered_paths: Vec<PathBuf>) -> Result<()> {
    if ordered_paths.is_empty() {
        return Ok(());
    }

    let mut temp_paths = Vec::new();
    for (idx, task_path) in ordered_paths.iter().enumerate() {
        let original_name = task_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("task");
        let temp_path = unique_child_path(path, &format!(".clt-reorder-{}-{}", idx, original_name));
        fs::rename(task_path, &temp_path).with_context(|| {
            format!(
                "Failed to prepare task {:?} for directory order update",
                task_path
            )
        })?;
        temp_paths.push((temp_path, original_name.to_string()));
    }

    for (idx, (temp_path, original_name)) in temp_paths.into_iter().enumerate() {
        let final_name = format!("{:04}-{}", idx + 1, strip_order_prefix(&original_name));
        let final_path = path.join(final_name);
        fs::rename(&temp_path, &final_path).with_context(|| {
            format!(
                "Failed to finish task {:?} directory order update",
                temp_path
            )
        })?;
    }

    Ok(())
}

fn move_path_into_directory(
    source_path: &Path,
    dest_dir: &Path,
    index: Option<usize>,
) -> Result<PathBuf> {
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("Failed to create destination directory {:?}", dest_dir))?;

    let original_name = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("task.md");
    let preferred_name = format!(
        "{:04}-{}",
        directory_task_paths(dest_dir)?.len() + 1,
        strip_order_prefix(original_name)
    );
    let dest_path = unique_child_path(dest_dir, &preferred_name);
    fs::rename(source_path, &dest_path).with_context(|| {
        format!(
            "Failed to move task {:?} into directory {:?}",
            source_path, dest_dir
        )
    })?;

    if let Some(source_parent) = source_path.parent() {
        normalize_directory_order(source_parent)?;
    }

    if let Some(idx) = index {
        reorder_path_in_directory(dest_dir, &dest_path, idx)?;
    } else {
        normalize_directory_order(dest_dir)?;
    }

    Ok(dest_path)
}

fn convert_status_to_directory(board_dir: &Path, status: &str) -> Result<PathBuf> {
    let dir_path = board_dir.join(status);
    if dir_path.is_dir() {
        return Ok(dir_path);
    }

    let file_path = board_dir.join(status_filename(status)?);
    fs::create_dir_all(&dir_path)
        .with_context(|| format!("Failed to create directory {:?}", dir_path))?;

    if file_path.exists() {
        let entries = read_markdown_entries(&file_path)?;
        for entry in entries {
            insert_content_into_directory(&dir_path, None, &entry.content)?;
        }

        let backup_name = format!("{}.bak", status_filename(status)?);
        let backup_path = unique_child_path(board_dir, &backup_name);
        fs::rename(&file_path, &backup_path).with_context(|| {
            format!(
                "Failed to preserve markdown status file as {:?}",
                backup_path
            )
        })?;
    }

    Ok(dir_path)
}

fn convert_archive_to_directory(archive_file: &Path) -> Result<PathBuf> {
    let board_dir = archive_file
        .parent()
        .context("Archive file has no parent directory")?;
    let archive_name = archive_file
        .file_stem()
        .and_then(|name| name.to_str())
        .context("Archive file has an invalid name")?;
    let archive_dir = board_dir.join(archive_name);
    fs::create_dir_all(&archive_dir)
        .with_context(|| format!("Failed to create archive directory {:?}", archive_dir))?;

    for entry in read_markdown_entries(archive_file)? {
        insert_content_into_directory(&archive_dir, None, &entry.content)?;
    }

    let backup_name = format!(
        "{}.bak",
        archive_file
            .file_name()
            .and_then(|name| name.to_str())
            .context("Archive file has an invalid name")?
    );
    let backup_path = unique_child_path(board_dir, &backup_name);
    fs::rename(archive_file, &backup_path)
        .with_context(|| format!("Failed to preserve archive file as {:?}", backup_path))?;

    Ok(archive_dir)
}

fn expand_status_for_command(board_dir: &Path, status: &'static str) -> Result<ExpansionSummary> {
    ensure_board_store(board_dir)?;

    let dir_path = board_dir.join(status);
    if dir_path.is_dir() {
        return Ok(ExpansionSummary::AlreadyDirectory {
            status,
            dir: dir_path,
        });
    }

    let file_path = board_dir.join(status_filename(status)?);
    let entries = read_markdown_entries(&file_path)?;
    let task_count = entries.len();
    fs::create_dir_all(&dir_path)
        .with_context(|| format!("Failed to create directory {:?}", dir_path))?;

    for entry in entries {
        insert_content_into_directory(&dir_path, None, &entry.content)?;
    }

    let backup_name = format!("{}.bak", status_filename(status)?);
    let backup_path = unique_child_path(board_dir, &backup_name);
    fs::rename(&file_path, &backup_path).with_context(|| {
        format!(
            "Failed to preserve markdown status file as {:?}",
            backup_path
        )
    })?;

    Ok(ExpansionSummary::Expanded {
        status,
        dir: dir_path,
        backup: backup_path,
        task_count,
    })
}

fn expand_tasks(root: &Path, filter_status: Option<String>) -> Result<()> {
    let board_dir = get_tasks_dir(root);
    let statuses: Vec<&'static str> = match filter_status {
        Some(status) => vec![normalize_status_arg(&status)?],
        None => TASK_STATUSES.to_vec(),
    };

    for status in statuses {
        match expand_status_for_command(&board_dir, status)? {
            ExpansionSummary::AlreadyDirectory { status, dir } => {
                println!("{} is already folder-backed at {:?}", status, dir);
            }
            ExpansionSummary::Expanded {
                status,
                dir,
                backup,
                task_count,
            } => {
                println!(
                    "Expanded {} to {:?} with {} task file(s). Backup: {:?}",
                    status, dir, task_count, backup
                );
            }
        }
    }

    Ok(())
}

fn delete_task(root: &Path, status: &str, task_index_str: &str) -> Result<()> {
    delete_task_in_board(&get_tasks_dir(root), status, task_index_str)
}

fn delete_task_in_board(board_dir: &Path, status: &str, task_index_str: &str) -> Result<()> {
    let task_index = parse_one_based_task_index(task_index_str)?;
    let entry = task_entry_at(board_dir, status, task_index)?;
    remove_task_entry(board_dir, status, &entry)
}

fn move_task(root: &Path, from: &str, to: &str, task_index_str: &str) -> Result<()> {
    move_task_in_board(&get_tasks_dir(root), from, to, task_index_str)
}

fn move_task_in_board(board_dir: &Path, from: &str, to: &str, task_index_str: &str) -> Result<()> {
    let task_index = parse_one_based_task_index(task_index_str)?;
    let entry = task_entry_at(board_dir, from, task_index)?;
    let dest_index = if to == "done" { Some(0) } else { None };

    match (&entry.source, get_status_store(board_dir, to)?) {
        (TaskSource::Path { path, .. }, StatusStore::Directory(dest_dir)) => {
            move_path_into_directory(path, &dest_dir, dest_index)?;
        }
        (TaskSource::Path { path, .. }, StatusStore::MarkdownFile(_)) => {
            let dest_dir = convert_status_to_directory(board_dir, to)?;
            move_path_into_directory(path, &dest_dir, dest_index)?;
        }
        (TaskSource::MarkdownLine { .. }, _) => {
            insert_task_content(board_dir, to, dest_index, &entry.content)?;
            remove_task_entry(board_dir, from, &entry)?;
        }
    }

    Ok(())
}

fn move_task_to_archive_in_board(board_dir: &Path, from: &str, task_index_str: &str) -> Result<()> {
    let task_index = parse_one_based_task_index(task_index_str)?;
    let entry = task_entry_at(board_dir, from, task_index)?;

    match (
        &entry.source,
        get_or_create_archive_status_store(board_dir)?,
    ) {
        (TaskSource::Path { path, .. }, StatusStore::Directory(archive_dir)) => {
            move_path_into_directory(path, &archive_dir, None)?;
        }
        (TaskSource::Path { path, .. }, StatusStore::MarkdownFile(archive_file)) => {
            let archive_dir = convert_archive_to_directory(&archive_file)?;
            move_path_into_directory(path, &archive_dir, None)?;
        }
        (TaskSource::MarkdownLine { .. }, StatusStore::Directory(archive_dir)) => {
            insert_content_into_directory(&archive_dir, None, &entry.content)?;
            remove_task_entry(board_dir, from, &entry)?;
        }
        (TaskSource::MarkdownLine { .. }, StatusStore::MarkdownFile(archive_file)) => {
            insert_content_into_markdown(&archive_file, None, &entry.content)?;
            remove_task_entry(board_dir, from, &entry)?;
        }
    }

    Ok(())
}

fn update_task_in_board(
    board_dir: &Path,
    status: &str,
    task_index: usize,
    new_description: &str,
) -> Result<()> {
    let entry = task_entry_at(board_dir, status, task_index)?;

    match entry.source {
        TaskSource::MarkdownLine { line_index } => {
            let StatusStore::MarkdownFile(path) = get_status_store(board_dir, status)? else {
                anyhow::bail!("Task storage changed while updating task.");
            };
            let content = fs::read_to_string(&path).context("Failed to read file")?;
            let mut lines: Vec<String> = content.lines().map(str::to_string).collect();

            if line_index >= lines.len() {
                anyhow::bail!("Task index {} out of range", task_index);
            }

            lines[line_index] = format!("- {}", new_description);
            write_lines(&path, &lines)?;
        }
        TaskSource::Path { path, is_dir } => {
            let target_path = if is_dir {
                directory_task_detail_path(&path)
            } else {
                path
            };
            fs::write(&target_path, format!("{}\n", new_description.trim_end()))
                .with_context(|| format!("Failed to write task file {:?}", target_path))?;
        }
    }

    Ok(())
}

fn reorder_task_in_board(
    board_dir: &Path,
    status: &str,
    from_idx: usize,
    to_idx: usize,
) -> Result<()> {
    match get_status_store(board_dir, status)? {
        StatusStore::MarkdownFile(path) => reorder_markdown_task(&path, from_idx, to_idx),
        StatusStore::Directory(path) => reorder_directory_task(&path, from_idx, to_idx),
    }
}

fn reorder_markdown_task(path: &Path, from_idx: usize, to_idx: usize) -> Result<()> {
    let content = fs::read_to_string(path).context("Failed to read file")?;
    let lines: Vec<String> = content.lines().map(str::to_string).collect();

    let task_indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("- "))
        .map(|(i, _)| i)
        .collect();

    if from_idx >= task_indices.len() {
        anyhow::bail!("Task index out of range");
    }

    let actual_from_idx = task_indices[from_idx];
    let task_line = lines[actual_from_idx].clone();

    let mut new_lines = lines.clone();
    new_lines.remove(actual_from_idx);

    let new_task_indices: Vec<usize> = new_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("- "))
        .map(|(i, _)| i)
        .collect();

    let insert_at_idx = if to_idx < new_task_indices.len() {
        new_task_indices[to_idx]
    } else {
        new_lines.len()
    };

    new_lines.insert(insert_at_idx, task_line);
    write_lines(path, &new_lines)
}

fn reorder_directory_task(path: &Path, from_idx: usize, to_idx: usize) -> Result<()> {
    let mut paths = directory_task_paths(path)?;
    paths.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });

    if from_idx >= paths.len() {
        anyhow::bail!("Task index out of range");
    }

    let task_path = paths.remove(from_idx);
    let to_idx = to_idx.min(paths.len());
    paths.insert(to_idx, task_path);
    rewrite_directory_order(path, paths)
}

fn list_tasks(root: &Path, filter_status: Option<String>) -> Result<()> {
    let board_dir = get_tasks_dir(root);

    if let Some(ref s) = filter_status {
        let status = match s.as_str() {
            "0" => "backlog",
            "1" => "todo",
            "2" => "doing",
            "3" => "done",
            _ => s.as_str(),
        };

        println!("\n--- {} ---", status.to_uppercase());
        for (index, entry) in read_task_entries(&board_dir, status)?.iter().enumerate() {
            println!(
                "{}. {}{}",
                index + 1,
                task_display_text(entry),
                if entry.has_subtasks {
                    " [subtasks]"
                } else {
                    ""
                }
            );
        }
    } else {
        for status in TASK_STATUSES {
            println!("\n--- {} ---", status.to_uppercase());
            for (index, entry) in read_task_entries(&board_dir, status)?.iter().enumerate() {
                println!(
                    "{}. {}{}",
                    index + 1,
                    task_display_text(entry),
                    if entry.has_subtasks {
                        " [subtasks]"
                    } else {
                        ""
                    }
                );
            }
        }
    }
    Ok(())
}

fn parse_add_task_args(args: Vec<String>) -> Result<(String, Option<String>)> {
    if args.is_empty() {
        anyhow::bail!("Task description cannot be empty.");
    }

    let mut args = args;
    let metadata = if args.len() > 1 && looks_like_metadata(args.last().unwrap()) {
        args.pop()
    } else {
        None
    };
    let description = args.join(" ");

    if description.trim().is_empty() {
        anyhow::bail!("Task description cannot be empty.");
    }

    Ok((description, metadata))
}

fn looks_like_metadata(value: &str) -> bool {
    value.contains(',')
        || (value.chars().any(|c| c.is_ascii_alphabetic())
            && value
                .chars()
                .all(|c| !c.is_ascii_lowercase() && !matches!(c, '"' | '\'')))
}

fn add_task(root: &Path, description: &str, metadata: Option<String>) -> Result<String> {
    insert_task(root, "todo", None, description, metadata)
        .map(|_| "Task added successfully.".to_string())
}

fn insert_task(
    root: &Path,
    status: &str,
    index: Option<usize>,
    description: &str,
    metadata: Option<String>,
) -> Result<()> {
    insert_task_in_board(&get_tasks_dir(root), status, index, description, metadata)
}

fn insert_task_in_board(
    board_dir: &Path,
    status: &str,
    index: Option<usize>,
    description: &str,
    metadata: Option<String>,
) -> Result<()> {
    let content = content_with_metadata(description, metadata);
    insert_task_content(board_dir, status, index, &content)
}

fn insert_task_at_selection_in_board(
    board_dir: &Path,
    status: &str,
    state: &ListState,
    description: &str,
    metadata: Option<String>,
) -> Result<()> {
    let index = selected_task_index_in_board(board_dir, status, state);
    insert_task_in_board(board_dir, status, index, description, metadata)
}

#[cfg(test)]
fn read_tasks(root: &Path, status: &str) -> Result<Vec<String>> {
    read_tasks_in_board(&get_tasks_dir(root), status)
}

fn read_tasks_in_board(board_dir: &Path, status: &str) -> Result<Vec<String>> {
    Ok(read_task_entries(board_dir, status)?
        .iter()
        .map(|entry| format!("- {}", task_display_text(entry)))
        .collect())
}

fn select_first_task_if_present_in_board(board_dir: &Path, status: &str, state: &mut ListState) {
    let has_tasks = read_tasks_in_board(board_dir, status)
        .map(|tasks| !tasks.is_empty())
        .unwrap_or(false);

    state.select(if has_tasks { Some(0) } else { None });
}

fn select_last_task_if_present_in_board(board_dir: &Path, status: &str, state: &mut ListState) {
    let last_idx = read_tasks_in_board(board_dir, status)
        .ok()
        .and_then(|tasks| tasks.len().checked_sub(1));

    state.select(last_idx);
}

#[cfg(test)]
fn selected_task_index(root: &Path, status: &str, state: &ListState) -> Option<usize> {
    selected_task_index_in_board(&get_tasks_dir(root), status, state)
}

fn selected_task_index_in_board(
    board_dir: &Path,
    status: &str,
    state: &ListState,
) -> Option<usize> {
    let idx = state.selected()?;
    let tasks = read_tasks_in_board(board_dir, status).ok()?;

    if idx < tasks.len() { Some(idx) } else { None }
}

#[cfg(test)]
fn selected_task(root: &Path, status: &str, state: &ListState) -> Option<(usize, String)> {
    selected_task_in_board(&get_tasks_dir(root), status, state)
}

#[cfg(test)]
fn selected_task_in_board(
    board_dir: &Path,
    status: &str,
    state: &ListState,
) -> Option<(usize, String)> {
    let idx = state.selected()?;
    let tasks = read_tasks_in_board(board_dir, status).ok()?;
    tasks.get(idx).cloned().map(|task| (idx, task))
}

fn selected_task_entry_in_board(
    board_dir: &Path,
    status: &str,
    state: &ListState,
) -> Option<(usize, TaskEntry)> {
    let idx = state.selected()?;
    let tasks = read_task_entries(board_dir, status).ok()?;
    tasks.get(idx).cloned().map(|task| (idx, task))
}

#[cfg(test)]
fn normalize_board_selection(root: &Path, status: &str, state: &mut ListState) {
    normalize_board_selection_in_board(&get_tasks_dir(root), status, state);
}

fn normalize_board_selection_in_board(board_dir: &Path, status: &str, state: &mut ListState) {
    let selected = state.selected();
    let task_count = read_tasks_in_board(board_dir, status)
        .map(|tasks| tasks.len())
        .unwrap_or(0);

    match (selected, task_count) {
        (Some(0), 0) => state.select(None),
        (Some(idx), count) if idx >= count => state.select(count.checked_sub(1)),
        _ => {}
    }
}

fn normalize_archive_selection_in_board(board_dir: &Path, state: &mut ListState) {
    let selected = state.selected();
    let task_count = read_archived_task_entries(board_dir)
        .map(|tasks| tasks.len())
        .unwrap_or(0);

    match (selected, task_count) {
        (Some(0), 0) => state.select(None),
        (Some(idx), count) if idx >= count => state.select(count.checked_sub(1)),
        _ => {}
    }
}

fn select_first_archive_task_if_present_in_board(board_dir: &Path, state: &mut ListState) {
    let has_tasks = read_archived_task_entries(board_dir)
        .map(|tasks| !tasks.is_empty())
        .unwrap_or(false);

    state.select(if has_tasks { Some(0) } else { None });
}

fn normalize_board_selections_in_board(
    board_dir: &Path,
    statuses: &[&str],
    states: &mut [ListState],
) {
    for (status, state) in statuses.iter().zip(states.iter_mut()) {
        normalize_board_selection_in_board(board_dir, status, state);
    }
}

fn visible_tui_board_indices(backlog_visible: bool) -> &'static [usize] {
    if backlog_visible {
        &TUI_BOARD_INDICES_WITH_BACKLOG
    } else {
        &DEFAULT_TUI_BOARD_INDICES
    }
}

fn adjacent_visible_tui_board(
    selected_board: usize,
    backlog_visible: bool,
    direction: isize,
) -> Option<usize> {
    let visible = visible_tui_board_indices(backlog_visible);
    let position = visible.iter().position(|board| *board == selected_board)?;
    let next = position as isize + direction;
    if next < 0 || next >= visible.len() as isize {
        None
    } else {
        Some(visible[next as usize])
    }
}

fn wrapped_visible_tui_board(
    selected_board: usize,
    backlog_visible: bool,
    direction: isize,
) -> usize {
    let visible = visible_tui_board_indices(backlog_visible);
    let position = visible
        .iter()
        .position(|board| *board == selected_board)
        .unwrap_or(0);
    let next = (position as isize + direction).rem_euclid(visible.len() as isize) as usize;
    visible[next]
}

fn toggle_tui_backlog_column(
    board_dir: &Path,
    board_states: &mut [ListState],
    selected_board: &mut usize,
    backlog_visible: &mut bool,
) -> String {
    *backlog_visible = !*backlog_visible;
    if *backlog_visible {
        let backlog_count = read_task_entries(board_dir, "backlog")
            .map(|entries| entries.len())
            .unwrap_or(0);
        format!("Backlog column shown ({backlog_count} tasks). Press B again to hide it.")
    } else {
        if *selected_board == BACKLOG_BOARD_INDEX {
            *selected_board = TODO_BOARD_INDEX;
            for state in board_states.iter_mut() {
                state.select(None);
            }
            select_first_task_if_present_in_board(
                board_dir,
                "todo",
                &mut board_states[TODO_BOARD_INDEX],
            );
        }
        "Backlog column hidden. Press B to show it.".to_string()
    }
}

fn move_selected_tui_task_to_backlog(
    board_dir: &Path,
    statuses: &[&str],
    board_states: &mut [ListState],
    selected_board: &mut usize,
    backlog_visible: bool,
) -> Result<String> {
    if *selected_board == BACKLOG_BOARD_INDEX {
        return Ok("Task is already in backlog.".to_string());
    }

    let Some(idx) = selected_task_index_in_board(
        board_dir,
        statuses[*selected_board],
        &board_states[*selected_board],
    ) else {
        return Ok("No task selected".to_string());
    };

    let from_board = *selected_board;
    move_task_in_board(
        board_dir,
        statuses[from_board],
        "backlog",
        &(idx + 1).to_string(),
    )?;

    if backlog_visible {
        *selected_board = BACKLOG_BOARD_INDEX;
        for state in board_states.iter_mut() {
            state.select(None);
        }
        select_last_task_if_present_in_board(
            board_dir,
            "backlog",
            &mut board_states[BACKLOG_BOARD_INDEX],
        );
    } else {
        normalize_board_selection_in_board(
            board_dir,
            statuses[from_board],
            &mut board_states[from_board],
        );
        board_states[BACKLOG_BOARD_INDEX].select(None);
    }

    Ok("Moved task to backlog".to_string())
}

fn move_selected_tui_task_to_archive(
    board_dir: &Path,
    statuses: &[&str],
    board_states: &mut [ListState],
    selected_board: usize,
) -> Result<String> {
    let Some(idx) = selected_task_index_in_board(
        board_dir,
        statuses[selected_board],
        &board_states[selected_board],
    ) else {
        return Ok("No task selected".to_string());
    };

    move_task_to_archive_in_board(board_dir, statuses[selected_board], &(idx + 1).to_string())?;
    normalize_board_selection_in_board(
        board_dir,
        statuses[selected_board],
        &mut board_states[selected_board],
    );

    Ok("Moved task to archive".to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiTaskReorderDirection {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiTaskReorganizeDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiTaskBoardMoveDirection {
    Left,
    Right,
}

fn tui_task_reorder_direction(key: &crossterm::event::KeyEvent) -> Option<TuiTaskReorderDirection> {
    match key.code {
        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(TuiTaskReorderDirection::Up)
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(TuiTaskReorderDirection::Down)
        }
        KeyCode::Char('p' | 'P') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(TuiTaskReorderDirection::Up)
        }
        KeyCode::Char('n' | 'N') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(TuiTaskReorderDirection::Down)
        }
        _ => None,
    }
}

fn tui_task_reorganize_direction(
    key: &crossterm::event::KeyEvent,
) -> Option<TuiTaskReorganizeDirection> {
    match key.code {
        KeyCode::Up => Some(TuiTaskReorganizeDirection::Up),
        KeyCode::Down => Some(TuiTaskReorganizeDirection::Down),
        KeyCode::Left => Some(TuiTaskReorganizeDirection::Left),
        KeyCode::Right => Some(TuiTaskReorganizeDirection::Right),
        _ => None,
    }
}

fn tui_starts_reorganize_mode(key: &crossterm::event::KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('r' | 'R'))
        && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}

fn reorder_selected_tui_task(
    board_dir: &Path,
    status: &str,
    state: &mut ListState,
    direction: TuiTaskReorderDirection,
) -> String {
    let Some(idx) = selected_task_index_in_board(board_dir, status, state) else {
        state.select(None);
        return "No task selected".to_string();
    };

    let target_idx = match direction {
        TuiTaskReorderDirection::Up if idx == 0 => return "Already at the top".to_string(),
        TuiTaskReorderDirection::Up => idx - 1,
        TuiTaskReorderDirection::Down => {
            let tasks = read_tasks_in_board(board_dir, status).unwrap_or_default();
            if tasks.is_empty() {
                state.select(None);
                return "No task selected".to_string();
            }
            if idx >= tasks.len() - 1 {
                return "Already at the bottom".to_string();
            }
            idx + 1
        }
    };

    let result = reorder_task_in_board(board_dir, status, idx, target_idx);
    state.select(Some(target_idx));

    match result {
        Ok(_) => {
            let direction = match direction {
                TuiTaskReorderDirection::Up => "up",
                TuiTaskReorderDirection::Down => "down",
            };
            format!("Moved task {direction} to position {}", target_idx + 1)
        }
        Err(error) => format!("Error: {error}"),
    }
}

fn move_selected_tui_task_between_boards(
    board_dir: &Path,
    statuses: &[&str],
    board_states: &mut [ListState],
    selected_board: &mut usize,
    backlog_visible: bool,
    direction: TuiTaskBoardMoveDirection,
) -> String {
    let Some(idx) = selected_task_index_in_board(
        board_dir,
        statuses[*selected_board],
        &board_states[*selected_board],
    ) else {
        board_states[*selected_board].select(None);
        return "No task selected".to_string();
    };

    let (offset, boundary_message) = match direction {
        TuiTaskBoardMoveDirection::Left => (-1, "Already at the first board"),
        TuiTaskBoardMoveDirection::Right => (1, "Already at the last board"),
    };
    let Some(to_board) = adjacent_visible_tui_board(*selected_board, backlog_visible, offset)
    else {
        return boundary_message.to_string();
    };
    let from = statuses[*selected_board];
    let to = statuses[to_board];

    match move_task_in_board(board_dir, from, to, &(idx + 1).to_string()) {
        Ok(_) => {
            *selected_board = to_board;
            for state in board_states.iter_mut() {
                state.select(None);
            }
            select_last_task_if_present_in_board(board_dir, to, &mut board_states[*selected_board]);
            format!("Moved task to {to}")
        }
        Err(error) => format!("Error: {error}"),
    }
}

fn reorganize_selected_tui_task(
    board_dir: &Path,
    statuses: &[&str],
    board_states: &mut [ListState],
    selected_board: &mut usize,
    backlog_visible: bool,
    direction: TuiTaskReorganizeDirection,
) -> String {
    match direction {
        TuiTaskReorganizeDirection::Up => reorder_selected_tui_task(
            board_dir,
            statuses[*selected_board],
            &mut board_states[*selected_board],
            TuiTaskReorderDirection::Up,
        ),
        TuiTaskReorganizeDirection::Down => reorder_selected_tui_task(
            board_dir,
            statuses[*selected_board],
            &mut board_states[*selected_board],
            TuiTaskReorderDirection::Down,
        ),
        TuiTaskReorganizeDirection::Left => move_selected_tui_task_between_boards(
            board_dir,
            statuses,
            board_states,
            selected_board,
            backlog_visible,
            TuiTaskBoardMoveDirection::Left,
        ),
        TuiTaskReorganizeDirection::Right => move_selected_tui_task_between_boards(
            board_dir,
            statuses,
            board_states,
            selected_board,
            backlog_visible,
            TuiTaskBoardMoveDirection::Right,
        ),
    }
}

fn task_display_height(
    task: &str,
    idx: usize,
    selected_idx: Option<usize>,
    col_width: usize,
) -> usize {
    let cleaned = task.replace("- ", "");
    let desc = if let Some(start) = cleaned.rfind(" (") {
        if cleaned.ends_with(')') {
            &cleaned[..start]
        } else {
            &cleaned[..]
        }
    } else {
        &cleaned[..]
    };

    if Some(idx) == selected_idx {
        wrap_text(desc, col_width.saturating_sub(5))
            .lines()
            .count()
            .max(1)
    } else {
        1
    }
}

fn keep_selected_task_visible(
    tasks: &[String],
    selected_idx: Option<usize>,
    scroll_offset: &mut usize,
    viewport_height: usize,
    col_width: usize,
) {
    if tasks.is_empty() || viewport_height == 0 {
        *scroll_offset = 0;
        return;
    }

    let Some(selected_idx) = selected_idx.filter(|idx| *idx < tasks.len()) else {
        *scroll_offset = (*scroll_offset).min(tasks.len() - 1);
        return;
    };

    if selected_idx < *scroll_offset {
        *scroll_offset = selected_idx;
    }

    while *scroll_offset < selected_idx {
        let visible_height: usize = tasks[*scroll_offset..=selected_idx]
            .iter()
            .enumerate()
            .map(|(offset, task)| {
                let idx = *scroll_offset + offset;
                task_display_height(task, idx, Some(selected_idx), col_width)
            })
            .sum();

        if visible_height <= viewport_height {
            break;
        }

        *scroll_offset += 1;
    }
}

enum Mode {
    View,
    Reorganize,
    Input,
    Edit,
    Help,
}

const PASTED_CONTENT_MARKER_START: u32 = 0xe000;
const PASTED_CONTENT_MARKER_END: u32 = 0xf8ff;

struct PastedContent {
    marker: char,
    content: String,
    line_count: usize,
}

impl PastedContent {
    fn label(&self) -> String {
        let noun = if self.line_count == 1 {
            "line"
        } else {
            "lines"
        };
        format!("[Pasted Content {} {}]", self.line_count, noun)
    }
}

#[derive(Default)]
struct TaskInput {
    input: Input,
    pasted_content: Vec<PastedContent>,
}

impl TaskInput {
    fn new(value: String) -> Self {
        Self {
            input: Input::new(value),
            pasted_content: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.input.reset();
        self.pasted_content.clear();
    }

    fn insert_paste(&mut self, content: String) {
        if content.is_empty() {
            return;
        }

        let content = content.replace("\r\n", "\n").replace('\r', "\n");
        if !content.contains('\n') {
            for ch in content.chars() {
                self.input.handle(InputRequest::InsertChar(ch));
            }
            return;
        }

        let marker_value = PASTED_CONTENT_MARKER_START + self.pasted_content.len() as u32;
        let Some(marker) = (marker_value <= PASTED_CONTENT_MARKER_END)
            .then(|| char::from_u32(marker_value))
            .flatten()
        else {
            for ch in content.chars() {
                self.input.handle(InputRequest::InsertChar(ch));
            }
            return;
        };

        let line_count = content.lines().count().max(1);
        self.input.handle(InputRequest::InsertChar(marker));
        self.pasted_content.push(PastedContent {
            marker,
            content,
            line_count,
        });
    }

    fn pasted_for_marker(&self, marker: char) -> Option<&PastedContent> {
        self.pasted_content
            .iter()
            .find(|pasted| pasted.marker == marker)
    }

    fn display_value(&self) -> String {
        let mut value = String::new();
        for ch in self.input.value().chars() {
            if let Some(pasted) = self.pasted_for_marker(ch) {
                value.push_str(&pasted.label());
            } else {
                value.push(ch);
            }
        }
        value
    }

    fn display_cursor(&self) -> usize {
        self.input
            .value()
            .chars()
            .take(self.input.cursor())
            .map(|ch| {
                self.pasted_for_marker(ch)
                    .map(|pasted| pasted.label().chars().count())
                    .unwrap_or(1)
            })
            .sum()
    }

    fn submitted_value(&self) -> String {
        let mut value = String::new();
        for ch in self.input.value().chars() {
            if let Some(pasted) = self.pasted_for_marker(ch) {
                value.push_str(&pasted.content);
            } else {
                value.push(ch);
            }
        }
        value
    }
}

fn append_styled_wrapped_text(
    lines: &mut Vec<Vec<Span<'static>>>,
    col: &mut usize,
    text: &str,
    width: usize,
    style: Style,
) {
    for ch in text.chars() {
        if ch == '\n' {
            lines.push(Vec::new());
            *col = 0;
            continue;
        }
        if width > 0 && *col >= width {
            lines.push(Vec::new());
            *col = 0;
        }
        lines
            .last_mut()
            .expect("input display always has a line")
            .push(Span::styled(ch.to_string(), style));
        *col += 1;
    }
}

fn styled_task_input_lines(label: &str, input: &TaskInput, width: usize) -> Vec<Line<'static>> {
    let normal_style = Style::default().fg(Color::White);
    let pasted_style = Style::default().fg(Color::Blue);
    let mut lines = vec![Vec::new()];
    let mut col = 0;

    append_styled_wrapped_text(&mut lines, &mut col, label, width, normal_style);
    for ch in input.input.value().chars() {
        if let Some(pasted) = input.pasted_for_marker(ch) {
            append_styled_wrapped_text(&mut lines, &mut col, &pasted.label(), width, pasted_style);
        } else {
            append_styled_wrapped_text(&mut lines, &mut col, &ch.to_string(), width, normal_style);
        }
    }

    lines.into_iter().map(Line::from).collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiPane {
    Tasks,
    AgentProjects,
    Models,
}

fn tui_pane_after_tab(current: TuiPane, active_board: bool) -> TuiPane {
    match current {
        TuiPane::Tasks => TuiPane::AgentProjects,
        TuiPane::AgentProjects if active_board => TuiPane::Tasks,
        TuiPane::AgentProjects => TuiPane::AgentProjects,
        TuiPane::Models => TuiPane::AgentProjects,
    }
}

fn tui_models_return_pane(opened_from: TuiPane) -> TuiPane {
    match opened_from {
        TuiPane::Tasks => TuiPane::Tasks,
        TuiPane::AgentProjects | TuiPane::Models => TuiPane::AgentProjects,
    }
}

struct TuiStartState {
    active_board: bool,
    current_pane: TuiPane,
    feedback_buffer: String,
}

fn tui_start_state(active_board: bool) -> TuiStartState {
    if active_board {
        TuiStartState {
            active_board,
            current_pane: TuiPane::Tasks,
            feedback_buffer: String::from(
                "Kanban View! Tab toggles between the task board and agent projects. Uppercase M opens Models from either pane; lowercase m cycles the selected Agent Project target. Enter opens a selected project, Space toggles it ON/OFF, g cycles Git off/commit/push, f/t change its Codex fast/thinking settings, c resumes a selected Done or blocked task in interactive Codex, l shows agent output, Backspace returns to parent, Space creates a task on the board, a archives a task, A opens archive view, b moves a task to backlog, B toggles the backlog column, tap r then an Arrow to reorganize once, Shift+Arrows reorder or move tasks, Ctrl-P/N reorder up/down, 'd' deletes, 'q' quits.",
            ),
        }
    } else {
        TuiStartState {
            active_board,
            current_pane: TuiPane::AgentProjects,
            feedback_buffer: String::from(TUI_NO_ACTIVE_BOARD_MESSAGE),
        }
    }
}

struct TuiAgentProject {
    project: agent_store::AgentProject,
    scan: AgentProjectScan,
    runtime_state: TuiAgentRuntimeState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiAgentRuntimeState {
    Idle,
    Running,
    Stale,
}

impl TuiAgentRuntimeState {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Running => "RUNNING",
            Self::Stale => "STALE",
        }
    }

    fn is_running(self) -> bool {
        self == Self::Running
    }
}

struct TuiAgentPanelSnapshot {
    projects: Vec<TuiAgentProject>,
    daemon_status: String,
}

struct TuiCurrentProjectRegistration {
    path: PathBuf,
    name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TuiAgentProjectRemoval {
    path: PathBuf,
    name: String,
}

enum TuiAgentPanelRow<'a> {
    RegisterCurrentProject(&'a TuiCurrentProjectRegistration),
    Project(&'a TuiAgentProject),
}

struct TuiAgentPanel {
    projects: Vec<TuiAgentProject>,
    current_project_registration: Option<TuiCurrentProjectRegistration>,
    daemon_status: String,
    state: ListState,
    scroll_offset: usize,
    last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiModelsFocus {
    Providers,
    Models,
}

struct TuiModelsPanel {
    providers: Vec<agent_store::AgentModelProvider>,
    models: Vec<agent_store::AgentModelTarget>,
    defaults: agent_store::AgentModelDefaults,
    codex_default: String,
    codex_default_provider: Option<String>,
    codex_default_model: Option<String>,
    focus: TuiModelsFocus,
    provider_state: ListState,
    model_state: ListState,
    last_error: Option<String>,
}

enum TuiModelInputKind {
    AddModel {
        provider_id: String,
    },
    CustomProvider {
        step: usize,
        provider_id: String,
        name: String,
        base_url: String,
    },
}

struct TuiModelInput {
    kind: TuiModelInputKind,
    input: Input,
}

impl TuiModelInput {
    fn add_model(provider_id: String) -> Self {
        Self {
            kind: TuiModelInputKind::AddModel { provider_id },
            input: Input::default(),
        }
    }

    fn custom_provider() -> Self {
        Self {
            kind: TuiModelInputKind::CustomProvider {
                step: 0,
                provider_id: String::new(),
                name: String::new(),
                base_url: String::new(),
            },
            input: Input::default(),
        }
    }

    fn label(&self) -> &'static str {
        match &self.kind {
            TuiModelInputKind::AddModel { .. } => " Model ID: ",
            TuiModelInputKind::CustomProvider { step, .. } => match step {
                0 => " Endpoint Name: ",
                1 => " API Base URL (usually .../v1): ",
                _ => " API Key Env Var (optional): ",
            },
        }
    }

    fn guidance(&self) -> &'static str {
        match &self.kind {
            TuiModelInputKind::AddModel { .. } => "Enter the exact model ID used by the endpoint",
            TuiModelInputKind::CustomProvider { step, .. } => match step {
                0 => "Enter a friendly name, for example My Local Server",
                1 => {
                    "Enter the API root, for example http://127.0.0.1:9090/v1; do not include /chat, /models, or /responses"
                }
                _ => {
                    "Enter an API-key environment variable name, or press Enter if none is required"
                }
            },
        }
    }

    fn insert_paste(&mut self, content: &str) {
        for ch in content.chars().filter(|ch| *ch != '\r' && *ch != '\n') {
            self.input.handle(InputRequest::InsertChar(ch));
        }
    }
}

struct TuiAgentLogView {
    project_name: String,
    path: Option<PathBuf>,
    content: String,
    is_live: bool,
}

impl TuiAgentLogView {
    fn new(project_name: String, path: PathBuf, is_live: bool) -> Result<Self> {
        let mut view = Self {
            project_name,
            path: Some(path),
            content: String::new(),
            is_live,
        };
        view.refresh()?;
        Ok(view)
    }

    fn message(project_name: String, content: String) -> Self {
        Self {
            project_name,
            path: None,
            content,
            is_live: false,
        }
    }

    fn refresh(&mut self) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let bytes =
            fs::read(path).with_context(|| format!("Failed to read agent output log {path:?}"))?;
        self.content = if bytes.is_empty() {
            "Waiting for agent output...".to_string()
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        Ok(())
    }
}

impl TuiAgentPanel {
    fn new(active_root: &Path) -> Self {
        let mut panel = Self {
            projects: Vec::new(),
            current_project_registration: None,
            daemon_status: "unknown".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.refresh(active_root);
        panel
    }

    fn refresh(&mut self, active_root: &Path) {
        let selected_row = self.selected_row_identity();
        self.apply_refresh_result(
            active_root,
            selected_row,
            load_tui_agent_panel_snapshot(active_root),
        );
    }

    fn apply_refresh_result(
        &mut self,
        active_root: &Path,
        selected_row: Option<TuiAgentPanelRowIdentity>,
        result: Result<TuiAgentPanelSnapshot>,
    ) {
        match result {
            Ok(snapshot) => {
                self.projects = snapshot.projects;
                self.daemon_status = snapshot.daemon_status;
                self.current_project_registration =
                    current_project_registration(active_root, &self.projects);
                self.last_error = None;
                self.restore_or_normalize_selection(selected_row);
            }
            Err(err) => {
                self.last_error = Some(format!("Agent registry unavailable: {err}"));
            }
        }
    }

    fn row_count(&self) -> usize {
        self.projects.len() + usize::from(self.current_project_registration.is_some())
    }

    fn project_start_index(&self) -> usize {
        usize::from(self.current_project_registration.is_some())
    }

    fn selected_row_identity(&self) -> Option<TuiAgentPanelRowIdentity> {
        match self.selected_row()? {
            TuiAgentPanelRow::RegisterCurrentProject(registration) => Some(
                TuiAgentPanelRowIdentity::RegisterCurrentProject(registration.path.clone()),
            ),
            TuiAgentPanelRow::Project(project) => {
                Some(TuiAgentPanelRowIdentity::Project(project.project.id))
            }
        }
    }

    fn selected_row(&self) -> Option<TuiAgentPanelRow<'_>> {
        let idx = self.state.selected()?;
        if let Some(registration) = self.current_project_registration.as_ref()
            && idx == 0
        {
            return Some(TuiAgentPanelRow::RegisterCurrentProject(registration));
        }

        self.projects
            .get(idx.checked_sub(self.project_start_index())?)
            .map(TuiAgentPanelRow::Project)
    }

    fn selected_project(&self) -> Option<&TuiAgentProject> {
        match self.selected_row()? {
            TuiAgentPanelRow::Project(project) => Some(project),
            TuiAgentPanelRow::RegisterCurrentProject(_) => None,
        }
    }

    fn selected_current_project_registration(&self) -> Option<&TuiCurrentProjectRegistration> {
        match self.selected_row()? {
            TuiAgentPanelRow::RegisterCurrentProject(registration) => Some(registration),
            TuiAgentPanelRow::Project(_) => None,
        }
    }

    fn restore_or_normalize_selection(&mut self, selected_row: Option<TuiAgentPanelRowIdentity>) {
        let row_count = self.row_count();
        if row_count == 0 {
            self.state.select(None);
            self.scroll_offset = 0;
            return;
        }

        if let Some(selected_row) = selected_row {
            match selected_row {
                TuiAgentPanelRowIdentity::RegisterCurrentProject(path) => {
                    if self
                        .current_project_registration
                        .as_ref()
                        .is_some_and(|registration| registration.path == path)
                    {
                        self.state.select(Some(0));
                        return;
                    }

                    if let Some(project_idx) = self
                        .projects
                        .iter()
                        .position(|project| project.project.path == path)
                    {
                        self.state
                            .select(Some(self.project_start_index() + project_idx));
                        return;
                    }
                }
                TuiAgentPanelRowIdentity::Project(project_id) => {
                    if let Some(project_idx) = self
                        .projects
                        .iter()
                        .position(|project| project.project.id == project_id)
                    {
                        self.state
                            .select(Some(self.project_start_index() + project_idx));
                        return;
                    }
                }
            }
        }

        let idx = self
            .state
            .selected()
            .filter(|idx| *idx < row_count)
            .unwrap_or(0);
        self.state.select(Some(idx));
    }

    fn select_nearest_row(&mut self, preferred_idx: usize) {
        let row_count = self.row_count();
        if row_count == 0 {
            self.state.select(None);
            self.scroll_offset = 0;
        } else {
            self.state.select(Some(preferred_idx.min(row_count - 1)));
            self.scroll_offset = self.scroll_offset.min(row_count - 1);
        }
    }

    fn select_project_for_path(&mut self, path: &Path) -> bool {
        if self
            .current_project_registration
            .as_ref()
            .is_some_and(|registration| registration.path == path)
        {
            self.state.select(Some(0));
            return true;
        }

        if let Some(project_idx) = self
            .projects
            .iter()
            .position(|project| project.project.path == path)
        {
            self.state
                .select(Some(self.project_start_index() + project_idx));
            true
        } else {
            false
        }
    }

    fn select_previous(&mut self) {
        let row_count = self.row_count();
        if row_count == 0 {
            self.state.select(None);
            self.scroll_offset = 0;
            return;
        }

        let idx = self.state.selected().unwrap_or(0);
        if idx > 0 {
            self.state.select(Some(idx - 1));
        } else {
            self.state.select(Some(row_count - 1));
        }
    }

    fn select_next(&mut self) {
        let row_count = self.row_count();
        if row_count == 0 {
            self.state.select(None);
            self.scroll_offset = 0;
            return;
        }

        let idx = self.state.selected().unwrap_or(0);
        if idx + 1 < row_count {
            self.state.select(Some(idx + 1));
        } else {
            self.state.select(Some(0));
        }
    }

    fn keep_selection_visible(&mut self, viewport_height: usize) {
        let row_count = self.row_count();
        if row_count == 0 || viewport_height == 0 {
            self.scroll_offset = 0;
            return;
        }

        let Some(selected_idx) = self.state.selected().filter(|idx| *idx < row_count) else {
            self.scroll_offset = self.scroll_offset.min(row_count - 1);
            return;
        };

        if selected_idx < self.scroll_offset {
            self.scroll_offset = selected_idx;
        }

        let last_visible = self
            .scroll_offset
            .saturating_add(viewport_height.saturating_sub(1));
        if selected_idx > last_visible {
            self.scroll_offset = selected_idx.saturating_sub(viewport_height.saturating_sub(1));
        }
    }
}

impl TuiModelsPanel {
    fn new() -> Self {
        let mut panel = Self {
            providers: Vec::new(),
            models: Vec::new(),
            defaults: agent_store::AgentModelDefaults::default(),
            codex_default: "not explicitly set".to_string(),
            codex_default_provider: None,
            codex_default_model: None,
            focus: TuiModelsFocus::Providers,
            provider_state: ListState::default(),
            model_state: ListState::default(),
            last_error: None,
        };
        panel.refresh();
        panel
    }

    fn selected_provider(&self) -> Option<&agent_store::AgentModelProvider> {
        self.provider_state
            .selected()
            .and_then(|index| self.providers.get(index))
    }

    fn selected_model(&self) -> Option<&agent_store::AgentModelTarget> {
        self.model_state
            .selected()
            .and_then(|index| self.models.get(index))
    }

    fn refresh(&mut self) {
        let selected_provider = self.selected_provider().map(|provider| provider.id.clone());
        let selected_model = self.selected_model().map(|model| model.model_id.clone());
        let result = (|| -> Result<_> {
            let store = open_agent_store()?;
            let providers = store.list_model_providers_blocking()?;
            let defaults = store.model_defaults_blocking()?;
            Ok((store, providers, defaults))
        })();
        let (store, providers, defaults) = match result {
            Ok(value) => value,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return;
            }
        };

        self.providers = providers;
        self.defaults = defaults;
        match codex_config_path().and_then(|path| read_codex_default_config_at(&path)) {
            Ok((provider, model)) => {
                self.codex_default = match (&provider, &model) {
                    (Some(provider), Some(model)) => format!("{provider}/{model}"),
                    (_, Some(model)) => model.clone(),
                    _ => "not explicitly set".to_string(),
                };
                self.codex_default_provider = provider;
                self.codex_default_model = model;
            }
            Err(error) => {
                self.codex_default = format!("config error: {error}");
                self.codex_default_provider = None;
                self.codex_default_model = None;
            }
        }
        let provider_index = selected_provider
            .as_ref()
            .and_then(|id| {
                self.providers
                    .iter()
                    .position(|provider| &provider.id == id)
            })
            .unwrap_or(0);
        if self.providers.is_empty() {
            self.provider_state.select(None);
            self.models.clear();
            self.model_state.select(None);
        } else {
            self.provider_state.select(Some(provider_index));
            let provider_id = self.providers[provider_index].id.clone();
            match store
                .list_model_targets_blocking(Some(&provider_id))
                .and_then(|mut models| {
                    include_codex_default_model_target(
                        &store,
                        &provider_id,
                        self.codex_default_provider.as_deref(),
                        self.codex_default_model.as_deref(),
                        &mut models,
                    )?;
                    Ok(models)
                }) {
                Ok(models) => {
                    self.models = models;
                    let model_index = selected_model
                        .as_ref()
                        .and_then(|id| self.models.iter().position(|model| &model.model_id == id))
                        .unwrap_or(0);
                    self.model_state
                        .select((!self.models.is_empty()).then_some(model_index));
                    self.last_error = None;
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
    }

    fn refresh_models(&mut self) {
        let selected_model = self.selected_model().map(|model| model.model_id.clone());
        let Some(provider_id) = self.selected_provider().map(|provider| provider.id.clone()) else {
            self.models.clear();
            self.model_state.select(None);
            return;
        };
        match open_agent_store().and_then(|store| {
            self.defaults = store.model_defaults_blocking()?;
            let mut models = store.list_model_targets_blocking(Some(&provider_id))?;
            include_codex_default_model_target(
                &store,
                &provider_id,
                self.codex_default_provider.as_deref(),
                self.codex_default_model.as_deref(),
                &mut models,
            )?;
            Ok(models)
        }) {
            Ok(models) => {
                self.models = models;
                let index = selected_model
                    .as_ref()
                    .and_then(|id| self.models.iter().position(|model| &model.model_id == id))
                    .unwrap_or(0);
                self.model_state
                    .select((!self.models.is_empty()).then_some(index));
                self.last_error = None;
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn select_previous(&mut self) {
        match self.focus {
            TuiModelsFocus::Providers => {
                let len = self.providers.len();
                if len > 0 {
                    let index = self.provider_state.selected().unwrap_or(0);
                    self.provider_state
                        .select(Some(if index == 0 { len - 1 } else { index - 1 }));
                    self.refresh_models();
                }
            }
            TuiModelsFocus::Models => {
                let len = self.models.len();
                if len > 0 {
                    let index = self.model_state.selected().unwrap_or(0);
                    self.model_state
                        .select(Some(if index == 0 { len - 1 } else { index - 1 }));
                }
            }
        }
    }

    fn select_next(&mut self) {
        match self.focus {
            TuiModelsFocus::Providers => {
                let len = self.providers.len();
                if len > 0 {
                    let index = self.provider_state.selected().unwrap_or(0);
                    self.provider_state.select(Some((index + 1) % len));
                    self.refresh_models();
                }
            }
            TuiModelsFocus::Models => {
                let len = self.models.len();
                if len > 0 {
                    let index = self.model_state.selected().unwrap_or(0);
                    self.model_state.select(Some((index + 1) % len));
                }
            }
        }
    }
}

fn custom_provider_id(name: &str, providers: &[agent_store::AgentModelProvider]) -> String {
    let mut stem = String::new();
    let mut pending_separator = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_separator && !stem.is_empty() {
                stem.push('-');
            }
            stem.push(ch.to_ascii_lowercase());
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }
    if stem.is_empty() {
        stem = "local".to_string();
    }

    let mut candidate = stem.clone();
    let mut suffix = 2;
    while providers.iter().any(|provider| provider.id == candidate) {
        candidate = format!("{stem}-{suffix}");
        suffix += 1;
    }
    candidate
}

fn openai_models_url(base_url: &str) -> String {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/models") {
        base_url.to_string()
    } else {
        format!("{base_url}/models")
    }
}

fn normalize_openai_api_base_url(input: &str) -> Result<String> {
    let base_url = input.trim().trim_end_matches('/');
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        anyhow::bail!("API base URL must start with http:// or https://");
    }
    if base_url.contains(['?', '#']) {
        anyhow::bail!("API base URL cannot contain a query string or fragment");
    }

    let lowercase = base_url.to_ascii_lowercase();
    let operation_paths = ["/chat", "/chat/completions", "/models", "/responses"];
    if operation_paths
        .iter()
        .any(|operation| lowercase.ends_with(operation))
    {
        anyhow::bail!(
            "Enter the API root (for example http://127.0.0.1:9090/v1), not a /chat, /chat/completions, /models, or /responses URL"
        );
    }

    Ok(base_url.to_string())
}

fn parse_openai_model_ids(value: &serde_json::Value) -> Result<Vec<String>> {
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("the response does not contain an OpenAI-style data list")
        })?;
    let mut model_ids = data
        .iter()
        .filter_map(|model| model.get("id").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|model_id| !model_id.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    model_ids.sort_by_key(|model_id| model_id.to_ascii_lowercase());
    model_ids.dedup();
    if model_ids.is_empty() {
        anyhow::bail!("the endpoint returned no model IDs");
    }
    Ok(model_ids)
}

fn discover_openai_model_ids(base_url: &str, api_key: Option<&str>) -> Result<Vec<String>> {
    let url = openai_models_url(base_url);
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(
            TUI_MODEL_DISCOVERY_TIMEOUT_SECONDS,
        )))
        .build();
    let agent: ureq::Agent = config.into();
    let mut request = agent.get(&url).header("Accept", "application/json");
    if let Some(api_key) = api_key {
        request = request.header("Authorization", format!("Bearer {api_key}"));
    }
    let mut response = request
        .call()
        .with_context(|| format!("could not query {url}"))?;
    let value = response
        .body_mut()
        .read_json::<serde_json::Value>()
        .with_context(|| format!("{url} did not return valid JSON"))?;
    parse_openai_model_ids(&value)
}

fn save_discovered_model_ids(
    store: &agent_store::TursoAgentStore,
    provider_id: &str,
    model_ids: &[String],
) -> Result<usize> {
    let existing = store.list_model_targets_blocking(Some(provider_id))?;
    let mut added = 0;
    for model_id in model_ids {
        if existing.iter().any(|model| model.model_id == *model_id) {
            continue;
        }
        store.upsert_model_target_blocking(&agent_store::AgentModelTarget {
            provider_id: provider_id.to_string(),
            model_id: model_id.clone(),
            label: model_id.clone(),
            enabled: false,
            favorite: false,
            reasoning_effort: None,
        })?;
        added += 1;
    }
    Ok(added)
}

fn discover_tui_provider_models(panel: &mut TuiModelsPanel) -> Result<String> {
    let provider = panel
        .selected_provider()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Select a provider first"))?;
    let base_url = provider
        .base_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("{} uses Codex's built-in model catalog", provider.name))?;
    let api_key = match provider.env_key.as_deref() {
        Some(env_key) => Some(
            std::env::var(env_key)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{env_key} is not set; export it, restart CLT, then press r again"
                    )
                })?,
        ),
        None => None,
    };
    let model_ids = discover_openai_model_ids(base_url, api_key.as_deref())?;
    let store = open_agent_store()?;
    let added = save_discovered_model_ids(&store, &provider.id, &model_ids)?;
    panel.refresh_models();
    panel.focus = TuiModelsFocus::Models;
    Ok(format!(
        "Found {} models for {}; {} new models start OFF. Choose with Up/Down and Space",
        model_ids.len(),
        provider.name,
        added
    ))
}

fn add_tui_model_provider_preset(panel: &mut TuiModelsPanel, index: usize) -> Result<String> {
    let preset = AGENT_PROVIDER_PRESETS
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("Unknown provider preset"))?;
    if let Some(base_url) = preset.base_url {
        upsert_codex_provider_config_at(
            &codex_config_path()?,
            preset.id,
            preset.name,
            base_url,
            preset.env_key,
        )?;
    }
    let store = open_agent_store()?;
    store.upsert_model_provider_blocking(&agent_store::AgentModelProvider {
        id: preset.id.to_string(),
        name: preset.name.to_string(),
        base_url: preset.base_url.map(str::to_string),
        env_key: preset.env_key.map(str::to_string),
        built_in: preset.built_in,
        enabled: true,
    })?;
    if preset.id == "openrouter" {
        store.upsert_model_target_blocking(&agent_store::AgentModelTarget {
            provider_id: preset.id.to_string(),
            model_id: "openai/gpt-5.6".to_string(),
            label: "OpenAI GPT-5.6".to_string(),
            enabled: true,
            favorite: false,
            reasoning_effort: None,
        })?;
    }
    panel.refresh();
    if let Some(index) = panel
        .providers
        .iter()
        .position(|provider| provider.id == preset.id)
    {
        panel.provider_state.select(Some(index));
        panel.refresh_models();
    }
    let added = format!("Added/enabled {} provider preset", preset.name);
    if matches!(preset.id, "ollama" | "lmstudio") {
        return Ok(match discover_tui_provider_models(panel) {
            Ok(discovered) => format!("{added}. {discovered}"),
            Err(error) => format!(
                "{added}, but model discovery failed: {error}. Start the server, then press r to retry"
            ),
        });
    }
    Ok(added)
}

fn remove_tui_model_provider(panel: &mut TuiModelsPanel) -> Result<String> {
    if panel.focus != TuiModelsFocus::Providers {
        anyhow::bail!("Press Left to select a provider before removing it");
    }
    let provider = panel
        .selected_provider()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No provider selected"))?;
    if provider.built_in {
        anyhow::bail!(
            "{} is built in and cannot be removed; press Space to disable it",
            provider.name
        );
    }

    let store = open_agent_store()?;
    if !store.delete_model_provider_blocking(&provider.id)? {
        anyhow::bail!("Provider {} no longer exists", provider.name);
    }
    let config_cleanup =
        codex_config_path().and_then(|path| remove_codex_provider_config_at(&path, &provider.id));
    panel.refresh();

    Ok(match config_cleanup {
        Ok(_) => format!(
            "Removed provider {} and its models; affected project/default selections now follow CLT defaults",
            provider.name
        ),
        Err(error) => format!(
            "Removed provider {} and its models, but Codex config cleanup failed: {error}",
            provider.name
        ),
    })
}

fn toggle_tui_models_enabled(panel: &mut TuiModelsPanel) -> Result<String> {
    let store = open_agent_store()?;
    let message = match panel.focus {
        TuiModelsFocus::Providers => {
            let provider = panel
                .selected_provider()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("No provider selected"))?;
            store.set_model_provider_enabled_blocking(&provider.id, !provider.enabled)?;
            format!(
                "{} provider {}",
                if provider.enabled {
                    "Disabled"
                } else {
                    "Enabled"
                },
                provider.name
            )
        }
        TuiModelsFocus::Models => {
            let model = panel
                .selected_model()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("No model selected"))?;
            store.set_model_target_flags_blocking(
                &model.provider_id,
                &model.model_id,
                !model.enabled,
                model.favorite,
            )?;
            format!(
                "{} model {}/{}",
                if model.enabled {
                    "Hidden"
                } else {
                    "Made available"
                },
                model.provider_id,
                model.model_id
            )
        }
    };
    panel.refresh();
    Ok(message)
}

fn toggle_tui_model_favorite(panel: &mut TuiModelsPanel) -> Result<String> {
    let model = panel
        .selected_model()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Select a model first"))?;
    open_agent_store()?.set_model_target_flags_blocking(
        &model.provider_id,
        &model.model_id,
        model.enabled,
        !model.favorite,
    )?;
    panel.refresh_models();
    Ok(format!(
        "{} favorite: {}/{}",
        if model.favorite { "Removed" } else { "Added" },
        model.provider_id,
        model.model_id
    ))
}

fn cycle_tui_model_reasoning(panel: &mut TuiModelsPanel) -> Result<String> {
    let model = panel
        .selected_model()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Select a model first"))?;
    let reasoning = next_agent_codex_setting(
        model.reasoning_effort.as_deref(),
        &AGENT_CODEX_REASONING_EFFORTS,
    );
    let label = reasoning.as_deref().unwrap_or("system").to_string();
    let changed = open_agent_store()?.set_model_target_reasoning_blocking(
        &model.provider_id,
        &model.model_id,
        reasoning.as_deref(),
    )?;
    if !changed {
        anyhow::bail!(
            "Model {}/{} no longer exists",
            model.provider_id,
            model.model_id
        );
    }
    let updated_codex_default = if tui_model_matches_codex_default(
        panel.codex_default_provider.as_deref(),
        panel.codex_default_model.as_deref(),
        &model,
    ) {
        set_codex_model_reasoning_if_default_at(
            &codex_config_path()?,
            &model.provider_id,
            &model.model_id,
            reasoning.as_deref(),
        )?
    } else {
        false
    };
    panel.refresh();
    let mut message = format!(
        "Default reasoning for {}/{}: {}",
        model.provider_id, model.model_id, label
    );
    if updated_codex_default {
        message.push_str("; updated the Codex top-level default");
    }
    Ok(message)
}

fn set_tui_model_default(panel: &mut TuiModelsPanel) -> Result<String> {
    let model = panel
        .selected_model()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Select a model first"))?;
    let store = open_agent_store()?;
    store.set_model_provider_enabled_blocking(&model.provider_id, true)?;
    store.set_model_target_flags_blocking(
        &model.provider_id,
        &model.model_id,
        true,
        model.favorite,
    )?;
    store.set_model_default_blocking(&model.provider_id, &model.model_id)?;
    panel.refresh_models();
    Ok(format!(
        "New CLT runs default to {}/{}; existing runs are unchanged",
        model.provider_id, model.model_id
    ))
}

fn set_tui_codex_default(panel: &mut TuiModelsPanel) -> Result<String> {
    let model = panel
        .selected_model()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Select a model first"))?;
    let path = codex_config_path()?;
    set_codex_default_config_at(
        &path,
        &model.provider_id,
        &model.model_id,
        model.reasoning_effort.as_deref(),
    )?;
    let provider_id = model.provider_id.clone();
    let model_id = model.model_id.clone();
    let reasoning = model.reasoning_effort.as_deref().unwrap_or("system");
    panel.refresh();
    Ok(format!(
        "Updated Codex top-level default in {} to {}/{} with {} reasoning",
        path.display(),
        provider_id,
        model_id,
        reasoning
    ))
}

fn submit_tui_model_input(
    model_input: &mut TuiModelInput,
    panel: &mut TuiModelsPanel,
) -> Result<Option<String>> {
    let entered = model_input.input.value().trim().to_string();
    match &mut model_input.kind {
        TuiModelInputKind::AddModel { provider_id } => {
            if entered.is_empty() {
                anyhow::bail!("Model ID cannot be empty");
            }
            open_agent_store()?.upsert_model_target_blocking(&agent_store::AgentModelTarget {
                provider_id: provider_id.clone(),
                model_id: entered.clone(),
                label: entered.clone(),
                enabled: true,
                favorite: false,
                reasoning_effort: None,
            })?;
            panel.refresh_models();
            Ok(Some(format!("Added model {provider_id}/{entered}")))
        }
        TuiModelInputKind::CustomProvider {
            step,
            provider_id,
            name,
            base_url,
        } => {
            match *step {
                0 => {
                    if entered.is_empty() {
                        anyhow::bail!("Endpoint name cannot be empty");
                    }
                    *provider_id = custom_provider_id(&entered, &panel.providers);
                    *name = entered;
                }
                1 => {
                    *base_url = normalize_openai_api_base_url(&entered)?;
                }
                _ => {
                    if !entered.is_empty() && !valid_environment_variable_name(&entered) {
                        anyhow::bail!(
                            "Environment variable must use the standard NAME_WITH_UNDERSCORES form"
                        );
                    }
                    let env_key = (!entered.is_empty()).then_some(entered.as_str());
                    upsert_codex_provider_config_at(
                        &codex_config_path()?,
                        provider_id,
                        name,
                        base_url,
                        env_key,
                    )?;
                    open_agent_store()?.upsert_model_provider_blocking(
                        &agent_store::AgentModelProvider {
                            id: provider_id.clone(),
                            name: name.clone(),
                            base_url: Some(base_url.clone()),
                            env_key: env_key.map(str::to_string),
                            built_in: false,
                            enabled: true,
                        },
                    )?;
                    panel.refresh();
                    if let Some(index) = panel
                        .providers
                        .iter()
                        .position(|provider| provider.id == *provider_id)
                    {
                        panel.provider_state.select(Some(index));
                        panel.refresh_models();
                    }
                    let added = format!("Added local endpoint {name} ({provider_id})");
                    let message = match discover_tui_provider_models(panel) {
                        Ok(discovered) => format!("{added}. {discovered}"),
                        Err(error) => format!(
                            "{added}, but model discovery failed: {error}. Press r to retry or a to add a model ID"
                        ),
                    };
                    return Ok(Some(message));
                }
            }
            *step += 1;
            model_input.input = Input::default();
            Ok(None)
        }
    }
}

#[derive(Clone)]
enum TuiAgentPanelRowIdentity {
    RegisterCurrentProject(PathBuf),
    Project(i64),
}

fn load_tui_agent_panel_snapshot(_active_root: &Path) -> Result<TuiAgentPanelSnapshot> {
    let state_dir = agent_state_dir()?;
    let service_status = agent_service_status(&state_dir);
    let store = open_agent_store_at(&state_dir)?;
    let mut checkins = store.list_daemon_checkins_blocking()?;
    let now = agent_timestamp_seconds();
    let service_restarted = agent_service_needs_restart(&service_status, &checkins, now);
    if service_restarted {
        restart_running_agent_service().context("Failed to restart stale agent service")?;
        for checkin in checkins
            .iter()
            .filter(|checkin| checkin.mode == "service" && !daemon_checkin_is_fresh(checkin, now))
        {
            store.clear_daemon_checkin_blocking(&checkin.holder)?;
        }
        checkins
            .retain(|checkin| checkin.mode != "service" || daemon_checkin_is_fresh(checkin, now));
    }
    let daemon_status = if service_restarted {
        "service restarting".to_string()
    } else {
        format_agent_daemon_runtime_status(&service_status, &checkins, now)
    };
    let projects = store.list_projects_blocking()?;
    let active_leases = store.list_active_leases_blocking(&agent_timestamp())?;

    let projects = projects
        .into_iter()
        .map(|project| {
            let scan = scan_agent_project(&project.path);
            let runtime_state = tui_agent_runtime_state(project.id, &active_leases);
            TuiAgentProject {
                project,
                scan,
                runtime_state,
            }
        })
        .collect();

    Ok(TuiAgentPanelSnapshot {
        projects,
        daemon_status,
    })
}

fn agent_service_needs_restart(
    service_status: &str,
    checkins: &[agent_store::AgentDaemonCheckin],
    now: u64,
) -> bool {
    service_status == "running"
        && checkins
            .iter()
            .filter(|checkin| checkin.mode == "service")
            .any(|checkin| !daemon_checkin_is_fresh(checkin, now))
        && !checkins
            .iter()
            .filter(|checkin| checkin.mode == "service")
            .any(|checkin| daemon_checkin_is_fresh(checkin, now))
}

fn tui_agent_runtime_state(
    project_id: i64,
    active_leases: &[agent_store::AgentLeaseRecord],
) -> TuiAgentRuntimeState {
    let Some(lease) = active_leases
        .iter()
        .find(|lease| lease.project_id == project_id)
    else {
        return TuiAgentRuntimeState::Idle;
    };

    match agent_lease_holder_liveness(&lease.holder) {
        AgentLeaseHolderLiveness::Dead => TuiAgentRuntimeState::Stale,
        AgentLeaseHolderLiveness::CurrentProcess
        | AgentLeaseHolderLiveness::Alive
        | AgentLeaseHolderLiveness::Unknown => TuiAgentRuntimeState::Running,
    }
}

fn format_agent_daemon_runtime_status(
    service_status: &str,
    checkins: &[agent_store::AgentDaemonCheckin],
    now: u64,
) -> String {
    let (fresh, stale): (Vec<_>, Vec<_>) = checkins
        .iter()
        .partition(|checkin| daemon_checkin_is_fresh(checkin, now));

    if !fresh.is_empty() {
        let active = format_daemon_checkin_modes(&fresh, "active");
        let has_service_checkin = fresh.iter().any(|checkin| checkin.mode == "service");
        if service_status == "running" && !has_service_checkin {
            return format!("{active}; service no-check-in");
        }
        return active;
    }

    if !stale.is_empty() {
        return format_daemon_checkin_modes(&stale, "stale");
    }

    match service_status {
        "running" => "service active (no check-in)".to_string(),
        "installed" => "service disabled".to_string(),
        "not-installed" => "disabled".to_string(),
        "unsupported" => "unsupported".to_string(),
        status if status.starts_with("unknown") => "unknown".to_string(),
        status => status.to_string(),
    }
}

fn daemon_checkin_is_fresh(checkin: &agent_store::AgentDaemonCheckin, now: u64) -> bool {
    checkin
        .expires_at
        .parse::<u64>()
        .map(|expires_at| expires_at > now)
        .unwrap_or(false)
}

fn format_daemon_checkin_modes(
    checkins: &[&agent_store::AgentDaemonCheckin],
    suffix: &str,
) -> String {
    let service_count = checkins
        .iter()
        .filter(|checkin| checkin.mode == "service")
        .count();
    let cli_count = checkins
        .iter()
        .filter(|checkin| checkin.mode == "cli")
        .count();
    let other_count = checkins.len().saturating_sub(service_count + cli_count);

    let label = match (service_count > 0, cli_count > 0, other_count > 0) {
        (true, true, _) => "service+cli",
        (true, false, false) => "service",
        (false, true, false) => "cli",
        (true, false, true) => "service+other",
        (false, true, true) => "cli+other",
        (false, false, true) => "daemon",
        (false, false, false) => "daemon",
    };

    if checkins.len() > 1 {
        format!("{label} {suffix} ({})", checkins.len())
    } else {
        format!("{label} {suffix}")
    }
}

fn current_project_registration(
    active_root: &Path,
    projects: &[TuiAgentProject],
) -> Option<TuiCurrentProjectRegistration> {
    if projects
        .iter()
        .any(|project| project.project.path == active_root)
    {
        return None;
    }

    Some(TuiCurrentProjectRegistration {
        path: active_root.to_path_buf(),
        name: project_display_name(active_root),
    })
}

fn register_selected_current_project(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(registration) = panel
        .selected_current_project_registration()
        .map(|registration| (registration.path.clone(), registration.name.clone()))
    else {
        return Ok("No current project registration row selected".to_string());
    };

    if !ensure_existing_board(&registration.0)? {
        return Ok(format!(
            "Project is not initialized: {}",
            registration.0.display()
        ));
    }

    let store = open_agent_store()?;
    let created = store.register_project_blocking(&registration.0, &registration.1)?;
    panel.refresh(active_root);

    if created {
        Ok(format!("Registered current project: {}", registration.1))
    } else {
        Ok(format!(
            "Project already registered: {}",
            registration.0.display()
        ))
    }
}

fn selected_tui_agent_project_removal(panel: &TuiAgentPanel) -> Option<TuiAgentProjectRemoval> {
    panel
        .selected_project()
        .map(|entry| TuiAgentProjectRemoval {
            path: entry.project.path.clone(),
            name: entry.project.name.clone(),
        })
}

fn tui_agent_project_removal_prompt(removal: &TuiAgentProjectRemoval) -> String {
    format!(
        "Remove agent project '{}' from the list? Press y to confirm; n or Esc cancels.",
        removal.name
    )
}

fn remove_tui_agent_project(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
    removal: &TuiAgentProjectRemoval,
) -> Result<String> {
    let store = open_agent_store()?;
    remove_tui_agent_project_with_store(panel, active_root, removal, &store)
}

fn remove_tui_agent_project_with_store(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
    removal: &TuiAgentProjectRemoval,
    store: &agent_store::TursoAgentStore,
) -> Result<String> {
    let selected_idx = panel.state.selected().unwrap_or(0);
    let removed = store.unregister_project_blocking(&removal.path)?;

    if removed {
        panel
            .projects
            .retain(|entry| entry.project.path != removal.path);
        panel.current_project_registration =
            current_project_registration(active_root, &panel.projects);
        panel.last_error = None;
        panel.select_nearest_row(selected_idx);
        Ok(format!("Removed agent project: {}", removal.name))
    } else {
        panel.refresh(active_root);
        Ok(format!(
            "Project is no longer registered: {}",
            removal.path.display()
        ))
    }
}

fn toggle_selected_tui_agent_project(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(project) = panel.selected_project().map(|entry| entry.project.clone()) else {
        return Ok("No registered project selected".to_string());
    };

    let enabled = !project.enabled;
    let store = open_agent_store()?;
    let changed = store.set_project_enabled_blocking(project.id, enabled)?;
    panel.refresh(active_root);

    if changed {
        let action = if enabled { "Turned on" } else { "Turned off" };
        Ok(format!("{} agent project: {}", action, project.name))
    } else {
        Ok(format!(
            "Project is no longer registered: {}",
            project.path.display()
        ))
    }
}

fn cycle_selected_tui_agent_project_git_mode(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(project) = panel.selected_project().map(|entry| entry.project.clone()) else {
        return Ok("No registered project selected".to_string());
    };

    let mode = project.git_mode.next();
    let store = open_agent_store()?;
    let changed = store.set_project_git_mode_blocking(project.id, mode)?;
    panel.refresh(active_root);

    if changed {
        Ok(format!("Git mode for {}: {}", project.name, mode.label()))
    } else {
        Ok(format!(
            "Project is no longer registered: {}",
            project.path.display()
        ))
    }
}

fn next_agent_codex_setting(current: Option<&str>, choices: &[&str]) -> Option<String> {
    let current_idx = choices
        .iter()
        .position(|choice| Some(*choice) == current)
        .unwrap_or(0);
    let next = choices[(current_idx + 1) % choices.len()];
    (!next.is_empty()).then(|| next.to_string())
}

fn update_selected_tui_agent_codex_settings(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    fast_enabled: bool,
) -> Result<bool> {
    let Some(project_id) = panel.selected_project().map(|entry| entry.project.id) else {
        return Ok(false);
    };

    let store = open_agent_store()?;
    let changed = store.set_project_codex_settings_blocking(
        project_id,
        provider.as_deref(),
        model.as_deref(),
        reasoning_effort.as_deref(),
        fast_enabled,
    )?;
    panel.refresh(active_root);
    Ok(changed)
}

fn cycle_selected_tui_agent_codex_model(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(project) = panel.selected_project().map(|entry| entry.project.clone()) else {
        return Ok("No registered project selected".to_string());
    };
    let store = open_agent_store()?;
    let targets = store.list_enabled_model_targets_blocking()?;
    let current_idx = project.codex_model.as_ref().and_then(|model| {
        let provider = project.codex_provider.as_deref().unwrap_or("openai");
        targets
            .iter()
            .position(|target| target.provider_id == provider && target.model_id == *model)
            .map(|index| index + 1)
    });
    let next_idx = (current_idx.unwrap_or(0) + 1) % (targets.len() + 1);
    let (provider, model, label) = if next_idx == 0 {
        (None, None, "CLT default".to_string())
    } else {
        let target = &targets[next_idx - 1];
        (
            Some(target.provider_id.clone()),
            Some(target.model_id.clone()),
            format!("{}/{}", target.provider_id, target.model_id),
        )
    };
    let changed = update_selected_tui_agent_codex_settings(
        panel,
        active_root,
        provider,
        model,
        project.codex_reasoning_effort,
        project.codex_fast_enabled,
    )?;

    if changed {
        Ok(format!("Codex model for {}: {}", project.name, label))
    } else {
        Ok(format!(
            "Project is no longer registered: {}",
            project.path.display()
        ))
    }
}

fn cycle_selected_tui_agent_codex_reasoning(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(project) = panel.selected_project().map(|entry| entry.project.clone()) else {
        return Ok("No registered project selected".to_string());
    };
    let reasoning = next_agent_codex_setting(
        project.codex_reasoning_effort.as_deref(),
        &AGENT_CODEX_REASONING_EFFORTS,
    );
    let label = reasoning.as_deref().unwrap_or("default").to_string();
    let changed = update_selected_tui_agent_codex_settings(
        panel,
        active_root,
        project.codex_provider,
        project.codex_model,
        reasoning,
        project.codex_fast_enabled,
    )?;

    if changed {
        Ok(format!("Codex thinking for {}: {}", project.name, label))
    } else {
        Ok(format!(
            "Project is no longer registered: {}",
            project.path.display()
        ))
    }
}

fn toggle_selected_tui_agent_codex_fast(
    panel: &mut TuiAgentPanel,
    active_root: &Path,
) -> Result<String> {
    let Some(project) = panel.selected_project().map(|entry| entry.project.clone()) else {
        return Ok("No registered project selected".to_string());
    };
    let fast_enabled = !project.codex_fast_enabled;
    let changed = update_selected_tui_agent_codex_settings(
        panel,
        active_root,
        project.codex_provider,
        project.codex_model,
        project.codex_reasoning_effort,
        fast_enabled,
    )?;

    if changed {
        Ok(format!(
            "Codex fast mode for {}: {}",
            project.name,
            if fast_enabled { "ON" } else { "OFF" }
        ))
    } else {
        Ok(format!(
            "Project is no longer registered: {}",
            project.path.display()
        ))
    }
}

fn tui_agent_panel_refresh_interval() -> Duration {
    Duration::from_secs(TUI_AGENT_PANEL_REFRESH_SECONDS)
}

fn tui_agent_log_refresh_interval() -> Duration {
    Duration::from_millis(TUI_AGENT_LOG_REFRESH_MILLIS)
}

fn tui_agent_panel_instructions() -> &'static str {
    "Up/Down selects, Enter opens/adds, Space toggles ON/OFF, Delete removes with confirmation, g cycles Git off/commit/push, m cycles the selected target, M opens Models, f toggles fast, t cycles thinking, l shows output. Tab returns to Kanban."
}

fn parse_agent_codex_session_id(line: &str) -> Option<String> {
    line.trim()
        .strip_prefix("session id:")
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .map(str::to_string)
}

fn agent_codex_session_id_from_log(path: &Path) -> Result<Option<String>> {
    let file = fs::File::open(path)
        .with_context(|| format!("Failed to open recorded agent output {path:?}"))?;

    for line in BufReader::new(file).lines().take(100) {
        if let Some(session_id) = parse_agent_codex_session_id(&line?) {
            return Ok(Some(session_id));
        }
    }

    Ok(None)
}

fn tui_agent_log_title(log_view: &TuiAgentLogView) -> String {
    let status = if log_view.is_live { "LIVE" } else { "LATEST" };
    format!(
        "Agent Output [{status}]: {} (l/Esc closes)",
        log_view.project_name
    )
}

fn latest_agent_log_path(log_dir: &Path, extension: &str) -> Result<Option<PathBuf>> {
    if !log_dir.exists() {
        return Ok(None);
    }

    let mut paths = Vec::new();
    for entry in fs::read_dir(log_dir)
        .with_context(|| format!("Failed to read agent log directory {:?}", log_dir))?
    {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file() && path.extension() == Some(OsStr::new(extension)) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths.pop())
}

fn preferred_recorded_agent_output_path(run: agent_store::AgentRunRecord) -> Option<PathBuf> {
    let stdout_path = run.stdout_path.map(PathBuf::from);
    let stdout_has_output = stdout_path
        .as_ref()
        .and_then(|path| fs::metadata(path).ok())
        .is_some_and(|metadata| metadata.len() > 0);

    if stdout_has_output {
        stdout_path
    } else {
        run.stderr_path.map(PathBuf::from).or(stdout_path)
    }
}

fn task_supports_interactive_codex_resume(status: &str, task: &TaskEntry) -> bool {
    status == "done" || matches!(status, "todo" | "doing") && task_entry_is_blocked(task)
}

fn codex_session_for_task(project_root: &Path, task: &TaskEntry) -> Result<Option<String>> {
    let store = open_agent_store()?;
    store.codex_session_for_task_blocking(project_root, task.content.trim_end())
}

fn selected_tui_agent_log_view(panel: &TuiAgentPanel) -> Result<Option<TuiAgentLogView>> {
    let state_dir = agent_state_dir()?;
    selected_tui_agent_log_view_at(panel, &state_dir)
}

fn selected_tui_agent_log_view_for_path(
    panel: &mut TuiAgentPanel,
    project_path: &Path,
) -> Result<Option<TuiAgentLogView>> {
    let state_dir = agent_state_dir()?;
    selected_tui_agent_log_view_for_path_at(panel, project_path, &state_dir)
}

fn selected_tui_agent_log_view_for_path_at(
    panel: &mut TuiAgentPanel,
    project_path: &Path,
    state_dir: &Path,
) -> Result<Option<TuiAgentLogView>> {
    if !panel.select_project_for_path(project_path) {
        return Ok(None);
    }

    selected_tui_agent_log_view_at(panel, state_dir)
}

fn selected_tui_agent_log_view_at(
    panel: &TuiAgentPanel,
    state_dir: &Path,
) -> Result<Option<TuiAgentLogView>> {
    let Some(selected) = panel.selected_project() else {
        return Ok(None);
    };

    let live_path = if selected.runtime_state.is_running() {
        latest_agent_log_path(
            &agent_project_run_log_dir(state_dir, &selected.project)?,
            "err",
        )?
    } else {
        None
    };

    let (path, is_live) = match live_path {
        Some(path) => (Some(path), true),
        None => {
            let store = open_agent_store_at(state_dir)?;
            (
                store
                    .latest_run_for_project_blocking(selected.project.id)?
                    .and_then(preferred_recorded_agent_output_path),
                false,
            )
        }
    };

    path.map(|path| TuiAgentLogView::new(selected.project.name.clone(), path, is_live))
        .transpose()
}

fn sync_open_tui_agent_log_view(panel: &TuiAgentPanel, log_view: &mut Option<TuiAgentLogView>) {
    if log_view.is_none() {
        return;
    }

    let selected_view =
        agent_state_dir().and_then(|state_dir| selected_tui_agent_log_view_at(panel, &state_dir));
    replace_open_tui_agent_log_view(panel, log_view, selected_view);
}

#[cfg(test)]
fn sync_open_tui_agent_log_view_at(
    panel: &TuiAgentPanel,
    log_view: &mut Option<TuiAgentLogView>,
    state_dir: &Path,
) {
    if log_view.is_none() {
        return;
    }

    let selected_view = selected_tui_agent_log_view_at(panel, state_dir);
    replace_open_tui_agent_log_view(panel, log_view, selected_view);
}

fn replace_open_tui_agent_log_view(
    panel: &TuiAgentPanel,
    log_view: &mut Option<TuiAgentLogView>,
    selected_view: Result<Option<TuiAgentLogView>>,
) {
    let project_name = panel
        .selected_project()
        .map(|selected| selected.project.name.clone())
        .or_else(|| {
            panel
                .selected_current_project_registration()
                .map(|registration| registration.name.clone())
        })
        .unwrap_or_else(|| "No Project Selected".to_string());

    *log_view = Some(match selected_view {
        Ok(Some(view)) => view,
        Ok(None) => {
            let content = if panel.selected_current_project_registration().is_some() {
                "Register this project before viewing agent output".to_string()
            } else if panel.selected_project().is_some() {
                "No agent output recorded for selected project".to_string()
            } else {
                "No registered project selected".to_string()
            };
            TuiAgentLogView::message(project_name, content)
        }
        Err(err) => {
            TuiAgentLogView::message(project_name, format!("Error loading agent output: {err}"))
        }
    });
}

fn tui_feedback_console_height(
    total_height: u16,
    total_width: u16,
    content: &str,
    agent_log_open: bool,
) -> u16 {
    let minimum_height = 3.min(total_height);
    let maximum_height = (total_height / 2).max(minimum_height).min(total_height);
    if agent_log_open {
        return maximum_height;
    }

    let content_width = total_width.saturating_sub(2) as usize;
    let content_height = wrap_input_text(content, content_width)
        .lines()
        .count()
        .max(1)
        .min(u16::MAX as usize) as u16;
    content_height
        .saturating_add(2)
        .max(minimum_height)
        .min(maximum_height)
}

fn tui_log_scroll_offset(content: &str, viewport_height: u16) -> u16 {
    let line_count = content.lines().count().max(1);
    let offset = line_count.saturating_sub(viewport_height as usize);
    offset.min(u16::MAX as usize) as u16
}

fn format_tui_agent_panel_top_status(
    daemon_status: &str,
    project_count: usize,
    enabled_count: usize,
    running_count: usize,
) -> String {
    let current_time = Local::now().format("%H:%M").to_string();
    format_tui_agent_panel_top_status_with_time(
        &current_time,
        daemon_status,
        project_count,
        enabled_count,
        running_count,
    )
}

fn format_tui_agent_panel_top_status_with_time(
    current_time: &str,
    daemon_status: &str,
    project_count: usize,
    enabled_count: usize,
    running_count: usize,
) -> String {
    format!(
        " {current_time}  daemon status: {daemon_status}  {project_count} projects  {enabled_count} enabled  {running_count} running "
    )
}

fn truncate_to_width(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len <= width {
        return value.to_string();
    }

    if width <= 3 {
        return value.chars().take(width).collect();
    }

    let mut truncated: String = value.chars().take(width - 3).collect();
    truncated.push_str("...");
    truncated
}

fn fit_cell(value: &str, width: usize) -> String {
    let value = truncate_to_width(value, width);
    format!("{value:<width$}")
}

fn fit_cell_right(value: &str, width: usize) -> String {
    let value = truncate_to_width(value, width);
    format!("{value:>width$}")
}

fn format_agent_table_last_run(project: &agent_store::AgentProject) -> String {
    let Some(raw) = project.last_run_at.as_deref() else {
        return "-".to_string();
    };
    let Ok(seconds) = raw.parse::<i64>() else {
        return raw.to_string();
    };
    let Some(utc) = DateTime::<Utc>::from_timestamp(seconds, 0) else {
        return raw.to_string();
    };

    utc.with_timezone(&Local).format("%m-%d %H:%M").to_string()
}

fn active_board_marker(is_current_board: bool) -> &'static str {
    if is_current_board { "*" } else { "" }
}

fn compact_agent_model_setting(provider: Option<&str>, model: Option<&str>) -> String {
    let model = model.unwrap_or("default");
    let compact = model
        .strip_prefix("gpt-")
        .unwrap_or(model)
        .replace("-codex", "");
    match provider.filter(|provider| *provider != "openai") {
        Some(provider) => format!("{provider}:{compact}"),
        None => compact,
    }
}

fn compact_agent_thinking_setting(thinking: Option<&str>) -> &str {
    match thinking {
        None => "def",
        Some("medium") => "med",
        Some(value) => value,
    }
}

fn compact_agent_codex_settings(
    provider: Option<&str>,
    model: Option<&str>,
    thinking: Option<&str>,
    fast_enabled: bool,
) -> String {
    let mut settings = Vec::new();

    if model.is_some() {
        settings.push(compact_agent_model_setting(provider, model));
    }
    if thinking.is_some() {
        settings.push(compact_agent_thinking_setting(thinking).to_string());
    }
    if fast_enabled {
        settings.push("fast".to_string());
    }

    if settings.is_empty() {
        "default".to_string()
    } else {
        settings.join("/")
    }
}

fn agent_codex_column_width(projects: &[TuiAgentProject], include_registration: bool) -> usize {
    let settings_width = projects
        .iter()
        .map(|item| {
            compact_agent_codex_settings(
                item.project.codex_provider.as_deref(),
                item.project.codex_model.as_deref(),
                item.project.codex_reasoning_effort.as_deref(),
                item.project.codex_fast_enabled,
            )
            .chars()
            .count()
        })
        .max()
        .unwrap_or(0);
    let registration_width = if include_registration {
        "Enter/Space".len()
    } else {
        0
    };

    "CODEX"
        .len()
        .max(settings_width)
        .max(registration_width)
        .min(TUI_AGENT_TABLE_CODEX_MAX_WIDTH)
}

fn agent_project_column_width(
    projects: &[TuiAgentProject],
    registration: Option<&TuiCurrentProjectRegistration>,
    table_width: usize,
    codex_width: usize,
) -> usize {
    let desired_width = projects
        .iter()
        .map(|item| item.project.name.chars().count())
        .chain(registration.map(|item| item.name.chars().count()))
        .max()
        .unwrap_or(0)
        .max("PROJECT".len());
    let fixed_width = if table_width < 120 { 52 } else { 53 } + codex_width;
    let available_width = table_width.saturating_sub(fixed_width);
    let max_project_width = if available_width > "PATH".len() {
        available_width - "PATH".len()
    } else {
        available_width
    };

    desired_width.min(max_project_width)
}

fn format_agent_project_table_row(
    idx: usize,
    item: &TuiAgentProject,
    width: usize,
    project_width: usize,
    codex_width: usize,
    is_current_board: bool,
) -> String {
    let marker = active_board_marker(is_current_board);
    let state = if item.project.enabled { "ON" } else { "OFF" };
    let runtime_state = item.runtime_state.label();
    let git = item.project.git_mode.tui_label();
    let codex = compact_agent_codex_settings(
        item.project.codex_provider.as_deref(),
        item.project.codex_model.as_deref(),
        item.project.codex_reasoning_effort.as_deref(),
        item.project.codex_fast_enabled,
    );
    let todo = item.scan.todo_count.to_string();
    let doing = item.scan.doing_count.to_string();
    let last_run = format_agent_table_last_run(&item.project);

    if width < 120 {
        let path_width = width.saturating_sub(52 + project_width + codex_width);
        return truncate_to_width(
            &format!(
                "{}{} {} {} {} {} {} {} {}{}{} {}",
                fit_cell(marker, 1),
                fit_cell_right(&(idx + 1).to_string(), 3),
                fit_cell(state, 6),
                fit_cell(git, 4),
                fit_cell(runtime_state, 7),
                fit_cell(&item.project.name, project_width),
                fit_cell_right(&todo, 4),
                fit_cell_right(&doing, 5),
                fit_cell(&codex, codex_width),
                TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
                fit_cell(&last_run, 11),
                fit_cell(&item.project.path.display().to_string(), path_width)
            ),
            width,
        );
    }

    let marker_width = 1;
    let number_width = 4;
    let state_width = 6;
    let runtime_width = 7;
    let git_width = 4;
    let todo_width = 4;
    let doing_width = 5;
    let last_run_width = 11;
    let gap_count = 8 + TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP.len();
    let fixed_width = number_width
        + marker_width
        + state_width
        + runtime_width
        + git_width
        + codex_width
        + todo_width
        + doing_width
        + last_run_width
        + gap_count;
    let path_width = width.saturating_sub(fixed_width + project_width);

    truncate_to_width(
        &format!(
            "{}{} {} {} {} {} {} {} {}{}{} {}",
            fit_cell(marker, marker_width),
            fit_cell_right(&(idx + 1).to_string(), number_width),
            fit_cell(state, state_width),
            fit_cell(git, git_width),
            fit_cell(runtime_state, runtime_width),
            fit_cell(&item.project.name, project_width),
            fit_cell_right(&todo, todo_width),
            fit_cell_right(&doing, doing_width),
            fit_cell(&codex, codex_width),
            TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
            fit_cell(&last_run, last_run_width),
            fit_cell(&item.project.path.display().to_string(), path_width)
        ),
        width,
    )
}

fn format_current_project_registration_row(
    registration: &TuiCurrentProjectRegistration,
    width: usize,
    project_width: usize,
    codex_width: usize,
) -> String {
    if width < 120 {
        let path_width = width.saturating_sub(52 + project_width + codex_width);
        return truncate_to_width(
            &format!(
                "{}{} {} {} {} {} {} {} {}{}{} {}",
                fit_cell("+", 1),
                fit_cell_right("", 3),
                fit_cell("ADD", 6),
                fit_cell("-", 4),
                fit_cell("-", 7),
                fit_cell(&registration.name, project_width),
                fit_cell_right("-", 4),
                fit_cell_right("-", 5),
                fit_cell("Enter/Space", codex_width),
                TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
                fit_cell("Enter/Space", 11),
                fit_cell(&registration.path.display().to_string(), path_width)
            ),
            width,
        );
    }

    let marker_width = 1;
    let number_width = 4;
    let state_width = 6;
    let runtime_width = 7;
    let git_width = 4;
    let todo_width = 4;
    let doing_width = 5;
    let last_run_width = 11;
    let gap_count = 8 + TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP.len();
    let fixed_width = number_width
        + marker_width
        + state_width
        + runtime_width
        + git_width
        + codex_width
        + todo_width
        + doing_width
        + last_run_width
        + gap_count;
    let path_width = width.saturating_sub(fixed_width + project_width);

    truncate_to_width(
        &format!(
            "{}{} {} {} {} {} {} {} {}{}{} {}",
            fit_cell("+", marker_width),
            fit_cell_right("", number_width),
            fit_cell("ADD", state_width),
            fit_cell("-", git_width),
            fit_cell("-", runtime_width),
            fit_cell(&registration.name, project_width),
            fit_cell_right("-", todo_width),
            fit_cell_right("-", doing_width),
            fit_cell("-", codex_width),
            TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
            fit_cell("Enter/Space", last_run_width),
            fit_cell(&registration.path.display().to_string(), path_width)
        ),
        width,
    )
}

fn format_agent_project_table_header(
    width: usize,
    project_width: usize,
    codex_width: usize,
) -> String {
    if width < 120 {
        let path_width = width.saturating_sub(52 + project_width + codex_width);
        return truncate_to_width(
            &format!(
                "{}{} {} {} {} {} {} {} {}{}{} {}",
                fit_cell("", 1),
                fit_cell_right("#", 3),
                fit_cell("STATUS", 6),
                fit_cell("GIT", 4),
                fit_cell("AGENT", 7),
                fit_cell("PROJECT", project_width),
                fit_cell_right("TODO", 4),
                fit_cell_right("DOING", 5),
                fit_cell("CODEX", codex_width),
                TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
                fit_cell("LAST RUN", 11),
                fit_cell("PATH", path_width)
            ),
            width,
        );
    }

    let marker_width = 1;
    let number_width = 4;
    let state_width = 6;
    let runtime_width = 7;
    let git_width = 4;
    let todo_width = 4;
    let doing_width = 5;
    let last_run_width = 11;
    let gap_count = 8 + TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP.len();
    let fixed_width = number_width
        + marker_width
        + state_width
        + runtime_width
        + git_width
        + codex_width
        + todo_width
        + doing_width
        + last_run_width
        + gap_count;
    let path_width = width.saturating_sub(fixed_width + project_width);

    truncate_to_width(
        &format!(
            "{}{} {} {} {} {} {} {} {}{}{} {}",
            fit_cell("", marker_width),
            fit_cell_right("#", number_width),
            fit_cell("STATUS", state_width),
            fit_cell("GIT", git_width),
            fit_cell("AGENT", runtime_width),
            fit_cell("PROJECT", project_width),
            fit_cell_right("TODO", todo_width),
            fit_cell_right("DOING", doing_width),
            fit_cell("CODEX", codex_width),
            TUI_AGENT_TABLE_CODEX_LAST_RUN_GAP,
            fit_cell("LAST RUN", last_run_width),
            fit_cell("PATH", path_width)
        ),
        width,
    )
}

fn render_tui_agent_panel(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    panel: &mut TuiAgentPanel,
    active_root: &Path,
    focused: bool,
    text_color: Color,
    c_highlight: Color,
) {
    let enabled_count = panel
        .projects
        .iter()
        .filter(|item| item.project.enabled)
        .count();
    let running_count = panel
        .projects
        .iter()
        .filter(|item| item.runtime_state.is_running())
        .count();
    let row_count = panel.row_count();
    let title = if focused {
        " Agent Projects  <<<<<< * >>>>>> "
    } else {
        " Agent Projects "
    };
    let block = Block::default()
        .title(title)
        .title(
            Line::from(vec![Span::raw(format_tui_agent_panel_top_status(
                &panel.daemon_status,
                panel.projects.len(),
                enabled_count,
                running_count,
            ))])
            .alignment(Alignment::Right),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            Color::Yellow
        } else {
            Color::Indexed(244)
        }));
    let inner_area = block.inner(area);
    f.render_widget(block, area);

    if inner_area.height == 0 || inner_area.width == 0 {
        return;
    }

    if row_count == 0 {
        f.render_widget(
            Paragraph::new("No registered projects. Run: clt agent register .")
                .style(Style::default().fg(Color::Indexed(244))),
            inner_area,
        );
        return;
    }

    let table_width = inner_area.width as usize;
    let row_viewport_height = inner_area.height.saturating_sub(2) as usize;
    panel.keep_selection_visible(row_viewport_height);
    let codex_width = agent_codex_column_width(
        &panel.projects,
        panel.current_project_registration.is_some(),
    );
    let project_width = agent_project_column_width(
        &panel.projects,
        panel.current_project_registration.as_ref(),
        table_width,
        codex_width,
    );

    let header_area = Rect {
        x: inner_area.x,
        y: inner_area.y,
        width: inner_area.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(format_agent_project_table_header(
            table_width,
            project_width,
            codex_width,
        ))
        .style(Style::default().fg(Color::Indexed(244))),
        header_area,
    );

    if inner_area.height > 1 {
        let separator_area = Rect {
            x: inner_area.x,
            y: inner_area.y + 1,
            width: inner_area.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new("-".repeat(table_width)).style(Style::default().fg(Color::Indexed(238))),
            separator_area,
        );
    }

    if row_viewport_height == 0 {
        return;
    }

    let selected_idx = panel.state.selected();
    let highlight_style = if focused {
        Style::default().fg(Color::Black).bg(c_highlight)
    } else {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    };

    for (row, idx) in (0..row_count)
        .skip(panel.scroll_offset)
        .take(row_viewport_height)
        .enumerate()
    {
        let (text, row_style) = if idx == 0 {
            if let Some(registration) = panel.current_project_registration.as_ref() {
                (
                    format_current_project_registration_row(
                        registration,
                        table_width,
                        project_width,
                        codex_width,
                    ),
                    Style::default().fg(Color::LightGreen),
                )
            } else {
                let item = &panel.projects[idx];
                (
                    format_agent_project_table_row(
                        idx,
                        item,
                        table_width,
                        project_width,
                        codex_width,
                        item.project.path == active_root,
                    ),
                    if item.project.enabled {
                        Style::default().fg(text_color)
                    } else {
                        Style::default().fg(Color::Indexed(244))
                    },
                )
            }
        } else {
            let project_idx = idx - panel.project_start_index();
            let item = &panel.projects[project_idx];
            (
                format_agent_project_table_row(
                    project_idx,
                    item,
                    table_width,
                    project_width,
                    codex_width,
                    item.project.path == active_root,
                ),
                if item.project.enabled {
                    Style::default().fg(text_color)
                } else {
                    Style::default().fg(Color::Indexed(244))
                },
            )
        };
        let style = if Some(idx) == selected_idx {
            highlight_style
        } else {
            row_style
        };
        let item_area = Rect {
            x: inner_area.x,
            y: inner_area.y + 2 + row as u16,
            width: inner_area.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(text).style(style), item_area);
    }
}

fn tui_models_instructions() -> &'static str {
    "Add a provider with 1 OpenAI, 2 OpenRouter, 3 Ollama, 4 LM Studio, or n local/custom. Select a non-built-in provider on the left and press x/Delete to remove it. Local endpoints discover /models automatically; r refreshes. New models start OFF: Right, Up/Down, then Space chooses them. a manually adds an ID. f favorites; t cycles model reasoning; d sets CLT; c sets Codex. M, Tab, or Esc returns to the previous pane. API keys come only from environment variables."
}

fn provider_env_status(provider: &agent_store::AgentModelProvider) -> String {
    match provider.env_key.as_deref() {
        Some(key) => {
            let visible = std::env::var_os(key)
                .is_some_and(|value| !value.to_string_lossy().trim().is_empty());
            format!("{key}:{}", if visible { "visible" } else { "missing" })
        }
        None => "no API key".to_string(),
    }
}

fn tui_models_provider_header() -> &'static str {
    "USE TYPE    PROVIDER (ID)"
}

fn tui_models_add_provider_menu() -> &'static str {
    "ADD PROVIDER PRESET\n[1] OpenAI   [2] OpenRouter\n[3] Ollama   [4] LM Studio\n[n] Local/custom endpoint"
}

fn include_codex_default_model_target(
    store: &agent_store::TursoAgentStore,
    selected_provider_id: &str,
    codex_provider_id: Option<&str>,
    codex_model_id: Option<&str>,
    models: &mut Vec<agent_store::AgentModelTarget>,
) -> Result<()> {
    let codex_provider_id = codex_provider_id.unwrap_or("openai");
    let Some(codex_model_id) = codex_model_id else {
        return Ok(());
    };
    if selected_provider_id != codex_provider_id
        || models
            .iter()
            .any(|model| model.provider_id == codex_provider_id && model.model_id == codex_model_id)
    {
        return Ok(());
    }

    let target = agent_store::AgentModelTarget {
        provider_id: codex_provider_id.to_string(),
        model_id: codex_model_id.to_string(),
        label: codex_model_id.to_string(),
        enabled: true,
        favorite: false,
        reasoning_effort: None,
    };
    store.upsert_model_target_blocking(&target)?;
    let insertion_index = models
        .iter()
        .position(|model| !model.favorite)
        .unwrap_or(models.len());
    models.insert(insertion_index, target);
    Ok(())
}

fn tui_models_provider_row(provider: &agent_store::AgentModelProvider) -> String {
    format!(
        "{:<3} {:<7} {} ({})",
        if provider.enabled { "ON" } else { "OFF" },
        if provider.built_in {
            "BUILTIN"
        } else {
            "CUSTOM"
        },
        provider.name,
        provider.id
    )
}

fn tui_models_model_header() -> String {
    format!(
        "{:<3} {:<3} {:<3} {:<5} {:<7} {:<16} {}",
        "USE", "FAV", "CLT", "CODEX", "THINK", "MODEL", "ID"
    )
}

fn tui_model_matches_clt_default(
    defaults: &agent_store::AgentModelDefaults,
    codex_provider_id: Option<&str>,
    codex_model_id: Option<&str>,
    model: &agent_store::AgentModelTarget,
) -> bool {
    match (
        defaults.provider_id.as_deref(),
        defaults.model_id.as_deref(),
    ) {
        (Some(provider_id), Some(model_id)) => {
            provider_id == model.provider_id && model_id == model.model_id
        }
        _ => tui_model_matches_codex_default(codex_provider_id, codex_model_id, model),
    }
}

fn tui_model_matches_codex_default(
    provider_id: Option<&str>,
    model_id: Option<&str>,
    model: &agent_store::AgentModelTarget,
) -> bool {
    let provider_id = provider_id.unwrap_or("openai");
    model_id == Some(model.model_id.as_str()) && provider_id == model.provider_id
}

fn tui_models_model_row(
    model: &agent_store::AgentModelTarget,
    is_clt_default: bool,
    is_codex_default: bool,
) -> String {
    format!(
        "{:<3} {:<3} {:<3} {:<5} {:<7} {:<16} {}",
        if model.enabled { "ON" } else { "OFF" },
        if model.favorite { "YES" } else { "-" },
        if is_clt_default { "YES" } else { "-" },
        if is_codex_default { "YES" } else { "-" },
        model.reasoning_effort.as_deref().unwrap_or("system"),
        model.label,
        model.model_id
    )
}

fn render_tui_models_panel(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    panel: &TuiModelsPanel,
    text_color: Color,
    c_highlight: Color,
) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(area);
    let clt_default = match (
        panel.defaults.provider_id.as_deref(),
        panel.defaults.model_id.as_deref(),
    ) {
        (Some(provider), Some(model)) => format!("{provider}/{model}"),
        _ => "Codex config".to_string(),
    };
    let default_label = format!(
        " CLT default: {clt_default} | Codex: {} ",
        panel.codex_default
    );
    let provider_focused = panel.focus == TuiModelsFocus::Providers;
    let provider_block = Block::default()
        .title(if provider_focused {
            " Providers  <<<<<< * >>>>>> "
        } else {
            " Providers "
        })
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if provider_focused {
            Color::Yellow
        } else {
            Color::Indexed(244)
        }));
    let provider_inner = provider_block.inner(chunks[0]);
    f.render_widget(provider_block, chunks[0]);

    if let Some(error) = panel.last_error.as_deref() {
        f.render_widget(
            Paragraph::new(error).style(Style::default().fg(Color::Red)),
            provider_inner,
        );
        return;
    }

    let add_menu_height = provider_inner.height.min(4);
    if add_menu_height > 0 {
        f.render_widget(
            Paragraph::new(tui_models_add_provider_menu())
                .style(Style::default().fg(Color::LightGreen)),
            Rect::new(
                provider_inner.x,
                provider_inner.y,
                provider_inner.width,
                add_menu_height,
            ),
        );
    }
    if provider_inner.height > add_menu_height {
        f.render_widget(
            Paragraph::new(truncate_to_width(
                tui_models_provider_header(),
                provider_inner.width as usize,
            ))
            .style(Style::default().fg(Color::Cyan)),
            Rect::new(
                provider_inner.x,
                provider_inner.y.saturating_add(add_menu_height),
                provider_inner.width,
                1,
            ),
        );
    }
    let provider_list_inner = Rect::new(
        provider_inner.x,
        provider_inner
            .y
            .saturating_add(add_menu_height)
            .saturating_add(1),
        provider_inner.width,
        provider_inner
            .height
            .saturating_sub(add_menu_height)
            .saturating_sub(1),
    );
    let provider_selected = panel.provider_state.selected();
    let provider_height = provider_list_inner.height as usize;
    let provider_offset = provider_selected
        .unwrap_or(0)
        .saturating_sub(provider_height.saturating_sub(1));
    for (index, provider) in panel
        .providers
        .iter()
        .enumerate()
        .skip(provider_offset)
        .take(provider_height)
    {
        let row = tui_models_provider_row(provider);
        let style = if Some(index) == provider_selected {
            if provider_focused {
                Style::default().fg(Color::Black).bg(c_highlight)
            } else {
                Style::default().fg(Color::White).bg(Color::DarkGray)
            }
        } else if provider.enabled {
            Style::default().fg(text_color)
        } else {
            Style::default().fg(Color::Indexed(244))
        };
        f.render_widget(
            Paragraph::new(truncate_to_width(&row, provider_list_inner.width as usize))
                .style(style),
            Rect::new(
                provider_list_inner.x,
                provider_list_inner.y + (index - provider_offset) as u16,
                provider_list_inner.width,
                1,
            ),
        );
    }

    let models_focused = panel.focus == TuiModelsFocus::Models;
    let selected_provider_name = panel
        .selected_provider()
        .map(|provider| provider.name.as_str())
        .unwrap_or("No provider");
    let models_block = Block::default()
        .title(if models_focused {
            format!(" {selected_provider_name} Models  <<<<<< * >>>>>> ")
        } else {
            format!(" {selected_provider_name} Models ")
        })
        .title(Line::from(default_label).alignment(Alignment::Right))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if models_focused {
            Color::Yellow
        } else {
            Color::Indexed(244)
        }));
    let models_inner = models_block.inner(chunks[1]);
    f.render_widget(models_block, chunks[1]);

    if models_inner.height > 0 {
        let detail = panel
            .selected_provider()
            .map(|provider| {
                format!(
                    "Provider: {}  Auth: {}  Endpoint: {}  [r] discover  Wire: responses",
                    provider.id,
                    provider_env_status(provider),
                    provider.base_url.as_deref().unwrap_or("Codex built-in")
                )
            })
            .unwrap_or_default();
        f.render_widget(
            Paragraph::new(truncate_to_width(&detail, models_inner.width as usize))
                .style(Style::default().fg(Color::Indexed(244))),
            Rect::new(models_inner.x, models_inner.y, models_inner.width, 1),
        );
    }
    if models_inner.height > 1 {
        let header = tui_models_model_header();
        f.render_widget(
            Paragraph::new(truncate_to_width(&header, models_inner.width as usize))
                .style(Style::default().fg(Color::Cyan)),
            Rect::new(
                models_inner.x,
                models_inner.y.saturating_add(1),
                models_inner.width,
                1,
            ),
        );
    }
    let models_list_inner = Rect::new(
        models_inner.x,
        models_inner.y.saturating_add(2),
        models_inner.width,
        models_inner.height.saturating_sub(2),
    );

    if panel.models.is_empty() && models_list_inner.height > 0 {
        f.render_widget(
            Paragraph::new("No models yet. Press r to discover or a to add a model ID.")
                .style(Style::default().fg(Color::Yellow)),
            models_list_inner,
        );
    }

    let model_selected = panel.model_state.selected();
    let models_height = models_list_inner.height as usize;
    let models_offset = model_selected
        .unwrap_or(0)
        .saturating_sub(models_height.saturating_sub(1));
    for (index, model) in panel
        .models
        .iter()
        .enumerate()
        .skip(models_offset)
        .take(models_height)
    {
        let row = tui_models_model_row(
            model,
            tui_model_matches_clt_default(
                &panel.defaults,
                panel.codex_default_provider.as_deref(),
                panel.codex_default_model.as_deref(),
                model,
            ),
            tui_model_matches_codex_default(
                panel.codex_default_provider.as_deref(),
                panel.codex_default_model.as_deref(),
                model,
            ),
        );
        let style = if Some(index) == model_selected {
            if models_focused {
                Style::default().fg(Color::Black).bg(c_highlight)
            } else {
                Style::default().fg(Color::White).bg(Color::DarkGray)
            }
        } else if model.enabled {
            Style::default().fg(text_color)
        } else {
            Style::default().fg(Color::Indexed(244))
        };
        f.render_widget(
            Paragraph::new(truncate_to_width(&row, models_list_inner.width as usize)).style(style),
            Rect::new(
                models_list_inner.x,
                models_list_inner.y + (index - models_offset) as u16,
                models_list_inner.width,
                1,
            ),
        );
    }
}

fn tui_console_content<'a>(
    agent_pane: bool,
    panel: &'a TuiAgentPanel,
    log_view: Option<&'a TuiAgentLogView>,
    feedback: &'a str,
) -> (&'a str, Color) {
    if agent_pane && let Some(error) = panel.last_error.as_deref() {
        return (error, Color::Red);
    }
    if let Some(log_view) = log_view {
        return (&log_view.content, Color::Gray);
    }

    (feedback, Color::Gray)
}

fn tui_keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
}

struct TerminalSession {
    keyboard_enhancement_enabled: bool,
    active: bool,
}

impl TerminalSession {
    fn enter(title: &str) -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = stdout();
        if let Err(err) = stdout.execute(EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(err.into());
        }
        if let Err(err) = stdout.execute(EnableBracketedPaste) {
            let _ = stdout.execute(LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(err.into());
        }

        // Request mode 2 modified-key reporting. In particular, tmux only forwards its
        // unambiguous extended key sequences when the application inside the pane asks for
        // them, which keeps Shift+Up/Down distinct from plain task navigation.
        #[cfg(not(windows))]
        if let Err(err) = stdout.execute(PushKeyboardEnhancementFlags(
            tui_keyboard_enhancement_flags(),
        )) {
            let _ = stdout.execute(DisableBracketedPaste);
            let _ = stdout.execute(LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(err.into());
        }
        let keyboard_enhancement_enabled = cfg!(not(windows));

        if let Err(err) = set_terminal_title(title) {
            if keyboard_enhancement_enabled {
                let _ = stdout.execute(PopKeyboardEnhancementFlags);
            }
            let _ = stdout.execute(DisableBracketedPaste);
            let _ = stdout.execute(LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(err);
        }

        Ok(Self {
            keyboard_enhancement_enabled,
            active: true,
        })
    }

    fn suspend(&mut self) {
        if !self.active {
            return;
        }
        if self.keyboard_enhancement_enabled {
            let _ = stdout().execute(PopKeyboardEnhancementFlags);
        }
        let _ = stdout().execute(DisableBracketedPaste);
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        let _ = stdout().flush();
        self.active = false;
    }

    fn resume(&mut self, title: &str) -> Result<()> {
        if self.active {
            return Ok(());
        }
        *self = Self::enter(title)?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.suspend();
    }
}

fn wrap_text(text: &str, width: usize) -> String {
    if width == 0 {
        return text.to_string();
    }
    let mut result = String::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        // Handle words longer than the width by breaking them
        let mut word_to_add = word;
        while word_to_add.len() > width {
            if !current_line.is_empty() {
                result.push_str(&current_line);
                result.push('\n');
                current_line.clear();
            }
            let (head, tail) = word_to_add.split_at(width);
            result.push_str(head);
            result.push('\n');
            word_to_add = tail;
        }

        if current_line.is_empty() {
            current_line.push_str(word_to_add);
        } else if current_line.len() + 1 + word_to_add.len() <= width {
            current_line.push(' ');
            current_line.push_str(word_to_add);
        } else {
            result.push_str(&current_line);
            result.push('\n');
            current_line.clear();
            current_line.push_str(word_to_add);
        }
    }
    result.push_str(&current_line);
    result
}

fn wrap_input_text(text: &str, width: usize) -> String {
    if width == 0 {
        return text.to_string();
    }

    let mut result = String::new();
    let mut col = 0;

    for ch in text.chars() {
        if ch == '\n' {
            result.push(ch);
            col = 0;
            continue;
        }

        if col >= width {
            result.push('\n');
            col = 0;
        }

        result.push(ch);
        col += 1;
    }

    result
}

fn input_cursor_offset_at(text: &str, width: usize, cursor_idx: usize) -> (u16, u16) {
    if width == 0 {
        return (0, 0);
    }

    let cursor_idx = clamp_to_char_boundary(text, cursor_idx.min(text.len()));
    let mut row = 0;
    let mut col = 0;

    for ch in text[..cursor_idx].chars() {
        if ch == '\n' {
            row += 1;
            col = 0;
            continue;
        }

        if col >= width {
            row += 1;
            col = 0;
        }

        col += 1;
    }

    if col >= width {
        (0, (row + 1) as u16)
    } else {
        (col as u16, row as u16)
    }
}

fn byte_index_at_char(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len())
}

fn input_cursor_offset_at_char(text: &str, width: usize, cursor_chars: usize) -> (usize, usize) {
    if width == 0 {
        return (0, 0);
    }

    let mut row = 0;
    let mut col = 0;

    for ch in text.chars().take(cursor_chars) {
        if ch == '\n' {
            row += 1;
            col = 0;
            continue;
        }

        if col >= width {
            row += 1;
            col = 0;
        }

        col += 1;
    }

    if col >= width {
        (0, row + 1)
    } else {
        (col, row)
    }
}

fn char_index_for_input_offset(
    text: &str,
    width: usize,
    target_row: usize,
    target_col: usize,
) -> usize {
    if width == 0 {
        return 0;
    }

    let mut row = 0;
    let mut col = 0;

    for (char_idx, ch) in text.chars().enumerate() {
        if row == target_row && col >= target_col {
            return char_idx;
        }

        if ch == '\n' {
            if row == target_row {
                return char_idx;
            }
            row += 1;
            col = 0;
            continue;
        }

        if col >= width {
            row += 1;
            col = 0;
            if row == target_row && col >= target_col {
                return char_idx;
            }
        }

        col += 1;
    }

    text.chars().count()
}

fn move_input_cursor_row(input: &mut Input, label: &str, width: usize, row_delta: isize) {
    if width == 0 {
        return;
    }

    let full_text = format!("{}{}", label, input.value());
    let label_chars = label.chars().count();
    let input_chars = input.value().chars().count();
    let cursor_chars = label_chars + input.cursor();
    let (target_col, current_row) = input_cursor_offset_at_char(&full_text, width, cursor_chars);
    let target_row = current_row.saturating_add_signed(row_delta);

    let target_chars = char_index_for_input_offset(&full_text, width, target_row, target_col);
    let input_cursor = target_chars.saturating_sub(label_chars).min(input_chars);
    input.handle(InputRequest::SetCursor(input_cursor));
}

fn handle_input_key(input: &mut Input, key: crossterm::event::KeyEvent, label: &str, width: usize) {
    let word_modifier =
        key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT);

    let request = match key.code {
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(InputRequest::GoToStart)
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(InputRequest::GoToEnd)
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(InputRequest::DeleteLine)
        }
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(InputRequest::DeleteTillEnd)
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(InputRequest::DeletePrevWord)
        }
        KeyCode::Char('b' | 'B') if key.modifiers.contains(KeyModifiers::ALT) => {
            Some(InputRequest::GoToPrevWord)
        }
        KeyCode::Char('f' | 'F') if key.modifiers.contains(KeyModifiers::ALT) => {
            Some(InputRequest::GoToNextWord)
        }
        KeyCode::Backspace if word_modifier => Some(InputRequest::DeletePrevWord),
        KeyCode::Delete if word_modifier => Some(InputRequest::DeleteNextWord),
        KeyCode::Left if word_modifier => Some(InputRequest::GoToPrevWord),
        KeyCode::Right if word_modifier => Some(InputRequest::GoToNextWord),
        KeyCode::Left => Some(InputRequest::GoToPrevChar),
        KeyCode::Right => Some(InputRequest::GoToNextChar),
        KeyCode::Home => Some(InputRequest::GoToStart),
        KeyCode::End => Some(InputRequest::GoToEnd),
        KeyCode::Backspace => Some(InputRequest::DeletePrevChar),
        KeyCode::Delete => Some(InputRequest::DeleteNextChar),
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            Some(InputRequest::InsertChar(c))
        }
        _ => None,
    };

    if let Some(request) = request {
        input.handle(request);
        return;
    }

    match key.code {
        KeyCode::Up => move_input_cursor_row(input, label, width, -1),
        KeyCode::Down => move_input_cursor_row(input, label, width, 1),
        _ => {}
    }
}

fn clamp_to_char_boundary(text: &str, idx: usize) -> usize {
    let mut idx = idx.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
fn previous_char_boundary(text: &str, idx: usize) -> usize {
    let idx = clamp_to_char_boundary(text, idx);
    text[..idx]
        .char_indices()
        .last()
        .map(|(char_idx, _)| char_idx)
        .unwrap_or(0)
}

#[cfg(test)]
fn next_char_boundary(text: &str, idx: usize) -> usize {
    let idx = clamp_to_char_boundary(text, idx);
    if idx >= text.len() {
        return text.len();
    }

    text[idx..]
        .char_indices()
        .nth(1)
        .map(|(offset, _)| idx + offset)
        .unwrap_or(text.len())
}

fn board_display_name(root: &Path, board_dir: &Path) -> String {
    let tasks_dir = get_tasks_dir(root);
    if board_dir == tasks_dir {
        return project_display_name(root);
    }

    let relative = board_dir.strip_prefix(&tasks_dir).unwrap_or(board_dir);
    let parts: Vec<String> = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => name.to_str().map(|name| {
                title_from_path(Path::new(strip_order_prefix(name)))
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
            _ => None,
        })
        .collect();

    if parts.is_empty() {
        project_display_name(root)
    } else {
        format!("{} / {}", project_display_name(root), parts.join(" / "))
    }
}

fn tui_console_block<'a>(title: &'a str, right_title: Option<&'a str>) -> Block<'a> {
    let block = Block::default().borders(Borders::ALL).title(title);

    if let Some(right_title) = right_title {
        block.title(Line::from(right_title).alignment(Alignment::Right))
    } else {
        block
    }
}

fn tui_view(root: &Path) -> Result<PathBuf> {
    tui_view_with_active_board(root, true)
}

fn tui_view_without_active_board(root: &Path) -> Result<PathBuf> {
    tui_view_with_active_board(root, false)
}

fn tui_view_with_active_board(root: &Path, start_with_active_board: bool) -> Result<PathBuf> {
    // Setup terminal
    let title = app_title(root);
    let mut terminal_session = TerminalSession::enter(&title)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut active_root = root.to_path_buf();
    let mut board_stack = if start_with_active_board {
        vec![get_tasks_dir(&active_root)]
    } else {
        Vec::new()
    };

    let start_state = tui_start_state(start_with_active_board);
    let mut active_board = start_state.active_board;
    let mut current_mode = Mode::View;
    let mut task_input = TaskInput::default();
    let mut feedback_buffer = start_state.feedback_buffer;
    let mut archive_view = false;
    let mut backlog_visible = false;
    let mut current_pane = start_state.current_pane;
    let mut models_return_pane = tui_models_return_pane(current_pane);
    let mut agent_panel = TuiAgentPanel::new(&active_root);
    let mut models_panel = TuiModelsPanel::new();
    let mut model_input: Option<TuiModelInput> = None;
    let mut pending_agent_project_removal: Option<TuiAgentProjectRemoval> = None;
    let mut last_agent_panel_refresh = Instant::now();
    let mut agent_log_view: Option<TuiAgentLogView> = None;
    let mut last_agent_log_refresh = Instant::now();

    let mut selected_board = TODO_BOARD_INDEX;
    let mut editing_task_idx: Option<usize> = None;
    let mut board_states = [
        ListState::default(),
        ListState::default(),
        ListState::default(),
        ListState::default(),
    ];
    let mut board_scroll_offsets = [0usize; 4];
    let mut archive_state = ListState::default();
    let mut archive_scroll_offset = 0usize;

    let statuses = TASK_STATUSES;
    let titles = ["To Do", "Doing", "Done", "Backlog"];
    // let c_1 = Color::LightCyan;
    // let c_2 = Color::LightGreen;
    // let c_3 = Color::LightMagenta;
    let c_1 = Color::Indexed(110);
    let c_2 = Color::Indexed(108);
    let c_3 = Color::Indexed(139);
    let c_backlog = Color::Indexed(244);
    let text_color = Color::Indexed(248); //Color::DarkGray;
    let c_highlight = Color::Indexed(221);
    let colors = [c_1, c_2, c_3, c_backlog];

    loop {
        if last_agent_panel_refresh.elapsed() >= tui_agent_panel_refresh_interval() {
            agent_panel.refresh(&active_root);
            sync_open_tui_agent_log_view(&agent_panel, &mut agent_log_view);
            last_agent_panel_refresh = Instant::now();
        }
        if last_agent_log_refresh.elapsed() >= tui_agent_log_refresh_interval() {
            if let Some(log_view) = agent_log_view.as_mut()
                && let Err(err) = log_view.refresh()
            {
                log_view.content = format!("Error refreshing agent output: {err}");
            }
            last_agent_log_refresh = Instant::now();
        }

        if !active_board && current_pane == TuiPane::Tasks {
            current_pane = TuiPane::AgentProjects;
            archive_view = false;
        }

        let board_dir = board_stack
            .last()
            .cloned()
            .unwrap_or_else(|| get_tasks_dir(&active_root));
        if active_board {
            if archive_view {
                normalize_archive_selection_in_board(&board_dir, &mut archive_state);
            } else {
                normalize_board_selections_in_board(&board_dir, &statuses, &mut board_states);
            }
        }

        terminal.draw(|f| {
            let size = f.area();
            let board_title = if active_board {
                board_display_name(&active_root, &board_dir)
            } else {
                "No Active Board".to_string()
            };
            let (console_title, console_right_title) = if let Some(log_view) = agent_log_view.as_ref() {
                (tui_agent_log_title(log_view), None)
            } else if current_pane == TuiPane::AgentProjects {
                ("Agent Projects Console".to_string(), None)
            } else if current_pane == TuiPane::Models {
                ("Models Console".to_string(), None)
            } else if archive_view {
                (format!("{board_title} Archive Console"), None)
            } else if !backlog_visible {
                let backlog_count = read_task_entries(&board_dir, "backlog")
                    .map(|entries| entries.len())
                    .unwrap_or(0);
                (
                    format!("{board_title} Console"),
                    Some(format!(" Backlog: {backlog_count} [B] ")),
                )
            } else {
                (format!("{board_title} Console"), None)
            };

            // Calculate input height if in Input or Edit mode
            let input_height = if model_input.is_some() {
                3
            } else if matches!(current_mode, Mode::Input) || matches!(current_mode, Mode::Edit) {
                    let label = if matches!(current_mode, Mode::Input) {
                        " Add Task: "
                    } else {
                        " Edit Task: "
                    };
                    let display_value = task_input.display_value();
                    let full_text = format!("{}{}", label, display_value);
                    // Subtract 2 for the borders of the block
                    let available_width = size.width.saturating_sub(2) as usize;
                    let wrapped = wrap_input_text(&full_text, available_width);
                    let lines = wrapped.lines().count();
                    let cursor_idx =
                        label.len() + byte_index_at_char(&display_value, task_input.display_cursor());
                    let cursor_row =
                        input_cursor_offset_at(&full_text, available_width, cursor_idx).1 as usize;
                    // Height = content rows + 2 (for top and bottom borders)
                    (lines.max(cursor_row + 1) + 2).max(3) as u16
            } else {
                0
            };

            let console_height = {
                let (console_content, _) = tui_console_content(
                    current_pane == TuiPane::AgentProjects,
                    &agent_panel,
                    agent_log_view.as_ref(),
                    feedback_buffer.as_str(),
                );
                tui_feedback_console_height(
                    size.height,
                    size.width,
                    console_content,
                    agent_log_view.is_some(),
                )
            };

            // Main layout: active pane, input area (if active), and feedback console
            let main_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(input_height),
                    Constraint::Length(console_height),
                ])
                .split(size);

            let content_area = main_layout[0];
            if current_pane == TuiPane::AgentProjects {
                render_tui_agent_panel(
                    f,
                    content_area,
                    &mut agent_panel,
                    &active_root,
                    true,
                    text_color,
                    c_highlight,
                );
            } else if current_pane == TuiPane::Models {
                render_tui_models_panel(
                    f,
                    content_area,
                    &models_panel,
                    text_color,
                    c_highlight,
                );
            } else if archive_view {
                let selected_idx = archive_state.selected();
                let col_width = content_area.width as usize;
                let entries = read_archived_task_entries(&board_dir).unwrap_or_default();
                let tasks: Vec<String> = entries
                    .iter()
                    .map(|entry| format!("- {}", task_display_text(entry)))
                    .collect();
                let display_tasks: Vec<String> = entries
                    .iter()
                    .enumerate()
                    .map(|(idx, entry)| {
                        format!(
                            "- {}",
                            task_tui_display_text(entry, Some(idx) == selected_idx)
                        )
                    })
                    .collect();
                let highlight_style = Style::default().fg(Color::Black).bg(c_highlight);
                let block = Block::default()
                    .title(" Archived - press A again to leave ")
                    .title(
                        Line::from(vec![Span::raw(format!(" {} ", tasks.len()))])
                            .alignment(Alignment::Right),
                    )
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Indexed(244)));

                let inner_area = block.inner(content_area);
                keep_selected_task_visible(
                    &display_tasks,
                    selected_idx,
                    &mut archive_scroll_offset,
                    inner_area.height as usize,
                    col_width,
                );

                let mut current_y = 0;
                for (idx, (t, entry)) in display_tasks
                    .iter()
                    .zip(entries.iter())
                    .enumerate()
                    .skip(archive_scroll_offset)
                {
                    let cleaned = t.strip_prefix("- ").unwrap_or(t);
                    let is_selected = Some(idx) == selected_idx;
                    let mut wrapped_content = if is_selected {
                        wrap_text(cleaned, col_width.saturating_sub(5))
                    } else {
                        cleaned.to_string()
                    };
                    if entry.has_subtasks {
                        wrapped_content.push_str(" >");
                    }

                    let line_count = wrapped_content.lines().count().max(1);
                    if current_y >= inner_area.height as usize {
                        break;
                    }

                    let visible_height =
                        (line_count as u16).min(inner_area.height.saturating_sub(current_y as u16));
                    let item_area = ratatui::layout::Rect {
                        x: inner_area.x,
                        y: inner_area.y + current_y as u16,
                        width: inner_area.width,
                        height: visible_height,
                    };
                    let style = if is_selected {
                        highlight_style
                    } else {
                        Style::default().fg(text_color)
                    };
                    let item_text = format!("{}. {}", idx + 1, wrapped_content);
                    f.render_widget(Paragraph::new(item_text).style(style), item_area);

                    current_y += line_count;
                    if inner_area.y + current_y as u16 >= content_area.height {
                        break;
                    }
                }

                f.render_widget(block, content_area);
            } else {
                let visible_boards = visible_tui_board_indices(backlog_visible);
                let column_count = visible_boards.len();
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(vec![Constraint::Ratio(1, column_count as u32); column_count])
                    .split(content_area);

                for (column_index, board_index) in visible_boards.iter().copied().enumerate() {
                    let status = statuses[board_index];
                    let selected_idx = board_states[board_index].selected();
                    let col_width = (size.width / column_count as u16) as usize;
                    let entries = read_task_entries(&board_dir, status).unwrap_or_default();
                    let tasks: Vec<String> = entries
                        .iter()
                        .map(|entry| format!("- {}", task_display_text(entry)))
                        .collect();
                    let display_tasks: Vec<String> = entries
                        .iter()
                        .enumerate()
                        .map(|(idx, entry)| {
                            format!(
                                "- {}",
                                task_tui_display_text(entry, Some(idx) == selected_idx)
                            )
                        })
                        .collect();
                    let _items: Vec<ListItem> = tasks
                        .clone()
                        .into_iter()
                        .enumerate()
                        .map(|(idx, t)| {
                            let cleaned = t.replace("- ", "");

                            let (desc, meta) = if let Some(start) = cleaned.rfind(" (") {
                                if cleaned.ends_with(')') {
                                    (
                                        &cleaned[..start],
                                        Some(&cleaned[start + 2..cleaned.len() - 1]),
                                    )
                                } else {
                                    (&cleaned[..], None)
                                }
                            } else {
                                (&cleaned[..], None)
                            };

                            let mut line = Line::from(vec![
                                Span::raw(format!("{}. ", idx + 1)),
                                Span::raw(if Some(idx) == selected_idx {
                                    wrap_text(desc, col_width.saturating_sub(5))
                                } else {
                                    desc.to_string()
                                }),
                            ]);

                            if let Some(m) = meta {
                                line.spans.push(
                                    Span::raw(format!(" ({})", m)).style(
                                        Style::default().bg(Color::DarkGray).fg(Color::White),
                                    ),
                                );
                            }

                            ListItem::new(line)
                        })
                        .collect();

                    let task_focus_active =
                        matches!(current_mode, Mode::View | Mode::Reorganize)
                            && current_pane == TuiPane::Tasks;
                    let highlight_style = if task_focus_active {
                        Style::default().fg(Color::Black).bg(c_highlight)
                    } else {
                        // Use a more subtle highlight when in Input/Edit mode
                        Style::default().fg(Color::White).bg(Color::DarkGray)
                    };

                    let block = Block::default()
                        .title(format!(
                            "{} {}",
                            titles[board_index],
                            if selected_board == board_index {
                                "  <<<<<< * >>>>>>     "
                            } else {
                                ""
                            }
                        ))
                        .title(
                            Line::from(vec![Span::raw(format!(" {} ", tasks.len()))])
                                .alignment(Alignment::Right),
                        )
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(colors[board_index]));

                    let inner_area = block.inner(chunks[column_index]);
                    keep_selected_task_visible(
                        &display_tasks,
                        selected_idx,
                        &mut board_scroll_offsets[board_index],
                        inner_area.height as usize,
                        col_width,
                    );

                    let mut current_y = 0;
                    for (idx, (t, entry)) in display_tasks
                        .iter()
                        .zip(entries.iter())
                        .enumerate()
                        .skip(board_scroll_offsets[board_index])
                    {
                        let cleaned = t.strip_prefix("- ").unwrap_or(t);
                        let is_selected = Some(idx) == selected_idx;

                        let text = if is_selected {
                            wrap_text(cleaned, col_width.saturating_sub(5))
                        } else {
                            cleaned.to_string()
                        };

                        let style = if is_selected {
                            highlight_style
                        } else {
                            Style::default().fg(text_color)
                        };

                        let content = format!("{}. {}", idx + 1, text);
                        let _paragraph = Paragraph::new(content).style(style);

                        let _area = ratatui::layout::Rect {
                            x: inner_area.x,
                            y: inner_area.y + current_y as u16,
                            width: inner_area.width,
                            height: 1, // This is a simplification; we should calculate height based on wrap_text
                        };

                        // To actually support multi-line expansion in a manual loop,
                        // we need to render the wrapped text as a Paragraph and increment current_y
                        // by the number of lines it actually takes.

                        let mut wrapped_content = if is_selected {
                            wrap_text(cleaned, col_width.saturating_sub(5))
                        } else {
                            cleaned.to_string()
                        };
                        if entry.has_subtasks {
                            wrapped_content.push_str(" >");
                        }

                        let line_count = wrapped_content.lines().count().max(1);
                        if current_y >= inner_area.height as usize {
                            break;
                        }

                        let visible_height = (line_count as u16)
                            .min(inner_area.height.saturating_sub(current_y as u16));
                        let item_area = ratatui::layout::Rect {
                            x: inner_area.x,
                            y: inner_area.y + current_y as u16,
                            width: inner_area.width,
                            height: visible_height,
                        };

                        let item_text = format!("{}. {}", idx + 1, wrapped_content);
                        f.render_widget(Paragraph::new(item_text).style(style), item_area);

                        current_y += line_count;
                        if inner_area.y + current_y as u16 >= chunks[column_index].height {
                            break;
                        }
                    }
                    f.render_widget(block, chunks[column_index]);
                }
            }

            if let Some(model_input) = model_input.as_ref() {
                let label = model_input.label();
                let input_text = format!("{}{}", label, model_input.input.value());
                let input_paragraph = Paragraph::new(input_text.as_str())
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Models Input (Enter advances/saves, Esc cancels)"),
                    )
                    .style(Style::default().fg(Color::White));
                f.render_widget(input_paragraph, main_layout[1]);
                let cursor = label.chars().count() + model_input.input.visual_cursor();
                let input_inner = main_layout[1].inner(ratatui::layout::Margin {
                    horizontal: 1,
                    vertical: 1,
                });
                f.set_cursor_position(Position::new(
                    input_inner.x
                        + (cursor as u16).min(input_inner.width.saturating_sub(1)),
                    input_inner.y,
                ));
            } else if matches!(current_mode, Mode::Input) || matches!(current_mode, Mode::Edit) {
                let label = if matches!(current_mode, Mode::Input) {
                    " Add Task: "
                } else {
                    " Edit Task: "
                };
                let display_value = task_input.display_value();
                let input_text = format!("{}{}", label, display_value);
                // Subtract 2 for the borders of the block
                let available_width = size.width.saturating_sub(2) as usize;
                let input_lines = styled_task_input_lines(label, &task_input, available_width);
                let input_paragraph = Paragraph::new(input_lines)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Input Mode (Enter to save, Esc to cancel)"),
                    )
                    .style(Style::default().fg(Color::White));
                f.render_widget(input_paragraph, main_layout[1]);

                let (cursor_x, cursor_y) = input_cursor_offset_at(
                    &input_text,
                    available_width,
                    label.len()
                        + byte_index_at_char(&display_value, task_input.display_cursor()),
                );
                let input_inner = main_layout[1].inner(ratatui::layout::Margin {
                    horizontal: 1,
                    vertical: 1,
                });
                f.set_cursor_position(Position::new(
                    input_inner.x + cursor_x.min(input_inner.width.saturating_sub(1)),
                    input_inner.y + cursor_y.min(input_inner.height.saturating_sub(1)),
                ));
            }

            let (console_content, console_color) = tui_console_content(
                current_pane == TuiPane::AgentProjects,
                &agent_panel,
                agent_log_view.as_ref(),
                feedback_buffer.as_str(),
            );
            let feedback_area = *main_layout.last().unwrap();
            let wrapped_console_content = wrap_input_text(
                console_content,
                feedback_area.width.saturating_sub(2) as usize,
            );
            let feedback_paragraph = Paragraph::new(wrapped_console_content.as_str())
                .block(tui_console_block(
                    console_title.as_str(),
                    console_right_title.as_deref(),
                ))
                .style(Style::default().fg(console_color))
                .scroll((
                    tui_log_scroll_offset(
                        &wrapped_console_content,
                        feedback_area.height.saturating_sub(2),
                    ),
                    0,
                ));

            // The feedback area is always the last element of main_layout
            f.render_widget(feedback_paragraph, feedback_area);

            if matches!(current_mode, Mode::Help) {
                let help_text = "TUI Commands:\n\n\
                                 [Space]        - Create new task / toggle selected agent project\n\
                                 [Enter]        - Open subtasks, edit selected task, or open selected agent project\n\
                                 [e]            - Edit selected task\n\
                                 [g]            - Cycle selected project's Git mode: off/commit/push\n\
                                 [c]            - Resume selected Done or blocked task in interactive Codex\n\
                                 [l]            - Toggle active/selected project's live/current agent output\n\
                                 [a]            - Move selected task to archive\n\
                                 [A]            - Toggle archive view\n\
                                 [b]            - Move selected task to backlog\n\
                                 [B]            - Show/hide backlog column\n\
                                 [Backspace]    - Return to parent board\n\
                                 [d/Del]        - Delete selected task\n\
                                 [Agent Del]    - Remove selected project after confirmation\n\
                                 [Tab]          - Toggle task board and agent projects\n\
                                 [Agent m]      - Cycle selected target\n\
                                 [M]            - Open Models from Tasks or Agent Projects\n\
                                 [Models n/r/a] - Add endpoint / discover models / manually add ID\n\
                                 [Models 1-4]   - Add OpenAI/OpenRouter/Ollama/LM Studio preset\n\
                                 [Models x/Del] - Remove selected non-built-in provider\n\
                                 [Models d/c]   - Set CLT default / explicitly set Codex default\n\
                                 [Arrows]       - Navigate boards and tasks\n\
                                 [r, then Arrow] - Reorganize task once (Esc cancels)\n\
                                 [Shift+Arrows] - Reorder/Move tasks\n\
                                 [Ctrl-P/N]     - Reorder task Up/Down\n\
                                 [0, 1, 2, 3]   - Focus Backlog/To Do/Doing/Done\n\
                                 [Input Arrows]         - Move cursor in wrapped input\n\
                                 [Ctrl/Alt+Left/Right]  - Jump input cursor by word\n\
                                 [Ctrl+A/E/W/U/K]       - Edit input line\n\
                                 [h / ?]        - Toggle Help\n\
                                 [q]            - Quit";

                let area = f.area();
                let popover_width = area.width.min(70);
                let popover_height = area.height.min(27);
                let x = area.width.saturating_sub(popover_width) / 2;
                let y = area.height.saturating_sub(popover_height) / 2;

                let popover_area = ratatui::layout::Rect {
                    x,
                    y,
                    width: popover_width,
                    height: popover_height,
                };

                let help_paragraph = Paragraph::new(help_text)
                    .block(
                        Block::default()
                            .title(" Help ")
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Yellow)),
                    )
                    .style(Style::default().fg(Color::White));

                f.render_widget(help_paragraph, popover_area);
            }
        })?;

        #[allow(clippy::collapsible_if)]
        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Paste(content) if model_input.is_some() => {
                    if let Some(input) = model_input.as_mut() {
                        input.insert_paste(&content);
                    }
                }
                Event::Paste(content) if matches!(current_mode, Mode::Input | Mode::Edit) => {
                    task_input.insert_paste(content);
                }
                Event::Key(key) => {
                    let input_available_width = terminal.size()?.width.saturating_sub(2) as usize;
                    if let Some(removal) = pending_agent_project_removal.take() {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                feedback_buffer = match remove_tui_agent_project(
                                    &mut agent_panel,
                                    &active_root,
                                    &removal,
                                ) {
                                    Ok(message) => {
                                        last_agent_panel_refresh = Instant::now();
                                        message
                                    }
                                    Err(error) => format!("Error: {error}"),
                                };
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                feedback_buffer = format!("Kept agent project: {}", removal.name);
                            }
                            _ => {
                                feedback_buffer = tui_agent_project_removal_prompt(&removal);
                                pending_agent_project_removal = Some(removal);
                            }
                        }
                        continue;
                    }
                    if let Some(input) = model_input.as_mut() {
                        match key.code {
                            KeyCode::Esc => {
                                model_input = None;
                                feedback_buffer = "Models input cancelled".to_string();
                            }
                            KeyCode::Enter => {
                                match submit_tui_model_input(input, &mut models_panel) {
                                    Ok(Some(message)) => {
                                        model_input = None;
                                        feedback_buffer = message;
                                    }
                                    Ok(None) => {
                                        feedback_buffer = input.guidance().to_string();
                                    }
                                    Err(error) => feedback_buffer = format!("Error: {error}"),
                                }
                            }
                            _ => {
                                let label = input.label();
                                handle_input_key(
                                    &mut input.input,
                                    key,
                                    label,
                                    input_available_width,
                                )
                            }
                        }
                        continue;
                    }
                    match current_mode {
                        Mode::View => {
                            if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                                let previous_pane = current_pane;
                                current_pane = if previous_pane == TuiPane::Models {
                                    models_return_pane
                                } else {
                                    tui_pane_after_tab(current_pane, active_board)
                                };
                                if previous_pane == TuiPane::Tasks {
                                    agent_panel.refresh(&active_root);
                                    agent_panel.select_project_for_path(&active_root);
                                    last_agent_panel_refresh = Instant::now();
                                } else if current_pane == TuiPane::Tasks {
                                    if agent_log_view.is_some() {
                                        agent_panel.select_project_for_path(&active_root);
                                        sync_open_tui_agent_log_view(
                                            &agent_panel,
                                            &mut agent_log_view,
                                        );
                                        last_agent_log_refresh = Instant::now();
                                    }
                                }
                                feedback_buffer = if current_pane == TuiPane::AgentProjects {
                                    tui_agent_panel_instructions().to_string()
                                } else {
                                    "Task board pane.".to_string()
                                };
                            } else if matches!(key.code, KeyCode::Esc)
                                && agent_log_view.take().is_some()
                            {
                                feedback_buffer = "Closed agent output log".to_string();
                            } else if current_pane == TuiPane::Models {
                                match key.code {
                                    KeyCode::Esc | KeyCode::Char('M') => {
                                        current_pane = models_return_pane;
                                        feedback_buffer = if current_pane == TuiPane::AgentProjects
                                        {
                                            tui_agent_panel_instructions().to_string()
                                        } else {
                                            "Task board pane.".to_string()
                                        };
                                    }
                                    KeyCode::Char('q') => break,
                                    KeyCode::Char('h')
                                    | KeyCode::Char('H')
                                    | KeyCode::Char('?') => current_mode = Mode::Help,
                                    KeyCode::Left => {
                                        models_panel.focus = TuiModelsFocus::Providers;
                                        feedback_buffer = tui_models_instructions().to_string();
                                    }
                                    KeyCode::Right => {
                                        models_panel.focus = TuiModelsFocus::Models;
                                        feedback_buffer = tui_models_instructions().to_string();
                                    }
                                    KeyCode::Up => models_panel.select_previous(),
                                    KeyCode::Down => models_panel.select_next(),
                                    KeyCode::Char(' ') => {
                                        feedback_buffer =
                                            match toggle_tui_models_enabled(&mut models_panel) {
                                                Ok(message) => message,
                                                Err(error) => format!("Error: {error}"),
                                            };
                                    }
                                    KeyCode::Char('x') | KeyCode::Char('X') | KeyCode::Delete => {
                                        feedback_buffer =
                                            match remove_tui_model_provider(&mut models_panel) {
                                                Ok(message) => message,
                                                Err(error) => format!("Error: {error}"),
                                            };
                                    }
                                    KeyCode::Char('f') | KeyCode::Char('F') => {
                                        models_panel.focus = TuiModelsFocus::Models;
                                        feedback_buffer =
                                            match toggle_tui_model_favorite(&mut models_panel) {
                                                Ok(message) => message,
                                                Err(error) => format!("Error: {error}"),
                                            };
                                    }
                                    KeyCode::Char('t') | KeyCode::Char('T') => {
                                        models_panel.focus = TuiModelsFocus::Models;
                                        feedback_buffer =
                                            match cycle_tui_model_reasoning(&mut models_panel) {
                                                Ok(message) => message,
                                                Err(error) => format!("Error: {error}"),
                                            };
                                    }
                                    KeyCode::Char('d') | KeyCode::Char('D') => {
                                        models_panel.focus = TuiModelsFocus::Models;
                                        feedback_buffer =
                                            match set_tui_model_default(&mut models_panel) {
                                                Ok(message) => message,
                                                Err(error) => format!("Error: {error}"),
                                            };
                                    }
                                    KeyCode::Char('c') | KeyCode::Char('C') => {
                                        models_panel.focus = TuiModelsFocus::Models;
                                        feedback_buffer =
                                            match set_tui_codex_default(&mut models_panel) {
                                                Ok(message) => message,
                                                Err(error) => format!("Error: {error}"),
                                            };
                                    }
                                    KeyCode::Char('a') | KeyCode::Char('A') => {
                                        if let Some(provider) = models_panel.selected_provider() {
                                            model_input =
                                                Some(TuiModelInput::add_model(provider.id.clone()));
                                            feedback_buffer =
                                                format!("Add a model ID for {}", provider.name);
                                        } else {
                                            feedback_buffer =
                                                "Add a provider before adding a model".to_string();
                                        }
                                    }
                                    KeyCode::Char('r') | KeyCode::Char('R') => {
                                        feedback_buffer =
                                            match discover_tui_provider_models(&mut models_panel) {
                                                Ok(message) => message,
                                                Err(error) => {
                                                    format!("Model discovery failed: {error}")
                                                }
                                            };
                                    }
                                    KeyCode::Char('n') | KeyCode::Char('N') => {
                                        model_input = Some(TuiModelInput::custom_provider());
                                        feedback_buffer = model_input
                                            .as_ref()
                                            .expect("custom provider input was just created")
                                            .guidance()
                                            .to_string();
                                    }
                                    KeyCode::Char(digit @ '1'..='4') => {
                                        let index = digit as usize - '1' as usize;
                                        feedback_buffer = match add_tui_model_provider_preset(
                                            &mut models_panel,
                                            index,
                                        ) {
                                            Ok(message) => message,
                                            Err(error) => format!("Error: {error}"),
                                        };
                                    }
                                    _ => feedback_buffer = tui_models_instructions().to_string(),
                                }
                            } else if current_pane == TuiPane::AgentProjects {
                                match key.code {
                                    KeyCode::Esc => {
                                        if active_board {
                                            current_pane = TuiPane::Tasks;
                                            feedback_buffer = "Task board pane.".to_string();
                                        } else {
                                            feedback_buffer =
                                                TUI_NO_ACTIVE_BOARD_MESSAGE.to_string();
                                        }
                                    }
                                    KeyCode::Char('q') => break,
                                    KeyCode::Char('h')
                                    | KeyCode::Char('H')
                                    | KeyCode::Char('?') => {
                                        current_mode = Mode::Help;
                                    }
                                    KeyCode::Delete => {
                                        if let Some(removal) =
                                            selected_tui_agent_project_removal(&agent_panel)
                                        {
                                            agent_log_view = None;
                                            feedback_buffer =
                                                tui_agent_project_removal_prompt(&removal);
                                            pending_agent_project_removal = Some(removal);
                                        } else {
                                            feedback_buffer =
                                                "No registered project selected to remove"
                                                    .to_string();
                                        }
                                    }
                                    KeyCode::Enter => {
                                        if agent_panel
                                            .selected_current_project_registration()
                                            .is_some()
                                        {
                                            match register_selected_current_project(
                                                &mut agent_panel,
                                                &active_root,
                                            ) {
                                                Ok(message) => {
                                                    last_agent_panel_refresh = Instant::now();
                                                    feedback_buffer = message;
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                            continue;
                                        }

                                        let Some(project) = agent_panel
                                            .selected_project()
                                            .map(|entry| entry.project.clone())
                                        else {
                                            feedback_buffer =
                                                "No registered project selected".to_string();
                                            continue;
                                        };

                                        match ensure_existing_board(&project.path) {
                                            Ok(true) => {}
                                            Ok(false) => {
                                                feedback_buffer = format!(
                                                    "Project is not initialized: {}",
                                                    project.path.display()
                                                );
                                                continue;
                                            }
                                            Err(error) => {
                                                feedback_buffer = format!(
                                                    "Failed to repair project board {}: {}",
                                                    project.path.display(),
                                                    error
                                                );
                                                continue;
                                            }
                                        }

                                        match std::env::set_current_dir(&project.path) {
                                            Ok(_) => {
                                                active_root = project.path.clone();
                                                active_board = true;
                                                board_stack.clear();
                                                board_stack.push(get_tasks_dir(&active_root));
                                                selected_board = TODO_BOARD_INDEX;
                                                for state in board_states.iter_mut() {
                                                    state.select(None);
                                                }
                                                board_scroll_offsets = [0usize; 4];
                                                archive_state.select(None);
                                                archive_scroll_offset = 0;
                                                archive_view = false;
                                                current_pane = TuiPane::Tasks;
                                                let board_dir = get_tasks_dir(&active_root);
                                                select_first_task_if_present_in_board(
                                                    &board_dir,
                                                    statuses[selected_board],
                                                    &mut board_states[selected_board],
                                                );
                                                feedback_buffer = match set_terminal_title(
                                                    &app_title(&active_root),
                                                ) {
                                                    Ok(_) => {
                                                        format!(
                                                            "Opened project board: {}",
                                                            project.name
                                                        )
                                                    }
                                                    Err(err) => {
                                                        format!(
                                                            "Opened project board: {}; failed to update title: {}",
                                                            project.name, err
                                                        )
                                                    }
                                                };
                                            }
                                            Err(err) => {
                                                feedback_buffer = format!(
                                                    "Failed to switch to {}: {}",
                                                    project.path.display(),
                                                    err
                                                );
                                            }
                                        }
                                    }
                                    KeyCode::Char(' ') => {
                                        if agent_panel
                                            .selected_current_project_registration()
                                            .is_some()
                                        {
                                            match register_selected_current_project(
                                                &mut agent_panel,
                                                &active_root,
                                            ) {
                                                Ok(message) => {
                                                    last_agent_panel_refresh = Instant::now();
                                                    feedback_buffer = message;
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                            continue;
                                        }

                                        match toggle_selected_tui_agent_project(
                                            &mut agent_panel,
                                            &active_root,
                                        ) {
                                            Ok(message) => {
                                                last_agent_panel_refresh = Instant::now();
                                                feedback_buffer = message;
                                            }
                                            Err(e) => feedback_buffer = format!("Error: {}", e),
                                        }
                                    }
                                    KeyCode::Char('g') | KeyCode::Char('G') => {
                                        if agent_panel
                                            .selected_current_project_registration()
                                            .is_some()
                                        {
                                            feedback_buffer =
                                            "Register current project before changing its Git mode"
                                                .to_string();
                                            continue;
                                        }

                                        match cycle_selected_tui_agent_project_git_mode(
                                            &mut agent_panel,
                                            &active_root,
                                        ) {
                                            Ok(message) => {
                                                last_agent_panel_refresh = Instant::now();
                                                feedback_buffer = message;
                                            }
                                            Err(e) => feedback_buffer = format!("Error: {}", e),
                                        }
                                    }
                                    KeyCode::Char('m') => {
                                        if agent_panel
                                            .selected_current_project_registration()
                                            .is_some()
                                        {
                                            feedback_buffer =
                                            "Register current project before changing its Codex model"
                                                .to_string();
                                            continue;
                                        }

                                        match cycle_selected_tui_agent_codex_model(
                                            &mut agent_panel,
                                            &active_root,
                                        ) {
                                            Ok(message) => {
                                                last_agent_panel_refresh = Instant::now();
                                                feedback_buffer = message;
                                            }
                                            Err(e) => feedback_buffer = format!("Error: {}", e),
                                        }
                                    }
                                    KeyCode::Char('M') => {
                                        models_return_pane = tui_models_return_pane(current_pane);
                                        models_panel.refresh();
                                        current_pane = TuiPane::Models;
                                        feedback_buffer = tui_models_instructions().to_string();
                                    }
                                    KeyCode::Char('f') | KeyCode::Char('F') => {
                                        if agent_panel
                                            .selected_current_project_registration()
                                            .is_some()
                                        {
                                            feedback_buffer =
                                            "Register current project before changing Codex fast mode"
                                                .to_string();
                                            continue;
                                        }

                                        match toggle_selected_tui_agent_codex_fast(
                                            &mut agent_panel,
                                            &active_root,
                                        ) {
                                            Ok(message) => {
                                                last_agent_panel_refresh = Instant::now();
                                                feedback_buffer = message;
                                            }
                                            Err(e) => feedback_buffer = format!("Error: {}", e),
                                        }
                                    }
                                    KeyCode::Char('t') | KeyCode::Char('T') => {
                                        if agent_panel
                                            .selected_current_project_registration()
                                            .is_some()
                                        {
                                            feedback_buffer =
                                            "Register current project before changing Codex thinking"
                                                .to_string();
                                            continue;
                                        }

                                        match cycle_selected_tui_agent_codex_reasoning(
                                            &mut agent_panel,
                                            &active_root,
                                        ) {
                                            Ok(message) => {
                                                last_agent_panel_refresh = Instant::now();
                                                feedback_buffer = message;
                                            }
                                            Err(e) => feedback_buffer = format!("Error: {}", e),
                                        }
                                    }
                                    KeyCode::Char('l') | KeyCode::Char('L') => {
                                        if agent_log_view.take().is_some() {
                                            feedback_buffer = "Closed agent output log".to_string();
                                            continue;
                                        }

                                        match selected_tui_agent_log_view(&agent_panel) {
                                            Ok(Some(log_view)) => {
                                                let output_kind = if log_view.is_live {
                                                    "live agent output"
                                                } else {
                                                    "latest agent output"
                                                };
                                                feedback_buffer = format!(
                                                    "Showing {output_kind} for {}",
                                                    log_view.project_name
                                                );
                                                agent_log_view = Some(log_view);
                                                last_agent_log_refresh = Instant::now();
                                            }
                                            Ok(None) => {
                                                feedback_buffer = if agent_panel
                                                    .selected_current_project_registration()
                                                    .is_some()
                                                {
                                                    "Register current project before viewing agent output"
                                                .to_string()
                                                } else {
                                                    "No agent output recorded for selected project"
                                                        .to_string()
                                                };
                                            }
                                            Err(e) => feedback_buffer = format!("Error: {}", e),
                                        }
                                    }
                                    KeyCode::Up => {
                                        agent_panel.select_previous();
                                        sync_open_tui_agent_log_view(
                                            &agent_panel,
                                            &mut agent_log_view,
                                        );
                                        last_agent_log_refresh = Instant::now();
                                    }
                                    KeyCode::Down => {
                                        agent_panel.select_next();
                                        sync_open_tui_agent_log_view(
                                            &agent_panel,
                                            &mut agent_log_view,
                                        );
                                        last_agent_log_refresh = Instant::now();
                                    }
                                    _ => {
                                        feedback_buffer =
                                            tui_agent_panel_instructions().to_string();
                                    }
                                }
                            } else if archive_view {
                                match key.code {
                                    KeyCode::Char('A') | KeyCode::Char('a')
                                        if key.modifiers.contains(KeyModifiers::SHIFT) =>
                                    {
                                        archive_view = false;
                                        archive_state.select(None);
                                        archive_scroll_offset = 0;
                                        feedback_buffer = "Returned to Kanban view".to_string();
                                    }
                                    KeyCode::Char('q') => break,
                                    KeyCode::Char('h')
                                    | KeyCode::Char('H')
                                    | KeyCode::Char('?') => {
                                        current_mode = Mode::Help;
                                    }
                                    KeyCode::Up => {
                                        let tasks = read_archived_task_entries(&board_dir)
                                            .unwrap_or_default();
                                        if !tasks.is_empty() {
                                            let i = archive_state.selected().unwrap_or(0);
                                            if i > 0 {
                                                archive_state.select(Some(i - 1));
                                            } else {
                                                archive_state.select(Some(tasks.len() - 1));
                                            }
                                        }
                                    }
                                    KeyCode::Down => {
                                        let tasks = read_archived_task_entries(&board_dir)
                                            .unwrap_or_default();
                                        if !tasks.is_empty() {
                                            let i = archive_state.selected().unwrap_or(0);
                                            if i < tasks.len() - 1 {
                                                archive_state.select(Some(i + 1));
                                            } else {
                                                archive_state.select(Some(0));
                                            }
                                        }
                                    }
                                    _ => {
                                        feedback_buffer =
                                            "Archive view is read-only. Press A again to leave."
                                                .to_string();
                                    }
                                }
                            } else if matches!(key.code, KeyCode::Char('M')) {
                                models_return_pane = tui_models_return_pane(current_pane);
                                models_panel.refresh();
                                current_pane = TuiPane::Models;
                                feedback_buffer = tui_models_instructions().to_string();
                            } else if let Some(direction) = tui_task_reorder_direction(&key) {
                                feedback_buffer = reorder_selected_tui_task(
                                    &board_dir,
                                    statuses[selected_board],
                                    &mut board_states[selected_board],
                                    direction,
                                );
                            } else if tui_starts_reorganize_mode(&key) {
                                current_mode = Mode::Reorganize;
                                feedback_buffer =
                                "Reorganize: press an Arrow to move the selected task (Esc cancels)."
                                    .to_string();
                            } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                                match key.code {
                                    KeyCode::Char('A') | KeyCode::Char('a') => {
                                        archive_view = true;
                                        current_mode = Mode::View;
                                        archive_scroll_offset = 0;
                                        select_first_archive_task_if_present_in_board(
                                            &board_dir,
                                            &mut archive_state,
                                        );
                                        feedback_buffer =
                                            "Archive view. Press A again to leave archive view."
                                                .to_string();
                                    }
                                    KeyCode::Char('B') | KeyCode::Char('b') => {
                                        feedback_buffer = toggle_tui_backlog_column(
                                            &board_dir,
                                            &mut board_states,
                                            &mut selected_board,
                                            &mut backlog_visible,
                                        );
                                    }
                                    KeyCode::Left => {
                                        feedback_buffer = move_selected_tui_task_between_boards(
                                            &board_dir,
                                            &statuses,
                                            &mut board_states,
                                            &mut selected_board,
                                            backlog_visible,
                                            TuiTaskBoardMoveDirection::Left,
                                        );
                                    }
                                    KeyCode::Right => {
                                        feedback_buffer = move_selected_tui_task_between_boards(
                                            &board_dir,
                                            &statuses,
                                            &mut board_states,
                                            &mut selected_board,
                                            backlog_visible,
                                            TuiTaskBoardMoveDirection::Right,
                                        );
                                    }
                                    _ => {}
                                }
                            } else if key.modifiers.contains(KeyModifiers::CONTROL)
                                || key.modifiers.contains(KeyModifiers::ALT)
                            {
                                // Other Alt/Ctrl modifiers are not used for moving tasks.
                                _ = ();
                            } else {
                                match key.code {
                                    KeyCode::Esc => {
                                        let state = &mut board_states[selected_board];
                                        state.select(None);
                                        feedback_buffer = "Task unselected".to_string();
                                    }
                                    KeyCode::Char('B') => {
                                        feedback_buffer = toggle_tui_backlog_column(
                                            &board_dir,
                                            &mut board_states,
                                            &mut selected_board,
                                            &mut backlog_visible,
                                        );
                                    }
                                    KeyCode::Char('b') => {
                                        feedback_buffer = match move_selected_tui_task_to_backlog(
                                            &board_dir,
                                            &statuses,
                                            &mut board_states,
                                            &mut selected_board,
                                            backlog_visible,
                                        ) {
                                            Ok(message) => message,
                                            Err(error) => format!("Error: {error}"),
                                        };
                                    }
                                    KeyCode::Char('a') => {
                                        feedback_buffer = match move_selected_tui_task_to_archive(
                                            &board_dir,
                                            &statuses,
                                            &mut board_states,
                                            selected_board,
                                        ) {
                                            Ok(message) => message,
                                            Err(error) => format!("Error: {error}"),
                                        };
                                    }
                                    KeyCode::Char('A') => {
                                        archive_view = true;
                                        current_mode = Mode::View;
                                        archive_scroll_offset = 0;
                                        select_first_archive_task_if_present_in_board(
                                            &board_dir,
                                            &mut archive_state,
                                        );
                                        feedback_buffer =
                                            "Archive view. Press A again to leave archive view."
                                                .to_string();
                                    }
                                    KeyCode::Char('c') => {
                                        let selected_status = statuses[selected_board];
                                        let Some((_, task)) = selected_task_entry_in_board(
                                            &board_dir,
                                            selected_status,
                                            &board_states[selected_board],
                                        ) else {
                                            feedback_buffer = "No task selected".to_string();
                                            continue;
                                        };

                                        if !task_supports_interactive_codex_resume(
                                            selected_status,
                                            &task,
                                        ) {
                                            feedback_buffer =
                                                "Codex sessions can be resumed from Done or blocked tasks."
                                                    .to_string();
                                            continue;
                                        }

                                        match codex_session_for_task(&active_root, &task) {
                                            Ok(Some(session_id)) => {
                                                agent_log_view = None;
                                                terminal_session.suspend();
                                                let resume_result =
                                                    resume_codex_session_interactively(
                                                        &active_root,
                                                        &session_id,
                                                    );
                                                terminal_session
                                                    .resume(&app_title(&active_root))?;
                                                terminal.clear()?;
                                                agent_panel.refresh(&active_root);
                                                last_agent_panel_refresh = Instant::now();
                                                normalize_board_selections_in_board(
                                                    &board_dir,
                                                    &statuses,
                                                    &mut board_states,
                                                );
                                                feedback_buffer = match resume_result {
                                                    Ok(status) if status.success() => format!(
                                                        "Returned from Codex session for: {}",
                                                        task_display_text(&task)
                                                    ),
                                                    Ok(status) => format!(
                                                        "Codex session exited with status {status}"
                                                    ),
                                                    Err(error) => format!("Error: {error}"),
                                                };
                                            }
                                            Ok(None) => {
                                                feedback_buffer =
                                                "No Codex session recorded for this task. Sessions are available for tasks handled by newer automated runs."
                                                    .to_string();
                                            }
                                            Err(error) => {
                                                feedback_buffer = format!("Error: {error}");
                                            }
                                        }
                                    }
                                    KeyCode::Char('l') | KeyCode::Char('L') => {
                                        if agent_log_view.take().is_some() {
                                            feedback_buffer = "Closed agent output log".to_string();
                                            continue;
                                        }

                                        agent_panel.refresh(&active_root);
                                        last_agent_panel_refresh = Instant::now();
                                        match selected_tui_agent_log_view_for_path(
                                            &mut agent_panel,
                                            &active_root,
                                        ) {
                                            Ok(Some(log_view)) => {
                                                let output_kind = if log_view.is_live {
                                                    "live agent output"
                                                } else {
                                                    "latest agent output"
                                                };
                                                feedback_buffer = format!(
                                                    "Showing {output_kind} for {}",
                                                    log_view.project_name
                                                );
                                                agent_log_view = Some(log_view);
                                                last_agent_log_refresh = Instant::now();
                                            }
                                            Ok(None) => {
                                                feedback_buffer = if agent_panel
                                                    .last_error
                                                    .is_some()
                                                {
                                                    agent_panel
                                                        .last_error
                                                        .clone()
                                                        .unwrap_or_default()
                                                } else if agent_panel
                                                    .selected_current_project_registration()
                                                    .is_some()
                                                {
                                                    "Register current project before viewing agent output"
                                                    .to_string()
                                                } else {
                                                    "No agent output recorded for current project"
                                                        .to_string()
                                                };
                                            }
                                            Err(e) => feedback_buffer = format!("Error: {}", e),
                                        }
                                    }
                                    KeyCode::Char('q') => break,
                                    KeyCode::Backspace => {
                                        if board_stack.len() > 1 {
                                            board_stack.pop();
                                            selected_board = TODO_BOARD_INDEX;
                                            for state in board_states.iter_mut() {
                                                state.select(None);
                                            }
                                            let parent_board = board_stack
                                                .last()
                                                .cloned()
                                                .unwrap_or_else(|| get_tasks_dir(&active_root));
                                            select_first_task_if_present_in_board(
                                                &parent_board,
                                                statuses[selected_board],
                                                &mut board_states[selected_board],
                                            );
                                            feedback_buffer =
                                                "Returned to parent board".to_string();
                                        } else {
                                            feedback_buffer =
                                                "Already at the top board".to_string();
                                        }
                                    }
                                    KeyCode::Enter => {
                                        if let Some((idx, entry)) = selected_task_entry_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            &board_states[selected_board],
                                        ) {
                                            match &entry.source {
                                                TaskSource::Path { path, is_dir: true }
                                                    if entry.has_subtasks =>
                                                {
                                                    ensure_board_store(path)?;
                                                    board_stack.push(path.clone());
                                                    selected_board = TODO_BOARD_INDEX;
                                                    for state in board_states.iter_mut() {
                                                        state.select(None);
                                                    }
                                                    select_first_task_if_present_in_board(
                                                        path,
                                                        statuses[selected_board],
                                                        &mut board_states[selected_board],
                                                    );
                                                    feedback_buffer =
                                                        "Opened subtask board".to_string();
                                                }
                                                _ => {
                                                    current_mode = Mode::Edit;
                                                    editing_task_idx = Some(idx + 1);
                                                    task_input = TaskInput::new(
                                                        entry.content.trim_end().to_string(),
                                                    );
                                                }
                                            }
                                        } else {
                                            board_states[selected_board].select(None);
                                            current_mode = Mode::Input;
                                            task_input.reset();
                                        }
                                    }
                                    KeyCode::Char('e') | KeyCode::Char('E') => {
                                        if let Some((idx, entry)) = selected_task_entry_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            &board_states[selected_board],
                                        ) {
                                            current_mode = Mode::Edit;
                                            editing_task_idx = Some(idx + 1);
                                            task_input = TaskInput::new(
                                                entry.content.trim_end().to_string(),
                                            );
                                        } else {
                                            feedback_buffer = "No task selected".to_string();
                                        }
                                    }
                                    KeyCode::Char(' ') => {
                                        if selected_task_index_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            &board_states[selected_board],
                                        )
                                        .is_none()
                                        {
                                            board_states[selected_board].select(None);
                                        }
                                        current_mode = Mode::Input;
                                        task_input.reset();
                                    }
                                    KeyCode::Char('0') => {
                                        backlog_visible = true;
                                        selected_board = BACKLOG_BOARD_INDEX;
                                        for state in board_states.iter_mut() {
                                            state.select(None);
                                        }
                                        select_first_task_if_present_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            &mut board_states[selected_board],
                                        );
                                        feedback_buffer =
                                            "Backlog column shown and focused.".to_string();
                                    }
                                    KeyCode::Char('1') => {
                                        selected_board = TODO_BOARD_INDEX;
                                        for state in board_states.iter_mut() {
                                            state.select(None);
                                        }
                                        select_first_task_if_present_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            &mut board_states[selected_board],
                                        );
                                    }
                                    KeyCode::Char('2') => {
                                        selected_board = 1;
                                        for state in board_states.iter_mut() {
                                            state.select(None);
                                        }
                                        select_first_task_if_present_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            &mut board_states[selected_board],
                                        );
                                    }
                                    KeyCode::Char('3') => {
                                        selected_board = DONE_BOARD_INDEX;
                                        for state in board_states.iter_mut() {
                                            state.select(None);
                                        }
                                        select_first_task_if_present_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            &mut board_states[selected_board],
                                        );
                                    }
                                    KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete => {
                                        if let Some(idx) = selected_task_index_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            &board_states[selected_board],
                                        ) {
                                            let status = statuses[selected_board];
                                            match delete_task_in_board(
                                                &board_dir,
                                                status,
                                                &(idx + 1).to_string(),
                                            ) {
                                                Ok(_) => {
                                                    feedback_buffer = format!(
                                                        "Deleted task {} from {}",
                                                        idx + 1,
                                                        status
                                                    );
                                                    board_states[selected_board].select(
                                                        if idx > 0 { Some(idx - 1) } else { None },
                                                    );
                                                }
                                                Err(e) => feedback_buffer = format!("Error: {}", e),
                                            }
                                        } else {
                                            board_states[selected_board].select(None);
                                            feedback_buffer =
                                                "No task selected to delete".to_string();
                                        }
                                    }
                                    KeyCode::Char('h')
                                    | KeyCode::Char('H')
                                    | KeyCode::Char('?') => {
                                        current_mode = Mode::Help;
                                    }
                                    KeyCode::Up => {
                                        let state = &mut board_states[selected_board];
                                        let tasks = read_tasks_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                        )
                                        .unwrap_or_default();
                                        if !tasks.is_empty() {
                                            let i = state.selected().unwrap_or(0);
                                            if i > 0 {
                                                state.select(Some(i - 1));
                                            } else {
                                                state.select(Some(tasks.len() - 1));
                                            }
                                        }
                                    }
                                    KeyCode::Down => {
                                        let state = &mut board_states[selected_board];
                                        let tasks = read_tasks_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                        )
                                        .unwrap_or_default();
                                        if !tasks.is_empty() {
                                            let i = state.selected().unwrap_or(0);
                                            if i < tasks.len() - 1 {
                                                state.select(Some(i + 1));
                                            } else {
                                                state.select(Some(0));
                                            }
                                        }
                                    }
                                    KeyCode::Left => {
                                        selected_board = wrapped_visible_tui_board(
                                            selected_board,
                                            backlog_visible,
                                            -1,
                                        );
                                        for state in board_states.iter_mut() {
                                            state.select(None);
                                        }
                                        select_first_task_if_present_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            &mut board_states[selected_board],
                                        );
                                    }
                                    KeyCode::Right => {
                                        selected_board = wrapped_visible_tui_board(
                                            selected_board,
                                            backlog_visible,
                                            1,
                                        );
                                        for state in board_states.iter_mut() {
                                            state.select(None);
                                        }
                                        select_first_task_if_present_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            &mut board_states[selected_board],
                                        );
                                    }
                                    KeyCode::Char(c) if c.is_ascii_digit() => {
                                        let new_pos = (c as u8 - b'0') as usize;
                                        if let Some(idx) = selected_task_index_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            &board_states[selected_board],
                                        ) {
                                            if new_pos > 0 {
                                                match reorder_task_in_board(
                                                    &board_dir,
                                                    statuses[selected_board],
                                                    idx,
                                                    new_pos - 1,
                                                ) {
                                                    Ok(_) => {
                                                        feedback_buffer = format!(
                                                            "Reordered task to position {}",
                                                            new_pos
                                                        )
                                                    }
                                                    Err(e) => {
                                                        feedback_buffer = format!("Error: {}", e)
                                                    }
                                                }
                                            }
                                        } else {
                                            board_states[selected_board].select(None);
                                            feedback_buffer = "No task selected".to_string();
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Mode::Reorganize => {
                            current_mode = Mode::View;
                            feedback_buffer = if matches!(key.code, KeyCode::Esc) {
                                "Reorganize cancelled.".to_string()
                            } else if let Some(direction) = tui_task_reorganize_direction(&key) {
                                reorganize_selected_tui_task(
                                    &board_dir,
                                    &statuses,
                                    &mut board_states,
                                    &mut selected_board,
                                    backlog_visible,
                                    direction,
                                )
                            } else {
                                "Reorganize cancelled: expected an Arrow.".to_string()
                            };
                        }
                        Mode::Help => match key.code {
                            KeyCode::Enter
                            | KeyCode::Esc
                            | KeyCode::Char('h')
                            | KeyCode::Char('H')
                            | KeyCode::Char('?') => {
                                current_mode = Mode::View;
                            }
                            _ => {}
                        },
                        Mode::Input => match key.code {
                            KeyCode::Enter => {
                                let task_value = task_input.submitted_value();
                                if !task_value.trim().is_empty() {
                                    match insert_task_at_selection_in_board(
                                        &board_dir,
                                        statuses[selected_board],
                                        &board_states[selected_board],
                                        &task_value,
                                        None,
                                    ) {
                                        Ok(_) => {
                                            feedback_buffer = "Task added successfully.".to_string()
                                        }
                                        Err(e) => feedback_buffer = format!("Error: {}", e),
                                    }
                                } else {
                                    feedback_buffer =
                                        "Task description cannot be empty.".to_string();
                                }
                                current_mode = Mode::View;
                                task_input.reset();
                            }
                            KeyCode::Esc => {
                                current_mode = Mode::View;
                                task_input.reset();
                            }
                            _ => handle_input_key(
                                &mut task_input.input,
                                key,
                                " Add Task: ",
                                input_available_width,
                            ),
                        },
                        Mode::Edit => match key.code {
                            KeyCode::Enter => {
                                let task_value = task_input.submitted_value();
                                if !task_value.trim().is_empty() {
                                    if let Some(idx) = editing_task_idx {
                                        match update_task_in_board(
                                            &board_dir,
                                            statuses[selected_board],
                                            idx,
                                            &task_value,
                                        ) {
                                            Ok(_) => {
                                                feedback_buffer =
                                                    format!("Task {} updated successfully.", idx)
                                            }
                                            Err(e) => feedback_buffer = format!("Error: {}", e),
                                        }
                                    }
                                } else {
                                    feedback_buffer =
                                        "Task description cannot be empty.".to_string();
                                }
                                current_mode = Mode::View;
                                task_input.reset();
                                editing_task_idx = None;
                            }
                            KeyCode::Esc => {
                                current_mode = Mode::View;
                                task_input.reset();
                                editing_task_idx = None;
                            }
                            _ => handle_input_key(
                                &mut task_input.input,
                                key,
                                " Edit Task: ",
                                input_available_width,
                            ),
                        },
                    }
                }
                _ => {}
            }
        }
    }

    Ok(active_root)
}

fn init_tasks(root: &Path, folders: bool) -> Result<()> {
    let tasks_dir = get_tasks_dir(root);
    if !tasks_dir.exists() {
        fs::create_dir_all(&tasks_dir).context("Failed to create tasks directory")?;
        println!("Created directory: {:?}", tasks_dir);
    }

    let directory_mode = folders
        || TASK_STATUSES
            .iter()
            .any(|status| tasks_dir.join(status).is_dir());

    for status in TASK_STATUSES {
        let dir_path = tasks_dir.join(status);
        let file_path = tasks_dir.join(status_filename(status)?);
        if dir_path.is_dir() {
            println!("Directory already exists: {:?}", dir_path);
        } else if file_path.exists() {
            println!("File already exists: {:?}", file_path);
        } else if directory_mode {
            fs::create_dir_all(&dir_path)
                .context(format!("Failed to create directory {:?}", dir_path))?;
            println!("Created directory: {:?}", dir_path);
        } else {
            let mut file = fs::File::create(&file_path)
                .context(format!("Failed to create file {:?}", file_path))?;
            file.write_all(status_header(status)?.as_bytes())
                .context(format!("Failed to write to file {:?}", file_path))?;
            println!("Created file: {:?}", file_path);
        }
    }

    println!("Initialization complete.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct FakeAgentRunner {
        result: AgentRunResult,
        ran_projects: Mutex<Vec<PathBuf>>,
        delay: Duration,
    }

    impl FakeAgentRunner {
        fn new(log_root: &Path, status: &'static str) -> Self {
            Self::with_delay(log_root, status, Duration::ZERO)
        }

        fn with_delay(log_root: &Path, status: &'static str, delay: Duration) -> Self {
            Self {
                result: AgentRunResult {
                    status,
                    exit_code: Some(0),
                    log_dir: log_root.join("runs/test-project"),
                    stdout_path: log_root.join("runs/test-project/test.out"),
                    stderr_path: log_root.join("runs/test-project/test.err"),
                    summary: format!("fake {status} result"),
                    codex_session_id: None,
                },
                ran_projects: Mutex::new(Vec::new()),
                delay,
            }
        }

        fn ran_project_count(&self) -> usize {
            self.ran_projects.lock().unwrap().len()
        }
    }

    impl AgentRunner for FakeAgentRunner {
        fn run_project(
            &self,
            project: &agent_store::AgentProject,
            _task_selection: AgentTaskSelection,
            _shutdown: &AgentShutdownSignal,
        ) -> Result<AgentRunResult> {
            self.ran_projects.lock().unwrap().push(project.path.clone());
            thread::sleep(self.delay);
            Ok(self.result.clone())
        }
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("clt-{}-{}", name, nonce))
    }

    #[test]
    fn tui_requests_unambiguous_reporting_for_every_key() {
        let flags = tui_keyboard_enhancement_flags();

        assert!(flags.contains(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
        assert!(flags.contains(KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES));
        assert!(!flags.contains(KeyboardEnhancementFlags::REPORT_EVENT_TYPES));
    }

    #[test]
    fn initialization_prompt_accepts_y_or_n_without_enter() {
        let key = |code, modifiers| crossterm::event::KeyEvent::new(code, modifiers);

        assert_eq!(
            initialization_prompt_choice(&key(KeyCode::Char('y'), KeyModifiers::NONE)),
            Some(true)
        );
        assert_eq!(
            initialization_prompt_choice(&key(KeyCode::Char('Y'), KeyModifiers::SHIFT)),
            Some(true)
        );
        assert_eq!(
            initialization_prompt_choice(&key(KeyCode::Char('n'), KeyModifiers::NONE)),
            Some(false)
        );
        assert_eq!(
            initialization_prompt_choice(&key(KeyCode::Char('N'), KeyModifiers::SHIFT)),
            Some(false)
        );
        assert_eq!(
            initialization_prompt_choice(&key(KeyCode::Enter, KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            initialization_prompt_choice(&key(KeyCode::Char('y'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn tui_reorder_shortcuts_support_shift_arrows_and_control_previous_next() {
        let key = |code, modifiers| crossterm::event::KeyEvent::new(code, modifiers);

        assert_eq!(
            tui_task_reorder_direction(&key(KeyCode::Up, KeyModifiers::SHIFT)),
            Some(TuiTaskReorderDirection::Up)
        );
        assert_eq!(
            tui_task_reorder_direction(&key(KeyCode::Down, KeyModifiers::SHIFT)),
            Some(TuiTaskReorderDirection::Down)
        );
        assert_eq!(
            tui_task_reorder_direction(&key(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(TuiTaskReorderDirection::Up)
        );
        assert_eq!(
            tui_task_reorder_direction(&key(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(TuiTaskReorderDirection::Down)
        );
        assert_eq!(
            tui_task_reorder_direction(&key(KeyCode::Up, KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            tui_task_reorder_direction(&key(KeyCode::Char('p'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn tui_reorganize_prefix_and_arrows_are_unambiguous() {
        let key = |code, modifiers| crossterm::event::KeyEvent::new(code, modifiers);

        assert!(tui_starts_reorganize_mode(&key(
            KeyCode::Char('r'),
            KeyModifiers::NONE
        )));
        assert!(tui_starts_reorganize_mode(&key(
            KeyCode::Char('R'),
            KeyModifiers::SHIFT
        )));
        assert!(!tui_starts_reorganize_mode(&key(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL
        )));

        for (code, direction) in [
            (KeyCode::Up, TuiTaskReorganizeDirection::Up),
            (KeyCode::Down, TuiTaskReorganizeDirection::Down),
            (KeyCode::Left, TuiTaskReorganizeDirection::Left),
            (KeyCode::Right, TuiTaskReorganizeDirection::Right),
        ] {
            assert_eq!(
                tui_task_reorganize_direction(&key(code, KeyModifiers::NONE)),
                Some(direction)
            );
        }
        assert_eq!(
            tui_task_reorganize_direction(&key(KeyCode::Esc, KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            tui_task_reorganize_direction(&key(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn tui_reorder_action_moves_the_selected_task_and_selection() {
        let root = temp_root("tui-reorder-action");
        add_task(&root, "alpha", None).unwrap();
        add_task(&root, "beta", None).unwrap();
        let board_dir = root.join("tasks");
        let mut state = ListState::default();
        state.select(Some(0));

        let message = reorder_selected_tui_task(
            &board_dir,
            "todo",
            &mut state,
            TuiTaskReorderDirection::Down,
        );

        assert_eq!(message, "Moved task down to position 2");
        assert_eq!(state.selected(), Some(1));
        assert_eq!(
            read_tasks(&root, "todo").unwrap(),
            vec!["- beta", "- alpha"]
        );

        let message =
            reorder_selected_tui_task(&board_dir, "todo", &mut state, TuiTaskReorderDirection::Up);

        assert_eq!(message, "Moved task up to position 1");
        assert_eq!(state.selected(), Some(0));
        assert_eq!(
            read_tasks(&root, "todo").unwrap(),
            vec!["- alpha", "- beta"]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tui_reorganize_action_moves_the_selected_task_between_boards() {
        let root = temp_root("tui-reorganize-horizontal-action");
        add_task(&root, "alpha", None).unwrap();
        let board_dir = root.join("tasks");
        let mut board_states = [
            ListState::default(),
            ListState::default(),
            ListState::default(),
            ListState::default(),
        ];
        let mut selected_board = TODO_BOARD_INDEX;
        board_states[selected_board].select(Some(0));

        let message = reorganize_selected_tui_task(
            &board_dir,
            &TASK_STATUSES,
            &mut board_states,
            &mut selected_board,
            false,
            TuiTaskReorganizeDirection::Right,
        );

        assert_eq!(message, "Moved task to doing");
        assert_eq!(selected_board, 1);
        assert_eq!(board_states[selected_board].selected(), Some(0));
        assert!(read_tasks(&root, "todo").unwrap().is_empty());
        assert_eq!(read_tasks(&root, "doing").unwrap(), vec!["- alpha"]);

        let message = reorganize_selected_tui_task(
            &board_dir,
            &TASK_STATUSES,
            &mut board_states,
            &mut selected_board,
            false,
            TuiTaskReorganizeDirection::Left,
        );

        assert_eq!(message, "Moved task to todo");
        assert_eq!(selected_board, TODO_BOARD_INDEX);
        assert_eq!(board_states[selected_board].selected(), Some(0));
        assert_eq!(read_tasks(&root, "todo").unwrap(), vec!["- alpha"]);
        assert!(read_tasks(&root, "doing").unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    fn tui_agent_project_for_test(id: i64, name: &str) -> TuiAgentProject {
        TuiAgentProject {
            project: agent_store::AgentProject {
                id,
                path: PathBuf::from(format!("/tmp/{name}")),
                name: name.to_string(),
                enabled: true,
                git_mode: AgentGitMode::Off,
                codex_provider: None,
                codex_model: None,
                codex_reasoning_effort: None,
                codex_fast_enabled: false,
                last_scan_at: None,
                last_run_at: None,
                last_success_at: None,
                last_failure_at: None,
                last_blocked_recovery_at: None,
                failure_count: 0,
            },
            scan: AgentProjectScan::empty(),
            runtime_state: TuiAgentRuntimeState::Idle,
        }
    }

    #[test]
    fn tui_agent_panel_restore_keeps_scroll_offset_when_selection_still_exists() {
        let mut panel = TuiAgentPanel {
            projects: vec![
                tui_agent_project_for_test(1, "alpha"),
                tui_agent_project_for_test(2, "beta"),
                tui_agent_project_for_test(3, "gamma"),
                tui_agent_project_for_test(4, "delta"),
            ],
            current_project_registration: None,
            daemon_status: "not-installed".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(2));

        panel.restore_or_normalize_selection(Some(TuiAgentPanelRowIdentity::Project(3)));

        assert_eq!(panel.state.selected(), Some(2));
        assert_eq!(panel.scroll_offset, 0);
    }

    #[test]
    fn tui_agent_panel_selects_nearest_row_after_removal() {
        let mut panel = TuiAgentPanel {
            projects: vec![
                tui_agent_project_for_test(1, "alpha"),
                tui_agent_project_for_test(2, "beta"),
            ],
            current_project_registration: None,
            daemon_status: "not-installed".to_string(),
            state: ListState::default(),
            scroll_offset: 1,
            last_error: None,
        };

        panel.projects.pop();
        panel.select_nearest_row(1);

        assert_eq!(panel.state.selected(), Some(0));
        assert_eq!(panel.scroll_offset, 0);
    }

    #[test]
    fn tui_agent_panel_refresh_selects_a_newly_registered_current_project() {
        let active_root = PathBuf::from("/tmp/beta");
        let mut panel = TuiAgentPanel {
            projects: vec![
                tui_agent_project_for_test(1, "alpha"),
                tui_agent_project_for_test(3, "gamma"),
            ],
            current_project_registration: Some(TuiCurrentProjectRegistration {
                path: active_root.clone(),
                name: "beta".to_string(),
            }),
            daemon_status: "not-installed".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));
        let selected_row = panel.selected_row_identity();
        let snapshot = TuiAgentPanelSnapshot {
            projects: vec![
                tui_agent_project_for_test(1, "alpha"),
                tui_agent_project_for_test(2, "beta"),
                tui_agent_project_for_test(3, "gamma"),
            ],
            daemon_status: "running".to_string(),
        };

        panel.apply_refresh_result(&active_root, selected_row, Ok(snapshot));

        assert_eq!(panel.state.selected(), Some(1));
        assert_eq!(panel.selected_project().unwrap().project.name, "beta");
        assert!(panel.current_project_registration.is_none());
    }

    #[test]
    fn tui_agent_panel_refresh_error_preserves_the_last_snapshot() {
        let mut panel = TuiAgentPanel {
            projects: vec![
                tui_agent_project_for_test(1, "alpha"),
                tui_agent_project_for_test(2, "beta"),
            ],
            current_project_registration: None,
            daemon_status: "running".to_string(),
            state: ListState::default(),
            scroll_offset: 1,
            last_error: None,
        };
        panel.state.select(Some(1));
        let selected_row = panel.selected_row_identity();
        let refresh_error = std::io::Error::other("database locked").into();

        panel.apply_refresh_result(Path::new("/tmp/alpha"), selected_row, Err(refresh_error));

        assert_eq!(panel.projects.len(), 2);
        assert_eq!(panel.daemon_status, "running");
        assert_eq!(panel.state.selected(), Some(1));
        assert_eq!(panel.scroll_offset, 1);
        assert_eq!(
            panel.last_error.as_deref(),
            Some("Agent registry unavailable: database locked")
        );
    }

    #[test]
    fn tui_agent_panel_refresh_error_uses_the_red_console() {
        let panel = TuiAgentPanel {
            projects: vec![tui_agent_project_for_test(1, "alpha")],
            current_project_registration: None,
            daemon_status: "running".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: Some("Agent registry unavailable: database locked".to_string()),
        };
        let log_view = TuiAgentLogView::message("alpha".to_string(), "latest log".to_string());

        let (content, color) =
            tui_console_content(true, &panel, Some(&log_view), "Agent pane instructions");

        assert_eq!(content, "Agent registry unavailable: database locked");
        assert_eq!(color, Color::Red);
    }

    #[test]
    fn tui_kanban_console_displays_an_open_agent_log() {
        let panel = TuiAgentPanel {
            projects: Vec::new(),
            current_project_registration: None,
            daemon_status: "running".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        let log_view = TuiAgentLogView::message("alpha".to_string(), "live output".to_string());

        let (content, color) =
            tui_console_content(false, &panel, Some(&log_view), "Kanban instructions");

        assert_eq!(content, "live output");
        assert_eq!(color, Color::Gray);
    }

    #[test]
    fn tui_agent_panel_selects_the_active_project_by_path() {
        let mut panel = TuiAgentPanel {
            projects: vec![
                tui_agent_project_for_test(1, "alpha"),
                tui_agent_project_for_test(2, "beta"),
                tui_agent_project_for_test(3, "gamma"),
            ],
            current_project_registration: None,
            daemon_status: "not-installed".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));
        let active_path = panel.projects[2].project.path.clone();

        panel.select_project_for_path(&active_path);

        assert_eq!(panel.state.selected(), Some(2));
        assert_eq!(panel.selected_project().unwrap().project.name, "gamma");
    }

    #[test]
    fn tui_agent_panel_selects_the_current_project_registration_by_path() {
        let active_path = PathBuf::from("/tmp/current");
        let mut panel = TuiAgentPanel {
            projects: vec![tui_agent_project_for_test(1, "alpha")],
            current_project_registration: Some(TuiCurrentProjectRegistration {
                path: active_path.clone(),
                name: "current".to_string(),
            }),
            daemon_status: "not-installed".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(1));

        panel.select_project_for_path(&active_path);

        assert_eq!(panel.state.selected(), Some(0));
        assert!(panel.selected_current_project_registration().is_some());
    }

    #[test]
    fn tui_agent_panel_uses_auto_refresh_without_command_panel_wording() {
        assert_eq!(
            tui_agent_panel_refresh_interval(),
            Duration::from_secs(TUI_AGENT_PANEL_REFRESH_SECONDS)
        );
        assert!(!tui_agent_panel_instructions().contains("Auto-refreshes"));
        assert!(!tui_agent_panel_instructions().contains("r refresh"));
        assert!(tui_agent_panel_instructions().contains("m cycles the selected target"));
        assert!(tui_agent_panel_instructions().contains("M opens Models"));
        assert!(tui_agent_panel_instructions().contains("f toggles fast"));
        assert!(tui_agent_panel_instructions().contains("t cycles thinking"));
        assert!(tui_agent_panel_instructions().contains("l shows output"));
        assert!(tui_agent_panel_instructions().contains("g cycles Git off/commit/push"));
        assert!(tui_agent_panel_instructions().contains("Delete removes with confirmation"));
    }

    #[test]
    fn tui_agent_project_removal_requires_confirmation_and_only_unregisters() {
        let root = temp_root("tui-agent-remove");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let mut panel = TuiAgentPanel {
            projects: vec![TuiAgentProject {
                project,
                scan: AgentProjectScan::empty(),
                runtime_state: TuiAgentRuntimeState::Idle,
            }],
            current_project_registration: None,
            daemon_status: "not-installed".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));

        let removal = selected_tui_agent_project_removal(&panel).unwrap();
        assert!(tui_agent_project_removal_prompt(&removal).contains("Press y to confirm"));
        let message =
            remove_tui_agent_project_with_store(&mut panel, &project_root, &removal, &store)
                .unwrap();

        assert_eq!(message, "Removed agent project: project");
        assert!(store.list_projects_blocking().unwrap().is_empty());
        assert!(project_root.exists());
        assert!(panel.projects.is_empty());
        assert!(panel.selected_current_project_registration().is_some());
        assert_eq!(panel.state.selected(), Some(0));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_codex_session_id_parser_reads_the_exec_header() {
        assert_eq!(
            parse_agent_codex_session_id("session id: 019fe7ab-f267-76e3-b82c-d7c5705be8d1")
                .as_deref(),
            Some("019fe7ab-f267-76e3-b82c-d7c5705be8d1")
        );
        assert_eq!(parse_agent_codex_session_id("session id:"), None);
        assert_eq!(parse_agent_codex_session_id("other output"), None);
    }

    #[test]
    fn interactive_codex_resume_command_uses_safe_task_session_settings() {
        let project_root = PathBuf::from("/tmp/project with spaces");
        let mut command = Command::new("codex");

        configure_interactive_codex_resume_command(&mut command, &project_root, "session-123");

        let args: Vec<OsString> = command.get_args().map(OsStr::to_os_string).collect();
        assert_eq!(
            args,
            vec![
                OsString::from("resume"),
                OsString::from("--include-non-interactive"),
                OsString::from("--sandbox"),
                OsString::from("workspace-write"),
                OsString::from("--ask-for-approval"),
                OsString::from("on-request"),
                OsString::from("-C"),
                project_root.as_os_str().to_os_string(),
                OsString::from("session-123"),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(project_root.as_path()));
    }

    #[test]
    fn interactive_codex_resume_accepts_done_and_currently_blocked_tasks() {
        let done = task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 1 },
            "finished task",
            "finished task",
            false,
        );
        let blocked = task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 1 },
            "waiting task",
            "waiting task — BLOCKED 2026-08-13: dependency unavailable",
            false,
        );
        let unblocked = task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 1 },
            "ready again",
            "ready again — BLOCKED 2026-08-12: waiting — UNBLOCKED 2026-08-13: restored",
            false,
        );

        assert!(task_supports_interactive_codex_resume("done", &done));
        assert!(task_supports_interactive_codex_resume("todo", &blocked));
        assert!(task_supports_interactive_codex_resume("doing", &blocked));
        assert!(!task_supports_interactive_codex_resume("todo", &unblocked));
        assert!(!task_supports_interactive_codex_resume("backlog", &blocked));
    }

    #[test]
    fn newly_added_done_task_is_found_even_when_its_content_matches_an_older_task() {
        let before = vec!["duplicate task".to_string(), "older task".to_string()];
        let after = vec![
            task_entry_from_text(
                TaskSource::MarkdownLine { line_index: 1 },
                "duplicate task",
                "duplicate task",
                false,
            ),
            task_entry_from_text(
                TaskSource::MarkdownLine { line_index: 2 },
                "duplicate task",
                "duplicate task",
                false,
            ),
            task_entry_from_text(
                TaskSource::MarkdownLine { line_index: 3 },
                "older task",
                "older task",
                false,
            ),
        ];

        let added = newly_added_task_entry(&before, &after).unwrap();

        assert!(matches!(
            added.source,
            TaskSource::MarkdownLine { line_index: 1 }
        ));
    }

    #[test]
    fn agent_log_console_expands_and_scrolls_to_latest_output() {
        assert_eq!(tui_feedback_console_height(40, 80, "short", false), 3);
        assert_eq!(
            tui_feedback_console_height(40, 12, "a message that wraps", false),
            4
        );
        assert_eq!(
            tui_feedback_console_height(40, 80, "one\ntwo\nthree", false),
            5
        );
        assert_eq!(
            tui_feedback_console_height(40, 12, &"x".repeat(1_000), false),
            20
        );
        assert_eq!(tui_feedback_console_height(40, 80, "short", true), 20);
        assert_eq!(tui_log_scroll_offset("one\ntwo\nthree\nfour", 2), 2);
    }

    #[test]
    fn latest_agent_log_path_uses_newest_file_with_requested_extension() {
        let root = temp_root("agent-latest-log");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("100-000-p1-1.out"), "older").unwrap();
        fs::write(root.join("200-000-p1-1.out"), "newer").unwrap();
        fs::write(root.join("300-000-p1-1.err"), "latest progress").unwrap();

        assert_eq!(
            latest_agent_log_path(&root, "out").unwrap(),
            Some(root.join("200-000-p1-1.out"))
        );
        assert_eq!(
            latest_agent_log_path(&root, "err").unwrap(),
            Some(root.join("300-000-p1-1.err"))
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recorded_agent_output_falls_back_to_stderr_when_stdout_is_empty() {
        let root = temp_root("agent-recorded-output-fallback");
        fs::create_dir_all(&root).unwrap();
        let stdout_path = root.join("run.out");
        let stderr_path = root.join("run.err");
        fs::write(&stdout_path, "").unwrap();
        fs::write(&stderr_path, "agent progress").unwrap();
        let run = agent_store::AgentRunRecord {
            id: 1,
            project_id: 1,
            project_name: "alpha".to_string(),
            project_path: root.clone(),
            status: "success".to_string(),
            started_at: "100".to_string(),
            finished_at: Some("101".to_string()),
            exit_code: Some(0),
            stdout_path: Some(stdout_path.display().to_string()),
            stderr_path: Some(stderr_path.display().to_string()),
            summary: Some("completed".to_string()),
        };

        assert_eq!(preferred_recorded_agent_output_path(run), Some(stderr_path));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn running_agent_log_view_streams_the_current_output_file() {
        let root = temp_root("agent-live-output");
        let state_dir = root.join("state/clt");
        let mut project = tui_agent_project_for_test(1, "alpha");
        project.runtime_state = TuiAgentRuntimeState::Running;
        let log_dir = agent_project_run_log_dir(&state_dir, &project.project).unwrap();
        fs::create_dir_all(&log_dir).unwrap();
        let stdout_path = log_dir.join("200-000-p1-1.out");
        let stderr_path = log_dir.join("200-000-p1-1.err");
        fs::write(&stdout_path, "").unwrap();
        fs::write(&stderr_path, "started\n").unwrap();

        let mut panel = TuiAgentPanel {
            projects: vec![project],
            current_project_registration: None,
            daemon_status: "running".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));

        let mut log_view = selected_tui_agent_log_view_at(&panel, &state_dir)
            .unwrap()
            .unwrap();
        assert!(log_view.is_live);
        assert!(tui_agent_log_title(&log_view).contains("[LIVE]"));
        assert_eq!(log_view.content, "started\n");

        append_agent_log_line(&stderr_path, "still working").unwrap();
        log_view.refresh().unwrap();
        assert!(log_view.content.contains("still working"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn kanban_agent_log_view_uses_the_active_project() {
        let root = temp_root("kanban-agent-log");
        let state_dir = root.join("state/clt");
        let mut alpha = tui_agent_project_for_test(1, "alpha");
        alpha.runtime_state = TuiAgentRuntimeState::Running;
        let active_path = alpha.project.path.clone();
        let log_dir = agent_project_run_log_dir(&state_dir, &alpha.project).unwrap();
        fs::create_dir_all(&log_dir).unwrap();
        fs::write(log_dir.join("200-000-p1-1.err"), "alpha is working\n").unwrap();

        let mut panel = TuiAgentPanel {
            projects: vec![alpha, tui_agent_project_for_test(2, "beta")],
            current_project_registration: None,
            daemon_status: "running".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(1));

        let log_view =
            selected_tui_agent_log_view_for_path_at(&mut panel, &active_path, &state_dir)
                .unwrap()
                .unwrap();

        assert_eq!(panel.selected_project().unwrap().project.name, "alpha");
        assert_eq!(log_view.project_name, "alpha");
        assert_eq!(log_view.content, "alpha is working\n");
        assert!(log_view.is_live);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn open_agent_log_follows_the_highlighted_project() {
        let root = temp_root("agent-log-follows-selection");
        let state_dir = root.join("state/clt");
        let alpha_root = root.join("alpha");
        let beta_root = root.join("beta");
        init_tasks(&alpha_root, false).unwrap();
        init_tasks(&beta_root, false).unwrap();
        let alpha_root = fs::canonicalize(alpha_root).unwrap();
        let beta_root = fs::canonicalize(beta_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&alpha_root, "alpha")
            .unwrap();
        store.register_project_blocking(&beta_root, "beta").unwrap();

        let projects = store.list_projects_blocking().unwrap();
        for project in &projects {
            let stdout_path = root.join(format!("{}.out", project.name));
            fs::write(&stdout_path, format!("{} output", project.name)).unwrap();
            store
                .record_run_outcome_blocking(agent_store::AgentRunOutcome {
                    project_id: project.id,
                    status: "success",
                    started_at: "100",
                    finished_at: Some("100"),
                    exit_code: Some(0),
                    log_dir: Some(root.to_str().unwrap()),
                    stdout_path: Some(stdout_path.to_str().unwrap()),
                    stderr_path: None,
                    summary: Some("completed"),
                    codex_session_id: None,
                    task_content: None,
                })
                .unwrap();
        }

        let mut panel = TuiAgentPanel {
            projects: projects
                .into_iter()
                .map(|project| TuiAgentProject {
                    project,
                    scan: AgentProjectScan::empty(),
                    runtime_state: TuiAgentRuntimeState::Idle,
                })
                .collect(),
            current_project_registration: None,
            daemon_status: "not-installed".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));
        let mut log_view = selected_tui_agent_log_view_at(&panel, &state_dir).unwrap();

        panel.select_next();
        sync_open_tui_agent_log_view_at(&panel, &mut log_view, &state_dir);

        let selected_name = &panel.selected_project().unwrap().project.name;
        let log_view = log_view.unwrap();
        assert_eq!(&log_view.project_name, selected_name);
        assert_eq!(log_view.content, format!("{selected_name} output"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tui_agent_panel_top_status_includes_time_and_daemon_status() {
        let status = format_tui_agent_panel_top_status_with_time("09:41", "running", 3, 2, 1);

        assert!(status.starts_with(" 09:41  daemon status: running"));
        assert!(status.contains("daemon status: running"));
        assert!(status.contains("3 projects"));
        assert!(status.contains("2 enabled"));
        assert!(status.contains("1 running"));
    }

    #[test]
    fn tui_agent_runtime_state_distinguishes_running_from_doing_tasks() {
        let no_leases = Vec::new();
        assert_eq!(
            tui_agent_runtime_state(1, &no_leases),
            TuiAgentRuntimeState::Idle
        );

        let active_lease = agent_store::AgentLeaseRecord {
            project_id: 1,
            project_name: "alpha".to_string(),
            project_path: PathBuf::from("/tmp/alpha"),
            holder: agent_lease_holder(),
            acquired_at: "100".to_string(),
            expires_at: "200".to_string(),
        };
        assert_eq!(
            tui_agent_runtime_state(1, &[active_lease]),
            TuiAgentRuntimeState::Running
        );
    }

    #[test]
    fn current_project_registration_is_present_only_when_active_project_is_unregistered() {
        let active_root = PathBuf::from("/tmp/current");
        let other_project = tui_agent_project_for_test(1, "other");

        let registration = current_project_registration(&active_root, &[other_project]).unwrap();

        assert_eq!(registration.path, active_root);
        assert_eq!(registration.name, "current");

        let mut current_project = tui_agent_project_for_test(2, "current");
        current_project.project.path = registration.path.clone();

        assert!(current_project_registration(&registration.path, &[current_project]).is_none());
    }

    #[test]
    fn tui_agent_panel_selects_current_project_registration_before_projects() {
        let mut panel = TuiAgentPanel {
            projects: vec![
                tui_agent_project_for_test(1, "alpha"),
                tui_agent_project_for_test(2, "beta"),
            ],
            current_project_registration: Some(TuiCurrentProjectRegistration {
                path: PathBuf::from("/tmp/current"),
                name: "current".to_string(),
            }),
            daemon_status: "running".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));

        assert!(panel.selected_current_project_registration().is_some());
        assert!(panel.selected_project().is_none());

        panel.select_next();
        assert_eq!(panel.selected_project().unwrap().project.name, "alpha");

        panel.select_previous();
        assert!(panel.selected_current_project_registration().is_some());
    }

    #[test]
    fn current_project_registration_row_prompts_enter_or_space() {
        let registration = TuiCurrentProjectRegistration {
            path: PathBuf::from("/tmp/current"),
            name: "current".to_string(),
        };

        let project_width =
            agent_project_column_width(&[], Some(&registration), 100, "Enter/Space".len());
        let row = format_current_project_registration_row(
            &registration,
            100,
            project_width,
            "Enter/Space".len(),
        );

        assert!(row.contains("ADD"));
        assert!(row.contains("current"));
        assert!(row.contains("Enter/Space"));
    }

    #[test]
    fn agent_project_table_shows_codex_settings() {
        let mut project = tui_agent_project_for_test(1, "alpha");
        project.scan = AgentProjectScan::pending_with_doing(12, 3);
        project.runtime_state = TuiAgentRuntimeState::Running;
        project.project.codex_model = Some("gpt-5.6-terra".to_string());
        project.project.codex_reasoning_effort = Some("high".to_string());
        project.project.codex_fast_enabled = true;

        let codex_width = agent_codex_column_width(std::slice::from_ref(&project), false);
        let compact_project_width =
            agent_project_column_width(std::slice::from_ref(&project), None, 100, codex_width);
        let wide_project_width =
            agent_project_column_width(std::slice::from_ref(&project), None, 160, codex_width);
        let compact_header =
            format_agent_project_table_header(100, compact_project_width, codex_width);
        let compact_row = format_agent_project_table_row(
            0,
            &project,
            100,
            compact_project_width,
            codex_width,
            false,
        );
        let active_compact_row = format_agent_project_table_row(
            0,
            &project,
            100,
            compact_project_width,
            codex_width,
            true,
        );
        let wide_header = format_agent_project_table_header(160, wide_project_width, codex_width);
        let wide_row = format_agent_project_table_row(
            0,
            &project,
            160,
            wide_project_width,
            codex_width,
            false,
        );

        for header in [&compact_header, &wide_header] {
            assert!(header.find("PROJECT").unwrap() < header.find("TODO").unwrap());
            assert!(header.find("TODO").unwrap() < header.find("DOING").unwrap());
            assert!(header.find("DOING").unwrap() < header.find("CODEX").unwrap());
            assert!(header.find("CODEX").unwrap() < header.find("LAST RUN").unwrap());
            assert!(header.find("LAST RUN").unwrap() < header.find("PATH").unwrap());
            assert!(!header.contains("FAST"));
            assert!(!header.contains("MODEL"));
            assert!(!header.contains("THINK"));
        }
        for row in [&compact_row, &wide_row] {
            assert!(row.contains("5.6-terra/high/fast"));
            assert!(!row.contains("gpt-"));
            assert!(row.contains("RUNNING"));
            assert!(row.contains("/tmp/alpha"));
        }
        assert!(active_compact_row.starts_with("*  1 "));
    }

    #[test]
    fn agent_project_table_abbreviates_all_git_modes() {
        let mut project = tui_agent_project_for_test(1, "alpha");

        for (mode, expected) in [
            (AgentGitMode::Off, "OFF"),
            (AgentGitMode::Commit, "COM"),
            (AgentGitMode::CommitAndPush, "PUSH"),
        ] {
            project.project.git_mode = mode;
            let codex_width = agent_codex_column_width(std::slice::from_ref(&project), false);
            let project_width =
                agent_project_column_width(std::slice::from_ref(&project), None, 100, codex_width);
            let header = format_agent_project_table_header(100, project_width, codex_width);
            let row =
                format_agent_project_table_row(0, &project, 100, project_width, codex_width, false);
            let git_column = header.find("GIT").unwrap();

            assert_eq!(row[git_column..git_column + 4].trim(), expected);
        }
    }

    #[test]
    fn agent_git_mode_cycles_off_commit_push() {
        assert_eq!(AgentGitMode::Off.next(), AgentGitMode::Commit);
        assert_eq!(AgentGitMode::Commit.next(), AgentGitMode::CommitAndPush);
        assert_eq!(AgentGitMode::CommitAndPush.next(), AgentGitMode::Off);
    }

    #[test]
    fn compact_codex_settings_omit_disabled_overrides() {
        assert_eq!(
            compact_agent_codex_settings(None, None, None, false),
            "default"
        );
        assert_eq!(
            compact_agent_codex_settings(None, Some("gpt-5.6"), Some("high"), false),
            "5.6/high"
        );
        assert_eq!(
            compact_agent_codex_settings(None, None, Some("high"), false),
            "high"
        );
        assert_eq!(compact_agent_codex_settings(None, None, None, true), "fast");
        assert_eq!(
            compact_agent_codex_settings(
                Some("openrouter"),
                Some("anthropic/claude-sonnet-4"),
                None,
                false,
            ),
            "openrouter:anthropic/claude-sonnet-4"
        );
    }

    #[test]
    fn codex_column_width_tracks_its_longest_value() {
        let default_project = tui_agent_project_for_test(1, "default");
        assert_eq!(agent_codex_column_width(&[default_project], false), 7);

        let mut configured_project = tui_agent_project_for_test(2, "configured");
        configured_project.project.codex_model = Some("gpt-5.6".to_string());
        configured_project.project.codex_reasoning_effort = Some("high".to_string());
        assert_eq!(agent_codex_column_width(&[configured_project], false), 8);
    }

    #[test]
    fn project_column_prioritizes_the_full_name_over_the_path() {
        let project_name = "customer-facing-analytics-dashboard";
        let project = tui_agent_project_for_test(1, project_name);
        let full_path = project.project.path.display().to_string();
        let codex_width = agent_codex_column_width(std::slice::from_ref(&project), false);
        let project_width =
            agent_project_column_width(std::slice::from_ref(&project), None, 100, codex_width);

        let row =
            format_agent_project_table_row(0, &project, 100, project_width, codex_width, false);

        assert_eq!(project_width, project_name.chars().count());
        assert!(row.contains(project_name));
        assert!(!row.contains(&full_path));
    }

    #[test]
    fn codex_reasoning_setting_cycles_return_to_project_default() {
        assert_eq!(
            AGENT_CODEX_REASONING_EFFORTS,
            ["", "low", "medium", "high", "xhigh", "max", "ultra"]
        );

        let mut reasoning = None;
        for _ in 0..AGENT_CODEX_REASONING_EFFORTS.len() {
            reasoning =
                next_agent_codex_setting(reasoning.as_deref(), &AGENT_CODEX_REASONING_EFFORTS);
        }
        assert_eq!(reasoning, None);
    }

    #[test]
    fn add_task_creates_missing_task_store() {
        let root = temp_root("auto-init");

        let result = add_task(&root, "write from a fresh directory", None);

        assert!(result.is_ok());
        let todo = fs::read_to_string(root.join("tasks/todo.md")).unwrap();
        let doing = fs::read_to_string(root.join("tasks/doing.md")).unwrap();
        let done = fs::read_to_string(root.join("tasks/done.md")).unwrap();
        let backlog = fs::read_to_string(root.join("tasks/backlog.md")).unwrap();

        assert!(todo.contains("# To Do Tasks"));
        assert!(todo.contains("- write from a fresh directory"));
        assert_eq!(doing, "# Doing Tasks\n");
        assert_eq!(done, "# Done Tasks\n");
        assert_eq!(backlog, "# Backlog Tasks\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn init_tasks_can_create_folder_backed_statuses() {
        let root = temp_root("init-folders");

        init_tasks(&root, true).unwrap();

        assert!(root.join("tasks/todo").is_dir());
        assert!(root.join("tasks/doing").is_dir());
        assert!(root.join("tasks/done").is_dir());
        assert!(root.join("tasks/backlog").is_dir());
        assert!(!root.join("tasks/todo.md").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_folder_board_repairs_status_directories_missing_after_clone() {
        let root = temp_root("repair-folder-board");
        let done_dir = root.join("tasks/done");
        fs::create_dir_all(&done_dir).unwrap();
        fs::write(done_dir.join("0001-shipped.md"), "Shipped already.\n").unwrap();

        assert!(ensure_existing_board(&root).unwrap());
        assert!(root.join("tasks/todo").is_dir());
        assert!(root.join("tasks/doing").is_dir());
        assert!(root.join("tasks/done").is_dir());
        assert!(root.join("tasks/backlog").is_dir());
        assert!(done_dir.join("0001-shipped.md").is_file());
        assert!(!root.join("tasks/todo.md").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_markdown_board_repairs_missing_status_files() {
        let root = temp_root("repair-markdown-board");
        let tasks_dir = root.join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(
            tasks_dir.join("done.md"),
            "# Done Tasks\n- shipped already\n",
        )
        .unwrap();

        assert!(ensure_existing_board(&root).unwrap());
        assert_eq!(
            fs::read_to_string(tasks_dir.join("todo.md")).unwrap(),
            "# To Do Tasks\n"
        );
        assert_eq!(
            fs::read_to_string(tasks_dir.join("doing.md")).unwrap(),
            "# Doing Tasks\n"
        );
        assert_eq!(
            fs::read_to_string(tasks_dir.join("done.md")).unwrap(),
            "# Done Tasks\n- shipped already\n"
        );
        assert_eq!(
            fs::read_to_string(tasks_dir.join("backlog.md")).unwrap(),
            "# Backlog Tasks\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tasks_directory_without_status_store_stays_uninitialized() {
        let root = temp_root("unrecognized-tasks-directory");
        let tasks_dir = root.join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("notes.md"), "Not a task board.\n").unwrap();

        assert!(!ensure_existing_board(&root).unwrap());
        assert!(!tasks_dir.join("todo.md").exists());
        assert!(!tasks_dir.join("todo").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ensure_task_store_preserves_existing_files() {
        let root = temp_root("preserve");
        let tasks_dir = root.join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("todo.md"), "# Custom Todo\n- keep me\n").unwrap();

        ensure_task_store(&root).unwrap();

        let todo = fs::read_to_string(tasks_dir.join("todo.md")).unwrap();
        let doing = fs::read_to_string(tasks_dir.join("doing.md")).unwrap();
        let done = fs::read_to_string(tasks_dir.join("done.md")).unwrap();
        let backlog = fs::read_to_string(tasks_dir.join("backlog.md")).unwrap();

        assert_eq!(todo, "# Custom Todo\n- keep me\n");
        assert_eq!(doing, "# Doing Tasks\n");
        assert_eq!(done, "# Done Tasks\n");
        assert_eq!(backlog, "# Backlog Tasks\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expand_tasks_can_expand_one_markdown_status_to_folder() {
        let root = temp_root("expand-one");
        let tasks_dir = root.join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(
            tasks_dir.join("todo.md"),
            "# To Do Tasks\n- first task\n- second task\n",
        )
        .unwrap();
        fs::write(tasks_dir.join("doing.md"), "# Doing Tasks\n").unwrap();
        fs::write(tasks_dir.join("done.md"), "# Done Tasks\n").unwrap();

        expand_tasks(&root, Some("todo".to_string())).unwrap();

        assert!(tasks_dir.join("todo").is_dir());
        assert!(tasks_dir.join("todo.md.bak").exists());
        assert!(tasks_dir.join("doing.md").exists());
        let entries = read_task_entries(&tasks_dir, "todo").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].summary, "first task");
        assert_eq!(entries[1].summary, "second task");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expand_tasks_without_status_expands_all_statuses() {
        let root = temp_root("expand-all");
        add_task(&root, "todo task", None).unwrap();
        move_task(&root, "todo", "doing", "1").unwrap();

        expand_tasks(&root, None).unwrap();

        assert!(root.join("tasks/todo").is_dir());
        assert!(root.join("tasks/doing").is_dir());
        assert!(root.join("tasks/done").is_dir());
        assert!(root.join("tasks/backlog").is_dir());
        assert!(root.join("tasks/todo.md.bak").exists());
        assert!(root.join("tasks/doing.md.bak").exists());
        assert!(root.join("tasks/done.md.bak").exists());
        assert!(root.join("tasks/backlog.md.bak").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_add_task_args_joins_unquoted_description_words() {
        let (description, metadata) = parse_add_task_args(vec![
            "write".to_string(),
            "release".to_string(),
            "notes".to_string(),
        ])
        .unwrap();

        assert_eq!(description, "write release notes");
        assert_eq!(metadata, None);
    }

    #[test]
    fn parse_add_task_args_keeps_tag_like_metadata() {
        let (description, metadata) =
            parse_add_task_args(vec!["Fix login bug".to_string(), "BUG, HIGH".to_string()])
                .unwrap();

        assert_eq!(description, "Fix login bug");
        assert_eq!(metadata, Some("BUG, HIGH".to_string()));
    }

    #[test]
    fn add_command_accepts_multiple_description_words() {
        let cli = Cli::try_parse_from(["clt", "add", "write", "release", "notes"]).unwrap();

        match cli.command {
            Some(Commands::Add { task }) => {
                assert_eq!(task, vec!["write", "release", "notes"]);
            }
            _ => panic!("expected add command"),
        }
    }

    #[test]
    fn no_args_still_parse_to_default_tui_path() {
        let cli = Cli::try_parse_from(["clt"]).unwrap();

        assert!(cli.command.is_none());
    }

    #[test]
    fn shell_init_command_and_cwd_handoff_flag_parse() {
        let cli = Cli::try_parse_from(["clt", "--cwd-file", "/tmp/clt-cwd", "shell-init", "zsh"])
            .unwrap();

        assert_eq!(cli.cwd_file, Some(PathBuf::from("/tmp/clt-cwd")));
        assert!(matches!(
            cli.command,
            Some(Commands::ShellInit {
                shell: ShellKind::Zsh
            })
        ));
    }

    #[test]
    fn shell_init_wraps_clt_and_changes_to_the_returned_directory() {
        for shell in [ShellKind::Bash, ShellKind::Zsh] {
            let script = shell_init_script(shell);

            assert!(script.contains("command clt --cwd-file"));
            assert!(script.contains("builtin cd --"));
            assert!(script.contains("command rm -f --"));
        }
    }

    #[test]
    fn tui_cwd_handoff_writes_the_active_project_path() {
        let cwd_file = temp_root("tui-cwd-file");
        let active_root = temp_root("tui-active-project");

        write_tui_cwd_file(Some(&cwd_file), &active_root).unwrap();

        assert_eq!(
            fs::read(&cwd_file).unwrap(),
            active_root.as_os_str().as_encoded_bytes()
        );
        fs::remove_file(cwd_file).unwrap();
    }

    #[test]
    fn tui_start_state_with_active_board_opens_task_pane() {
        let state = tui_start_state(true);

        assert!(state.active_board);
        assert_eq!(state.current_pane, TuiPane::Tasks);
        assert!(state.feedback_buffer.contains("Kanban View"));
    }

    #[test]
    fn tui_start_state_without_active_board_opens_agent_pane() {
        let state = tui_start_state(false);

        assert!(!state.active_board);
        assert_eq!(state.current_pane, TuiPane::AgentProjects);
        assert_eq!(state.feedback_buffer, TUI_NO_ACTIVE_BOARD_MESSAGE);
    }

    #[test]
    fn tab_toggles_kanban_and_agent_projects_without_cycling_models() {
        assert_eq!(
            tui_pane_after_tab(TuiPane::Tasks, true),
            TuiPane::AgentProjects
        );
        assert_eq!(
            tui_pane_after_tab(TuiPane::AgentProjects, true),
            TuiPane::Tasks
        );
        assert_eq!(
            tui_pane_after_tab(TuiPane::AgentProjects, false),
            TuiPane::AgentProjects
        );
        assert_eq!(
            tui_pane_after_tab(TuiPane::Models, true),
            TuiPane::AgentProjects
        );
        assert_eq!(tui_models_return_pane(TuiPane::Tasks), TuiPane::Tasks);
        assert_eq!(
            tui_models_return_pane(TuiPane::AgentProjects),
            TuiPane::AgentProjects
        );
    }

    #[test]
    fn agent_register_command_accepts_optional_path() {
        let cli = Cli::try_parse_from(["clt", "agent", "register", "."]).unwrap();

        match cli.command {
            Some(Commands::Agent {
                command: AgentCommands::Register { path },
            }) => {
                assert_eq!(path, Some(PathBuf::from(".")));
            }
            _ => panic!("expected agent register command"),
        }

        let cli = Cli::try_parse_from(["clt", "agent", "register"]).unwrap();

        match cli.command {
            Some(Commands::Agent {
                command: AgentCommands::Register { path },
            }) => {
                assert_eq!(path, None);
            }
            _ => panic!("expected agent register command"),
        }
    }

    #[test]
    fn agent_unregister_command_accepts_optional_path() {
        let cli = Cli::try_parse_from(["clt", "agent", "unregister", "/tmp/project"]).unwrap();

        match cli.command {
            Some(Commands::Agent {
                command: AgentCommands::Unregister { path },
            }) => {
                assert_eq!(path, Some(PathBuf::from("/tmp/project")));
            }
            _ => panic!("expected agent unregister command"),
        }
    }

    #[test]
    fn agent_pause_and_resume_commands_accept_optional_path() {
        let pause_cli = Cli::try_parse_from(["clt", "agent", "pause", "/tmp/project"]).unwrap();
        match pause_cli.command {
            Some(Commands::Agent {
                command: AgentCommands::Pause { path },
            }) => {
                assert_eq!(path, Some(PathBuf::from("/tmp/project")));
            }
            _ => panic!("expected agent pause command"),
        }

        let resume_cli = Cli::try_parse_from(["clt", "agent", "resume"]).unwrap();
        match resume_cli.command {
            Some(Commands::Agent {
                command: AgentCommands::Resume { path },
            }) => {
                assert_eq!(path, None);
            }
            _ => panic!("expected agent resume command"),
        }
    }

    #[test]
    fn agent_git_commit_commands_accept_optional_path() {
        let enable_cli =
            Cli::try_parse_from(["clt", "agent", "git-commit", "enable", "/tmp/project"]).unwrap();
        match enable_cli.command {
            Some(Commands::Agent {
                command:
                    AgentCommands::GitCommit {
                        command: AgentGitCommitCommands::Enable { path },
                    },
            }) => {
                assert_eq!(path, Some(PathBuf::from("/tmp/project")));
            }
            _ => panic!("expected agent git-commit enable command"),
        }

        let disable_cli = Cli::try_parse_from(["clt", "agent", "git-commit", "disable"]).unwrap();
        match disable_cli.command {
            Some(Commands::Agent {
                command:
                    AgentCommands::GitCommit {
                        command: AgentGitCommitCommands::Disable { path },
                    },
            }) => {
                assert_eq!(path, None);
            }
            _ => panic!("expected agent git-commit disable command"),
        }

        let push_cli =
            Cli::try_parse_from(["clt", "agent", "git-commit", "push", "/tmp/project"]).unwrap();
        match push_cli.command {
            Some(Commands::Agent {
                command:
                    AgentCommands::GitCommit {
                        command: AgentGitCommitCommands::Push { path },
                    },
            }) => {
                assert_eq!(path, Some(PathBuf::from("/tmp/project")));
            }
            _ => panic!("expected agent git-commit push command"),
        }
    }

    #[test]
    fn agent_run_command_accepts_once_flag() {
        let cli = Cli::try_parse_from(["clt", "agent", "run", "--once"]).unwrap();

        match cli.command {
            Some(Commands::Agent {
                command: AgentCommands::Run { once },
            }) => {
                assert!(once);
            }
            _ => panic!("expected agent run command"),
        }
    }

    #[test]
    fn agent_top_level_subcommands_parse() {
        for subcommand in [
            "projects", "daemon", "start", "stop", "status", "logs", "clean", "pause", "resume",
        ] {
            let cli = Cli::try_parse_from(["clt", "agent", subcommand]).unwrap();

            assert!(matches!(cli.command, Some(Commands::Agent { .. })));
        }
    }

    #[test]
    fn agent_state_dir_uses_explicit_override() {
        let override_dir = PathBuf::from("/tmp/custom-clt-state");

        let state_dir = resolve_agent_state_dir(
            AgentPlatform::Linux,
            Some(override_dir.clone()),
            Some(PathBuf::from("/tmp/xdg-state")),
            Some(PathBuf::from("/home/alex")),
        )
        .unwrap();

        assert_eq!(state_dir, override_dir);
    }

    #[test]
    fn agent_state_dir_uses_macos_application_support() {
        let state_dir = resolve_agent_state_dir(
            AgentPlatform::Macos,
            None,
            None,
            Some(PathBuf::from("/Users/alex")),
        )
        .unwrap();

        assert_eq!(
            state_dir,
            PathBuf::from("/Users/alex/Library/Application Support/clt")
        );
    }

    #[test]
    fn agent_state_dir_uses_xdg_state_home_on_linux() {
        let state_dir = resolve_agent_state_dir(
            AgentPlatform::Linux,
            None,
            Some(PathBuf::from("/var/state/alex")),
            Some(PathBuf::from("/home/alex")),
        )
        .unwrap();

        assert_eq!(state_dir, PathBuf::from("/var/state/alex/clt"));
    }

    #[test]
    fn agent_state_dir_uses_local_state_fallback_on_linux() {
        let state_dir = resolve_agent_state_dir(
            AgentPlatform::Linux,
            None,
            None,
            Some(PathBuf::from("/home/alex")),
        )
        .unwrap();

        assert_eq!(state_dir, PathBuf::from("/home/alex/.local/state/clt"));
    }

    #[test]
    fn agent_timeout_duration_requires_positive_integer() {
        assert_eq!(
            parse_agent_timeout_duration(AGENT_LEASE_TIMEOUT_SECONDS_ENV, "90").unwrap(),
            90
        );
        assert!(parse_agent_timeout_duration(AGENT_LEASE_TIMEOUT_SECONDS_ENV, "0").is_err());
        assert!(parse_agent_timeout_duration(AGENT_LEASE_TIMEOUT_SECONDS_ENV, "soon").is_err());
    }

    #[test]
    fn agent_timestamp_display_is_human_readable_but_preserves_invalid_values() {
        let formatted = format_agent_timestamp("1700000000");

        assert_ne!(formatted, "1700000000");
        assert!(formatted.contains('-'));
        assert!(formatted.contains(':'));
        assert_eq!(format_agent_timestamp("not-a-timestamp"), "not-a-timestamp");
        assert_eq!(format_optional_agent_timestamp(None), "-");
    }

    #[test]
    fn agent_project_summary_formats_readable_block() {
        let project = agent_store::AgentProject {
            id: 7,
            path: PathBuf::from("/tmp/demo-project"),
            name: "demo-project".to_string(),
            enabled: true,
            git_mode: AgentGitMode::Off,
            codex_provider: None,
            codex_model: None,
            codex_reasoning_effort: None,
            codex_fast_enabled: false,
            last_scan_at: None,
            last_run_at: Some("last-run".to_string()),
            last_success_at: Some("last-success".to_string()),
            last_failure_at: None,
            last_blocked_recovery_at: None,
            failure_count: 3,
        };
        let scan = AgentProjectScan::pending(2);

        let summary = format_agent_project_summary(&project, &scan, Some("last-scan"));

        let expected = [
            "7. demo-project [enabled]",
            "   queue     pending: yes todo: 2   todo-ready: 2   todo-blocked: 0   doing: 0   doing-blocked: 0   scan: pending",
            "   activity  last scan: last-scan",
            "             last run:  last-run",
            "             success:   last-success",
            "             failure:   -",
            "             blocked:   -",
            "   settings  git: off",
            "             target: CLT default  reasoning: default  fast: disabled",
            "   health    failures: 3",
            "   path      /tmp/demo-project",
        ]
        .join("\n");
        assert_eq!(summary, expected);
        assert!(!summary.contains("last_scan="));
        assert!(!summary.contains("path="));
    }

    #[test]
    fn agent_poll_interval_duration_requires_positive_integer() {
        assert_eq!(AGENT_DEFAULT_POLL_INTERVAL_SECONDS, 15);
        assert_eq!(
            parse_agent_timeout_duration(AGENT_POLL_INTERVAL_SECONDS_ENV, "15").unwrap(),
            15
        );
        assert!(parse_agent_timeout_duration(AGENT_POLL_INTERVAL_SECONDS_ENV, "0").is_err());
        assert!(parse_agent_timeout_duration(AGENT_POLL_INTERVAL_SECONDS_ENV, "soon").is_err());
    }

    #[test]
    fn agent_max_global_jobs_defaults_to_twelve_and_requires_positive_integer() {
        assert_eq!(AGENT_DEFAULT_MAX_GLOBAL_JOBS, 12);
        assert_eq!(
            parse_agent_positive_usize(AGENT_MAX_GLOBAL_JOBS_ENV, "12").unwrap(),
            12
        );
        assert!(parse_agent_positive_usize(AGENT_MAX_GLOBAL_JOBS_ENV, "0").is_err());
        assert!(parse_agent_positive_usize(AGENT_MAX_GLOBAL_JOBS_ENV, "many").is_err());
    }

    #[test]
    fn agent_bool_env_accepts_common_true_and_false_values() {
        assert!(parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "1").unwrap());
        assert!(parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "true").unwrap());
        assert!(parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "YES").unwrap());
        assert!(!parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "0").unwrap());
        assert!(!parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "false").unwrap());
        assert!(!parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "off").unwrap());
        assert!(parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "sometimes").is_err());
    }

    #[test]
    fn agent_daemon_uses_short_poll_when_no_projects_are_enabled() {
        let empty_pass = AgentSchedulerPass {
            scanned_projects: 0,
            pending_projects: 0,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 0,
            runs_recorded: 0,
        };
        let active_pass = AgentSchedulerPass {
            scanned_projects: 1,
            pending_projects: 0,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 0,
            runs_recorded: 0,
        };

        assert_eq!(
            agent_daemon_sleep_interval(
                &empty_pass,
                Duration::from_secs(AGENT_DEFAULT_POLL_INTERVAL_SECONDS)
            ),
            Duration::from_secs(AGENT_EMPTY_REGISTRY_POLL_INTERVAL_SECONDS)
        );
        assert_eq!(
            agent_daemon_sleep_interval(&empty_pass, Duration::from_secs(2)),
            Duration::from_secs(2)
        );
        assert_eq!(
            agent_daemon_sleep_interval(
                &active_pass,
                Duration::from_secs(AGENT_DEFAULT_POLL_INTERVAL_SECONDS)
            ),
            Duration::from_secs(AGENT_DEFAULT_POLL_INTERVAL_SECONDS)
        );
    }

    #[test]
    fn agent_lease_holder_liveness_detects_current_process_holder() {
        let holder = agent_lease_holder();

        assert_eq!(agent_lease_holder_pid(&holder), Some(std::process::id()));
        assert_eq!(
            agent_lease_holder_liveness(&holder),
            AgentLeaseHolderLiveness::CurrentProcess
        );
        assert_eq!(agent_lease_holder_pid("external-agent"), None);
    }

    #[test]
    fn agent_backoff_duration_requires_positive_integer() {
        assert_eq!(
            parse_agent_timeout_duration(AGENT_FAILURE_BACKOFF_SECONDS_ENV, "300").unwrap(),
            300
        );
        assert_eq!(
            parse_agent_timeout_duration(AGENT_SUCCESS_COOLDOWN_SECONDS_ENV, "5").unwrap(),
            5
        );
        assert!(parse_agent_timeout_duration(AGENT_FAILURE_BACKOFF_SECONDS_ENV, "0").is_err());
        assert!(parse_agent_timeout_duration(AGENT_SUCCESS_COOLDOWN_SECONDS_ENV, "soon").is_err());
    }

    #[test]
    fn agent_project_cooldown_reason_reports_success_and_failure_delays() {
        let mut project = agent_store::AgentProject {
            id: 1,
            path: PathBuf::from("/tmp/project"),
            name: "project".to_string(),
            enabled: true,
            git_mode: AgentGitMode::Off,
            codex_provider: None,
            codex_model: None,
            codex_reasoning_effort: None,
            codex_fast_enabled: false,
            last_scan_at: None,
            last_run_at: None,
            last_success_at: Some("100".to_string()),
            last_failure_at: None,
            last_blocked_recovery_at: None,
            failure_count: 0,
        };

        assert_eq!(
            agent_project_cooldown_reason(
                &project,
                102,
                Duration::from_secs(5),
                Duration::from_secs(300)
            ),
            Some("success cooldown active for 3s".to_string())
        );

        project.last_failure_at = Some("100".to_string());
        project.failure_count = 1;

        assert_eq!(
            agent_project_cooldown_reason(
                &project,
                250,
                Duration::from_secs(5),
                Duration::from_secs(300)
            ),
            Some("failure backoff active for 150s".to_string())
        );
    }

    #[test]
    fn blocked_task_recovery_uses_failure_backoff_without_delaying_todo_work() {
        let project = agent_store::AgentProject {
            id: 1,
            path: PathBuf::from("/tmp/project"),
            name: "project".to_string(),
            enabled: true,
            git_mode: AgentGitMode::Off,
            codex_provider: None,
            codex_model: None,
            codex_reasoning_effort: None,
            codex_fast_enabled: false,
            last_scan_at: None,
            last_run_at: Some("100".to_string()),
            last_success_at: None,
            last_failure_at: None,
            last_blocked_recovery_at: Some("100".to_string()),
            failure_count: 0,
        };

        assert_eq!(
            agent_task_cooldown_reason(
                &project,
                AgentTaskSelection::RecoverBlocked,
                250,
                Duration::from_secs(5),
                Duration::from_secs(300),
            ),
            Some("blocked-task recovery backoff active for 150s".to_string())
        );
        assert_eq!(
            agent_task_cooldown_reason(
                &project,
                AgentTaskSelection::NextTodo,
                250,
                Duration::from_secs(5),
                Duration::from_secs(300),
            ),
            None
        );
        assert_eq!(
            agent_task_cooldown_reason(
                &project,
                AgentTaskSelection::ResumeDoing,
                101,
                Duration::from_secs(5),
                Duration::from_secs(300),
            ),
            None
        );
    }

    #[test]
    fn ensure_agent_state_dir_creates_directory() {
        let root = temp_root("agent-state-dir");
        let state_dir = root.join("state/clt");

        ensure_agent_state_dir_at(&state_dir).unwrap();

        assert!(state_dir.is_dir());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn launchd_plist_runs_agent_daemon_with_state_dir() {
        let service_env = AgentServiceEnvironment {
            codex_path_override: None,
            path: OsString::from("/Users/alex/bin:/usr/bin:/bin"),
        };
        let plist = launchd_plist_content(
            Path::new("/Applications/CLT & Tools/clt"),
            Path::new("/Users/alex/Library/Application Support/clt"),
            &service_env,
        );

        assert!(plist.contains("<string>com.alpinevibrations.clt.agent</string>"));
        assert!(plist.contains("<string>/Applications/CLT &amp; Tools/clt</string>"));
        assert!(plist.contains("<string>agent</string>"));
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<key>CLT_AGENT_STATE_DIR</key>"));
        assert!(plist.contains("<string>/Users/alex/Library/Application Support/clt</string>"));
        assert!(plist.contains("<key>CLT_AGENT_DAEMON_MODE</key>"));
        assert!(plist.contains("<string>service</string>"));
        assert!(!plist.contains("<key>CLT_AGENT_CODEX_PATH</key>"));
        assert!(!plist.lines().any(|line| line.trim() == "\\"));
        assert!(plist.contains("<key>PATH</key>"));
        assert!(plist.contains("<string>/Users/alex/bin:/usr/bin:/bin</string>"));
        assert!(plist.contains(
            "<string>/Users/alex/Library/Application Support/clt/agent-service.out</string>"
        ));
        assert!(plist.contains(
            "<string>/Users/alex/Library/Application Support/clt/agent-service.err</string>"
        ));
    }

    #[test]
    fn launchd_user_domain_uses_non_root_uid() {
        assert_eq!(launchd_user_domain_for_uid(" 501\n").unwrap(), "gui/501");
    }

    #[test]
    fn launchd_user_domain_rejects_root_uid() {
        let err = launchd_user_domain_for_uid("0").unwrap_err().to_string();

        assert!(err.contains("Refusing to manage the macOS launchd user agent as root"));
        assert!(err.contains("without sudo"));
    }

    #[test]
    fn launchd_user_domain_rejects_invalid_uid() {
        let err = launchd_user_domain_for_uid("not-a-uid")
            .unwrap_err()
            .to_string();

        assert!(err.contains("invalid user id"));
    }

    #[test]
    fn systemd_unit_runs_agent_daemon_with_state_dir() {
        let service_env = AgentServiceEnvironment {
            codex_path_override: None,
            path: OsString::from("/home/alex/bin:/usr/bin:/bin"),
        };
        let unit = systemd_unit_content(
            Path::new("/home/alex/bin/clt with spaces"),
            Path::new("/home/alex/.local/state/clt"),
            &service_env,
        );

        assert!(unit.contains("Description=CLT Codex agent"));
        assert!(unit.contains("ExecStart=\"/home/alex/bin/clt with spaces\" agent daemon"));
        assert!(unit.contains("Environment=\"CLT_AGENT_STATE_DIR=/home/alex/.local/state/clt\""));
        assert!(unit.contains("Environment=\"CLT_AGENT_DAEMON_MODE=service\""));
        assert!(!unit.contains("CLT_AGENT_CODEX_PATH"));
        assert!(unit.contains("Environment=\"PATH=/home/alex/bin:/usr/bin:/bin\""));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn service_definitions_preserve_explicit_codex_path_overrides() {
        let launchd_env = AgentServiceEnvironment {
            codex_path_override: Some(PathBuf::from("/Users/alex/bin/Codex & Tools/codex")),
            path: OsString::from("/Users/alex/bin:/usr/bin:/bin"),
        };
        let plist = launchd_plist_content(
            Path::new("/Applications/CLT/clt"),
            Path::new("/Users/alex/Library/Application Support/clt"),
            &launchd_env,
        );
        assert!(plist.contains("<key>CLT_AGENT_CODEX_PATH</key>"));
        assert!(plist.contains("<string>/Users/alex/bin/Codex &amp; Tools/codex</string>"));

        let systemd_env = AgentServiceEnvironment {
            codex_path_override: Some(PathBuf::from("/home/alex/bin/codex with spaces")),
            path: OsString::from("/home/alex/bin:/usr/bin:/bin"),
        };
        let unit = systemd_unit_content(
            Path::new("/home/alex/bin/clt"),
            Path::new("/home/alex/.local/state/clt"),
            &systemd_env,
        );
        assert!(
            unit.contains("Environment=\"CLT_AGENT_CODEX_PATH=/home/alex/bin/codex with spaces\"")
        );
    }

    #[test]
    fn systemd_start_restarts_service_after_reloading_unit() {
        assert_eq!(
            systemd_start_command_args(),
            [
                &["--user", "daemon-reload"][..],
                &["--user", "enable", "clt-agent.service"][..],
                &["--user", "restart", "clt-agent.service"][..],
            ]
        );
    }

    #[test]
    fn systemd_user_runtime_dir_uses_numeric_uid() {
        assert_eq!(
            systemd_user_runtime_dir_for_uid(" 1000\n").unwrap(),
            PathBuf::from("/run/user/1000")
        );
    }

    #[test]
    fn systemd_user_runtime_dir_rejects_invalid_uid() {
        let err = systemd_user_runtime_dir_for_uid("not-a-uid")
            .unwrap_err()
            .to_string();

        assert!(err.contains("invalid user id"));
    }

    #[test]
    fn systemd_user_command_recovers_missing_runtime_dir() {
        let mut command = Command::new("systemctl");

        configure_systemd_user_command_with_runtime_dir(&mut command, None, "1000").unwrap();

        let configured_runtime_dir = command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new(XDG_RUNTIME_DIR_ENV))
            .and_then(|(_, value)| value);
        assert_eq!(configured_runtime_dir, Some(OsStr::new("/run/user/1000")));
    }

    #[test]
    fn systemd_user_command_preserves_inherited_runtime_dir() {
        let mut command = Command::new("systemctl");

        configure_systemd_user_command_with_runtime_dir(
            &mut command,
            Some(OsStr::new("/custom/runtime")),
            "1000",
        )
        .unwrap();

        assert!(
            command
                .get_envs()
                .all(|(key, _)| key != OsStr::new(XDG_RUNTIME_DIR_ENV))
        );
    }

    #[cfg(unix)]
    #[test]
    fn default_service_codex_command_is_left_for_path_lookup() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("agent-codex-path-lookup");
        fs::create_dir_all(&root).unwrap();
        let codex = root.join("codex");
        fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&codex, permissions).unwrap();

        let resolved =
            resolve_agent_codex_path_override_for_service(None, root.as_os_str()).unwrap();

        assert_eq!(resolved, None);
        validate_agent_codex_path(Path::new("codex"), root.as_os_str()).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn packaged_native_codex_binary_is_preferred_over_npm_shim() {
        use std::os::unix::fs::PermissionsExt;

        let Some((platform_package, target_triple, binary_name)) = codex_native_package() else {
            return;
        };

        let root = temp_root("agent-native-codex");
        let codex_js = root.join("node_modules/@openai/codex/bin/codex.js");
        let native_codex = root
            .join("node_modules/@openai")
            .join(platform_package)
            .join("vendor")
            .join(target_triple)
            .join("bin")
            .join(binary_name);
        fs::create_dir_all(codex_js.parent().unwrap()).unwrap();
        fs::create_dir_all(native_codex.parent().unwrap()).unwrap();
        fs::write(&codex_js, "#!/usr/bin/env node\n").unwrap();
        fs::write(&native_codex, "#!/bin/sh\n").unwrap();
        let mut permissions = fs::metadata(&native_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&native_codex, permissions).unwrap();

        assert_eq!(
            prefer_packaged_native_codex_binary(&codex_js),
            fs::canonicalize(&native_codex).unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn systemd_user_unit_path_prefers_xdg_config_home() {
        let unit_path = systemd_user_unit_path(
            Some(PathBuf::from("/tmp/config")),
            Some(PathBuf::from("/home/alex")),
        )
        .unwrap();

        assert_eq!(
            unit_path,
            PathBuf::from("/tmp/config/systemd/user/clt-agent.service")
        );
    }

    #[test]
    fn systemd_user_unit_path_falls_back_to_home_config() {
        let unit_path = systemd_user_unit_path(None, Some(PathBuf::from("/home/alex"))).unwrap();

        assert_eq!(
            unit_path,
            PathBuf::from("/home/alex/.config/systemd/user/clt-agent.service")
        );
    }

    #[test]
    fn agent_store_initializes_database_and_tables() {
        let root = temp_root("agent-store");
        let state_dir = root.join("state/clt");

        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(store.db_path(), state_dir.join(AGENT_DB_FILE));
        assert!(store.db_path().is_file());
        for table in [
            "schema_migrations",
            "projects",
            "runs",
            "leases",
            "daemon_checkins",
            "model_providers",
            "model_targets",
            "agent_settings",
        ] {
            assert!(
                store.table_exists_blocking(table).unwrap(),
                "missing table {table}"
            );
        }

        fs::remove_dir_all(root).unwrap();
    }

    const AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV: &str =
        "CLT_TEST_AGENT_STORE_MULTIPROCESS_STATE_DIR";

    #[test]
    fn agent_store_allows_a_second_process_to_open_the_database() {
        let root = temp_root("agent-store-multiprocess");
        let state_dir = root.join("state/clt");
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        let child_output = Command::new(std::env::current_exe().unwrap())
            .arg("tests::agent_store_multiprocess_child_opens_database")
            .arg("--exact")
            .arg("--nocapture")
            .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
            .output()
            .unwrap();

        assert!(
            child_output.status.success(),
            "second process failed to open agent store: stdout={}; stderr={}",
            String::from_utf8_lossy(&child_output.stdout),
            String::from_utf8_lossy(&child_output.stderr)
        );
        assert!(store.table_exists_blocking("projects").unwrap());

        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_multiprocess_child_opens_database() {
        let Some(state_dir) = std::env::var_os(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV) else {
            return;
        };

        let store = agent_store::TursoAgentStore::open_blocking(Path::new(&state_dir)).unwrap();
        assert!(store.table_exists_blocking("projects").unwrap());
    }

    #[test]
    fn agent_store_migrates_enabled_git_commits_to_commit_mode() {
        let root = temp_root("agent-store-git-mode-migration");
        let state_dir = root.join("state/clt");
        let db_path = state_dir.join(AGENT_DB_FILE);
        fs::create_dir_all(&state_dir).unwrap();

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let db = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
                .build()
                .await
                .unwrap();
            let conn = db.connect().unwrap();
            conn.execute(
                "CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                )",
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "CREATE TABLE projects (
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
                    failure_count INTEGER NOT NULL DEFAULT 0,
                    git_commit_enabled INTEGER NOT NULL DEFAULT 0,
                    codex_model TEXT,
                    codex_reasoning_effort TEXT,
                    codex_fast_enabled INTEGER NOT NULL DEFAULT 0
                )",
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "CREATE TABLE runs (
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
                (),
            )
            .await
            .unwrap();
            for version in 1..=4 {
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at)
                     VALUES (?1, datetime('now'))",
                    [version],
                )
                .await
                .unwrap();
            }
            conn.execute(
                "INSERT INTO projects (
                    path, name, registered_at, updated_at, git_commit_enabled
                 ) VALUES ('/tmp/legacy-project', 'legacy-project', datetime('now'), datetime('now'), 1)",
                (),
            )
            .await
            .unwrap();
        });

        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        assert_eq!(project.git_mode, AgentGitMode::Commit);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_register_is_idempotent_and_lists_projects() {
        let root = temp_root("agent-register");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert!(
            store
                .register_project_blocking(&project_root, "project")
                .unwrap()
        );
        assert!(
            !store
                .register_project_blocking(&project_root, "renamed")
                .unwrap()
        );

        let projects = store.list_projects_blocking().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, project_root);
        assert_eq!(projects[0].name, "renamed");
        assert!(projects[0].enabled);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_register_without_path_uses_default_project_root() {
        let root = temp_root("agent-register-default-root");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = open_agent_store_at(&state_dir).unwrap();

        register_agent_project(&store, None, false, &project_root).unwrap();

        let projects = store.list_projects_blocking().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, project_root);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_unregister_removes_registered_project() {
        let root = temp_root("agent-unregister");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        store
            .register_project_blocking(&project_root, "project")
            .unwrap();

        assert!(store.unregister_project_blocking(&project_root).unwrap());
        assert!(!store.unregister_project_blocking(&project_root).unwrap());
        assert!(store.list_projects_blocking().unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_can_pause_and_resume_registered_project() {
        let root = temp_root("agent-pause-resume");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        store
            .register_project_blocking(&project_root, "project")
            .unwrap();

        assert!(
            store
                .set_project_enabled_for_path_blocking(&project_root, false)
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(!project.enabled);

        assert!(
            store
                .set_project_enabled_blocking(project.id, true)
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(project.enabled);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_persists_all_git_modes_for_registered_project() {
        let root = temp_root("agent-git-commit");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.git_mode, AgentGitMode::Off);

        assert!(
            store
                .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.git_mode, AgentGitMode::Commit);

        assert!(
            store
                .set_project_git_mode_blocking(project.id, AgentGitMode::CommitAndPush)
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.git_mode, AgentGitMode::CommitAndPush);

        assert!(
            store
                .set_project_git_mode_blocking(project.id, AgentGitMode::Off)
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.git_mode, AgentGitMode::Off);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_persists_per_project_codex_settings() {
        let root = temp_root("agent-codex-settings");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.codex_model, None);
        assert_eq!(project.codex_reasoning_effort, None);
        assert!(!project.codex_fast_enabled);

        assert!(
            store
                .set_project_codex_settings_blocking(
                    project.id,
                    Some("openai"),
                    Some("gpt-5.6-terra"),
                    Some("high"),
                    true,
                )
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.codex_provider.as_deref(), Some("openai"));
        assert_eq!(project.codex_model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(project.codex_reasoning_effort.as_deref(), Some("high"));
        assert!(project.codex_fast_enabled);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_persists_provider_catalog_favorites_and_clt_default() {
        let root = temp_root("agent-model-catalog");
        let state_dir = root.join("state/clt");
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        let providers = store.list_model_providers_blocking().unwrap();
        assert_eq!(providers[0].id, "openai");
        assert!(providers[0].enabled);
        let openai_models = store.list_model_targets_blocking(Some("openai")).unwrap();
        assert!(
            openai_models.iter().any(|model| {
                model.model_id == "gpt-5.6-sol" && model.enabled && model.favorite
            })
        );
        assert!(
            !openai_models
                .iter()
                .any(|model| model.model_id == "gpt-5.6")
        );
        let gpt_5_6_models = openai_models
            .iter()
            .filter(|model| model.model_id.starts_with("gpt-5.6"))
            .map(|model| model.model_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            gpt_5_6_models,
            std::collections::BTreeSet::from(["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra"])
        );
        assert_eq!(
            store.model_defaults_blocking().unwrap(),
            agent_store::AgentModelDefaults::default()
        );

        let provider = agent_store::AgentModelProvider {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            env_key: Some("OPENROUTER_API_KEY".to_string()),
            built_in: false,
            enabled: true,
        };
        store.upsert_model_provider_blocking(&provider).unwrap();
        let target = agent_store::AgentModelTarget {
            provider_id: "openrouter".to_string(),
            model_id: "anthropic/claude-sonnet-4".to_string(),
            label: "Claude Sonnet 4".to_string(),
            enabled: true,
            favorite: true,
            reasoning_effort: Some("high".to_string()),
        };
        store.upsert_model_target_blocking(&target).unwrap();
        assert_eq!(
            store
                .model_target_reasoning_blocking(&target.provider_id, &target.model_id)
                .unwrap()
                .as_deref(),
            Some("high")
        );
        store
            .set_model_default_blocking(&target.provider_id, &target.model_id)
            .unwrap();

        assert_eq!(
            store.model_defaults_blocking().unwrap(),
            agent_store::AgentModelDefaults {
                provider_id: Some("openrouter".to_string()),
                model_id: Some("anthropic/claude-sonnet-4".to_string()),
            }
        );
        assert!(
            store
                .list_enabled_model_targets_blocking()
                .unwrap()
                .contains(&target)
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_deletes_provider_models_and_dependent_selections() {
        let root = temp_root("agent-model-provider-delete");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let provider = agent_store::AgentModelProvider {
            id: "local-delete".to_string(),
            name: "Local Delete".to_string(),
            base_url: Some("http://localhost:9090/v1".to_string()),
            env_key: None,
            built_in: false,
            enabled: true,
        };
        let model = agent_store::AgentModelTarget {
            provider_id: provider.id.clone(),
            model_id: "local-model".to_string(),
            label: "Local Model".to_string(),
            enabled: true,
            favorite: true,
            reasoning_effort: None,
        };
        store.upsert_model_provider_blocking(&provider).unwrap();
        store.upsert_model_target_blocking(&model).unwrap();
        store
            .set_project_codex_settings_blocking(
                project.id,
                Some(&provider.id),
                Some(&model.model_id),
                Some("high"),
                true,
            )
            .unwrap();
        store
            .set_model_default_blocking(&provider.id, &model.model_id)
            .unwrap();

        assert!(store.delete_model_provider_blocking(&provider.id).unwrap());
        assert!(!store.delete_model_provider_blocking(&provider.id).unwrap());
        assert!(
            !store
                .list_model_providers_blocking()
                .unwrap()
                .iter()
                .any(|candidate| candidate.id == provider.id)
        );
        assert!(
            store
                .list_model_targets_blocking(Some(&provider.id))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.model_defaults_blocking().unwrap(),
            agent_store::AgentModelDefaults::default()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.codex_provider, None);
        assert_eq!(project.codex_model, None);
        assert_eq!(project.codex_reasoning_effort.as_deref(), Some("high"));
        assert!(project.codex_fast_enabled);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_migrates_plain_gpt_5_6_selections_to_sol() {
        let root = temp_root("agent-model-gpt-5-6-sol-migration");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .upsert_model_target_blocking(&agent_store::AgentModelTarget {
                provider_id: "openai".to_string(),
                model_id: "gpt-5.6".to_string(),
                label: "GPT-5.6".to_string(),
                enabled: true,
                favorite: true,
                reasoning_effort: None,
            })
            .unwrap();
        store
            .upsert_model_target_blocking(&agent_store::AgentModelTarget {
                provider_id: "openai".to_string(),
                model_id: "gpt-5.6-sol".to_string(),
                label: "GPT-5.6 Sol".to_string(),
                enabled: false,
                favorite: false,
                reasoning_effort: None,
            })
            .unwrap();
        store
            .set_project_codex_settings_blocking(
                project.id,
                Some("openai"),
                Some("gpt-5.6"),
                None,
                false,
            )
            .unwrap();
        store
            .set_model_default_blocking("openai", "gpt-5.6")
            .unwrap();
        drop(store);

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let db =
                turso::Builder::new_local(state_dir.join(AGENT_DB_FILE).to_string_lossy().as_ref())
                    .build()
                    .await
                    .unwrap();
            db.connect()
                .unwrap()
                .execute("DELETE FROM schema_migrations WHERE version = 9", ())
                .await
                .unwrap();
        });

        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let models = store.list_model_targets_blocking(Some("openai")).unwrap();
        let sol = models
            .iter()
            .find(|model| model.model_id == "gpt-5.6-sol")
            .unwrap();
        assert!(sol.enabled && sol.favorite);
        assert!(!models.iter().any(|model| model.model_id == "gpt-5.6"));
        assert_eq!(
            store.list_projects_blocking().unwrap()[0]
                .codex_model
                .as_deref(),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            store.model_defaults_blocking().unwrap().model_id.as_deref(),
            Some("gpt-5.6-sol")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tui_models_rows_have_labeled_columns_and_independent_defaults() {
        let provider = agent_store::AgentModelProvider {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            base_url: None,
            env_key: None,
            built_in: true,
            enabled: true,
        };
        assert_eq!(
            tui_models_provider_header()
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["USE", "TYPE", "PROVIDER", "(ID)"]
        );
        assert_eq!(
            tui_models_provider_row(&provider)
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["ON", "BUILTIN", "OpenAI", "(openai)"]
        );

        let model = agent_store::AgentModelTarget {
            provider_id: "openai".to_string(),
            model_id: "gpt-5.6".to_string(),
            label: "GPT-5.6".to_string(),
            enabled: true,
            favorite: true,
            reasoning_effort: None,
        };
        let defaults = agent_store::AgentModelDefaults {
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-5.6".to_string()),
        };
        assert!(tui_model_matches_clt_default(
            &defaults,
            None,
            Some("a-different-codex-model"),
            &model
        ));
        assert!(tui_model_matches_clt_default(
            &agent_store::AgentModelDefaults::default(),
            None,
            Some("gpt-5.6"),
            &model
        ));
        assert!(tui_model_matches_codex_default(
            None,
            Some("gpt-5.6"),
            &model
        ));
        assert_eq!(
            tui_models_model_header()
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["USE", "FAV", "CLT", "CODEX", "THINK", "MODEL", "ID"]
        );
        let row = tui_models_model_row(&model, true, true);
        assert_eq!(
            row.split_whitespace().collect::<Vec<_>>(),
            ["ON", "YES", "YES", "YES", "system", "GPT-5.6", "gpt-5.6"]
        );
        assert!(!row.contains('★'));

        let same_id_on_openrouter = agent_store::AgentModelTarget {
            provider_id: "openrouter".to_string(),
            ..model
        };
        assert!(!tui_model_matches_codex_default(
            None,
            Some("gpt-5.6"),
            &same_id_on_openrouter
        ));
        assert!(tui_model_matches_codex_default(
            Some("openrouter"),
            Some("gpt-5.6"),
            &same_id_on_openrouter
        ));

        let root = temp_root("tui-import-codex-default-model");
        let store = agent_store::TursoAgentStore::open_blocking(&root).unwrap();
        let mut models = store.list_model_targets_blocking(Some("openai")).unwrap();
        include_codex_default_model_target(
            &store,
            "openai",
            None,
            Some("gpt-config-only"),
            &mut models,
        )
        .unwrap();
        include_codex_default_model_target(
            &store,
            "openai",
            None,
            Some("gpt-config-only"),
            &mut models,
        )
        .unwrap();
        assert_eq!(
            models
                .iter()
                .filter(|model| model.model_id == "gpt-config-only")
                .count(),
            1
        );
        assert!(
            store
                .list_model_targets_blocking(Some("openai"))
                .unwrap()
                .iter()
                .any(|model| model.model_id == "gpt-config-only" && model.enabled)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_model_endpoint_helpers_create_stable_ids_and_parse_openai_catalogs() {
        let providers = vec![agent_store::AgentModelProvider {
            id: "my-local-server".to_string(),
            name: "Existing".to_string(),
            base_url: None,
            env_key: None,
            built_in: false,
            enabled: true,
        }];
        assert_eq!(
            custom_provider_id("My Local Server", &providers),
            "my-local-server-2"
        );
        assert_eq!(custom_provider_id("🦙", &[]), "local");
        assert_eq!(
            openai_models_url("http://localhost:11434/v1/"),
            "http://localhost:11434/v1/models"
        );
        assert_eq!(
            openai_models_url("http://localhost:11434/v1/models"),
            "http://localhost:11434/v1/models"
        );
        assert_eq!(
            normalize_openai_api_base_url(" http://127.0.0.1:9090/v1/ ").unwrap(),
            "http://127.0.0.1:9090/v1"
        );
        assert_eq!(
            normalize_openai_api_base_url("http://127.0.0.1:9090").unwrap(),
            "http://127.0.0.1:9090"
        );
        for operation_url in [
            "http://localhost:9090/chat",
            "http://localhost:9090/v1/chat/completions",
            "http://localhost:9090/v1/models",
            "http://localhost:9090/v1/responses",
        ] {
            assert!(
                normalize_openai_api_base_url(operation_url).is_err(),
                "accepted operation URL {operation_url}"
            );
        }

        let model_ids = parse_openai_model_ids(&serde_json::json!({
            "object": "list",
            "data": [
                {"id": "zeta"},
                {"id": " alpha "},
                {"id": "zeta"},
                {"not_an_id": "ignored"}
            ]
        }))
        .unwrap();
        assert_eq!(model_ids, ["alpha", "zeta"]);
        assert!(parse_openai_model_ids(&serde_json::json!({"models": []})).is_err());
        assert!(tui_models_add_provider_menu().contains("[3] Ollama"));
        assert!(tui_models_instructions().contains("r refreshes"));
        assert!(tui_models_instructions().contains("x/Delete to remove"));
        assert!(tui_models_instructions().contains("t cycles model reasoning"));
        let mut input = TuiModelInput::custom_provider();
        if let TuiModelInputKind::CustomProvider { step, .. } = &mut input.kind {
            *step = 1;
        }
        assert!(input.label().contains("usually .../v1"));
        assert!(input.guidance().contains("http://127.0.0.1:9090/v1"));
        assert!(input.guidance().contains("do not include /chat"));
    }

    #[test]
    fn discovered_models_start_off_and_existing_choices_are_preserved() {
        let root = temp_root("discovered-model-choices");
        let store = agent_store::TursoAgentStore::open_blocking(&root).unwrap();
        let provider = agent_store::AgentModelProvider {
            id: "local-test".to_string(),
            name: "Local Test".to_string(),
            base_url: Some("http://localhost:8080/v1".to_string()),
            env_key: None,
            built_in: false,
            enabled: true,
        };
        store.upsert_model_provider_blocking(&provider).unwrap();
        store
            .upsert_model_target_blocking(&agent_store::AgentModelTarget {
                provider_id: provider.id.clone(),
                model_id: "already-selected".to_string(),
                label: "Already Selected".to_string(),
                enabled: true,
                favorite: true,
                reasoning_effort: Some("medium".to_string()),
            })
            .unwrap();

        assert_eq!(
            save_discovered_model_ids(
                &store,
                &provider.id,
                &["already-selected".to_string(), "new-model".to_string()]
            )
            .unwrap(),
            1
        );
        let models = store
            .list_model_targets_blocking(Some(&provider.id))
            .unwrap();
        let existing = models
            .iter()
            .find(|model| model.model_id == "already-selected")
            .unwrap();
        let discovered = models
            .iter()
            .find(|model| model.model_id == "new-model")
            .unwrap();
        assert!(existing.enabled && existing.favorite);
        assert_eq!(existing.reasoning_effort.as_deref(), Some("medium"));
        assert!(!discovered.enabled && !discovered.favorite);
        assert_eq!(discovered.reasoning_effort, None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_discovery_queries_v1_models_with_bearer_auth() {
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to start model discovery test server: {error}"),
        };
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0_u8; 4096];
            let read = std::io::Read::read(&mut stream, &mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..read]);
            assert!(request.starts_with("GET /v1/models HTTP/1.1"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer test-key")
            );
            let body = r#"{"object":"list","data":[{"id":"llama3.2"},{"id":"qwen3"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
        });

        let model_ids =
            discover_openai_model_ids(&format!("http://{address}/v1"), Some("test-key")).unwrap();
        server.join().unwrap();
        assert_eq!(model_ids, ["llama3.2", "qwen3"]);
    }

    #[test]
    fn codex_config_edits_preserve_existing_content_and_create_backup() {
        let root = temp_root("codex-config-model-provider");
        let config_path = root.join("codex/config.toml");
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            "# keep this comment\napproval_policy = \"never\"\n\n[projects.\"/tmp/demo\"]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();

        assert_eq!(
            read_codex_default_config_at(&config_path).unwrap(),
            (None, None),
            "optional top-level defaults must not be indexed as required TOML keys"
        );

        upsert_codex_provider_config_at(
            &config_path,
            "openrouter",
            "OpenRouter",
            "https://openrouter.ai/api/v1",
            Some("OPENROUTER_API_KEY"),
        )
        .unwrap();
        set_codex_default_config_at(
            &config_path,
            "openrouter",
            "anthropic/claude-sonnet-4",
            Some("low"),
        )
        .unwrap();

        let updated = fs::read_to_string(&config_path).unwrap();
        let parsed = updated.parse::<DocumentMut>().unwrap();
        assert!(updated.contains("# keep this comment"));
        assert_eq!(parsed["approval_policy"].as_str(), Some("never"));
        assert_eq!(
            parsed["projects"]["/tmp/demo"]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(
            parsed["model_providers"]["openrouter"]["wire_api"].as_str(),
            Some("responses")
        );
        assert_eq!(
            parsed["model_providers"]["openrouter"]["env_key"].as_str(),
            Some("OPENROUTER_API_KEY")
        );
        assert_eq!(parsed["model_provider"].as_str(), Some("openrouter"));
        assert_eq!(parsed["model"].as_str(), Some("anthropic/claude-sonnet-4"));
        assert_eq!(parsed["model_reasoning_effort"].as_str(), Some("low"));
        assert!(
            config_path
                .parent()
                .unwrap()
                .join("config.toml.clt.bak")
                .is_file()
        );

        assert!(
            !set_codex_model_reasoning_if_default_at(
                &config_path,
                "openrouter",
                "another-model",
                Some("high")
            )
            .unwrap()
        );
        assert!(
            set_codex_model_reasoning_if_default_at(
                &config_path,
                "openrouter",
                "anthropic/claude-sonnet-4",
                Some("high")
            )
            .unwrap()
        );
        let updated_reasoning = fs::read_to_string(&config_path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            updated_reasoning["model_reasoning_effort"].as_str(),
            Some("high")
        );
        assert!(
            set_codex_model_reasoning_if_default_at(
                &config_path,
                "openrouter",
                "anthropic/claude-sonnet-4",
                None
            )
            .unwrap()
        );
        let system_reasoning = fs::read_to_string(&config_path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert!(system_reasoning.get("model_reasoning_effort").is_none());

        set_codex_default_config_at(
            &config_path,
            "openrouter",
            "anthropic/claude-sonnet-4",
            Some("low"),
        )
        .unwrap();

        assert!(remove_codex_provider_config_at(&config_path, "openrouter").unwrap());
        let removed = fs::read_to_string(&config_path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(removed["approval_policy"].as_str(), Some("never"));
        assert_eq!(
            removed["projects"]["/tmp/demo"]["trust_level"].as_str(),
            Some("trusted")
        );
        assert!(removed.get("model_providers").is_none());
        assert!(removed.get("model_provider").is_none());
        assert!(removed.get("model").is_none());
        assert!(removed.get("model_reasoning_effort").is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_config_edit_rejects_invalid_toml_without_overwriting_it() {
        let root = temp_root("codex-config-invalid");
        let config_path = root.join("config.toml");
        let invalid = "model = [not valid";
        fs::create_dir_all(&root).unwrap();
        fs::write(&config_path, invalid).unwrap();

        assert!(
            set_codex_default_config_at(&config_path, "openai", "gpt-5.6", Some("low")).is_err()
        );
        assert_eq!(fs::read_to_string(&config_path).unwrap(), invalid);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scan_detects_markdown_backed_pending_tasks() {
        let root = temp_root("agent-scan-markdown");
        add_task(&root, "agent should run", None).unwrap();
        add_task(&root, "agent is running this", None).unwrap();
        move_task(&root, "todo", "doing", "2").unwrap();

        let scan = scan_agent_project(&root);

        assert_eq!(scan.status, AgentProjectScanStatus::Pending);
        assert_eq!(scan.todo_count, 1);
        assert_eq!(scan.doing_count, 1);
        assert!(has_pending_agent_task(&root));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scan_detects_folder_backed_pending_tasks() {
        let root = temp_root("agent-scan-folder");
        init_tasks(&root, true).unwrap();
        fs::write(
            root.join("tasks/todo/0010-write-agent-runner.md"),
            "Write agent runner. Include tests.\n",
        )
        .unwrap();

        let scan = scan_agent_project(&root);

        assert_eq!(scan.status, AgentProjectScanStatus::Pending);
        assert_eq!(scan.todo_count, 1);
        assert_eq!(scan.doing_count, 0);
        assert!(has_pending_agent_task(&root));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scan_only_reports_blocked_when_every_doing_task_has_a_blocked_note() {
        let root = temp_root("agent-scan-blocked-markdown");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/doing.md"),
            "# Doing Tasks\n- first task — BLOCKED 2026-08-09: waiting on a fixture\n- second task\n",
        )
        .unwrap();

        let partially_blocked = scan_agent_project(&root);

        assert_eq!(partially_blocked.status, AgentProjectScanStatus::Empty);
        assert_eq!(partially_blocked.doing_count, 2);
        assert_eq!(partially_blocked.blocked_doing_count, 1);
        assert!(!partially_blocked.all_actionable_tasks_blocked());

        fs::write(
            root.join("tasks/doing.md"),
            "# Doing Tasks\n- first task — BLOCKED 2026-08-09: waiting on a fixture\n- second task — blocked 2026-08-09: same fixture\n",
        )
        .unwrap();

        let all_blocked = scan_agent_project(&root);

        assert_eq!(all_blocked.status, AgentProjectScanStatus::Blocked);
        assert_eq!(all_blocked.blocked_doing_count, 2);
        assert!(all_blocked.all_actionable_tasks_blocked());
        assert!(all_blocked.has_schedulable_work());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scan_skips_blocked_todos_until_a_newer_note_unblocks_them() {
        let root = temp_root("agent-scan-blocked-todo");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/todo.md"),
            "# Todo Tasks\n- first — BLOCKED 2026-08-09: dependency unavailable\n- second — BLOCKED 2026-08-09: fixture unavailable\n",
        )
        .unwrap();

        let all_blocked = scan_agent_project(&root);

        assert_eq!(all_blocked.status, AgentProjectScanStatus::Blocked);
        assert_eq!(all_blocked.todo_count, 2);
        assert_eq!(all_blocked.blocked_todo_count, 2);
        assert_eq!(all_blocked.available_todo_count(), 0);
        assert!(!all_blocked.has_pending_task());
        assert!(all_blocked.all_actionable_tasks_blocked());

        fs::write(
            root.join("tasks/todo.md"),
            "# Todo Tasks\n- first — BLOCKED 2026-08-09: dependency unavailable — UNBLOCKED 2026-08-09: dependency restored\n- second — BLOCKED 2026-08-09: fixture unavailable\n",
        )
        .unwrap();

        let one_available = scan_agent_project(&root);

        assert_eq!(one_available.status, AgentProjectScanStatus::Pending);
        assert_eq!(one_available.todo_count, 2);
        assert_eq!(one_available.blocked_todo_count, 1);
        assert_eq!(one_available.available_todo_count(), 1);
        assert!(one_available.has_pending_task());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scan_detects_folder_task_blocked_note_headings_without_matching_titles() {
        let root = temp_root("agent-scan-blocked-folder");
        init_tasks(&root, true).unwrap();
        fs::write(
            root.join("tasks/doing/0001-waiting.md"),
            "Waiting task.\n\nBlocked note:\n- BLOCKED 2026-08-09: dependency unavailable.\n",
        )
        .unwrap();

        let blocked = scan_agent_project(&root);

        assert_eq!(blocked.status, AgentProjectScanStatus::Blocked);
        assert_eq!(blocked.blocked_doing_count, 1);

        fs::write(
            root.join("tasks/doing/0001-waiting.md"),
            "Add blocked-task monitoring without a blocker note.\n",
        )
        .unwrap();

        let title_only = scan_agent_project(&root);

        assert_eq!(title_only.status, AgentProjectScanStatus::Empty);
        assert_eq!(title_only.blocked_doing_count, 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scan_does_not_treat_backlog_as_actionable_work() {
        let root = temp_root("agent-scan-backlog");
        add_task(&root, "not ready for an agent", None).unwrap();
        move_task(&root, "todo", "backlog", "1").unwrap();

        let scan = scan_agent_project(&root);

        assert_eq!(scan.status, AgentProjectScanStatus::Empty);
        assert_eq!(scan.todo_count, 0);
        assert_eq!(scan.doing_count, 0);
        assert!(!has_pending_agent_task(&root));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scan_reports_empty_missing_uninitialized_and_unavailable_projects() {
        let root = temp_root("agent-scan-states");
        let empty_project = root.join("empty");
        init_tasks(&empty_project, false).unwrap();

        let uninitialized_project = root.join("uninitialized");
        fs::create_dir_all(&uninitialized_project).unwrap();

        let unreadable_project = root.join("unavailable");
        fs::create_dir_all(unreadable_project.join("tasks")).unwrap();
        fs::write(unreadable_project.join("tasks/todo.md"), [0xff, 0xfe]).unwrap();
        fs::write(unreadable_project.join("tasks/doing.md"), "# Doing Tasks\n").unwrap();
        fs::write(unreadable_project.join("tasks/done.md"), "# Done Tasks\n").unwrap();

        assert_eq!(
            scan_agent_project(&empty_project),
            AgentProjectScan::empty()
        );
        assert_eq!(
            scan_agent_project(&root.join("missing")),
            AgentProjectScan::missing()
        );
        assert_eq!(
            scan_agent_project(&uninitialized_project),
            AgentProjectScan::uninitialized()
        );

        let scan = scan_agent_project(&unreadable_project);
        assert_eq!(scan.todo_count, 0);
        assert!(matches!(
            scan.status,
            AgentProjectScanStatus::Unavailable(_)
        ));
        assert!(!has_pending_agent_task(&unreadable_project));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_records_project_scan_timestamp() {
        let root = temp_root("agent-scan-store");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();

        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.last_scan_at, None);

        let scanned_at = store.record_project_scan_blocking(project.id).unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        assert_eq!(project.last_scan_at, Some(scanned_at));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_records_and_clears_daemon_checkins() {
        let root = temp_root("agent-daemon-checkin-store");
        let state_dir = root.join("state/clt");
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        store
            .record_daemon_checkin_blocking("clt-agent-1", "cli", "100", "110", "155")
            .unwrap();
        let checkins = store.list_daemon_checkins_blocking().unwrap();

        assert_eq!(checkins.len(), 1);
        assert_eq!(checkins[0].holder, "clt-agent-1");
        assert_eq!(checkins[0].mode, "cli");
        assert_eq!(checkins[0].started_at, "100");
        assert_eq!(checkins[0].checked_in_at, "110");
        assert_eq!(checkins[0].expires_at, "155");

        assert!(store.clear_daemon_checkin_blocking("clt-agent-1").unwrap());
        assert!(store.list_daemon_checkins_blocking().unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_daemon_runtime_status_prefers_fresh_checkins() {
        let fresh_cli = agent_store::AgentDaemonCheckin {
            holder: "clt-agent-1".to_string(),
            mode: "cli".to_string(),
            started_at: "100".to_string(),
            checked_in_at: "120".to_string(),
            expires_at: "200".to_string(),
        };
        let fresh_service = agent_store::AgentDaemonCheckin {
            holder: "clt-agent-2".to_string(),
            mode: "service".to_string(),
            started_at: "100".to_string(),
            checked_in_at: "120".to_string(),
            expires_at: "200".to_string(),
        };
        let stale_cli = agent_store::AgentDaemonCheckin {
            expires_at: "150".to_string(),
            ..fresh_cli.clone()
        };

        assert_eq!(
            format_agent_daemon_runtime_status("installed", std::slice::from_ref(&fresh_cli), 160,),
            "cli active"
        );
        assert_eq!(
            format_agent_daemon_runtime_status("running", &[fresh_cli], 160),
            "cli active; service no-check-in"
        );
        assert_eq!(
            format_agent_daemon_runtime_status("running", &[fresh_service], 160),
            "service active"
        );
        assert_eq!(
            format_agent_daemon_runtime_status("installed", &[stale_cli], 160),
            "cli stale"
        );
        assert_eq!(
            format_agent_daemon_runtime_status("installed", &[], 160),
            "service disabled"
        );
        assert_eq!(
            format_agent_daemon_runtime_status("not-installed", &[], 160),
            "disabled"
        );
    }

    #[test]
    fn agent_service_restart_requires_running_service_with_only_stale_service_checkins() {
        let stale_service = agent_store::AgentDaemonCheckin {
            holder: "clt-agent-1".to_string(),
            mode: "service".to_string(),
            started_at: "100".to_string(),
            checked_in_at: "120".to_string(),
            expires_at: "150".to_string(),
        };
        let fresh_service = agent_store::AgentDaemonCheckin {
            holder: "clt-agent-2".to_string(),
            expires_at: "200".to_string(),
            ..stale_service.clone()
        };
        let stale_cli = agent_store::AgentDaemonCheckin {
            mode: "cli".to_string(),
            ..stale_service.clone()
        };

        assert!(agent_service_needs_restart(
            "running",
            std::slice::from_ref(&stale_service),
            160
        ));
        assert!(agent_service_needs_restart(
            "running",
            &[stale_service.clone(), stale_cli.clone()],
            160
        ));
        assert!(!agent_service_needs_restart(
            "installed",
            std::slice::from_ref(&stale_service),
            160
        ));
        assert!(!agent_service_needs_restart(
            "running",
            &[stale_service, fresh_service],
            160
        ));
        assert!(!agent_service_needs_restart("running", &[stale_cli], 160));
    }

    #[test]
    fn agent_run_once_records_pending_projects_up_to_default_capacity() {
        let root = temp_root("agent-run-once");
        let state_dir = root.join("state/clt");
        let first_project = root.join("alpha");
        let second_project = root.join("beta");
        init_tasks(&first_project, false).unwrap();
        init_tasks(&second_project, false).unwrap();
        add_task(&first_project, "first task", None).unwrap();
        add_task(&second_project, "second task", None).unwrap();
        let first_project = fs::canonicalize(first_project).unwrap();
        let second_project = fs::canonicalize(second_project).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&first_project, "alpha")
            .unwrap();
        store
            .register_project_blocking(&second_project, "beta")
            .unwrap();
        let runner = FakeAgentRunner::new(&state_dir, "success");
        drop(store);

        let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(
            pass,
            AgentSchedulerPass {
                scanned_projects: 2,
                pending_projects: 2,
                active_agent_jobs: 0,
                skipped_active_lease: 0,
                deferred_projects: 0,
                runs_started: 2,
                runs_recorded: 2,
            }
        );
        assert_eq!(store.run_count_blocking().unwrap(), 2);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
        assert_eq!(runner.ran_project_count(), 2);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_daemon_loop_repeats_passes_and_respects_success_cooldown() {
        let root = temp_root("agent-daemon-loop");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "daemon task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let runner = Arc::new(FakeAgentRunner::new(&state_dir, "success"));
        drop(store);

        let daemon_runner: Arc<dyn AgentRunner> = runner.clone();
        run_agent_daemon_loop(&state_dir, daemon_runner, Duration::ZERO, Some(2)).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(store.run_count_blocking().unwrap(), 1);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
        assert_eq!(runner.ran_project_count(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_daemon_loop_polls_while_run_is_active_without_reclaiming_own_lease() {
        let root = temp_root("agent-daemon-active-poll");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "long daemon task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let runner = Arc::new(FakeAgentRunner::with_delay(
            &state_dir,
            "success",
            Duration::from_millis(75),
        ));
        drop(store);

        let daemon_runner: Arc<dyn AgentRunner> = runner.clone();
        run_agent_daemon_loop(&state_dir, daemon_runner, Duration::from_millis(5), Some(2))
            .unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(store.run_count_blocking().unwrap(), 1);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
        assert_eq!(runner.ran_project_count(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_daemon_scheduler_returns_acquired_jobs_before_recording_runs() {
        let root = temp_root("agent-daemon-start-job");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "async daemon task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let mut start = run_agent_daemon_scheduler_pass(&state_dir).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(
            start.pass,
            AgentSchedulerPass {
                scanned_projects: 1,
                pending_projects: 1,
                active_agent_jobs: 0,
                skipped_active_lease: 0,
                deferred_projects: 0,
                runs_started: 1,
                runs_recorded: 0,
            }
        );
        assert_eq!(start.jobs.len(), 1);
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        assert_eq!(store.lease_count_blocking().unwrap(), 1);
        drop(store);

        let blocked_by_lease = run_agent_daemon_scheduler_pass(&state_dir).unwrap();
        assert_eq!(blocked_by_lease.jobs.len(), 0);
        assert_eq!(blocked_by_lease.pass.skipped_active_lease, 1);

        let runner = FakeAgentRunner::new(&state_dir, "success");
        let shutdown = new_agent_shutdown_signal();
        let completion = run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(completion.status, "success");
        assert_eq!(store.run_count_blocking().unwrap(), 1);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
        assert_eq!(runner.ran_project_count(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_daemon_scheduler_records_checkin_with_registry_lookup() {
        let root = temp_root("agent-daemon-checkin");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let checkin = AgentDaemonCheckinSource {
            holder: "clt-agent-test".to_string(),
            mode: "cli".to_string(),
            started_at: "100".to_string(),
        };
        let start = run_agent_daemon_scheduler_pass_with_active_and_checkin(
            &state_dir,
            Vec::new(),
            Some(&checkin),
        )
        .unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let checkins = store.list_daemon_checkins_blocking().unwrap();

        assert_eq!(start.pass.scanned_projects, 1);
        assert_eq!(checkins.len(), 1);
        assert_eq!(checkins[0].mode, "cli");
        assert!(daemon_checkin_is_fresh(
            &checkins[0],
            agent_timestamp_seconds()
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_daemon_scheduler_defers_pending_projects_when_active_jobs_fill_capacity() {
        let root = temp_root("agent-daemon-active-capacity");
        let state_dir = root.join("state/clt");
        let first_project = root.join("alpha");
        let second_project = root.join("beta");
        init_tasks(&first_project, false).unwrap();
        init_tasks(&second_project, false).unwrap();
        add_task(&first_project, "active task", None).unwrap();
        add_task(&second_project, "deferred task", None).unwrap();
        let first_project = fs::canonicalize(first_project).unwrap();
        let second_project = fs::canonicalize(second_project).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&first_project, "alpha")
            .unwrap();
        store
            .register_project_blocking(&second_project, "beta")
            .unwrap();
        let projects = store.list_projects_blocking().unwrap();
        let active_project = projects
            .iter()
            .find(|project| project.name == "alpha")
            .unwrap();
        assert!(
            store
                .try_acquire_lease_blocking(
                    active_project.id,
                    "active-daemon-run",
                    "100",
                    "9999999999"
                )
                .unwrap()
        );
        let active_project_id = active_project.id;
        drop(store);

        let start = run_agent_scheduler_pass_with_max_global_jobs(
            &state_dir,
            false,
            &[active_project_id],
            1,
            None,
        )
        .unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(start.jobs.len(), 0);
        assert_eq!(
            start.pass,
            AgentSchedulerPass {
                scanned_projects: 2,
                pending_projects: 1,
                active_agent_jobs: 1,
                skipped_active_lease: 0,
                deferred_projects: 1,
                runs_started: 0,
                runs_recorded: 0,
            }
        );
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        assert_eq!(store.lease_count_blocking().unwrap(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_run_once_skips_disabled_projects() {
        let root = temp_root("agent-run-disabled");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "disabled task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .set_project_enabled_blocking(project.id, false)
            .unwrap();
        let runner = FakeAgentRunner::new(&state_dir, "success");
        drop(store);

        let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(
            pass,
            AgentSchedulerPass {
                scanned_projects: 0,
                pending_projects: 0,
                active_agent_jobs: 0,
                skipped_active_lease: 0,
                deferred_projects: 0,
                runs_started: 0,
                runs_recorded: 0,
            }
        );
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        assert_eq!(runner.ran_project_count(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_run_once_skips_projects_with_active_lease() {
        let root = temp_root("agent-run-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "leased task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "test-holder", "100", "9999999999")
                .unwrap()
        );
        let runner = FakeAgentRunner::new(&state_dir, "success");
        drop(store);

        let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(
            pass,
            AgentSchedulerPass {
                scanned_projects: 1,
                pending_projects: 1,
                active_agent_jobs: 0,
                skipped_active_lease: 1,
                deferred_projects: 0,
                runs_started: 0,
                runs_recorded: 0,
            }
        );
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        assert_eq!(store.lease_count_blocking().unwrap(), 1);
        assert_eq!(runner.ran_project_count(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_run_once_ignores_legacy_local_lock_directory() {
        let root = temp_root("agent-run-local-lock");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "locally locked task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        fs::create_dir(project_root.join(".codex-task-loop.lock")).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let runner = FakeAgentRunner::new(&state_dir, "success");
        drop(store);

        let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(
            pass,
            AgentSchedulerPass {
                scanned_projects: 1,
                pending_projects: 1,
                active_agent_jobs: 0,
                skipped_active_lease: 0,
                deferred_projects: 0,
                runs_started: 1,
                runs_recorded: 1,
            }
        );
        assert_eq!(store.run_count_blocking().unwrap(), 1);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
        assert_eq!(runner.ran_project_count(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn agent_run_once_reclaims_dead_local_process_lease() {
        let root = temp_root("agent-run-dead-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "dead leased task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "clt-agent-4294967295", "100", "9999999999")
                .unwrap()
        );
        let runner = FakeAgentRunner::new(&state_dir, "success");
        drop(store);

        let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(
            pass,
            AgentSchedulerPass {
                scanned_projects: 1,
                pending_projects: 1,
                active_agent_jobs: 0,
                skipped_active_lease: 0,
                deferred_projects: 0,
                runs_started: 1,
                runs_recorded: 1,
            }
        );
        assert_eq!(store.run_count_blocking().unwrap(), 1);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
        assert_eq!(runner.ran_project_count(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn agent_scheduler_resumes_doing_task_after_crashed_process() {
        let root = temp_root("agent-resume-doing-dead-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "interrupted task", None).unwrap();
        move_task(&project_root, "todo", "doing", "1").unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "clt-agent-4294967295", "100", "9999999999")
                .unwrap()
        );
        drop(store);

        let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

        assert_eq!(start.pass.pending_projects, 1);
        assert_eq!(start.pass.runs_started, 1);
        assert_eq!(start.jobs.len(), 1);
        assert_eq!(
            start.jobs[0].task_selection,
            AgentTaskSelection::ResumeDoing
        );

        let runner = FakeAgentRunner::new(&state_dir, "success");
        let shutdown = new_agent_shutdown_signal();
        run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(runner.ran_project_count(), 1);
        assert_eq!(store.run_count_blocking().unwrap(), 1);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scheduler_resumes_doing_task_after_lease_expiry() {
        let root = temp_root("agent-resume-doing-expired-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "expired interrupted task", None).unwrap();
        move_task(&project_root, "todo", "doing", "1").unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "old-holder", "100", "101")
                .unwrap()
        );
        drop(store);

        let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

        assert_eq!(start.pass.pending_projects, 1);
        assert_eq!(start.pass.runs_started, 1);
        assert_eq!(start.jobs.len(), 1);
        assert_eq!(
            start.jobs[0].task_selection,
            AgentTaskSelection::ResumeDoing
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scheduler_monitors_when_all_todo_and_doing_tasks_are_blocked() {
        let root = temp_root("agent-recover-blocked");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- queued — BLOCKED 2026-08-09: credentials unavailable\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- first — BLOCKED 2026-08-09: dependency unavailable\n- second — BLOCKED 2026-08-09: tests cannot start\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

        assert_eq!(start.pass.pending_projects, 1);
        assert_eq!(start.pass.runs_started, 1);
        assert_eq!(start.jobs.len(), 1);
        assert_eq!(
            start.jobs[0].task_selection,
            AgentTaskSelection::RecoverBlocked
        );
        assert_eq!(start.jobs[0].blocked_task_count_before, 3);

        let runner = FakeAgentRunner::new(&state_dir, "success");
        let shutdown = new_agent_shutdown_signal();
        let completion = run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();

        assert_eq!(completion.status, "blocked");
        assert!(
            completion
                .summary
                .contains("left all 3 task(s) blocked across todo and doing")
        );

        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(project.last_blocked_recovery_at.is_some());
        assert_eq!(project.failure_count, 0);
        drop(store);

        let backed_off = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert!(backed_off.jobs.is_empty());
        assert_eq!(backed_off.pass.runs_started, 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scheduler_prefers_todo_work_over_blocked_task_monitoring() {
        let root = temp_root("agent-blocked-prefers-todo");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- queued blocker — BLOCKED 2026-08-09: dependency unavailable\n- ready task\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- waiting task — BLOCKED 2026-08-09: dependency unavailable\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

        assert_eq!(start.jobs.len(), 1);
        assert_eq!(start.jobs[0].task_selection, AgentTaskSelection::NextTodo);
        assert_eq!(start.jobs[0].blocked_task_count_before, 2);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scheduler_leaves_unblocked_doing_work_for_its_owner() {
        let root = temp_root("agent-unblocked-doing");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- queued blocker — BLOCKED 2026-08-09: dependency unavailable\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- manually active task\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

        assert!(start.jobs.is_empty());
        assert_eq!(start.pass.pending_projects, 0);
        assert_eq!(start.pass.runs_started, 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_agent_run_records_its_codex_session_for_the_done_task() {
        let root = temp_root("agent-task-codex-session");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        add_task(&project_root, "resumable task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(start.jobs.len(), 1);
        move_task(&project_root, "todo", "doing", "1").unwrap();
        move_task(&project_root, "doing", "done", "1").unwrap();

        let mut runner = FakeAgentRunner::new(&state_dir, "success");
        runner.result.codex_session_id = Some("session-for-task".to_string());
        let shutdown = new_agent_shutdown_signal();
        run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();

        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store
                .codex_session_for_task_blocking(&project_root, "resumable task")
                .unwrap()
                .as_deref(),
            Some("session-for-task")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn blocked_agent_run_records_its_codex_session_for_the_blocked_task() {
        let root = temp_root("agent-blocked-task-codex-session");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        add_task(&project_root, "resumable blocker", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(start.jobs.len(), 1);
        move_task(&project_root, "todo", "doing", "1").unwrap();
        let blocked_content = "resumable blocker — BLOCKED 2026-08-13: dependency unavailable";
        fs::write(
            project_root.join("tasks/doing.md"),
            format!("# Doing Tasks\n- {blocked_content}\n"),
        )
        .unwrap();

        let mut runner = FakeAgentRunner::new(&state_dir, "success");
        runner.result.codex_session_id = Some("session-for-blocked-task".to_string());
        let shutdown = new_agent_shutdown_signal();
        run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();

        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store
                .codex_session_for_task_blocking(&project_root, blocked_content)
                .unwrap()
                .as_deref(),
            Some("session-for-blocked-task")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_recovers_stale_leases() {
        let root = temp_root("agent-run-stale-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        assert!(
            store
                .try_acquire_lease_blocking(project.id, "old-holder", "100", "101")
                .unwrap()
        );
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "new-holder", "102", "200")
                .unwrap()
        );

        assert_eq!(store.lease_count_blocking().unwrap(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_lists_active_leases() {
        let root = temp_root("agent-active-leases");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        assert!(
            store
                .try_acquire_lease_blocking(project.id, "holder", "100", "200")
                .unwrap()
        );

        let leases = store.list_active_leases_blocking("150").unwrap();

        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].project_id, project.id);
        assert_eq!(leases[0].project_name, "project");
        assert_eq!(leases[0].project_path, project_root);
        assert_eq!(leases[0].holder, "holder");
        assert_eq!(leases[0].acquired_at, "100");
        assert_eq!(leases[0].expires_at, "200");
        assert!(store.list_active_leases_blocking("250").unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_lists_recent_runs() {
        let root = temp_root("agent-recent-runs");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        store
            .record_run_outcome_blocking(agent_store::AgentRunOutcome {
                project_id: project.id,
                status: "success",
                started_at: "100",
                finished_at: Some("101"),
                exit_code: Some(0),
                log_dir: Some("/tmp/logs"),
                stdout_path: Some("/tmp/logs/run.out"),
                stderr_path: Some("/tmp/logs/run.err"),
                summary: Some("completed"),
                codex_session_id: Some("session-123"),
                task_content: Some("completed task"),
            })
            .unwrap();

        let runs = store.list_recent_runs_blocking(5).unwrap();

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].project_id, project.id);
        assert_eq!(runs[0].project_name, "project");
        assert_eq!(runs[0].project_path, project_root);
        assert_eq!(runs[0].status, "success");
        assert_eq!(runs[0].started_at, "100");
        assert_eq!(runs[0].finished_at.as_deref(), Some("101"));
        assert_eq!(runs[0].exit_code, Some(0));
        assert_eq!(runs[0].stdout_path.as_deref(), Some("/tmp/logs/run.out"));
        assert_eq!(runs[0].stderr_path.as_deref(), Some("/tmp/logs/run.err"));
        assert_eq!(runs[0].summary.as_deref(), Some("completed"));
        assert_eq!(
            store
                .codex_session_for_task_blocking(&project_root, "completed task")
                .unwrap()
                .as_deref(),
            Some("session-123")
        );
        assert_eq!(
            store
                .codex_session_for_task_blocking(&project_root, "different task")
                .unwrap(),
            None
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_finds_latest_run_for_selected_project() {
        let root = temp_root("agent-latest-project-run");
        let state_dir = root.join("state/clt");
        let first_root = root.join("first");
        let second_root = root.join("second");
        init_tasks(&first_root, false).unwrap();
        init_tasks(&second_root, false).unwrap();
        let first_root = fs::canonicalize(first_root).unwrap();
        let second_root = fs::canonicalize(second_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&first_root, "first")
            .unwrap();
        store
            .register_project_blocking(&second_root, "second")
            .unwrap();
        let projects = store.list_projects_blocking().unwrap();
        let first = projects
            .iter()
            .find(|project| project.name == "first")
            .unwrap();
        let second = projects
            .iter()
            .find(|project| project.name == "second")
            .unwrap();

        for (project_id, started_at, stdout_path) in [
            (first.id, "100", "/tmp/first-old.out"),
            (first.id, "200", "/tmp/first-new.out"),
            (second.id, "300", "/tmp/second-newest.out"),
        ] {
            store
                .record_run_outcome_blocking(agent_store::AgentRunOutcome {
                    project_id,
                    status: "success",
                    started_at,
                    finished_at: Some(started_at),
                    exit_code: Some(0),
                    log_dir: Some("/tmp"),
                    stdout_path: Some(stdout_path),
                    stderr_path: None,
                    summary: Some("completed"),
                    codex_session_id: None,
                    task_content: None,
                })
                .unwrap();
        }

        let latest = store
            .latest_run_for_project_blocking(first.id)
            .unwrap()
            .unwrap();

        assert_eq!(latest.project_id, first.id);
        assert_eq!(latest.stdout_path.as_deref(), Some("/tmp/first-new.out"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn successful_agent_run_clears_previous_failure_timestamp() {
        let root = temp_root("agent-success-clears-failure");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        store
            .record_run_outcome_blocking(agent_store::AgentRunOutcome {
                project_id: project.id,
                status: "failure",
                started_at: "100",
                finished_at: Some("101"),
                exit_code: None,
                log_dir: None,
                stdout_path: None,
                stderr_path: None,
                summary: Some("failed"),
                codex_session_id: None,
                task_content: None,
            })
            .unwrap();
        store
            .record_run_outcome_blocking(agent_store::AgentRunOutcome {
                project_id: project.id,
                status: "blocked",
                started_at: "150",
                finished_at: Some("151"),
                exit_code: Some(0),
                log_dir: None,
                stdout_path: None,
                stderr_path: None,
                summary: Some("still blocked"),
                codex_session_id: None,
                task_content: None,
            })
            .unwrap();
        store
            .record_run_outcome_blocking(agent_store::AgentRunOutcome {
                project_id: project.id,
                status: "success",
                started_at: "200",
                finished_at: Some("201"),
                exit_code: Some(0),
                log_dir: None,
                stdout_path: None,
                stderr_path: None,
                summary: Some("completed"),
                codex_session_id: None,
                task_content: None,
            })
            .unwrap();

        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.failure_count, 0);
        assert_eq!(project.last_failure_at, None);
        assert_eq!(project.last_blocked_recovery_at, None);
        assert_eq!(project.last_success_at.as_deref(), Some("201"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_clean_resets_failures_and_removes_logs() {
        let root = temp_root("agent-clean-state");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        store
            .record_run_outcome_blocking(agent_store::AgentRunOutcome {
                project_id: project.id,
                status: "failure",
                started_at: "100",
                finished_at: Some("101"),
                exit_code: None,
                log_dir: Some("/tmp/logs"),
                stdout_path: Some("/tmp/logs/run.out"),
                stderr_path: Some("/tmp/logs/run.err"),
                summary: Some("failed"),
                codex_session_id: None,
                task_content: None,
            })
            .unwrap();
        store
            .record_run_outcome_blocking(agent_store::AgentRunOutcome {
                project_id: project.id,
                status: "blocked",
                started_at: "102",
                finished_at: Some("103"),
                exit_code: Some(0),
                log_dir: Some("/tmp/logs"),
                stdout_path: Some("/tmp/logs/run.out"),
                stderr_path: Some("/tmp/logs/run.err"),
                summary: Some("still blocked"),
                codex_session_id: None,
                task_content: None,
            })
            .unwrap();
        store
            .record_daemon_checkin_blocking("stale-daemon", "service", "90", "95", "99")
            .unwrap();
        fs::create_dir_all(state_dir.join("runs/project/run-1")).unwrap();
        fs::write(state_dir.join("runs/project/run-1/stdout.log"), "old run").unwrap();
        fs::write(state_dir.join("agent-service.out"), "service out").unwrap();
        fs::write(state_dir.join("agent-service.err"), "service err").unwrap();

        clean_agent_state(&store, &state_dir).unwrap();

        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.failure_count, 0);
        assert_eq!(project.last_failure_at, None);
        assert_eq!(project.last_blocked_recovery_at, None);
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        assert!(store.list_daemon_checkins_blocking().unwrap().is_empty());
        assert_eq!(fs::read_dir(state_dir.join("runs")).unwrap().count(), 0);
        assert_eq!(
            fs::read_to_string(state_dir.join("agent-service.out")).unwrap(),
            ""
        );
        assert_eq!(
            fs::read_to_string(state_dir.join("agent-service.err")).unwrap(),
            ""
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tail_lines_returns_only_the_requested_suffix() {
        let content = "one\ntwo\nthree\nfour\n";

        assert_eq!(tail_lines(content, 2), vec!["three", "four"]);
        assert_eq!(tail_lines(content, 10), vec!["one", "two", "three", "four"]);
    }

    #[test]
    fn agent_codex_prompt_follows_git_mode() {
        let mut project = agent_store::AgentProject {
            id: 1,
            path: PathBuf::from("/tmp/project"),
            name: "project".to_string(),
            enabled: true,
            git_mode: AgentGitMode::Off,
            codex_provider: None,
            codex_model: None,
            codex_reasoning_effort: None,
            codex_fast_enabled: false,
            last_scan_at: None,
            last_run_at: None,
            last_success_at: None,
            last_failure_at: None,
            last_blocked_recovery_at: None,
            failure_count: 0,
        };

        let base_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, true, true);
        assert!(base_prompt.contains("Use the existing task-management CLI tooling: clt."));
        assert!(base_prompt.contains("Use the $clt-task-management skill"));
        assert!(base_prompt.contains("Pick the next available unblocked TODO"));
        assert!(base_prompt.contains("skip tasks whose latest dated state note is `BLOCKED"));
        assert!(!base_prompt.contains("Embedded skill fallback:"));
        assert!(!base_prompt.contains("Interrupted task recovery:"));
        assert!(!base_prompt.contains("$git-commit"));
        assert!(!base_prompt.contains("Git push:"));

        project.git_mode = AgentGitMode::Commit;
        let commit_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, true, true);
        assert!(commit_prompt.contains("$git-commit"));
        assert!(commit_prompt.contains("Do not commit when there are no tasks left"));
        assert!(!commit_prompt.contains("Git push:"));

        project.git_mode = AgentGitMode::CommitAndPush;
        let push_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, true, true);
        assert!(push_prompt.contains("Git commit:"));
        assert!(push_prompt.contains("Git push:"));
        assert!(
            push_prompt.contains("pull first with the locally configured merge/rebase strategy")
        );
        assert!(!push_prompt.contains("pull-with-rebase"));
        assert!(push_prompt.contains("Never force-push"));

        let recovery_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::ResumeDoing, true, true);
        assert!(recovery_prompt.contains("Interrupted task recovery:"));
        assert!(recovery_prompt.contains("Resume and finish exactly one existing doing task."));
        assert!(recovery_prompt.contains("Do not pick or move a TODO task"));

        let blocked_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::RecoverBlocked, true, true);
        assert!(blocked_prompt.contains("Blocked-task monitor:"));
        assert!(blocked_prompt.contains("every task across todo and doing is currently blocked"));
        assert!(blocked_prompt.contains("Todo does not have to be empty"));
        assert!(blocked_prompt.contains("blocked task from todo or doing"));
        assert!(blocked_prompt.contains("Update the existing task; do not create a replacement"));
        assert!(blocked_prompt.contains("`UNBLOCKED YYYY-MM-DD:` note"));
        assert!(blocked_prompt.contains("Stop after handling that one blocked task"));
        assert!(!blocked_prompt.contains("Interrupted task recovery:"));
    }

    #[test]
    fn agent_codex_prompt_embeds_only_missing_required_skills() {
        let mut project = agent_store::AgentProject {
            id: 1,
            path: PathBuf::from("/tmp/project"),
            name: "project".to_string(),
            enabled: true,
            git_mode: AgentGitMode::Off,
            codex_provider: None,
            codex_model: None,
            codex_reasoning_effort: None,
            codex_fast_enabled: false,
            last_scan_at: None,
            last_run_at: None,
            last_success_at: None,
            last_failure_at: None,
            last_blocked_recovery_at: None,
            failure_count: 0,
        };

        let base_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, false, false);
        assert!(base_prompt.contains("<name>clt-task-management</name>"));
        assert!(base_prompt.contains("# Skills: Project Task Management with `clt`"));
        assert!(!base_prompt.contains("<name>git-commit</name>"));

        project.git_mode = AgentGitMode::Commit;
        let commit_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, true, false);
        assert!(!commit_prompt.contains("<name>clt-task-management</name>"));
        assert!(commit_prompt.contains("<name>git-commit</name>"));
        assert!(commit_prompt.contains("# Git Commit Workflow"));
    }

    #[test]
    fn agent_skill_lookup_uses_frontmatter_name() {
        let root = temp_root("agent-skill-lookup");
        let skills_root = root.join("skills");
        let skill_dir = skills_root.join("custom-folder-name");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: \"git-commit\"\ndescription: Test skill.\n---\n",
        )
        .unwrap();

        assert!(agent_skill_root_contains_name(&skills_root, "git-commit"));
        assert!(!agent_skill_root_contains_name(
            &skills_root,
            "clt-task-management"
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn wait_for_child_with_timeout_emits_heartbeats() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 0.2")
            .spawn()
            .unwrap();
        let mut heartbeats = 0;

        let result = wait_for_child_with_timeout_and_heartbeat(
            &mut child,
            Duration::from_secs(2),
            Duration::from_millis(25),
            |_| {
                heartbeats += 1;
                Ok(())
            },
            || false,
        )
        .unwrap();

        match result {
            AgentProcessWait::Exited(status) => assert!(status.success()),
            AgentProcessWait::TimedOut(_) => panic!("child should not time out"),
            AgentProcessWait::Interrupted(_) => panic!("child should not be interrupted"),
        }
        assert!(heartbeats > 0);
    }

    #[cfg(unix)]
    #[test]
    fn wait_for_child_with_timeout_stops_child_on_shutdown() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("sleep 10");
        configure_agent_child_command(&mut command);
        let mut child = command.spawn().unwrap();
        let shutdown = new_agent_shutdown_signal();
        shutdown.store(true, Ordering::SeqCst);

        let result = wait_for_child_with_timeout_and_heartbeat(
            &mut child,
            Duration::from_secs(10),
            Duration::from_millis(25),
            |_| Ok(()),
            || shutdown.load(Ordering::SeqCst),
        )
        .unwrap();

        match result {
            AgentProcessWait::Interrupted(_) => {}
            AgentProcessWait::Exited(_) => panic!("child should be interrupted"),
            AgentProcessWait::TimedOut(_) => panic!("child should not time out"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn codex_runner_writes_logs_and_treats_no_tasks_left_as_idle() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("agent-codex-runner");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        agent_store::TursoAgentStore::open_blocking(&state_dir)
            .unwrap()
            .set_model_target_reasoning_blocking("openai", "gpt-5.6-terra", Some("low"))
            .unwrap();

        let fake_codex = root.join("fake-codex");
        fs::write(
            &fake_codex,
            "#!/bin/sh\nprintf 'arg=%s\\n' \"$@\" >&2\nprintf 'session id: session-42\\n' >&2\nprintf 'NO_TASKS_LEFT\\n'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();

        let project = agent_store::AgentProject {
            id: 42,
            path: project_root.clone(),
            name: "Project With Spaces".to_string(),
            enabled: true,
            git_mode: AgentGitMode::Off,
            codex_provider: Some("openai".to_string()),
            codex_model: Some("gpt-5.6-terra".to_string()),
            codex_reasoning_effort: Some("high".to_string()),
            codex_fast_enabled: true,
            last_scan_at: None,
            last_run_at: None,
            last_success_at: None,
            last_failure_at: None,
            last_blocked_recovery_at: None,
            failure_count: 0,
        };
        let runner =
            CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);
        let shutdown = new_agent_shutdown_signal();

        let result = runner
            .run_project(&project, AgentTaskSelection::NextTodo, &shutdown)
            .unwrap();

        assert_eq!(result.status, "idle");
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.codex_session_id.as_deref(), Some("session-42"));
        assert!(result.log_dir.starts_with(state_dir.join("runs")));
        assert!(
            fs::read_to_string(&result.stdout_path)
                .unwrap()
                .contains(AGENT_NO_TASKS_LEFT_MARKER)
        );
        let stderr = fs::read_to_string(&result.stderr_path).unwrap();
        assert!(stderr.contains(
            "arg=--sandbox\narg=danger-full-access\narg=--ask-for-approval\narg=never\narg=--config\narg=model_provider=\"openai\"\narg=--model\narg=gpt-5.6-terra\narg=--config\narg=model_reasoning_effort=\"high\"\narg=--enable\narg=fast_mode\narg=--config\narg=service_tier=\"fast\"\narg=exec\narg=-C\n"
        ));
        assert!(!stderr.contains("arg=model_reasoning_effort=\"low\"\n"));
        assert!(stderr.contains(&format!("arg={}\n", project_root.display())));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn codex_runner_resolves_the_latest_clt_default_for_new_runs() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("agent-codex-runner-default");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let store = agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .upsert_model_provider_blocking(&agent_store::AgentModelProvider {
                id: "openrouter".to_string(),
                name: "OpenRouter".to_string(),
                base_url: Some("https://openrouter.ai/api/v1".to_string()),
                env_key: Some("OPENROUTER_API_KEY".to_string()),
                built_in: false,
                enabled: true,
            })
            .unwrap();
        store
            .upsert_model_target_blocking(&agent_store::AgentModelTarget {
                provider_id: "openrouter".to_string(),
                model_id: "anthropic/claude-sonnet-4".to_string(),
                label: "Claude Sonnet 4".to_string(),
                enabled: true,
                favorite: true,
                reasoning_effort: Some("high".to_string()),
            })
            .unwrap();
        store
            .set_model_default_blocking("openrouter", "anthropic/claude-sonnet-4")
            .unwrap();

        let fake_codex = root.join("fake-codex");
        fs::write(
            &fake_codex,
            "#!/bin/sh\nprintf 'arg=%s\\n' \"$@\" >&2\nprintf 'NO_TASKS_LEFT\\n'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();

        let project = agent_store::AgentProject {
            id: 44,
            path: project_root,
            name: "Default Target Project".to_string(),
            enabled: true,
            git_mode: AgentGitMode::Off,
            codex_provider: None,
            codex_model: None,
            codex_reasoning_effort: None,
            codex_fast_enabled: false,
            last_scan_at: None,
            last_run_at: None,
            last_success_at: None,
            last_failure_at: None,
            last_blocked_recovery_at: None,
            failure_count: 0,
        };
        let runner = CodexAgentRunner::with_command(state_dir, Duration::from_secs(5), fake_codex);
        let result = runner
            .run_project(
                &project,
                AgentTaskSelection::NextTodo,
                &new_agent_shutdown_signal(),
            )
            .unwrap();
        let stderr = fs::read_to_string(result.stderr_path).unwrap();

        assert!(stderr.contains("arg=model_provider=\"openrouter\"\n"));
        assert!(stderr.contains("arg=--model\narg=anthropic/claude-sonnet-4\n"));
        assert!(stderr.contains("arg=model_reasoning_effort=\"high\"\n"));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn codex_runner_marks_shutdown_as_interrupted() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("agent-codex-runner-shutdown");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        agent_store::TursoAgentStore::open_blocking(&state_dir).unwrap();

        let fake_codex = root.join("fake-codex");
        let started_marker = root.join("fake-codex-started");
        fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\nprintf 'arg=%s\\n' \"$@\" >&2\nprintf 'started\\n'\nprintf 'started\\n' > \"{}\"\nsleep 10\n",
                started_marker.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();

        let project = agent_store::AgentProject {
            id: 43,
            path: project_root,
            name: "Shutdown Project".to_string(),
            enabled: true,
            git_mode: AgentGitMode::Off,
            codex_provider: None,
            codex_model: None,
            codex_reasoning_effort: None,
            codex_fast_enabled: false,
            last_scan_at: None,
            last_run_at: None,
            last_success_at: None,
            last_failure_at: None,
            last_blocked_recovery_at: None,
            failure_count: 0,
        };
        let runner =
            CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(10), fake_codex);
        let shutdown = new_agent_shutdown_signal();
        let shutdown_thread_signal = Arc::clone(&shutdown);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !started_marker.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            shutdown_thread_signal.store(true, Ordering::SeqCst);
        });

        let result = runner
            .run_project(&project, AgentTaskSelection::NextTodo, &shutdown)
            .unwrap();

        assert_eq!(result.status, "interrupted");
        let stderr = fs::read_to_string(&result.stderr_path).unwrap();
        assert!(stderr.contains("arg=--disable\narg=fast_mode\narg=exec\n"));
        assert!(stderr.contains("agent is shutting down"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_display_name_uses_folder_name_with_root_fallback() {
        assert_eq!(
            project_display_name(Path::new("/Users/pro/code/lls/clt")),
            "clt"
        );
        assert_eq!(project_display_name(Path::new("/")), "/");
    }

    #[test]
    fn app_title_includes_project_name() {
        assert_eq!(
            app_title(Path::new("/Users/pro/code/lls/example")),
            "clt | example"
        );
    }

    #[test]
    fn tui_console_block_right_aligns_the_backlog_status() {
        use ratatui::{buffer::Buffer, widgets::Widget};

        let area = Rect::new(0, 0, 48, 3);
        let mut buffer = Buffer::empty(area);

        tui_console_block("clt Console", Some(" Backlog: 2 [B] ")).render(area, &mut buffer);

        let top_border = (0..area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<Vec<_>>()
            .join("");
        assert!(top_border.starts_with("┌clt Console"));
        assert!(top_border.ends_with(" Backlog: 2 [B] ┐"));
    }

    #[test]
    fn move_task_writes_destination_and_removes_source() {
        let root = temp_root("move");

        add_task(&root, "ship the fix", None).unwrap();
        move_task(&root, "todo", "doing", "1").unwrap();

        let todo = fs::read_to_string(root.join("tasks/todo.md")).unwrap();
        let doing = fs::read_to_string(root.join("tasks/doing.md")).unwrap();

        assert_eq!(todo, "# To Do Tasks\n");
        assert_eq!(doing, "# Doing Tasks\n- ship the fix\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn move_task_supports_backlog_as_a_status() {
        let root = temp_root("move-backlog");

        add_task(&root, "consider this later", None).unwrap();
        move_task(&root, "todo", "backlog", "1").unwrap();

        assert!(read_tasks(&root, "todo").unwrap().is_empty());
        assert_eq!(
            read_tasks(&root, "backlog").unwrap(),
            vec!["- consider this later"]
        );
        assert_eq!(normalize_status_arg("0").unwrap(), "backlog");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tui_backlog_visibility_preserves_existing_board_order() {
        assert_eq!(visible_tui_board_indices(false), &[0, 1, 2]);
        assert_eq!(visible_tui_board_indices(true), &[3, 0, 1, 2]);
        assert_eq!(adjacent_visible_tui_board(0, false, -1), None);
        assert_eq!(adjacent_visible_tui_board(0, true, -1), Some(3));
        assert_eq!(adjacent_visible_tui_board(3, true, 1), Some(0));
        assert_eq!(wrapped_visible_tui_board(0, false, -1), 2);
        assert_eq!(wrapped_visible_tui_board(0, true, -1), 3);
    }

    #[test]
    fn tui_backlog_action_moves_the_selected_task_while_column_is_hidden() {
        let root = temp_root("tui-move-backlog-hidden");
        add_task(&root, "first", None).unwrap();
        add_task(&root, "move me", None).unwrap();
        let board_dir = root.join("tasks");
        let mut states: [ListState; 4] = std::array::from_fn(|_| ListState::default());
        states[TODO_BOARD_INDEX].select(Some(1));
        let mut selected_board = TODO_BOARD_INDEX;

        let message = move_selected_tui_task_to_backlog(
            &board_dir,
            &TASK_STATUSES,
            &mut states,
            &mut selected_board,
            false,
        )
        .unwrap();

        assert_eq!(message, "Moved task to backlog");
        assert_eq!(selected_board, TODO_BOARD_INDEX);
        assert_eq!(states[TODO_BOARD_INDEX].selected(), Some(0));
        assert_eq!(read_tasks(&root, "todo").unwrap(), vec!["- first"]);
        assert_eq!(read_tasks(&root, "backlog").unwrap(), vec!["- move me"]);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tui_archive_action_moves_the_selected_task() {
        let root = temp_root("tui-move-archive");
        add_task(&root, "keep this active", None).unwrap();
        add_task(&root, "archive me", None).unwrap();
        let board_dir = root.join("tasks");
        let mut states: [ListState; 4] = std::array::from_fn(|_| ListState::default());
        states[TODO_BOARD_INDEX].select(Some(1));

        let message = move_selected_tui_task_to_archive(
            &board_dir,
            &TASK_STATUSES,
            &mut states,
            TODO_BOARD_INDEX,
        )
        .unwrap();

        assert_eq!(message, "Moved task to archive");
        assert_eq!(states[TODO_BOARD_INDEX].selected(), Some(0));
        assert_eq!(
            read_tasks(&root, "todo").unwrap(),
            vec!["- keep this active"]
        );
        let archived = read_archived_task_entries(&board_dir).unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].summary, "archive me");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hiding_focused_backlog_returns_focus_to_todo() {
        let root = temp_root("tui-hide-backlog");
        add_task(&root, "todo task", None).unwrap();
        let board_dir = root.join("tasks");
        let mut states: [ListState; 4] = std::array::from_fn(|_| ListState::default());
        states[BACKLOG_BOARD_INDEX].select(Some(0));
        let mut selected_board = BACKLOG_BOARD_INDEX;
        let mut backlog_visible = true;

        let message = toggle_tui_backlog_column(
            &board_dir,
            &mut states,
            &mut selected_board,
            &mut backlog_visible,
        );

        assert_eq!(message, "Backlog column hidden. Press B to show it.");
        assert!(!backlog_visible);
        assert_eq!(selected_board, TODO_BOARD_INDEX);
        assert_eq!(states[TODO_BOARD_INDEX].selected(), Some(0));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn move_task_to_done_adds_to_top() {
        let root = temp_root("move-done-top");

        add_task(&root, "older done task", None).unwrap();
        add_task(&root, "newer done task", None).unwrap();
        move_task(&root, "todo", "done", "1").unwrap();
        move_task(&root, "todo", "done", "1").unwrap();

        let done = fs::read_to_string(root.join("tasks/done.md")).unwrap();

        assert_eq!(done, "# Done Tasks\n- newer done task\n- older done task\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_backed_status_reads_task_files_as_first_sentence() {
        let root = temp_root("folder-read");
        let todo_dir = root.join("tasks/todo");
        fs::create_dir_all(&todo_dir).unwrap();
        fs::write(
            todo_dir.join("write-launch-plan.md"),
            "Write launch plan. Include rollout details and owners.\n\nAdd links later.\n",
        )
        .unwrap();

        let tasks = read_tasks(&root, "todo").unwrap();

        assert_eq!(tasks, vec!["- Write launch plan."]);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archive_reader_uses_archived_directory_without_creating_a_store() {
        let root = temp_root("archive-dir-read");
        init_tasks(&root, false).unwrap();
        let archived_dir = root.join("tasks/archived");
        fs::create_dir_all(&archived_dir).unwrap();
        fs::write(
            archived_dir.join("old-task.md"),
            "Review the old launch plan. Keep historical notes here.\n",
        )
        .unwrap();

        let entries = read_archived_task_entries(&root.join("tasks")).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "Review the old launch plan.");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archive_reader_returns_empty_when_archive_store_is_absent() {
        let root = temp_root("archive-missing-read");
        init_tasks(&root, false).unwrap();

        let entries = read_archived_task_entries(&root.join("tasks")).unwrap();

        assert!(entries.is_empty());
        assert!(!root.join("tasks/archived").exists());
        assert!(!root.join("tasks/archived.md").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archiving_folder_task_preserves_content_and_legacy_archive() {
        let root = temp_root("archive-folder-task");
        let tasks_dir = root.join("tasks");
        fs::create_dir_all(tasks_dir.join("todo")).unwrap();
        fs::write(
            tasks_dir.join("todo/long-task.md"),
            "Archive this task. Preserve the full details.\n\n- First detail\n- Second detail\n",
        )
        .unwrap();
        fs::write(
            tasks_dir.join("archived.md"),
            "# Archived Tasks\n- older archived task\n",
        )
        .unwrap();

        move_task_to_archive_in_board(&tasks_dir, "todo", "1").unwrap();

        assert!(
            directory_task_paths(&tasks_dir.join("todo"))
                .unwrap()
                .is_empty()
        );
        assert!(tasks_dir.join("archived.md.bak").exists());
        let archived = read_archived_task_entries(&tasks_dir).unwrap();
        assert_eq!(archived.len(), 2);
        assert_eq!(archived[0].summary, "older archived task");
        assert_eq!(archived[1].summary, "Archive this task.");
        assert!(archived[1].content.contains("Second detail"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn moving_folder_backed_task_preserves_long_file_content() {
        let root = temp_root("folder-move");
        let todo_dir = root.join("tasks/todo");
        fs::create_dir_all(&todo_dir).unwrap();
        fs::write(
            todo_dir.join("research-api.md"),
            "Research the API migration. This file keeps the longer task notes.\n\n- Audit callers\n- Draft rollout\n",
        )
        .unwrap();

        move_task(&root, "todo", "doing", "1").unwrap();

        assert!(directory_task_paths(&todo_dir).unwrap().is_empty());
        let doing_entries = read_task_entries(&root.join("tasks"), "doing").unwrap();
        assert_eq!(doing_entries.len(), 1);
        assert_eq!(doing_entries[0].summary, "Research the API migration.");
        assert!(doing_entries[0].content.contains("Audit callers"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn moving_folder_task_converts_markdown_destination_to_directory() {
        let root = temp_root("folder-convert-dest");
        let tasks_dir = root.join("tasks");
        fs::create_dir_all(tasks_dir.join("todo")).unwrap();
        fs::write(
            tasks_dir.join("todo/long-task.md"),
            "Move this rich task. Preserve all follow-up detail.\n\nSecond paragraph.\n",
        )
        .unwrap();
        fs::write(
            tasks_dir.join("doing.md"),
            "# Doing Tasks\n- existing task\n",
        )
        .unwrap();

        move_task(&root, "todo", "doing", "1").unwrap();

        assert!(tasks_dir.join("doing").is_dir());
        assert!(tasks_dir.join("doing.md.bak").exists());
        let doing_entries = read_task_entries(&tasks_dir, "doing").unwrap();
        assert_eq!(doing_entries.len(), 2);
        assert!(
            doing_entries
                .iter()
                .any(|entry| entry.summary == "existing task")
        );
        assert!(
            doing_entries
                .iter()
                .any(|entry| entry.content.contains("Second paragraph."))
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_task_with_status_stores_is_detected_as_subtask_board() {
        let root = temp_root("folder-subtasks");
        let epic_dir = root.join("tasks/doing/ship-epic");
        fs::create_dir_all(&epic_dir).unwrap();
        fs::write(epic_dir.join("task.md"), "Ship epic. Parent task detail.\n").unwrap();
        fs::write(epic_dir.join("todo.md"), "# To Do Tasks\n- draft spec\n").unwrap();
        fs::write(epic_dir.join("doing.md"), "# Doing Tasks\n").unwrap();
        fs::write(epic_dir.join("done.md"), "# Done Tasks\n").unwrap();

        let entries = read_task_entries(&root.join("tasks"), "doing").unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "Ship epic.");
        assert!(entries[0].has_subtasks);
        assert_eq!(
            read_tasks_in_board(&epic_dir, "todo").unwrap(),
            vec!["- draft spec"]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_tui_task_text_uses_full_task_content() {
        let entry = task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 1 },
            "Write launch plan. This is hidden in summary.",
            "Write launch plan. This is hidden in summary.\n\n- Add rollout notes",
            false,
        );

        assert_eq!(task_tui_display_text(&entry, false), "Write launch plan.");
        assert_eq!(
            task_tui_display_text(&entry, true),
            "Write launch plan. This is hidden in summary. Add rollout notes"
        );
    }

    #[test]
    fn selected_task_ignores_stale_selection() {
        let root = temp_root("stale-selection");
        ensure_task_store(&root).unwrap();

        let mut state = ListState::default();
        state.select(Some(0));

        assert_eq!(selected_task(&root, "todo", &state), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_task_index_ignores_stale_selection() {
        let root = temp_root("stale-index");
        add_task(&root, "only task", None).unwrap();

        let mut state = ListState::default();
        state.select(Some(1));

        assert_eq!(selected_task_index(&root, "todo", &state), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tui_add_inserts_above_selected_markdown_task() {
        let root = temp_root("tui-add-above-markdown");
        add_task(&root, "first task", None).unwrap();
        add_task(&root, "selected task", None).unwrap();
        add_task(&root, "last task", None).unwrap();

        let mut state = ListState::default();
        state.select(Some(1));
        insert_task_at_selection_in_board(&root.join("tasks"), "todo", &state, "new task", None)
            .unwrap();

        assert_eq!(
            read_tasks(&root, "todo").unwrap(),
            vec![
                "- first task",
                "- new task",
                "- selected task",
                "- last task",
            ]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tui_add_inserts_above_selected_folder_task() {
        let root = temp_root("tui-add-above-folder");
        init_tasks(&root, true).unwrap();
        add_task(&root, "first task", None).unwrap();
        add_task(&root, "selected task", None).unwrap();
        add_task(&root, "last task", None).unwrap();

        let mut state = ListState::default();
        state.select(Some(1));
        insert_task_at_selection_in_board(&root.join("tasks"), "todo", &state, "new task", None)
            .unwrap();

        assert_eq!(
            read_tasks(&root, "todo").unwrap(),
            vec![
                "- first task",
                "- new task",
                "- selected task",
                "- last task",
            ]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalize_board_selection_clears_empty_board_selection() {
        let root = temp_root("normalize-empty");
        ensure_task_store(&root).unwrap();

        let mut state = ListState::default();
        state.select(Some(0));

        normalize_board_selection(&root, "todo", &mut state);

        assert_eq!(state.selected(), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalize_board_selection_clamps_out_of_range_selection() {
        let root = temp_root("normalize-range");
        add_task(&root, "only task", None).unwrap();

        let mut state = ListState::default();
        state.select(Some(4));

        normalize_board_selection(&root, "todo", &mut state);

        assert_eq!(state.selected(), Some(0));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keep_selected_task_visible_scrolls_down_to_selection() {
        let tasks = vec![
            "- task one".to_string(),
            "- task two".to_string(),
            "- task three".to_string(),
            "- task four".to_string(),
        ];
        let mut scroll_offset = 0;

        keep_selected_task_visible(&tasks, Some(3), &mut scroll_offset, 3, 20);

        assert_eq!(scroll_offset, 1);
    }

    #[test]
    fn keep_selected_task_visible_scrolls_up_to_selection() {
        let tasks = vec![
            "- task one".to_string(),
            "- task two".to_string(),
            "- task three".to_string(),
        ];
        let mut scroll_offset = 2;

        keep_selected_task_visible(&tasks, Some(0), &mut scroll_offset, 3, 20);

        assert_eq!(scroll_offset, 0);
    }

    #[test]
    fn input_cursor_offset_tracks_cursor_inside_wrapped_text() {
        let text = " Add Task: hello world";

        assert_eq!(wrap_input_text(text, 10), " Add Task:\n hello wor\nld");
        assert_eq!(
            input_cursor_offset_at(text, 10, " Add Task: hello".len()),
            (6, 1)
        );
        assert_eq!(input_cursor_offset_at(text, 10, text.len()), (2, 2));
    }

    #[test]
    fn input_cursor_helpers_preserve_utf8_boundaries() {
        let text = "aéb";
        let inside_e = 2;

        assert_eq!(clamp_to_char_boundary(text, inside_e), 1);
        assert_eq!(previous_char_boundary(text, text.len()), 3);
        assert_eq!(previous_char_boundary(text, 3), 1);
        assert_eq!(next_char_boundary(text, 1), 3);
        assert_eq!(next_char_boundary(text, 3), text.len());
    }

    #[test]
    fn input_key_handler_moves_and_edits_by_words() {
        let mut input = Input::new("first second third".to_string());

        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
            " Add Task: ",
            80,
        );
        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
            " Add Task: ",
            80,
        );

        assert_eq!(input.value(), "first second Xthird");

        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
            " Add Task: ",
            80,
        );

        assert_eq!(input.value(), "first second third");
    }

    #[test]
    fn input_key_handler_supports_alt_b_and_alt_f_word_jumps() {
        let mut input = Input::new("first second third".to_string());

        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
            " Add Task: ",
            80,
        );
        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
            " Add Task: ",
            80,
        );
        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
            " Add Task: ",
            80,
        );
        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
            " Add Task: ",
            80,
        );

        assert_eq!(input.value(), "first second Xthird!");
    }

    #[test]
    fn input_key_handler_moves_vertically_through_wrapped_input() {
        let mut input = Input::new("hello world".to_string());

        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            " Add Task: ",
            10,
        );

        assert_eq!(input.cursor(), 1);

        handle_input_key(
            &mut input,
            crossterm::event::KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            " Add Task: ",
            10,
        );

        assert_eq!(input.cursor(), input.value().chars().count());
    }

    #[test]
    fn task_input_collapses_multiline_paste_until_submission() {
        let mut input = TaskInput::new("before ".to_string());
        input.insert_paste("first\r\nsecond\r\nthird".to_string());
        handle_input_key(
            &mut input.input,
            crossterm::event::KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
            " Add Task: ",
            80,
        );

        assert_eq!(input.display_value(), "before [Pasted Content 3 lines]!");
        assert_eq!(input.submitted_value(), "before first\nsecond\nthird!");
        assert_eq!(
            input.display_cursor(),
            input.display_value().chars().count()
        );

        let lines = styled_task_input_lines(" Add Task: ", &input, 80);
        let blue_text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.fg == Some(Color::Blue))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(blue_text, "[Pasted Content 3 lines]");
    }

    #[test]
    fn task_input_treats_a_paste_placeholder_as_one_editable_character() {
        let mut input = TaskInput::default();
        input.insert_paste("first\nsecond".to_string());

        handle_input_key(
            &mut input.input,
            crossterm::event::KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            " Add Task: ",
            80,
        );

        assert_eq!(input.display_value(), "");
        assert_eq!(input.submitted_value(), "");
    }

    #[test]
    fn task_input_inserts_single_line_paste_directly() {
        let mut input = TaskInput::new("before ".to_string());
        input.insert_paste("one line".to_string());

        assert_eq!(input.display_value(), "before one line");
        assert_eq!(input.submitted_value(), "before one line");
        assert!(input.pasted_content.is_empty());
    }
}
