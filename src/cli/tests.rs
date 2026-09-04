use super::{
    AgentCommands, AgentGitCommitCommands, Cli, Commands, ShellKind, shell_init_script,
    write_tui_cwd_file,
};
use crate::test_support::prelude::*;
use crate::test_support::*;

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
        parse_add_task_args(vec!["Fix login bug".to_string(), "BUG, HIGH".to_string()]).unwrap();

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
    let cli =
        Cli::try_parse_from(["clt", "--cwd-file", "/tmp/clt-cwd", "shell-init", "zsh"]).unwrap();

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

    let retry_cli = Cli::try_parse_from(["clt", "agent", "retry", "/tmp/project"]).unwrap();
    match retry_cli.command {
        Some(Commands::Agent {
            command: AgentCommands::Retry { path },
        }) => {
            assert_eq!(path, Some(PathBuf::from("/tmp/project")));
        }
        _ => panic!("expected agent retry command"),
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
fn exact_session_resume_worker_command_preserves_project_and_session_ids() {
    let cli = Cli::try_parse_from([
        "clt",
        "agent",
        "resume-session-worker",
        "--project-id",
        "42",
        "--session-id",
        "session-123",
    ])
    .unwrap();

    match cli.command {
        Some(Commands::Agent {
            command:
                AgentCommands::ResumeSessionWorker {
                    project_id,
                    session_id,
                },
        }) => {
            assert_eq!(project_id, 42);
            assert_eq!(session_id, "session-123");
        }
        _ => panic!("expected exact-session resume worker command"),
    }
}

#[test]
fn interactive_guardian_worker_command_preserves_handoff_generation() {
    let cli = Cli::try_parse_from([
        "clt",
        "agent",
        "interactive-session-worker",
        "--project-id",
        "42",
        "--session-id",
        "session-123",
        "--from-holder",
        "clt-interactive-7-generation-2",
        "--resume-exec",
    ])
    .unwrap();

    match cli.command {
        Some(Commands::Agent {
            command:
                AgentCommands::InteractiveSessionWorker {
                    project_id,
                    session_id,
                    from_holder,
                    resume_exec,
                    shared_project,
                    control_fd,
                },
        }) => {
            assert_eq!(project_id, 42);
            assert_eq!(session_id, "session-123");
            assert_eq!(from_holder, "clt-interactive-7-generation-2");
            assert!(resume_exec);
            assert!(!shared_project);
            assert_eq!(control_fd, None);
        }
        _ => panic!("expected interactive guardian worker command"),
    }

    for shared_flag in ["--shared-project", "--read-only"] {
        let shared = Cli::try_parse_from([
            "clt",
            "agent",
            "interactive-session-worker",
            "--project-id",
            "42",
            "--session-id",
            "session-123",
            "--from-holder",
            "clt-shared-interactive-7-generation-2",
            shared_flag,
        ])
        .unwrap();
        assert!(matches!(
            shared.command,
            Some(Commands::Agent {
                command: AgentCommands::InteractiveSessionWorker {
                    resume_exec: false,
                    shared_project: true,
                    ..
                },
            })
        ));
    }
}

#[test]
fn agent_top_level_subcommands_parse() {
    for subcommand in [
        "projects", "daemon", "start", "stop", "status", "logs", "clean", "pause", "resume",
        "retry",
    ] {
        let cli = Cli::try_parse_from(["clt", "agent", subcommand]).unwrap();

        assert!(matches!(cli.command, Some(Commands::Agent { .. })));
    }
}
