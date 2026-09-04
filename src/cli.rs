use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use crate::runner::{AutomatedSupervisorSpec, run_automated_session_supervisor};
use crate::{
    agent::{AgentGitMode, ensure_agent_state_dir, open_agent_store, open_agent_store_at},
    application::{
        AgentTaskSelection, ManagedTaskWorkflow, TaskDoneOutcome, clean_agent_state, expand_tasks,
        get_task_root, list_agent_projects, list_tasks, recover_agent_state,
        register_agent_project, retry_agent_project, set_agent_project_enabled,
        set_agent_project_git_mode, show_agent_logs, show_agent_status, unregister_agent_project,
    },
    platform::{AgentServiceAction, manage_agent_service},
    runner::run_automated_exec_gate,
    scheduler::{print_agent_scheduler_pass, run_agent_daemon, run_agent_once},
    session_control::{
        InteractiveCodexResumeMode, run_agent_interactive_session_worker,
        run_agent_session_resume_worker, run_interactive_exec_gate,
    },
    task::{TaskStatus, add_task, ensure_existing_board, init_tasks, parse_add_task_args},
    tui::{prompt_to_initialize_tasks, tui_view, tui_view_without_active_board},
    worker::run_independent_agent_worker,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ShellKind {
    Bash,
    Zsh,
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
    /// Clears a registered project's failure cooldown for an immediate retry
    Retry {
        /// Project path to retry. Defaults to the current directory.
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
    /// Internal independent owner for one scheduled Codex run
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long)]
        project_id: i64,
        #[arg(long)]
        worker_token: String,
        #[arg(long)]
        task_selection: String,
        #[arg(long)]
        resume_session_id: Option<String>,
    },
    /// Internal exact-session worker used after an interactive handoff
    #[command(hide = true)]
    ResumeSessionWorker {
        #[arg(long)]
        project_id: i64,
        #[arg(long)]
        session_id: String,
    },
    /// Internal terminal guardian used while Codex is interactive
    #[command(hide = true)]
    InteractiveSessionWorker {
        #[arg(long)]
        project_id: i64,
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        from_holder: String,
        #[arg(long, default_value_t = false)]
        resume_exec: bool,
        #[arg(long, alias = "read-only", default_value_t = false)]
        shared_project: bool,
        #[arg(long)]
        control_fd: Option<i32>,
    },
    /// Internal launch gate used to register a known Codex session before exec
    #[command(hide = true)]
    AutomatedExecGate {
        program: PathBuf,
        #[arg(num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<OsString>,
    },
    /// Internal owner that keeps the live Child handle for automated Codex
    #[command(hide = true)]
    AutomatedSessionSupervisor {
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long)]
        project_id: i64,
        #[arg(long)]
        run_token: String,
        #[arg(long)]
        lease_holder: String,
        #[arg(long)]
        stdout_path: PathBuf,
        #[arg(long)]
        stderr_path: PathBuf,
        program: PathBuf,
        #[arg(num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<OsString>,
    },
    /// Internal launch gate used to register interactive Codex before exec
    #[command(hide = true)]
    InteractiveExecGate {
        #[arg(long)]
        control_fd: Option<i32>,
        program: PathBuf,
        #[arg(num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<OsString>,
    },
    /// Runs the foreground scheduler loop
    Daemon,
    /// Starts the background agent service
    Start,
    /// Stops the background agent service
    Stop,
    /// Recovers the agent registry after stopping services and preserving its database bundle
    Recover,
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

