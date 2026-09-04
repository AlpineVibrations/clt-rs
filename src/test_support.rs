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

pub(crate) fn run_test_git(project_root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(args)
        .output()
        .unwrap();
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
    let mut command = Command::new("git");
    command.arg("-C").arg(project_root).args(args);
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
