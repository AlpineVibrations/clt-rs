use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{agent::AgentGitMode, managed_git::configure_agent_git_identity};

pub(crate) mod prelude {
    #![allow(unused_imports)]

    pub(crate) use std::{
        cell::Cell,
        collections::HashSet,
        ffi::{OsStr, OsString},
        fs,
        io::{self, BufRead, Read, Write},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::{
            Arc, Barrier, Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc::{self},
        },
        thread,
        time::{Duration, Instant},
    };

    pub(crate) use anyhow::{Context, Result};

    pub(crate) use clap::Parser;
    pub(crate) use crossterm::{
        ExecutableCommand,
        event::{self, KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags},
    };
    pub(crate) use ratatui::{
        Terminal,
        layout::Rect,
        style::Color,
        widgets::{ListState, Paragraph},
    };
    pub(crate) use toml_edit::DocumentMut;
    pub(crate) use tui_input::Input;

    pub(crate) use crate::{
        agent::{self, *},
        application::*,
        cli::*,
        managed_git::*,
        platform::*,
        runner::*,
        scheduler::*,
        session_control::*,
        task::*,
        tui::*,
        worker::*,
    };

    #[cfg(unix)]
    pub(crate) use std::os::fd::FromRawFd;
}

pub(crate) fn test_git_command(project_root: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(project_root);
    // Automated runs export an agent identity. Human fixtures must use their
    // repository config; callers can still opt into explicit agent identities.
    for key in [
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
    ] {
        command.env_remove(key);
    }
    command
}

pub(crate) fn run_test_git(project_root: &Path, args: &[&str]) -> String {
    let output = test_git_command(project_root).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={}; stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

pub(crate) fn run_test_agent_git(project_root: &Path, args: &[&str]) -> String {
    let mut command = test_git_command(project_root);
    command.args(args);
    configure_agent_git_identity(&mut command, AgentGitMode::Commit);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "agent git {:?} failed: stdout={}; stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

pub(crate) fn initialize_test_git_repository(project_root: &Path) -> String {
    run_test_git(project_root, &["init"]);
    run_test_git(project_root, &["config", "user.name", "CLT Test"]);
    run_test_git(
        project_root,
        &["config", "user.email", "clt-test@example.invalid"],
    );
    run_test_git(project_root, &["config", "commit.gpgsign", "false"]);
    run_test_git(project_root, &["add", "--all"]);
    run_test_git(project_root, &["commit", "-m", "Initial state"]);
    run_test_git(project_root, &["rev-parse", "HEAD"])
}

pub(crate) fn temp_root(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("clt-{name}-{nonce}"))
}

#[test]
fn git_fixture_identities_ignore_inherited_agent_environment() {
    const CHILD_ENV: &str = "CLT_TEST_GIT_FIXTURE_IDENTITY_CHILD";
    if std::env::var_os(CHILD_ENV).is_none() {
        // Inject the inherited identity in a subprocess so parallel tests never
        // share a mutable process environment.
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "test_support::git_fixture_identities_ignore_inherited_agent_environment",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1");
        configure_agent_git_identity(&mut command, AgentGitMode::Commit);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "fixture identity subprocess failed: stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let root = temp_root("git-fixture-identity");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("fixture.txt"), "initial\n").unwrap();
    initialize_test_git_repository(&root);
    let identity = || run_test_git(&root, &["log", "-1", "--format=%an <%ae>|%cn <%ce>"]);
    let human = "CLT Test <clt-test@example.invalid>|CLT Test <clt-test@example.invalid>";
    assert_eq!(identity(), human);

    run_test_agent_git(&root, &["commit", "--allow-empty", "-m", "Agent fixture"]);
    assert_eq!(
        identity(),
        "CLT Agent <clt-agent@localhost>|CLT Agent <clt-agent@localhost>"
    );

    run_test_git(&root, &["commit", "--allow-empty", "-m", "Human fixture"]);
    assert_eq!(identity(), human);

    let output = test_git_command(&root)
        .env("GIT_COMMITTER_EMAIL", "clt-agent@localhost")
        .args(["commit", "--allow-empty", "-m", "Agent committer fixture"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        identity(),
        "CLT Test <clt-test@example.invalid>|CLT Test <clt-agent@localhost>"
    );
    std::fs::remove_dir_all(root).unwrap();
}