pub(super) fn run() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Commands::ShellInit { shell }) = cli.command.as_ref() {
        print!("{}", shell_init_script(*shell));
        return Ok(());
    }
    if let Some(Commands::Agent {
        command: AgentCommands::AutomatedExecGate { program, arguments },
    }) = cli.command.as_ref()
    {
        return run_automated_exec_gate(program, arguments);
    }
    if let Some(Commands::Agent {
        command:
            AgentCommands::Worker {
                state_dir,
                project_id,
                worker_token,
                task_selection,
                resume_session_id,
            },
    }) = cli.command.as_ref()
    {
        return run_independent_agent_worker(
            state_dir,
            *project_id,
            worker_token,
            AgentTaskSelection::from_label(task_selection)?,
            resume_session_id.as_deref(),
        );
    }
    #[cfg(unix)]
    if let Some(Commands::Agent {
        command:
            AgentCommands::AutomatedSessionSupervisor {
                state_dir,
                project_id,
                run_token,
                lease_holder,
                stdout_path,
                stderr_path,
                program,
                arguments,
            },
    }) = cli.command.as_ref()
    {
        let exit_code = run_automated_session_supervisor(
            AutomatedSupervisorSpec {
                state_dir,
                project_id: *project_id,
                run_token,
                lease_holder,
                stdout_path,
                stderr_path,
            },
            program,
            arguments,
        )?;
        std::process::exit(exit_code);
    }
    if let Some(Commands::Agent {
        command:
            AgentCommands::InteractiveExecGate {
                control_fd,
                program,
                arguments,
            },
    }) = cli.command.as_ref()
    {
        return run_interactive_exec_gate(*control_fd, program, arguments);
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
            let from_status = TaskStatus::parse(&from)?;
            let to_status = TaskStatus::parse(&to)?;
            let workflow = ManagedTaskWorkflow::new(&root);
            if to_status == TaskStatus::Done {
                if let TaskDoneOutcome::ExternalCompletion(session_id) =
                    workflow.complete_task(from_status, &task_index)?
                {
                    println!(
                        "Task {task_index} from {from} marked as externally completed; cancelled idle managed Git journal for Codex session {session_id}."
                    );
                }
            } else {
                workflow.move_task(from_status, to_status, &task_index)?;
            }
        }
        Some(Commands::Done { status, task_index }) => {
            let task_status = TaskStatus::parse(&status)?;
            let workflow = ManagedTaskWorkflow::new(&root);
            if task_status == TaskStatus::Done {
                if workflow.reseal_completed_task(&task_index)? {
                    println!(
                        "Task {} in done was resealed; Git finalization is pending.",
                        task_index
                    );
                } else {
                    println!("Task is already done.");
                }
            } else {
                match workflow.complete_task(task_status, &task_index)? {
                    TaskDoneOutcome::Normal => {
                        println!("Task {} from {} marked as done.", task_index, status);
                    }
                    TaskDoneOutcome::Provisional => {
                        println!(
                            "Task {} from {} moved provisionally; Git finalization is pending.",
                            task_index, status
                        );
                    }
                    TaskDoneOutcome::ExternalCompletion(session_id) => {
                        println!(
                            "Task {task_index} from {status} marked as externally completed; cancelled idle managed Git journal for Codex session {session_id}."
                        );
                    }
                }
            }
        }
        Some(Commands::Delete { status, task_index }) => {
            let task_status = TaskStatus::parse(&status)?;
            ManagedTaskWorkflow::new(&root).delete_task(task_status, &task_index)?;
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

#[cfg(test)]
mod tests;

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
        AgentCommands::Recover => return recover_agent_state(),
        _ => {}
    }

    match command {
        AgentCommands::Register { path } => {
            let store = open_agent_store()?;
            register_agent_project(&store, path.as_deref(), local, default_root)?;
        }
        AgentCommands::Unregister { path } => {
            let state_dir = ensure_agent_state_dir()?;
            let store = open_agent_store_at(&state_dir)?;
            unregister_agent_project(&store, &state_dir, path.as_deref(), local, default_root)?;
        }
        AgentCommands::Pause { path } => {
            let store = open_agent_store()?;
            set_agent_project_enabled(&store, path.as_deref(), local, default_root, false)?;
        }
        AgentCommands::Resume { path } => {
            let store = open_agent_store()?;
            set_agent_project_enabled(&store, path.as_deref(), local, default_root, true)?;
        }
        AgentCommands::Retry { path } => {
            let store = open_agent_store()?;
            retry_agent_project(&store, path.as_deref(), local, default_root)?;
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
        AgentCommands::ResumeSessionWorker {
            project_id,
            session_id,
        } => {
            run_agent_session_resume_worker(project_id, &session_id)?;
        }
        AgentCommands::InteractiveSessionWorker {
            project_id,
            session_id,
            from_holder,
            resume_exec,
            shared_project,
            control_fd,
        } => {
            let mode = if resume_exec {
                InteractiveCodexResumeMode::ResumeExec
            } else if shared_project {
                InteractiveCodexResumeMode::WritableShared
            } else {
                InteractiveCodexResumeMode::WritableIdle
            };
            run_agent_interactive_session_worker(
                project_id,
                &session_id,
                &from_holder,
                mode,
                control_fd,
            )?;
        }
        AgentCommands::AutomatedExecGate { .. }
        | AgentCommands::AutomatedSessionSupervisor { .. }
        | AgentCommands::InteractiveExecGate { .. }
        | AgentCommands::Worker { .. } => {
            unreachable!("Codex exec gate handled before task-root discovery")
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
        AgentCommands::Start | AgentCommands::Stop | AgentCommands::Recover => {
            unreachable!("handled before store open")
        }
    }

    Ok(())
}
