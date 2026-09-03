use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use ratatui::layout::{Alignment, Position, Rect};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write, stdout};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver, TryRecvError},
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

mod agent;
mod application;
mod cli;
mod managed_git;
mod platform;
mod runner;
mod scheduler;
mod session_control;
mod task;
mod tui;
mod worker;

use agent::*;
use application::*;
use managed_git::*;
use platform::*;
use runner::*;
use scheduler::*;
use session_control::*;
use task::*;
use tui::*;
use worker::*;

#[cfg(test)]
use clap::Parser;
#[cfg(test)]
use cli::{
    AgentCommands, AgentGitCommitCommands, Cli, Commands, ShellKind, shell_init_script,
    write_tui_cwd_file,
};

/// Runs the CLT command-line application.
pub fn run() -> Result<()> {
    cli::run()
}

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Barrier, mpsc};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn run_test_git(project_root: &Path, args: &[&str]) -> String {
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

    fn run_test_agent_git(project_root: &Path, args: &[&str]) -> String {
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

    fn initialize_test_git_repository(project_root: &Path) -> String {
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

    #[cfg(unix)]
    #[test]
    fn interactive_terminal_event_source_process_entry() {
        if std::env::var_os("CLT_TEST_INTERACTIVE_TERMINAL_SOURCE").is_none() {
            return;
        }
        // SAFETY: isatty only inspects the inherited standard-input fd.
        assert_eq!(unsafe { libc::isatty(libc::STDIN_FILENO) }, 1);
        event::poll(Duration::ZERO)
            .expect("crossterm must initialize from the inherited terminal descriptor");
    }

    #[cfg(unix)]
    #[test]
    fn interactive_exec_gate_process_entry() {
        if std::env::var_os(TEST_INTERACTIVE_EXEC_GATE_ENV).is_none() {
            return;
        }
        let argument_count = std::env::var("CLT_TEST_INTERACTIVE_GATE_ARGUMENT_COUNT")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let arguments = (0..argument_count)
            .map(|index| {
                std::env::var_os(format!("CLT_TEST_INTERACTIVE_GATE_ARGUMENT_{index}")).unwrap()
            })
            .collect::<Vec<_>>();
        let control_fd = std::env::var("CLT_TEST_INTERACTIVE_GATE_CONTROL_FD")
            .unwrap()
            .parse::<i32>()
            .unwrap();
        run_interactive_exec_gate(
            Some(control_fd),
            Path::new(&std::env::var_os("CLT_TEST_INTERACTIVE_GATE_PROGRAM").unwrap()),
            &arguments,
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn automated_session_supervisor_process_entry() {
        if std::env::var_os(TEST_AUTOMATED_SUPERVISOR_ENV).is_none() {
            return;
        }
        let argument_count = std::env::var("CLT_TEST_SUPERVISOR_ARGUMENT_COUNT")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let arguments = (0..argument_count)
            .map(|index| std::env::var_os(format!("CLT_TEST_SUPERVISOR_ARGUMENT_{index}")).unwrap())
            .collect::<Vec<_>>();
        let state_dir = PathBuf::from(std::env::var_os("CLT_TEST_SUPERVISOR_STATE_DIR").unwrap());
        let run_token = std::env::var("CLT_TEST_SUPERVISOR_RUN_TOKEN").unwrap();
        let lease_holder = std::env::var("CLT_TEST_SUPERVISOR_LEASE_HOLDER").unwrap();
        let stdout_path =
            PathBuf::from(std::env::var_os("CLT_TEST_SUPERVISOR_STDOUT_PATH").unwrap());
        let stderr_path =
            PathBuf::from(std::env::var_os("CLT_TEST_SUPERVISOR_STDERR_PATH").unwrap());
        let exit_code = run_automated_session_supervisor(
            AutomatedSupervisorSpec {
                state_dir: &state_dir,
                project_id: std::env::var("CLT_TEST_SUPERVISOR_PROJECT_ID")
                    .unwrap()
                    .parse()
                    .unwrap(),
                run_token: &run_token,
                lease_holder: &lease_holder,
                stdout_path: &stdout_path,
                stderr_path: &stderr_path,
            },
            Path::new(&std::env::var_os("CLT_TEST_SUPERVISOR_PROGRAM").unwrap()),
            &arguments,
        )
        .unwrap();
        std::process::exit(exit_code);
    }

    #[cfg(unix)]
    #[test]
    fn automated_runner_owner_process_entry() {
        if std::env::var_os("CLT_TEST_AUTOMATED_RUNNER_OWNER").is_none() {
            return;
        }
        let state_dir = PathBuf::from(std::env::var_os("CLT_TEST_OWNER_STATE_DIR").unwrap());
        let project_id = std::env::var("CLT_TEST_OWNER_PROJECT_ID")
            .unwrap()
            .parse::<i64>()
            .unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let project = store
            .list_projects_blocking()
            .unwrap()
            .into_iter()
            .find(|project| project.id == project_id)
            .unwrap();
        let mut runner = CodexAgentRunner::with_command(
            state_dir,
            Duration::from_secs(30),
            PathBuf::from(std::env::var_os("CLT_TEST_OWNER_CODEX").unwrap()),
        );
        runner.heartbeat_interval = Duration::from_millis(50);
        runner
            .run_project(
                &project,
                AgentTaskSelection::ResumeSession,
                Some(&std::env::var("CLT_TEST_OWNER_SESSION_ID").unwrap()),
                &std::env::var("CLT_TEST_OWNER_LEASE_HOLDER").unwrap(),
                None,
                &new_agent_shutdown_signal(),
            )
            .unwrap();
    }

    #[cfg(unix)]
    fn assert_crashed_automated_owner_control_is_reaped(action: AgentSessionControlAction) {
        use std::os::unix::fs::PermissionsExt;

        let suffix = match action {
            AgentSessionControlAction::Stop => "stop",
            AgentSessionControlAction::Interrupt => "interrupt",
        };
        let root = temp_root(&format!("automated-owner-crash-{suffix}"));
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "crash-owner-project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let session_id = format!("session-owner-crash-{suffix}");
        store
            .set_session_control_state_blocking(
                project.id,
                &session_id,
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap();
        let lease_holder = format!("crash-owner-lease-{suffix}");
        assert!(
            store
                .try_acquire_lease_blocking(
                    project.id,
                    &lease_holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );

        let launch_marker = root.join("codex-launches");
        let fake_codex = root.join("fake-codex");
        fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\nprintf 'launched\\n' >> '{}'\ntrap '' TERM HUP\nwhile :; do sleep 1; done\n",
                launch_marker.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();

        let executable = std::env::current_exe().unwrap();
        let mut owner = Command::new(executable)
            .arg("--exact")
            .arg("tests::automated_runner_owner_process_entry")
            .arg("--nocapture")
            .env("CLT_TEST_AUTOMATED_RUNNER_OWNER", "1")
            .env("CLT_TEST_OWNER_STATE_DIR", &state_dir)
            .env("CLT_TEST_OWNER_PROJECT_ID", project.id.to_string())
            .env("CLT_TEST_OWNER_CODEX", &fake_codex)
            .env("CLT_TEST_OWNER_SESSION_ID", &session_id)
            .env("CLT_TEST_OWNER_LEASE_HOLDER", &lease_holder)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        let started = Instant::now();
        let running = loop {
            if let Some(control) = store
                .session_control_blocking(project.id, &session_id)
                .unwrap()
                && control.state == AgentSessionControlState::Running
                && control.child_pid.is_some()
                && launch_marker.exists()
            {
                break control;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "runner never registered and launched supervised Codex"
            );
            thread::sleep(Duration::from_millis(25));
        };
        let child_pid = running.child_pid.unwrap();
        let run_token = running.run_token.clone().unwrap();

        owner.kill().unwrap();
        owner.wait().unwrap();
        let interactive_lease = match action {
            AgentSessionControlAction::Stop => {
                let message =
                    toggle_tui_codex_session_stop_at(&state_dir, project.id, &session_id).unwrap();
                assert!(message.starts_with("Stopping this Codex task session"));
                None
            }
            AgentSessionControlAction::Interrupt => Some(
                prepare_tui_codex_session_interrupt_at(
                    &state_dir,
                    project.id,
                    &session_id,
                    60,
                    Duration::from_secs(10),
                )
                .unwrap(),
            ),
        };

        let expected_state = match action {
            AgentSessionControlAction::Stop => AgentSessionControlState::Stopped,
            AgentSessionControlAction::Interrupt => AgentSessionControlState::ReadyInteractive,
        };
        let started = Instant::now();
        let finalized = loop {
            let control = store
                .session_control_blocking(project.id, &session_id)
                .unwrap()
                .unwrap();
            if control.state == expected_state {
                break control;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "supervisor did not finalize crashed-owner {suffix} request; state={} ",
                control.state.database_value()
            );
            thread::sleep(Duration::from_millis(25));
        };
        assert!(finalized.child_pid.is_none());
        assert_eq!(finalized.run_token.as_deref(), Some(run_token.as_str()));
        assert_eq!(
            automated_agent_process_group_is_running(child_pid),
            Some(false)
        );
        assert_eq!(
            fs::read_to_string(&launch_marker).unwrap().lines().count(),
            1
        );
        let lease = store.lease_for_project_blocking(project.id).unwrap();
        match action {
            AgentSessionControlAction::Stop => assert!(lease.is_none()),
            AgentSessionControlAction::Interrupt => {
                let interactive_holder = &interactive_lease.as_ref().unwrap().holder;
                assert_eq!(
                    finalized.interactive_holder.as_deref(),
                    Some(interactive_holder.as_str())
                );
                assert_eq!(
                    lease.as_ref().map(|lease| lease.holder.as_str()),
                    Some(interactive_holder.as_str())
                );
            }
        }
        if let Some(lease) = interactive_lease {
            lease.release().unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn crashed_automated_owner_stop_is_completed_by_supervisor() {
        assert_crashed_automated_owner_control_is_reaped(AgentSessionControlAction::Stop);
    }

    #[cfg(unix)]
    #[test]
    fn crashed_automated_owner_interrupt_is_handed_to_interactive_holder() {
        assert_crashed_automated_owner_control_is_reaped(AgentSessionControlAction::Interrupt);
    }

    #[cfg(unix)]
    #[test]
    fn automated_supervisor_monitor_panic_still_reaps_its_codex_group() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("automated-supervisor-monitor-panic");
        fs::create_dir_all(&root).unwrap();
        let stdout_path = root.join("codex.out");
        let stderr_path = root.join("codex.err");
        let fake_codex = root.join("fake-codex");
        fs::write(
            &fake_codex,
            "#!/bin/sh\ntrap '' TERM HUP\nwhile :; do sleep 1; done\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();

        let target = Command::new(&fake_codex);
        let supervisor_stderr = fs::File::create(&stderr_path).unwrap();
        let mut supervised = spawn_automated_session_supervisor(
            &target,
            AutomatedSupervisorSpec {
                state_dir: &root,
                project_id: 1,
                run_token: "panic-supervisor-after-launch-test",
                lease_holder: "test-holder",
                stdout_path: &stdout_path,
                stderr_path: &stderr_path,
            },
            supervisor_stderr,
        )
        .unwrap();
        supervised.control.write_all(b"x").unwrap();
        supervised.control.flush().unwrap();
        drop(supervised.control);

        let status =
            wait_for_automated_supervisor_reaped(&mut supervised.process, &mut supervised.proof)
                .unwrap();

        assert!(!status.success());
        assert_eq!(
            automated_agent_process_group_is_running(supervised.child_pid),
            Some(false)
        );
        assert!(
            fs::read_to_string(&stderr_path)
                .unwrap()
                .contains("monitor panicked; stopping its owned Codex process group")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    fn assert_disconnected_no_session_launch_recovery(mutate_checkout: bool) {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root(if mutate_checkout {
            "automated-supervisor-pre-session-mutated"
        } else {
            "automated-supervisor-pre-session-unchanged"
        });
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        add_task(&project_root, "committed task", None).unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        assert!(
            store
                .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "99", "999")
                .unwrap()
        );
        let run_token = "inline-crash-before-session";
        assert!(reserve_test_inline_worker(
            &store,
            project.id,
            run_token,
            "scheduler",
            std::process::id(),
            "100",
        ));
        let lease_holder = agent_worker_lease_holder(run_token);
        let launch_marker = root.join("codex-launched");
        let fake_codex = root.join("fake-codex");
        fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\nprintf launched > '{}'\n",
                launch_marker.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();
        let stdout_path = root.join("codex.out");
        let stderr_path = root.join("codex.err");
        let target = Command::new(&fake_codex);
        let supervisor_stderr = fs::File::create(&stderr_path).unwrap();
        let mut supervised = spawn_automated_session_supervisor(
            &target,
            AutomatedSupervisorSpec {
                state_dir: &state_dir,
                project_id: project.id,
                run_token,
                lease_holder: &lease_holder,
                stdout_path: &stdout_path,
                stderr_path: &stderr_path,
            },
            supervisor_stderr,
        )
        .unwrap();
        let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        assert!(
            store
                .record_git_launch_state_blocking(
                    project.id,
                    run_token,
                    AgentGitMode::Commit,
                    &git_start,
                    "101",
                )
                .unwrap()
        );
        assert!(
            store
                .git_launch_state_blocking(project.id, run_token)
                .unwrap()
                .is_some()
        );

        drop(supervised.control);
        wait_for_automated_supervisor_reaped(&mut supervised.process, &mut supervised.proof)
            .unwrap();

        assert_eq!(
            automated_agent_process_group_is_running(supervised.child_pid),
            Some(false)
        );
        assert!(!launch_marker.exists());
        assert!(
            store
                .git_launch_state_blocking(project.id, run_token)
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .is_none()
        );
        assert!(store.list_active_workers_blocking().unwrap().is_empty());
        let worker = store
            .list_terminal_workers_blocking()
            .unwrap()
            .into_iter()
            .find(|worker| worker.worker_token == run_token)
            .unwrap();
        assert_eq!(worker.state, "abandoned");
        assert!(
            worker
                .error
                .unwrap()
                .contains("supervised Codex process group was proven reaped")
        );
        if mutate_checkout {
            fs::write(project_root.join("unexpected.txt"), "changed after crash\n").unwrap();
            let error = prepare_agent_git_start_state_for_run(
                &store,
                &project,
                AgentTaskSelection::NextTodo,
                false,
                false,
                "replacement-run",
            )
            .unwrap_err();
            assert!(format!("{error:#}").contains("unconsumed launch boundary"));
            assert!(
                store
                    .git_launch_state_blocking(project.id, run_token)
                    .unwrap()
                    .is_some()
            );
        } else {
            assert!(
                prepare_agent_git_start_state_for_run(
                    &store,
                    &project,
                    AgentTaskSelection::NextTodo,
                    false,
                    false,
                    "replacement-run",
                )
                .unwrap()
                .is_some()
            );
            assert!(
                store
                    .git_launch_state_blocking(project.id, run_token)
                    .unwrap()
                    .is_none()
            );
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn disconnected_no_session_reclaims_unchanged_launch_after_worker_abandonment() {
        assert_disconnected_no_session_launch_recovery(false);
    }

    #[cfg(unix)]
    #[test]
    fn disconnected_no_session_preserves_launch_after_checkout_mutation() {
        assert_disconnected_no_session_launch_recovery(true);
    }

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
                    session_run_token: None,
                    control_action: None,
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
            project: &agent::AgentProject,
            _task_selection: AgentTaskSelection,
            _resume_session_id: Option<&str>,
            _lease_holder: &str,
            _run_token: Option<&str>,
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

    fn reserve_test_worker(
        store: &agent::TursoAgentStore,
        project_id: i64,
        worker_token: &str,
        expected_lease_holder: &str,
        created_at: &str,
        max_active_workers: usize,
    ) -> bool {
        let service_label = format!("clt-agent-worker-{worker_token}.service");
        let command_arguments = serde_json::to_string(&vec![
            "--local",
            "agent",
            "worker",
            "--worker-token",
            worker_token,
        ])
        .unwrap();
        store
            .reserve_worker_blocking(agent::AgentWorkerReservation {
                project_id,
                worker_token,
                expected_lease_holder,
                max_active_workers,
                protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                service_label: &service_label,
                binary_path: Path::new("/tmp/test-worker-clt"),
                command_arguments: &command_arguments,
                path_env: OsStr::new("/usr/bin:/bin"),
                codex_path: None,
                task_selection: "next_todo",
                resume_session_id: None,
                created_at,
            })
            .unwrap()
    }

    fn reserve_test_inline_worker(
        store: &agent::TursoAgentStore,
        project_id: i64,
        worker_token: &str,
        expected_lease_holder: &str,
        worker_pid: u32,
        observed_at: &str,
    ) -> bool {
        let service_label = format!("{AGENT_INLINE_WORKER_SERVICE_LABEL_PREFIX}{worker_token}");
        store
            .reserve_and_claim_worker_blocking(
                agent::AgentWorkerReservation {
                    project_id,
                    worker_token,
                    expected_lease_holder,
                    max_active_workers: 12,
                    protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                    service_label: &service_label,
                    binary_path: Path::new("/tmp/test-inline-clt"),
                    command_arguments: "[]",
                    path_env: OsStr::new("/usr/bin:/bin"),
                    codex_path: None,
                    task_selection: "next_todo",
                    resume_session_id: None,
                    created_at: observed_at,
                },
                worker_pid,
                observed_at,
            )
            .unwrap()
    }

    struct InspectInlineOwnershipRunner {
        state_dir: PathBuf,
        break_lease_after_observation: bool,
        observed_run_token: Mutex<Option<String>>,
    }

    impl AgentRunner for InspectInlineOwnershipRunner {
        fn run_project(
            &self,
            project: &agent::AgentProject,
            _task_selection: AgentTaskSelection,
            _resume_session_id: Option<&str>,
            lease_holder: &str,
            run_token: Option<&str>,
            _shutdown: &AgentShutdownSignal,
        ) -> Result<AgentRunResult> {
            let run_token = run_token.context("inline run did not receive its worker token")?;
            assert_eq!(lease_holder, agent_worker_lease_holder(run_token));
            assert!(inline_agent_worker_generation_is_registered(run_token));
            let store = agent::TursoAgentStore::open_blocking(&self.state_dir)?;
            let worker = store
                .list_active_workers_blocking()?
                .into_iter()
                .find(|worker| worker.worker_token == run_token)
                .context("inline worker was not durably visible to its runner")?;
            assert_eq!(worker.state, AGENT_WORKER_STATE_RUNNING);
            assert_eq!(worker.worker_pid, Some(std::process::id()));
            assert!(is_inline_agent_worker(&worker));
            *self.observed_run_token.lock().unwrap() = Some(run_token.to_string());
            if self.break_lease_after_observation {
                assert!(store.release_lease_blocking(project.id, lease_holder)?);
            }
            Ok(AgentRunResult {
                status: "idle",
                exit_code: Some(0),
                log_dir: self.state_dir.join("runs/inline"),
                stdout_path: self.state_dir.join("runs/inline/run.out"),
                stderr_path: self.state_dir.join("runs/inline/run.err"),
                summary: "inline ownership observed".to_string(),
                codex_session_id: None,
                session_run_token: None,
                control_action: None,
            })
        }
    }

    fn prepare_inline_git_job(root: &Path) -> (PathBuf, AgentRunJob) {
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        add_task(&project_root, "inline Git task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let mut project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .set_project_git_mode_blocking(project.id, AgentGitMode::Commit)
                .unwrap()
        );
        project.git_mode = AgentGitMode::Commit;
        let holder = "foreground-inline-holder".to_string();
        assert!(
            store
                .try_acquire_lease_blocking(
                    project.id,
                    &holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );
        let job = AgentRunJob {
            state_dir: state_dir.clone(),
            project,
            holder,
            worker_token: None,
            max_global_jobs: 12,
            task_selection: AgentTaskSelection::NextTodo,
            resume_session_id: None,
            blocked_task_count_before: 0,
            done_task_contents_before: Vec::new(),
            blocked_task_snapshots_before: Vec::new(),
        };
        (state_dir, job)
    }

    fn assert_independent_worker_control_finalizes(action: AgentSessionControlAction) {
        let (suffix, expected_status, expected_control_state) = match action {
            AgentSessionControlAction::Stop => {
                ("stop", "stopped", AgentSessionControlState::Stopped)
            }
            AgentSessionControlAction::Interrupt => (
                "interrupt",
                "handoff",
                AgentSessionControlState::ReadyInteractive,
            ),
        };
        let root = temp_root(&format!("independent-worker-{suffix}"));
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let session_id = format!("session-{suffix}");
        fs::write(
            project_root.join("tasks/done.md"),
            format!("# Done Tasks\n- controlled task codex:{session_id}\n"),
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- next task\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let worker_token = format!("control-{suffix}-token");
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "9999999999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            &worker_token,
            "scheduler",
            "101",
            12,
        ));
        assert!(
            store
                .claim_worker_blocking(&worker_token, std::process::id(), "102")
                .unwrap()
        );
        store
            .mark_session_running_blocking(
                project.id,
                &session_id,
                4242,
                &worker_token,
                &root.join(format!("{suffix}.out")),
                &root.join(format!("{suffix}.err")),
            )
            .unwrap();
        let interactive_holder = format!("clt-interactive-{suffix}");
        match action {
            AgentSessionControlAction::Stop => assert!(
                store
                    .request_session_stop_blocking(project.id, &session_id, 4242, &worker_token,)
                    .unwrap()
            ),
            AgentSessionControlAction::Interrupt => assert!(
                store
                    .request_session_interrupt_blocking(
                        project.id,
                        &session_id,
                        4242,
                        &worker_token,
                        &interactive_holder,
                    )
                    .unwrap()
            ),
        }

        let runner = FakeAgentRunner {
            result: AgentRunResult {
                status: expected_status,
                exit_code: Some(0),
                log_dir: root.join("runs/project"),
                stdout_path: root.join(format!("{suffix}.out")),
                stderr_path: root.join(format!("{suffix}.err")),
                summary: format!("controlled {suffix}"),
                codex_session_id: Some(session_id.clone()),
                session_run_token: Some(worker_token.clone()),
                control_action: Some(action),
            },
            ran_projects: Mutex::new(Vec::new()),
            delay: Duration::ZERO,
        };
        let worker_holder = agent::worker_lease_holder(&worker_token);
        let completion = run_agent_job(
            AgentRunJob {
                state_dir: state_dir.clone(),
                project: project.clone(),
                holder: worker_holder,
                worker_token: Some(worker_token.clone()),
                max_global_jobs: 12,
                task_selection: AgentTaskSelection::NextTodo,
                resume_session_id: None,
                blocked_task_count_before: 0,
                done_task_contents_before: completed_task_contents(&project_root).unwrap(),
                blocked_task_snapshots_before: Vec::new(),
            },
            &runner,
            &new_agent_shutdown_signal(),
        )
        .unwrap();

        assert_eq!(completion.status, expected_status);
        assert_eq!(
            store
                .session_control_blocking(project.id, &session_id)
                .unwrap()
                .unwrap()
                .state,
            expected_control_state
        );
        assert_eq!(
            store
                .latest_run_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .status,
            expected_status
        );
        let terminal = store.list_terminal_workers_blocking().unwrap();
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].state, "completed");
        match action {
            AgentSessionControlAction::Stop => {
                assert!(
                    store
                        .lease_for_project_blocking(project.id)
                        .unwrap()
                        .is_none()
                );
                drop(store);
                let scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
                assert_eq!(scheduled.jobs.len(), 1);
                assert_eq!(
                    scheduled.jobs[0].task_selection,
                    AgentTaskSelection::NextTodo
                );
                let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
                assert!(
                    store
                        .release_lease_blocking(project.id, &scheduled.jobs[0].holder)
                        .unwrap()
                );
            }
            AgentSessionControlAction::Interrupt => {
                assert_eq!(
                    store
                        .lease_for_project_blocking(project.id)
                        .unwrap()
                        .unwrap()
                        .holder,
                    interactive_holder
                );
            }
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn independent_worker_stop_finalizes_after_supervisor_reap() {
        assert_independent_worker_control_finalizes(AgentSessionControlAction::Stop);
    }

    #[test]
    fn independent_worker_handoff_finalizes_after_supervisor_reap() {
        assert_independent_worker_control_finalizes(AgentSessionControlAction::Interrupt);
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
    fn tui_subtask_shortcuts_support_n_and_both_terminal_forms_of_plus() {
        let key = |code, modifiers| crossterm::event::KeyEvent::new(code, modifiers);

        assert!(tui_starts_subtask_input(&key(
            KeyCode::Char('n'),
            KeyModifiers::NONE
        )));
        assert!(tui_starts_subtask_input(&key(
            KeyCode::Char('+'),
            KeyModifiers::NONE
        )));
        assert!(tui_starts_subtask_input(&key(
            KeyCode::Char('+'),
            KeyModifiers::SHIFT
        )));
        assert!(!tui_starts_subtask_input(&key(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL
        )));
        assert_eq!(
            tui_task_reorder_direction(&key(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(TuiTaskReorderDirection::Down)
        );
    }

    #[test]
    fn tui_task_prompt_cancel_shortcuts_support_escape_and_control_c() {
        let key = |code, modifiers| crossterm::event::KeyEvent::new(code, modifiers);

        assert!(tui_cancels_task_prompt(&key(
            KeyCode::Esc,
            KeyModifiers::NONE
        )));
        assert!(tui_cancels_task_prompt(&key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(tui_cancels_task_prompt(&key(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(!tui_cancels_task_prompt(&key(
            KeyCode::Char('c'),
            KeyModifiers::NONE
        )));
        assert!(!tui_cancels_task_prompt(&key(
            KeyCode::Char('c'),
            KeyModifiers::ALT
        )));
    }

    #[test]
    fn tui_reorganize_prefix_and_arrows_are_unambiguous() {
        let key = |code, modifiers| crossterm::event::KeyEvent::new(code, modifiers);

        assert!(tui_toggles_reorganize_mode(&key(
            KeyCode::Char('r'),
            KeyModifiers::NONE
        )));
        assert!(tui_toggles_reorganize_mode(&key(
            KeyCode::Char('R'),
            KeyModifiers::SHIFT
        )));
        assert!(!tui_toggles_reorganize_mode(&key(
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

        assert_eq!(
            tui_reorganize_input(&key(KeyCode::Char('r'), KeyModifiers::NONE)),
            TuiReorganizeInput::Exit
        );
        assert_eq!(
            tui_reorganize_input(&key(KeyCode::Esc, KeyModifiers::NONE)),
            TuiReorganizeInput::Exit
        );
        assert_eq!(
            tui_reorganize_input(&key(KeyCode::Down, KeyModifiers::NONE)),
            TuiReorganizeInput::Move(TuiTaskReorganizeDirection::Down)
        );
        assert_eq!(
            tui_reorganize_input(&key(KeyCode::Char('x'), KeyModifiers::NONE)),
            TuiReorganizeInput::Ignore
        );
    }

    #[test]
    fn tui_reorganize_mode_has_a_distinct_title_and_border_color() {
        assert_eq!(
            tui_task_column_title("To Do", true, true),
            " REORGANIZE MODE: To Do [r/Esc exits] "
        );
        assert_eq!(
            tui_task_column_title("To Do", true, false),
            "To Do   <<<<<< * >>>>>>     "
        );
        assert_eq!(tui_task_column_title("Doing", false, true), "Doing");
        assert_eq!(
            tui_task_column_border_color(Color::Indexed(110), true),
            Color::Yellow
        );
        assert_eq!(
            tui_task_column_border_color(Color::Indexed(110), false),
            Color::Indexed(110)
        );
    }

    #[test]
    fn tui_task_column_keeps_controls_on_title_line_without_reserving_a_row() {
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(36, 5);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                let task_area = render_tui_task_column_header(
                    frame,
                    frame.area(),
                    "To Do",
                    1,
                    true,
                    false,
                    Color::Indexed(110),
                );
                frame.render_widget(Paragraph::new("1. task"), task_area);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rows = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>();

        assert!(rows[0].contains("To Do"));
        assert!(rows[0].contains("<<<<<< * >>>>>>"));
        assert!(rows[1].contains("1. task"));
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
            TaskStatus::Todo,
            &mut state,
            TuiTaskReorderDirection::Down,
        );

        assert_eq!(message, "Moved task down to position 2");
        assert_eq!(state.selected(), Some(1));
        assert_eq!(
            read_tasks(&root, "todo").unwrap(),
            vec!["- beta", "- alpha"]
        );

        let message = reorder_selected_tui_task(
            &board_dir,
            TaskStatus::Todo,
            &mut state,
            TuiTaskReorderDirection::Up,
        );

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
            project: agent::AgentProject {
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
                last_daemon_scan_status: None,
                last_daemon_scan_error: None,
                last_run_at: None,
                last_success_at: None,
                last_failure_at: None,
                last_blocked_recovery_at: None,
                failure_count: 0,
            },
            scan: AgentProjectScan::empty(),
            runtime_state: TuiAgentRuntimeState::Idle,
            daemon_scan_problem: None,
            failure_problem: None,
        }
    }

    #[test]
    fn tui_agent_panel_starts_in_loading_state_without_fetching_a_snapshot() {
        let active_root = PathBuf::from("/tmp/current");

        let panel = TuiAgentPanel::new(&active_root);

        assert!(panel.projects.is_empty());
        assert_eq!(panel.daemon_status, "loading");
        assert_eq!(
            panel
                .selected_current_project_registration()
                .map(|registration| registration.path.as_path()),
            Some(active_root.as_path())
        );
    }

    #[test]
    fn tui_agent_panel_refresh_worker_does_not_block_the_caller() {
        let active_root = PathBuf::from("/tmp/current");
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let mut worker = TuiAgentPanelRefreshWorker::new();

        assert!(worker.request_with(&active_root, move |active_root| {
            started_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
            TuiAgentPanelRefreshResult {
                active_root,
                panel_snapshot: Ok(TuiAgentPanelSnapshot {
                    projects: Vec::new(),
                    daemon_status: "running".to_string(),
                }),
                task_session_states: Ok(TaskAgentSessionStates::default()),
            }
        }));
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(worker.try_result().is_none());
        assert!(!worker.request_with(&active_root, |_| unreachable!()));

        release_sender.send(()).unwrap();
        let started = Instant::now();
        let result = loop {
            if let Some(result) = worker.try_result() {
                break result;
            }
            assert!(started.elapsed() < Duration::from_secs(1));
            thread::yield_now();
        };

        assert_eq!(result.active_root, active_root);
        assert_eq!(result.panel_snapshot.unwrap().daemon_status, "running");
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
        assert!(tui_agent_panel_instructions().contains("r retries after fixing an error"));
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let mut panel = TuiAgentPanel {
            projects: vec![TuiAgentProject {
                project,
                scan: AgentProjectScan::empty(),
                runtime_state: TuiAgentRuntimeState::Idle,
                daemon_scan_problem: None,
                failure_problem: None,
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
    fn terminal_codex_session_marker_is_parsed_and_hidden_from_task_text() {
        let content = "Fix keyboard navigation — COMPLETED 2026-08-25: done codex:019fe7ab-f267-76e3-b82c-d7c5705be8d1\n";

        assert_eq!(
            codex_session_id_from_task_content(content),
            Some("019fe7ab-f267-76e3-b82c-d7c5705be8d1")
        );
        assert_eq!(
            task_content_without_codex_session(content),
            "Fix keyboard navigation — COMPLETED 2026-08-25: done"
        );
        assert_eq!(
            normalize_task_text(content),
            "Fix keyboard navigation — COMPLETED 2026-08-25: done"
        );
        assert_eq!(
            codex_session_id_from_task_content("codex:session-123 is mentioned in the task"),
            None
        );
        assert_eq!(
            normalize_task_text("codex:session-123 is mentioned in the task"),
            "codex:session-123 is mentioned in the task"
        );
        assert_eq!(codex_session_id_from_task_content("task codex:"), None);
    }

    #[test]
    fn linked_unfinished_tasks_only_display_stopped_flags() {
        let running = task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 0 },
            "running task codex:session-running",
            "running task codex:session-running",
            false,
        );
        let stopped = task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 1 },
            "stopped task codex:session-stopped",
            "stopped task codex:session-stopped",
            false,
        );
        let session_states = TaskAgentSessionStates::from([
            (
                "session-running".to_string(),
                AgentSessionControlState::Interactive,
            ),
            (
                "session-stopped".to_string(),
                AgentSessionControlState::Stopped,
            ),
        ]);

        assert_eq!(
            task_display_text_with_agent_flag(&running, TaskStatus::Doing, &session_states),
            "running task"
        );
        assert_eq!(
            task_tui_display_text_with_agent_flag(
                &stopped,
                TaskStatus::Doing,
                true,
                &session_states
            ),
            "[STOPPED] stopped task"
        );
        assert_eq!(
            task_display_text_with_agent_flag(&stopped, TaskStatus::Done, &session_states),
            "stopped task"
        );
    }

    #[test]
    fn displaced_codex_session_marker_is_recovered_and_repositioned() {
        let content = "Fix keyboard navigation. codex:session-123\n\nCompletion note.\n";

        assert_eq!(codex_session_id_from_task_content(content), None);
        assert_eq!(
            recoverable_codex_session_id_from_task_content(content),
            Some("session-123")
        );
        assert_eq!(
            task_content_without_recoverable_codex_session(content),
            "Fix keyboard navigation.\n\nCompletion note."
        );
        assert_eq!(
            task_content_with_codex_session(content, "session-123"),
            "Fix keyboard navigation.\n\nCompletion note. codex:session-123"
        );

        let session_id = "019fe7ab-f267-76e3-b82c-d7c5705be8d1";
        let inline = format!("Fix keyboard navigation codex:{session_id} — COMPLETED: done");
        assert_eq!(
            recoverable_codex_session_id_from_task_content(&inline),
            Some(session_id)
        );
        assert_eq!(
            task_content_with_codex_session(&inline, session_id),
            format!("Fix keyboard navigation — COMPLETED: done codex:{session_id}")
        );
    }

    #[test]
    fn task_edit_hides_and_preserves_terminal_codex_session_marker() {
        let root = temp_root("edit-codex-session-marker");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/done.md"),
            "# Done Tasks\n- original task codex:session-123\n",
        )
        .unwrap();
        let board_dir = root.join("tasks");
        let entry = read_task_entries(&board_dir, TaskStatus::Done)
            .unwrap()
            .remove(0);

        assert_eq!(task_display_text(&entry), "original task");
        assert_eq!(task_full_display_text(&entry), "original task");
        assert_eq!(
            task_content_without_codex_session(&entry.content),
            "original task"
        );

        update_task_in_board(&board_dir, TaskStatus::Done, 1, "edited task").unwrap();

        assert_eq!(
            fs::read_to_string(root.join("tasks/done.md")).unwrap(),
            "# Done Tasks\n- edited task codex:session-123\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn working_git_task_updates_preserve_the_stable_payload() {
        let identity = durable_task_identity("Implement durable finalization").unwrap();
        ensure_working_task_content_preserves_identity(
            "session-working",
            &identity,
            "Implement durable finalization\n\nCompletion note:\n- COMPLETED 2026-09-02: cargo test passed codex:session-working",
        )
        .unwrap();

        let error = ensure_working_task_content_preserves_identity(
            "session-working",
            &identity,
            "Implement a different feature codex:session-working",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("cannot change its durable task payload"));
    }

    #[test]
    fn folder_task_stores_codex_session_marker_without_displaying_it() {
        let root = temp_root("folder-codex-session-marker");
        init_tasks(&root, true).unwrap();
        let done_path = root.join("tasks/done/0001-finished-task.md");
        fs::write(&done_path, "Finished task.\n\nCompletion details.\n").unwrap();
        let board_dir = root.join("tasks");
        let entry = read_task_entries(&board_dir, TaskStatus::Done)
            .unwrap()
            .remove(0);

        let content =
            attach_codex_session_to_task(&root, TaskStatus::Done, &entry, "session-456").unwrap();

        assert_eq!(
            content,
            "Finished task.\n\nCompletion details. codex:session-456"
        );
        assert_eq!(
            fs::read_to_string(&done_path).unwrap(),
            "Finished task.\n\nCompletion details. codex:session-456\n"
        );
        let entry = read_task_entries(&board_dir, TaskStatus::Done)
            .unwrap()
            .remove(0);
        assert_eq!(task_display_text(&entry), "Finished task.");
        assert_eq!(
            task_full_display_text(&entry),
            "Finished task. Completion details."
        );

        update_task_in_board(
            &board_dir,
            TaskStatus::Done,
            1,
            "Edited task.\n\nNew details.",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&done_path).unwrap(),
            "Edited task.\n\nNew details. codex:session-456\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_edits_move_displaced_codex_session_markers_to_the_end() {
        let root = temp_root("edit-displaced-codex-session-marker");
        init_tasks(&root, true).unwrap();
        let done_path = root.join("tasks/done/0001-finished-task.md");
        fs::write(
            &done_path,
            "Finished task. codex:session-456\n\nCompletion note.\n",
        )
        .unwrap();
        let board_dir = root.join("tasks");
        let entry = read_task_entries(&board_dir, TaskStatus::Done)
            .unwrap()
            .remove(0);

        assert_eq!(
            codex_session_for_task(&entry).as_deref(),
            Some("session-456")
        );
        assert_eq!(task_display_text(&entry), "Finished task.");
        update_task_in_board(
            &board_dir,
            TaskStatus::Done,
            1,
            "Edited task.\n\nUpdated completion note.",
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(&done_path).unwrap(),
            "Edited task.\n\nUpdated completion note. codex:session-456\n"
        );
        let updated = read_task_entries(&board_dir, TaskStatus::Done)
            .unwrap()
            .remove(0);
        assert_eq!(
            codex_session_id_from_task_content(&updated.content),
            Some("session-456")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn moving_a_markdown_task_preserves_its_codex_session_marker() {
        let root = temp_root("move-codex-session-marker");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/doing.md"),
            "# Doing Tasks\n- resumable task codex:session-123\n",
        )
        .unwrap();

        move_task(&root, TaskStatus::Doing, TaskStatus::Done, "1").unwrap();

        let done = read_task_entries(&get_tasks_dir(&root), TaskStatus::Done)
            .unwrap()
            .remove(0);
        assert_eq!(
            codex_session_for_task(&done).as_deref(),
            Some("session-123")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_session_attachment_does_not_overwrite_a_concurrent_markdown_edit() {
        let root = temp_root("markdown-session-attachment-cas");
        init_tasks(&root, false).unwrap();
        let doing_path = root.join("tasks/doing.md");
        fs::write(&doing_path, "# Doing Tasks\n- original task\n").unwrap();
        let stale = read_task_entries(&root.join("tasks"), TaskStatus::Doing)
            .unwrap()
            .remove(0);
        fs::write(&doing_path, "# Doing Tasks\n- concurrently edited task\n").unwrap();

        assert!(
            attach_codex_session_to_task(&root, TaskStatus::Doing, &stale, "session-123").is_err()
        );
        assert_eq!(
            fs::read_to_string(&doing_path).unwrap(),
            "# Doing Tasks\n- concurrently edited task\n"
        );

        let fresh = read_task_entries(&root.join("tasks"), TaskStatus::Doing)
            .unwrap()
            .remove(0);
        attach_codex_session_to_task(&root, TaskStatus::Doing, &fresh, "session-123").unwrap();
        assert_eq!(
            fs::read_to_string(&doing_path).unwrap(),
            "# Doing Tasks\n- concurrently edited task codex:session-123\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_session_attachment_does_not_overwrite_a_concurrent_folder_task_edit() {
        let root = temp_root("folder-session-attachment-cas");
        init_tasks(&root, true).unwrap();
        let doing_path = root.join("tasks/doing/0001-active-task.md");
        fs::write(&doing_path, "Original task.\n").unwrap();
        let stale = read_task_entries(&root.join("tasks"), TaskStatus::Doing)
            .unwrap()
            .remove(0);
        fs::write(&doing_path, "Concurrently edited task.\n").unwrap();

        assert!(
            attach_codex_session_to_task(&root, TaskStatus::Doing, &stale, "session-123").is_err()
        );
        assert_eq!(
            fs::read_to_string(&doing_path).unwrap(),
            "Concurrently edited task.\n"
        );

        let fresh = read_task_entries(&root.join("tasks"), TaskStatus::Doing)
            .unwrap()
            .remove(0);
        attach_codex_session_to_task(&root, TaskStatus::Doing, &fresh, "session-123").unwrap();
        assert_eq!(
            fs::read_to_string(&doing_path).unwrap(),
            "Concurrently edited task. codex:session-123\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_session_attachment_serializes_with_a_concurrent_task_edit() {
        let root = temp_root("session-attachment-concurrent-edit");
        init_tasks(&root, false).unwrap();
        let board_dir = root.join("tasks");
        fs::write(
            board_dir.join("doing.md"),
            "# Doing Tasks\n- original task\n",
        )
        .unwrap();
        let entry = read_task_entries(&board_dir, TaskStatus::Doing)
            .unwrap()
            .remove(0);

        let (marker_ready_tx, marker_ready_rx) = mpsc::channel();
        let (release_marker_tx, release_marker_rx) = mpsc::channel();
        let marker_root = root.clone();
        let marker_thread = thread::spawn(move || {
            attach_codex_session_to_task_with_before_replace(
                &marker_root,
                TaskStatus::Doing,
                &entry,
                "session-123",
                move || {
                    marker_ready_tx.send(()).unwrap();
                    release_marker_rx.recv().unwrap();
                },
            )
        });
        marker_ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("marker attachment did not reach its final check");

        let (edit_contended_tx, edit_contended_rx) = mpsc::channel();
        let edit_board_dir = board_dir.clone();
        let edit_thread = thread::spawn(move || {
            update_task_in_board_with_contention_callback(
                &edit_board_dir,
                TaskStatus::Doing,
                1,
                "edited task",
                move || edit_contended_tx.send(()).unwrap(),
            )
        });
        let edit_contended = edit_contended_rx.recv_timeout(Duration::from_secs(2));
        release_marker_tx.send(()).unwrap();

        marker_thread.join().unwrap().unwrap();
        edit_thread.join().unwrap().unwrap();
        assert!(
            edit_contended.is_ok(),
            "task edit did not wait for the in-flight marker attachment"
        );
        assert_eq!(
            fs::read_to_string(board_dir.join("doing.md")).unwrap(),
            "# Doing Tasks\n- edited task codex:session-123\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_session_attachment_serializes_with_a_concurrent_task_move() {
        let root = temp_root("session-attachment-concurrent-move");
        init_tasks(&root, false).unwrap();
        let board_dir = root.join("tasks");
        fs::write(
            board_dir.join("doing.md"),
            "# Doing Tasks\n- original task\n",
        )
        .unwrap();
        let entry = read_task_entries(&board_dir, TaskStatus::Doing)
            .unwrap()
            .remove(0);

        let (marker_ready_tx, marker_ready_rx) = mpsc::channel();
        let (release_marker_tx, release_marker_rx) = mpsc::channel();
        let marker_root = root.clone();
        let marker_thread = thread::spawn(move || {
            attach_codex_session_to_task_with_before_replace(
                &marker_root,
                TaskStatus::Doing,
                &entry,
                "session-123",
                move || {
                    marker_ready_tx.send(()).unwrap();
                    release_marker_rx.recv().unwrap();
                },
            )
        });
        marker_ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("marker attachment did not reach its final check");

        let (move_contended_tx, move_contended_rx) = mpsc::channel();
        let move_board_dir = board_dir.clone();
        let move_thread = thread::spawn(move || {
            move_task_in_board_with_contention_callback(
                &move_board_dir,
                TaskStatus::Doing,
                TaskStatus::Done,
                "1",
                move || move_contended_tx.send(()).unwrap(),
            )
        });
        let move_contended = move_contended_rx.recv_timeout(Duration::from_secs(2));
        release_marker_tx.send(()).unwrap();

        marker_thread.join().unwrap().unwrap();
        move_thread.join().unwrap().unwrap();
        assert!(
            move_contended.is_ok(),
            "task move did not wait for the in-flight marker attachment"
        );
        assert!(
            read_task_entries(&board_dir, TaskStatus::Doing)
                .unwrap()
                .is_empty()
        );
        let done = read_task_entries(&board_dir, TaskStatus::Done).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(
            codex_session_for_task(&done[0]).as_deref(),
            Some("session-123")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_run_does_not_copy_an_existing_live_session_marker_to_another_task() {
        let root = temp_root("completion-session-marker-dedup");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/done.md"),
            "# Done Tasks\n- unrelated concurrent completion\n- actual task codex:session-123\n",
        )
        .unwrap();
        let mut project = tui_agent_project_for_test(1, "project").project;
        project.path = root.clone();
        let job = AgentRunJob {
            state_dir: root.join("state/clt"),
            project,
            holder: "holder".to_string(),
            worker_token: None,
            max_global_jobs: 12,
            task_selection: AgentTaskSelection::NextTodo,
            resume_session_id: None,
            blocked_task_count_before: 0,
            done_task_contents_before: Vec::new(),
            blocked_task_snapshots_before: Vec::new(),
        };

        attach_codex_session_after_run(&job, "session-123", "success").unwrap();

        assert_eq!(
            fs::read_to_string(root.join("tasks/done.md")).unwrap(),
            "# Done Tasks\n- unrelated concurrent completion\n- actual task codex:session-123\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_run_reports_when_its_session_marker_target_is_ambiguous() {
        let root = temp_root("completion-session-marker-ambiguous");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/done.md"),
            "# Done Tasks\n- concurrent completion one\n- concurrent completion two\n",
        )
        .unwrap();
        let mut project = tui_agent_project_for_test(1, "project").project;
        project.path = root.clone();
        let job = AgentRunJob {
            state_dir: root.join("state/clt"),
            project,
            holder: "holder".to_string(),
            worker_token: None,
            max_global_jobs: 12,
            task_selection: AgentTaskSelection::NextTodo,
            resume_session_id: None,
            blocked_task_count_before: 0,
            done_task_contents_before: Vec::new(),
            blocked_task_snapshots_before: Vec::new(),
        };

        let error = attach_codex_session_after_run(&job, "session-123", "success")
            .expect_err("ambiguous completion must be reported");
        assert!(
            error
                .to_string()
                .contains("exactly one completed or blocked task")
        );
        assert!(
            !fs::read_to_string(root.join("tasks/done.md"))
                .unwrap()
                .contains("codex:session-123")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_and_blocked_outcomes_are_jointly_ambiguous_for_session_attachment() {
        let root = temp_root("completion-and-blocked-session-ambiguous");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/done.md"),
            "# Done Tasks\n- unrelated concurrent completion\n",
        )
        .unwrap();
        fs::write(
            root.join("tasks/todo.md"),
            "# Todo Tasks\n- agent target — BLOCKED 2026-08-25: waiting\n",
        )
        .unwrap();
        let mut project = tui_agent_project_for_test(1, "project").project;
        project.path = root.clone();
        let job = AgentRunJob {
            state_dir: root.join("state/clt"),
            project,
            holder: "holder".to_string(),
            worker_token: None,
            max_global_jobs: 12,
            task_selection: AgentTaskSelection::NextTodo,
            resume_session_id: None,
            blocked_task_count_before: 0,
            done_task_contents_before: Vec::new(),
            blocked_task_snapshots_before: Vec::new(),
        };

        assert!(attach_codex_session_after_run(&job, "session-123", "blocked").is_err());
        assert!(
            !fs::read_to_string(root.join("tasks/done.md"))
                .unwrap()
                .contains("codex:session-123")
        );
        assert!(
            !fs::read_to_string(root.join("tasks/todo.md"))
                .unwrap()
                .contains("codex:session-123")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interactive_codex_resume_command_is_always_writable() {
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
    fn legacy_read_only_guardian_holders_recover_as_shared_sessions() {
        assert_eq!(
            InteractiveGuardianDisposition::from_guardian_holder(
                "clt-readonly-interactive-worker-42-generation"
            ),
            Some(InteractiveGuardianDisposition::PreserveSharedSession)
        );
        assert_eq!(
            InteractiveGuardianDisposition::from_guardian_holder(
                "clt-stopped-readonly-interactive-worker-42-generation"
            ),
            Some(InteractiveGuardianDisposition::RestoreStoppedShared)
        );
        assert!(is_stopped_shared_interactive_holder(
            "clt-stopped-readonly-interactive-42-generation"
        ));
    }

    #[test]
    fn automated_codex_commands_allow_registered_non_git_projects() {
        let project_root = PathBuf::from("/tmp/non-git-project");
        let mut project = tui_agent_project_for_test(1, "non-git-project").project;
        project.path = project_root.clone();

        let mut new_command = Command::new("codex");
        let new_session = configure_automated_codex_subcommand(
            &mut new_command,
            &project,
            AgentTaskSelection::NextTodo,
            None,
        )
        .unwrap();
        let new_args: Vec<_> = new_command.get_args().collect();
        assert_eq!(new_session, None);
        assert_eq!(new_args[0], OsStr::new("exec"));
        assert_eq!(new_args[1], OsStr::new("--skip-git-repo-check"));
        assert_eq!(new_args[2], OsStr::new("-C"));
        assert_eq!(new_args[3], project_root.as_os_str());

        let mut resumed_command = Command::new("codex");
        let resumed_session = configure_automated_codex_subcommand(
            &mut resumed_command,
            &project,
            AgentTaskSelection::ResumeSession,
            Some("session-123"),
        )
        .unwrap();
        let resumed_args: Vec<_> = resumed_command.get_args().collect();
        assert_eq!(resumed_session.as_deref(), Some("session-123"));
        assert_eq!(resumed_args[0], OsStr::new("exec"));
        assert_eq!(resumed_args[1], OsStr::new("resume"));
        assert_eq!(resumed_args[2], OsStr::new("--skip-git-repo-check"));
        assert_eq!(resumed_args[3], OsStr::new("session-123"));
    }

    #[test]
    fn interactive_codex_handoff_holds_the_project_lease_until_exit() {
        let root = temp_root("interactive-codex-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let project_id = project.id;

        let lease = InteractiveAgentLease::try_acquire_at(&state_dir, project_id, 60)
            .unwrap()
            .unwrap();
        assert!(
            InteractiveAgentLease::try_acquire_at(&state_dir, project_id, 60)
                .unwrap()
                .is_none()
        );
        drop(lease);
        let reacquired = InteractiveAgentLease::try_acquire_at(&state_dir, project_id, 60)
            .unwrap()
            .unwrap();
        drop(reacquired);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stopped_session_interactive_prepare_uses_one_holder_for_control_and_lease() {
        let root = temp_root("stopped-interactive-holder");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                101,
                "stopped-generation",
                &root.join("stopped.out"),
                &root.join("stopped.err"),
            )
            .unwrap();
        assert!(
            store
                .request_session_stop_blocking(
                    project_id,
                    "session-123",
                    101,
                    "stopped-generation",
                )
                .unwrap()
        );
        assert!(
            store
                .complete_session_stop_blocking(project_id, "session-123", "stopped-generation",)
                .unwrap()
        );

        let lease = prepare_tui_codex_session_interrupt_at(
            &state_dir,
            project_id,
            "session-123",
            60,
            Duration::from_millis(50),
        )
        .unwrap();
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        let recorded_lease = store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap();

        assert_eq!(control.state, AgentSessionControlState::ReadyInteractive);
        assert_eq!(control.interactive_holder.as_deref(), Some(&*lease.holder));
        assert_eq!(recorded_lease.holder, lease.holder);
        lease.release().unwrap();

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_session_control_cas_rejects_stale_run_generation() {
        let root = temp_root("session-control-generation");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let stdout_path = root.join("session.out");
        let stderr_path = root.join("session.err");

        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                101,
                "run-one",
                &stdout_path,
                &stderr_path,
            )
            .unwrap();
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::Running);
        assert_eq!(control.child_pid, Some(101));
        assert_eq!(control.run_token.as_deref(), Some("run-one"));
        assert!(
            !store
                .request_session_stop_blocking(project_id, "session-123", 102, "run-one")
                .unwrap()
        );
        assert!(
            !store
                .request_session_stop_blocking(project_id, "session-123", 101, "run-two")
                .unwrap()
        );
        assert!(
            store
                .request_session_stop_blocking(project_id, "session-123", 101, "run-one")
                .unwrap()
        );

        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                101,
                "run-one",
                &stdout_path,
                &stderr_path,
            )
            .unwrap();
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::StopRequested
        );

        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                202,
                "run-two",
                &stdout_path,
                &stderr_path,
            )
            .unwrap();
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::Running);
        assert_eq!(control.child_pid, Some(202));
        assert_eq!(control.run_token.as_deref(), Some("run-two"));
        assert!(
            !store
                .complete_session_stop_blocking(project_id, "session-123", "run-one")
                .unwrap()
        );
        assert!(
            !store
                .request_session_stop_blocking(project_id, "session-123", 101, "run-one")
                .unwrap()
        );
        assert!(
            store
                .request_session_stop_blocking(project_id, "session-123", 202, "run-two")
                .unwrap()
        );
        assert!(
            !store
                .complete_session_stop_blocking(project_id, "session-123", "run-one")
                .unwrap()
        );
        assert!(
            store
                .complete_session_stop_blocking(project_id, "session-123", "run-two")
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Stopped
        );
        assert!(
            !store
                .request_stopped_session_resume_blocking(
                    project_id,
                    "session-123",
                    Some("run-one"),
                )
                .unwrap()
        );
        assert!(
            store
                .request_stopped_session_resume_blocking(
                    project_id,
                    "session-123",
                    Some("run-two"),
                )
                .unwrap()
        );

        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                303,
                "run-three",
                &stdout_path,
                &stderr_path,
            )
            .unwrap();
        assert!(
            store
                .request_session_stop_blocking(project_id, "session-123", 303, "run-three")
                .unwrap()
        );
        assert!(
            store
                .complete_session_stop_blocking(project_id, "session-123", "run-three")
                .unwrap()
        );
        assert!(
            !store
                .begin_stopped_session_interactive_blocking(
                    project_id,
                    "session-123",
                    "clt-interactive-stale",
                    Some("run-two"),
                )
                .unwrap()
        );
        assert!(
            store
                .begin_stopped_session_interactive_blocking(
                    project_id,
                    "session-123",
                    "clt-interactive-current",
                    Some("run-three"),
                )
                .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn newer_scheduler_controls_old_worker_without_overwriting_its_generation() {
        let root = temp_root("cross-generation-worker-control");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let stdout_path = root.join("session.out");
        let stderr_path = root.join("session.err");
        store
            .mark_session_running_blocking(
                project_id,
                "session-old",
                101,
                "old-generation",
                &stdout_path,
                &stderr_path,
            )
            .unwrap();
        assert!(
            store
                .try_acquire_lease_blocking(project_id, "scheduler", "100", "9999999999")
                .unwrap()
        );
        assert!(
            store
                .reserve_worker_blocking(agent::AgentWorkerReservation {
                    project_id,
                    worker_token: "new-generation",
                    expected_lease_holder: "scheduler",
                    max_active_workers: 12,
                    protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                    command_arguments: "[]",
                    path_env: OsStr::new("/usr/bin:/bin"),
                    codex_path: None,
                    service_label: "clt-worker-new-generation",
                    binary_path: Path::new("/tmp/new-clt-generation"),
                    task_selection: "resume_session",
                    resume_session_id: Some("session-old"),
                    created_at: "101",
                })
                .unwrap()
        );
        assert!(
            store
                .claim_worker_blocking("new-generation", std::process::id(), "102")
                .unwrap()
        );

        assert!(
            store
                .mark_session_running_blocking(
                    project_id,
                    "session-old",
                    202,
                    "new-generation",
                    &stdout_path,
                    &stderr_path,
                )
                .is_err()
        );
        assert!(
            !store
                .clear_running_session_control_blocking(project_id, "session-old", None)
                .unwrap()
        );
        let old_control = store
            .session_control_blocking(project_id, "session-old")
            .unwrap()
            .unwrap();
        assert_eq!(old_control.child_pid, Some(101));
        assert_eq!(old_control.run_token.as_deref(), Some("old-generation"));
        assert!(
            store
                .request_session_stop_blocking(project_id, "session-old", 101, "old-generation")
                .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelling_interrupt_preserves_a_live_runner_but_queues_after_handoff() {
        let root = temp_root("interrupt-cancel-state");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let daemon_holder = "clt-agent-123";
        let interactive_holder = "clt-interactive-456-1-1";
        assert!(
            store
                .try_acquire_lease_blocking(
                    project_id,
                    daemon_holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );
        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                101,
                "run-one",
                &root.join("run.out"),
                &root.join("run.err"),
            )
            .unwrap();
        assert!(
            store
                .request_session_interrupt_blocking(
                    project_id,
                    "session-123",
                    101,
                    "run-one",
                    interactive_holder,
                )
                .unwrap()
        );
        assert!(
            store
                .cancel_session_interrupt_handoff_blocking(
                    project_id,
                    "session-123",
                    interactive_holder,
                )
                .unwrap()
        );
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::Running);
        assert_eq!(control.child_pid, Some(101));
        assert_eq!(control.run_token.as_deref(), Some("run-one"));
        assert!(control.interactive_holder.is_none());

        assert!(
            store
                .request_session_interrupt_blocking(
                    project_id,
                    "session-123",
                    101,
                    "run-one",
                    interactive_holder,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .complete_session_interrupt_handoff_blocking(
                    project_id,
                    "session-123",
                    "run-one",
                    daemon_holder,
                    60,
                )
                .unwrap()
                .as_deref(),
            Some(interactive_holder)
        );
        assert!(
            store
                .cancel_session_interrupt_handoff_blocking(
                    project_id,
                    "session-123",
                    interactive_holder,
                )
                .unwrap()
        );
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::ResumeRequested);
        assert!(control.child_pid.is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tui_stop_key_toggles_running_session_to_stop_then_resume_requested() {
        let root = temp_root("tui-session-stop-toggle");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                101,
                "run-one",
                &root.join("session.out"),
                &root.join("session.err"),
            )
            .unwrap();

        let message =
            toggle_tui_codex_session_stop_at(&state_dir, project_id, "session-123").unwrap();
        assert!(message.starts_with("Stopping this Codex task session"));
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::StopRequested
        );
        assert!(
            store
                .complete_session_stop_blocking(project_id, "session-123", "run-one")
                .unwrap()
        );

        let message =
            toggle_tui_codex_session_stop_at(&state_dir, project_id, "session-123").unwrap();
        assert!(message.starts_with("Resuming this stopped Codex task session"));
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::ResumeRequested
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_runs_next_todo_while_stopped_session_waits_for_exact_resume() {
        let root = temp_root("scheduler-session-control");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- paused task codex:session-123\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- next task\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        drop(store);

        let scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(scheduled.jobs.len(), 1);
        assert_eq!(scheduled.pass.runs_started, 1);
        assert_eq!(
            scheduled.jobs[0].task_selection,
            AgentTaskSelection::NextTodo
        );

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Stopped
        );
        assert!(
            store
                .release_lease_blocking(project_id, &scheduled.jobs[0].holder)
                .unwrap()
        );
        assert!(
            store
                .transition_session_control_state_blocking(
                    project_id,
                    "session-123",
                    AgentSessionControlState::Stopped,
                    AgentSessionControlState::ResumeRequested,
                )
                .unwrap()
        );
        drop(store);

        let resumed = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(resumed.jobs.len(), 1);
        assert_eq!(resumed.pass.runs_started, 1);
        let job = &resumed.jobs[0];
        assert_eq!(job.task_selection, AgentTaskSelection::ResumeSession);
        assert_eq!(job.resume_session_id.as_deref(), Some("session-123"));

        let mut command = Command::new("codex");
        let configured_session = configure_automated_codex_subcommand(
            &mut command,
            &job.project,
            job.task_selection,
            job.resume_session_id.as_deref(),
        )
        .unwrap();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(configured_session.as_deref(), Some("session-123"));
        assert_eq!(args[0], OsStr::new("exec"));
        assert_eq!(args[1], OsStr::new("resume"));
        assert_eq!(args[2], OsStr::new("--skip-git-repo-check"));
        assert_eq!(args[3], OsStr::new("session-123"));
        assert!(
            args[4]
                .to_string_lossy()
                .contains("Interactive handoff recovery:")
        );

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::ResumeRequested
        );
        assert!(
            store
                .release_lease_blocking(project_id, &job.holder)
                .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stopped_sessions_remain_persisted_without_stranding_markerless_projects() {
        let root = temp_root("stopped-session-done");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- paused task codex:session-123\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- next task\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        drop(store);

        move_task(&project_root, TaskStatus::Doing, TaskStatus::Done, "1").unwrap();
        let scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(scheduled.jobs.len(), 1);
        assert_eq!(
            scheduled.jobs[0].task_selection,
            AgentTaskSelection::NextTodo
        );
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Stopped
        );
        assert!(
            store
                .release_lease_blocking(project_id, &scheduled.jobs[0].holder)
                .unwrap()
        );
        drop(store);
        toggle_tui_codex_session_stop_at(&state_dir, project_id, "session-123").unwrap();
        let exact_resume = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(exact_resume.jobs.len(), 1);
        assert_eq!(
            exact_resume.jobs[0].task_selection,
            AgentTaskSelection::ResumeSession
        );
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .release_lease_blocking(project_id, &exact_resume.jobs[0].holder)
            .unwrap();
        drop(store);
        fs::remove_dir_all(root).unwrap();

        let root = temp_root("stopped-session-deleted");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- orphaned task codex:session-456\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- next task\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .set_session_control_state_blocking(
                project_id,
                "session-456",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        delete_task(&project_root, "doing", "1").unwrap();
        drop(store);

        let scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(scheduled.jobs.len(), 1);
        assert_eq!(
            scheduled.jobs[0].task_selection,
            AgentTaskSelection::NextTodo
        );
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-456")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Stopped
        );
        store
            .release_lease_blocking(project_id, &scheduled.jobs[0].holder)
            .unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_unstarted_resume_session_remains_requested_before_registration() {
        let root = temp_root("failed-claimed-session-resume");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- retry task codex:session-123\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap();
        drop(store);

        let mut scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(scheduled.jobs.len(), 1);
        assert_eq!(
            scheduled.jobs[0].task_selection,
            AgentTaskSelection::ResumeSession
        );
        assert_eq!(
            scheduled.jobs[0].resume_session_id.as_deref(),
            Some("session-123")
        );
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let unstarted = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(unstarted.state, AgentSessionControlState::ResumeRequested);
        assert_eq!(unstarted.child_pid, None);
        assert_eq!(unstarted.run_token, None);
        drop(store);

        let runner = FakeAgentRunner::new(&state_dir, "failure");
        let completion = run_agent_job(
            scheduled.jobs.pop().unwrap(),
            &runner,
            &new_agent_shutdown_signal(),
        )
        .unwrap();
        assert_eq!(completion.status, "failure");
        assert_eq!(runner.ran_project_count(), 1);

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let restored = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(restored.state, AgentSessionControlState::ResumeRequested);
        assert_eq!(restored.child_pid, None);
        assert_eq!(restored.run_token, None);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_resume_claim_registers_child_and_extends_the_live_lease_atomically() {
        let root = temp_root("atomic-exact-resume-child-claim");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap();

        assert!(
            store
                .try_acquire_lease_blocking(project_id, "expired-holder", "0", "1")
                .unwrap()
        );
        assert!(
            !store
                .register_known_session_with_child_blocking(agent::AgentKnownSessionRegistration {
                    project_id,
                    codex_session_id: "session-123",
                    child_pid: 123,
                    run_token: "expired-generation",
                    stdout_path: &root.join("expired.out"),
                    stderr_path: &root.join("expired.err"),
                    lease_holder: "expired-holder",
                    lease_timeout_seconds: 120,
                    claim_requested_resume: true,
                },)
                .unwrap()
        );
        assert!(
            store
                .release_lease_blocking(project_id, "expired-holder")
                .unwrap()
        );

        let holder = "live-holder";
        let initial_expiry = agent_timestamp_after(5);
        assert!(
            store
                .try_acquire_lease_blocking(
                    project_id,
                    holder,
                    &agent_timestamp(),
                    &initial_expiry,
                )
                .unwrap()
        );
        assert!(
            !store
                .register_known_session_with_child_blocking(agent::AgentKnownSessionRegistration {
                    project_id,
                    codex_session_id: "session-123",
                    child_pid: 456,
                    run_token: "wrong-holder-generation",
                    stdout_path: &root.join("wrong.out"),
                    stderr_path: &root.join("wrong.err"),
                    lease_holder: "wrong-holder",
                    lease_timeout_seconds: 120,
                    claim_requested_resume: true,
                },)
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::ResumeRequested
        );

        assert!(
            store
                .register_known_session_with_child_blocking(agent::AgentKnownSessionRegistration {
                    project_id,
                    codex_session_id: "session-123",
                    child_pid: 789,
                    run_token: "registered-generation",
                    stdout_path: &root.join("registered.out"),
                    stderr_path: &root.join("registered.err"),
                    lease_holder: holder,
                    lease_timeout_seconds: 120,
                    claim_requested_resume: true,
                },)
                .unwrap()
        );
        let registered = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(registered.state, AgentSessionControlState::Running);
        assert_eq!(registered.child_pid, Some(789));
        assert_eq!(
            registered.run_token.as_deref(),
            Some("registered-generation")
        );
        let renewed_expiry = store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap()
            .expires_at;
        assert!(renewed_expiry.parse::<u64>().unwrap() > initial_expiry.parse::<u64>().unwrap());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unproven_spawned_child_keeps_an_exact_resume_claim_fenced() {
        struct UnprovenTerminationRunner {
            state_dir: PathBuf,
        }

        impl AgentRunner for UnprovenTerminationRunner {
            fn run_project(
                &self,
                project: &agent::AgentProject,
                _task_selection: AgentTaskSelection,
                resume_session_id: Option<&str>,
                lease_holder: &str,
                _run_token: Option<&str>,
                _shutdown: &AgentShutdownSignal,
            ) -> Result<AgentRunResult> {
                let session_id = resume_session_id.unwrap();
                let store = agent::TursoAgentStore::open_blocking(&self.state_dir)?;
                assert!(store.register_known_session_with_child_blocking(
                    agent::AgentKnownSessionRegistration {
                        project_id: project.id,
                        codex_session_id: session_id,
                        child_pid: std::process::id(),
                        run_token: "registered-unproven-generation",
                        stdout_path: &self.state_dir.join("unproven.out"),
                        stderr_path: &self.state_dir.join("unproven.err"),
                        lease_holder,
                        lease_timeout_seconds: 60,
                        claim_requested_resume: true,
                    },
                )?);
                Err(anyhow::Error::new(AgentChildTerminationUnproven(
                    "spawned process group may still be alive".to_string(),
                )))
            }
        }

        let root = temp_root("unproven-child-exact-resume");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- retry task codex:session-123\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap();
        drop(store);

        let mut scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        let completion = run_agent_job(
            scheduled.jobs.pop().unwrap(),
            &UnprovenTerminationRunner {
                state_dir: state_dir.clone(),
            },
            &new_agent_shutdown_signal(),
        )
        .unwrap();
        assert_eq!(completion.status, "failure");

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::Running);
        assert_eq!(control.child_pid, Some(std::process::id()));
        assert_eq!(
            control.run_token.as_deref(),
            Some("registered-unproven-generation")
        );
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
        let next_pass = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert!(next_pass.jobs.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupt_handoff_atomically_transfers_lease_then_schedules_exact_resume() {
        let root = temp_root("interrupt-handoff-transaction");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- interrupted task codex:session-123\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let daemon_holder = "clt-agent-111";
        let interactive_holder = "clt-interactive-222";
        assert!(
            store
                .try_acquire_lease_blocking(project_id, daemon_holder, "100", "9999999999",)
                .unwrap()
        );
        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                101,
                "run-one",
                &root.join("session.out"),
                &root.join("session.err"),
            )
            .unwrap();
        assert!(
            store
                .request_session_interrupt_blocking(
                    project_id,
                    "session-123",
                    101,
                    "run-one",
                    interactive_holder,
                )
                .unwrap()
        );

        assert_eq!(
            store
                .complete_session_interrupt_handoff_blocking(
                    project_id,
                    "session-123",
                    "stale-run",
                    daemon_holder,
                    60,
                )
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .unwrap()
                .holder,
            daemon_holder
        );
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::InterruptRequested
        );

        assert_eq!(
            store
                .complete_session_interrupt_handoff_blocking(
                    project_id,
                    "session-123",
                    "run-one",
                    daemon_holder,
                    60,
                )
                .unwrap()
                .as_deref(),
            Some(interactive_holder)
        );
        let lease = store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap();
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(lease.holder, interactive_holder);
        assert_eq!(control.state, AgentSessionControlState::ReadyInteractive);
        assert_eq!(
            control.interactive_holder.as_deref(),
            Some(interactive_holder)
        );
        assert_eq!(control.child_pid, None);

        assert!(
            store
                .transition_session_control_state_blocking(
                    project_id,
                    "session-123",
                    AgentSessionControlState::ReadyInteractive,
                    AgentSessionControlState::Interactive,
                )
                .unwrap()
        );
        assert!(
            store
                .transition_session_control_state_blocking(
                    project_id,
                    "session-123",
                    AgentSessionControlState::Interactive,
                    AgentSessionControlState::ResumeRequested,
                )
                .unwrap()
        );
        assert!(
            store
                .release_lease_blocking(project_id, interactive_holder)
                .unwrap()
        );
        drop(store);

        let resumed = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(resumed.jobs.len(), 1);
        assert_eq!(resumed.pass.runs_started, 1);
        let job = &resumed.jobs[0];
        assert_eq!(job.task_selection, AgentTaskSelection::ResumeSession);
        assert_eq!(job.resume_session_id.as_deref(), Some("session-123"));

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::ResumeRequested);
        assert_eq!(control.codex_session_id, "session-123");
        assert!(
            store
                .release_lease_blocking(project_id, &job.holder)
                .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_clears_orphaned_resume_session_and_runs_next_todo() {
        let root = temp_root("scheduler-orphaned-resume-session");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "next task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .set_session_control_state_blocking(
                project_id,
                "session-without-task-marker",
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap();
        drop(store);

        let scheduled = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

        assert_eq!(scheduled.jobs.len(), 1);
        assert_eq!(scheduled.pass.runs_started, 1);
        assert_eq!(
            scheduled.jobs[0].task_selection,
            AgentTaskSelection::NextTodo
        );
        assert!(scheduled.jobs[0].resume_session_id.is_none());
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert!(
            store
                .session_control_blocking(project_id, "session-without-task-marker")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .release_lease_blocking(project_id, &scheduled.jobs[0].holder)
                .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interactive_guardian_adoption_and_finish_are_atomic_full_holder_cas() {
        let root = temp_root("interactive-guardian-holder-cas");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let daemon_holder = "clt-agent-daemon-generation";
        let tui_holder = "clt-interactive-tui-generation";
        let guardian_holder = "clt-interactive-worker-guardian-generation";
        assert!(
            store
                .try_acquire_lease_blocking(project_id, daemon_holder, "100", "9999999999")
                .unwrap()
        );
        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                101,
                "run-one",
                &root.join("session.out"),
                &root.join("session.err"),
            )
            .unwrap();
        assert!(
            store
                .request_session_interrupt_blocking(
                    project_id,
                    "session-123",
                    101,
                    "run-one",
                    tui_holder,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .complete_session_interrupt_handoff_blocking(
                    project_id,
                    "session-123",
                    "run-one",
                    daemon_holder,
                    60,
                )
                .unwrap()
                .as_deref(),
            Some(tui_holder)
        );

        assert!(
            !store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    "clt-interactive-stale-generation",
                    guardian_holder,
                    60,
                )
                .unwrap()
        );
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::ReadyInteractive);
        assert_eq!(control.interactive_holder.as_deref(), Some(tui_holder));
        assert_eq!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .unwrap()
                .holder,
            tui_holder
        );

        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    tui_holder,
                    guardian_holder,
                    60,
                )
                .unwrap()
        );
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::Interactive);
        assert_eq!(control.interactive_holder.as_deref(), Some(guardian_holder));
        assert_eq!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .unwrap()
                .holder,
            guardian_holder
        );

        let disconnected = AtomicBool::new(false);
        let terminal_input = fs::File::create(root.join("test-terminal-input")).unwrap();
        assert_eq!(
            run_guarded_interactive_codex(
                &store,
                &store.list_projects_blocking().unwrap().remove(0),
                "session-123",
                guardian_holder,
                Duration::from_secs(60),
                terminal_input,
                &disconnected,
            )
            .unwrap(),
            None
        );
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Interactive
        );

        assert!(
            !store
                .finish_interactive_guardian_blocking(
                    project_id,
                    "session-123",
                    "clt-interactive-worker-stale-generation",
                    InteractiveGuardianDisposition::ResumeExec,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Interactive
        );
        assert_eq!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .unwrap()
                .holder,
            guardian_holder
        );

        assert!(
            store
                .finish_interactive_guardian_blocking(
                    project_id,
                    "session-123",
                    guardian_holder,
                    InteractiveGuardianDisposition::ResumeExec,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::ResumeRequested
        );
        assert!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stopping_interactive_takeover_keeps_the_exact_session_stopped() {
        let root = temp_root("interactive-guardian-explicit-stop");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let requester = InteractiveAgentLease::holder_for_current_process();
        assert!(
            store
                .try_acquire_lease_blocking(project_id, &requester, "100", "9999999999")
                .unwrap()
        );
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        assert!(
            store
                .begin_stopped_session_interactive_blocking(
                    project_id,
                    "session-123",
                    &requester,
                    None,
                )
                .unwrap()
        );
        let guardian = interactive_guardian_holder(InteractiveGuardianDisposition::ResumeExec);
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    &requester,
                    &guardian,
                    60,
                )
                .unwrap()
        );
        assert!(
            store
                .register_interactive_guardian_child_blocking(
                    project_id,
                    "session-123",
                    &guardian,
                    std::process::id(),
                    60,
                )
                .unwrap()
        );
        assert!(
            store
                .request_interactive_session_stop_blocking(
                    project_id,
                    "session-123",
                    std::process::id(),
                    &guardian,
                )
                .unwrap()
        );
        assert!(
            store
                .finish_interactive_guardian_blocking(
                    project_id,
                    "session-123",
                    &guardian,
                    InteractiveGuardianDisposition::ResumeExec,
                )
                .unwrap()
        );
        let stopped = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(stopped.state, AgentSessionControlState::Stopped);
        assert!(stopped.child_pid.is_none());
        assert!(stopped.interactive_holder.is_none());
        assert!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_task_session_can_be_opened_repeatedly() {
        let root = temp_root("interactive-completed-task-repeat");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- finished task codex:session-123\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;

        let first_requester = InteractiveAgentLease::holder_for_idle_session();
        assert!(
            store
                .try_acquire_lease_blocking(project_id, &first_requester, "100", "9999999999",)
                .unwrap()
        );
        assert!(
            store
                .reserve_idle_session_interactive_blocking(
                    project_id,
                    "session-123",
                    &first_requester,
                    None,
                )
                .unwrap()
        );
        let first_disposition = InteractiveGuardianDisposition::from_handoff(
            InteractiveCodexResumeMode::WritableIdle,
            &first_requester,
        );
        assert_eq!(
            first_disposition,
            InteractiveGuardianDisposition::PreserveIdleSession
        );
        let first_guardian = interactive_guardian_holder(first_disposition);
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    &first_requester,
                    &first_guardian,
                    60,
                )
                .unwrap()
        );
        assert!(
            store
                .finish_interactive_guardian_blocking(
                    project_id,
                    "session-123",
                    &first_guardian,
                    first_disposition,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Stopped
        );
        assert!(
            codex_session_task_supports_interactive_resume(&project_root, "session-123").unwrap()
        );

        let second_requester = InteractiveAgentLease::holder_for_stopped_session();
        assert!(
            store
                .try_acquire_lease_blocking(project_id, &second_requester, "100", "9999999999",)
                .unwrap()
        );
        assert!(
            store
                .reserve_idle_session_interactive_blocking(
                    project_id,
                    "session-123",
                    &second_requester,
                    None,
                )
                .unwrap()
        );
        let second_disposition = InteractiveGuardianDisposition::from_handoff(
            InteractiveCodexResumeMode::WritableIdle,
            &second_requester,
        );
        assert_eq!(
            second_disposition,
            InteractiveGuardianDisposition::RestoreStopped
        );
        let second_guardian = interactive_guardian_holder(second_disposition);
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    &second_requester,
                    &second_guardian,
                    60,
                )
                .unwrap()
        );
        assert!(
            store
                .finish_interactive_guardian_blocking(
                    project_id,
                    "session-123",
                    &second_guardian,
                    second_disposition,
                )
                .unwrap()
        );
        let preserved = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(preserved.state, AgentSessionControlState::Stopped);
        assert!(preserved.interactive_holder.is_none());
        assert!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_idle_guardian_recovery_preserves_the_session_and_releases_the_lease() {
        let root = temp_root("interactive-guardian-no-resume");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let tui_holder = "clt-interactive-done-task-generation";
        let guardian_holder = "clt-interactive-worker-done-task-generation";
        assert!(
            store
                .try_acquire_lease_blocking(project_id, tui_holder, "100", "9999999999")
                .unwrap()
        );
        assert!(
            store
                .reserve_idle_session_interactive_blocking(
                    project_id,
                    "session-123",
                    tui_holder,
                    None,
                )
                .unwrap()
        );
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    tui_holder,
                    guardian_holder,
                    60,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .unwrap()
                .holder,
            guardian_holder
        );
        assert!(
            store
                .recover_stale_interactive_guardian_blocking(
                    project_id,
                    "session-123",
                    guardian_holder,
                    None,
                    InteractiveGuardianDisposition::PreserveIdleSession,
                )
                .unwrap()
        );
        assert!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .is_none()
        );
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::Stopped);
        assert!(control.interactive_holder.is_none());
        assert!(control.interactive_launch_token.is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dead_interactive_holder_with_a_matching_session_is_not_blindly_reclaimed() {
        let root = temp_root("interactive-guardian-session-fence");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let requester = "clt-idle-interactive-requester";
        let guardian = format!("clt-idle-interactive-worker-{}-1-1", u32::MAX);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, requester, "100", "9999999999",)
                .unwrap()
        );
        assert!(
            store
                .reserve_idle_session_interactive_blocking(
                    project.id,
                    "session-123",
                    requester,
                    None,
                )
                .unwrap()
        );
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project.id,
                    Some("session-123"),
                    requester,
                    &guardian,
                    60,
                )
                .unwrap()
        );
        let lease = store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap();
        drop(store);

        assert!(
            !try_reclaim_inactive_agent_lease(&state_dir, &project, None, &lease, false).unwrap()
        );
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .holder,
            guardian
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reaped_guardian_releases_its_lease_when_the_session_row_disappeared() {
        let root = temp_root("interactive-guardian-missing-session");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let requester = InteractiveAgentLease::holder_for_idle_session();
        let guardian =
            interactive_guardian_holder(InteractiveGuardianDisposition::PreserveIdleSession);
        assert!(
            store
                .try_acquire_lease_blocking(project_id, &requester, "100", "9999999999",)
                .unwrap()
        );
        assert!(
            store
                .adopt_interactive_guardian_blocking(project_id, None, &requester, &guardian, 60,)
                .unwrap()
        );

        let error = finish_interactive_guardian_after_reap(
            &store,
            project_id,
            "missing-session",
            &guardian,
            Duration::from_secs(60),
            InteractiveGuardianDisposition::PreserveIdleSession,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("released the orphaned project reservation"));
        assert!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_stopped_guardian_recovery_restores_the_stopped_generation() {
        let root = temp_root("interactive-guardian-restore-stopped");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let tui_holder = "clt-stopped-interactive-tui-generation";
        let guardian_holder = "clt-stopped-interactive-worker-guardian-generation";
        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                101,
                "stopped-generation",
                &root.join("stopped.out"),
                &root.join("stopped.err"),
            )
            .unwrap();
        assert!(
            store
                .request_session_stop_blocking(
                    project_id,
                    "session-123",
                    101,
                    "stopped-generation",
                )
                .unwrap()
        );
        assert!(
            store
                .complete_session_stop_blocking(project_id, "session-123", "stopped-generation",)
                .unwrap()
        );
        assert!(
            store
                .try_acquire_lease_blocking(project_id, tui_holder, "100", "9999999999")
                .unwrap()
        );
        assert!(
            !store
                .reserve_idle_session_interactive_blocking(
                    project_id,
                    "session-123",
                    tui_holder,
                    Some("stale-generation"),
                )
                .unwrap()
        );
        assert!(
            store
                .reserve_idle_session_interactive_blocking(
                    project_id,
                    "session-123",
                    tui_holder,
                    Some("stopped-generation"),
                )
                .unwrap()
        );
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::ReadyInteractive);
        assert_eq!(control.interactive_holder.as_deref(), Some(tui_holder));
        assert!(
            store
                .cancel_idle_session_interactive_blocking(project_id, "session-123", tui_holder)
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Stopped
        );
        assert!(
            store
                .reserve_idle_session_interactive_blocking(
                    project_id,
                    "session-123",
                    tui_holder,
                    Some("stopped-generation"),
                )
                .unwrap()
        );

        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    tui_holder,
                    guardian_holder,
                    60,
                )
                .unwrap()
        );
        assert!(
            store
                .recover_stale_interactive_guardian_blocking(
                    project_id,
                    "session-123",
                    guardian_holder,
                    None,
                    InteractiveGuardianDisposition::RestoreStopped,
                )
                .unwrap()
        );
        let control = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::Stopped);
        assert!(control.interactive_holder.is_none());
        assert!(control.interactive_launch_token.is_none());
        assert!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn interactive_guardian_stops_owned_group_descendants_without_touching_another_group() {
        let spawn_group_with_term_ignoring_descendant = || {
            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg(
                    "(trap '' TERM; count=0; while [ \"$count\" -lt 20 ]; do sleep 1; count=$((count + 1)); done) & wait",
                );
            configure_interactive_child_command(&mut command);
            command.spawn().unwrap()
        };
        let mut owned_child = spawn_group_with_term_ignoring_descendant();
        let mut unrelated_child = spawn_group_with_term_ignoring_descendant();
        thread::sleep(Duration::from_millis(50));

        assert_eq!(
            // SAFETY: the child PID is live and getpgid only inspects process metadata.
            unsafe { libc::getpgid(i32::try_from(owned_child.id()).unwrap()) },
            i32::try_from(owned_child.id()).unwrap()
        );

        let stop_result = stop_interactive_child_process(&mut owned_child);
        let owned_child_was_reaped = owned_child.try_wait().unwrap().is_some();
        let unrelated_child_survived = unrelated_child.try_wait().unwrap().is_none();
        let _ = stop_interactive_child_process(&mut unrelated_child);

        assert!(stop_result.unwrap().is_some());
        assert!(owned_child_was_reaped);
        assert!(unrelated_child_survived);
    }

    #[test]
    fn stale_interactive_session_without_lease_is_queued_for_exec_resume() {
        let root = temp_root("stale-interactive-session");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        assert!(
            store
                .begin_stopped_session_interactive_blocking(
                    project_id,
                    "session-123",
                    "clt-interactive-4294967295",
                    None,
                )
                .unwrap()
        );
        assert!(
            !store
                .recover_stale_interactive_session_control_blocking(
                    project_id,
                    "session-123",
                    AgentSessionControlState::ReadyInteractive,
                    AgentSessionControlState::ResumeRequested,
                    Some("clt-interactive-stale-generation"),
                )
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .interactive_holder
                .as_deref(),
            Some("clt-interactive-4294967295")
        );
        drop(store);

        reconcile_stale_agent_session_controls(
            &state_dir,
            project_id,
            None,
            false,
            agent_timestamp_seconds()
                .saturating_add(TUI_SESSION_HANDOFF_TIMEOUT_SECONDS)
                .saturating_add(10),
        )
        .unwrap();

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::ResumeRequested
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stale_gated_guardian_recovers_after_dying_before_child_registration() {
        let root = temp_root("stale-interactive-guardian");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let provisional_holder = "clt-interactive-1-2-3";
        let guardian_holder = format!("clt-interactive-worker-{}-5-6", u32::MAX);
        assert!(
            store
                .try_acquire_lease_blocking(
                    project_id,
                    provisional_holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        assert!(
            store
                .begin_stopped_session_interactive_blocking(
                    project_id,
                    "session-123",
                    provisional_holder,
                    None,
                )
                .unwrap()
        );
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    provisional_holder,
                    &guardian_holder,
                    60,
                )
                .unwrap()
        );
        let adopted = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(adopted.state, AgentSessionControlState::Interactive);
        assert_eq!(adopted.child_pid, None);
        assert_eq!(
            adopted.interactive_holder.as_deref(),
            Some(guardian_holder.as_str())
        );
        assert_eq!(
            adopted.interactive_launch_token.as_deref(),
            Some(guardian_holder.as_str())
        );
        let orphaned_lease = store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap();
        assert_eq!(orphaned_lease.holder, guardian_holder);
        drop(store);

        reconcile_stale_agent_session_controls(
            &state_dir,
            project_id,
            Some(&orphaned_lease),
            false,
            agent_timestamp_seconds(),
        )
        .unwrap();

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let recovered = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(recovered.state, AgentSessionControlState::ResumeRequested);
        assert_eq!(recovered.child_pid, None);
        assert!(recovered.interactive_holder.is_none());
        assert!(recovered.interactive_launch_token.is_none());
        assert!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_guardian_without_a_launch_token_remains_fail_closed() {
        let root = temp_root("legacy-interactive-guardian");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let provisional_holder = "clt-interactive-legacy-requester";
        let guardian_holder = "clt-interactive-worker-legacy-guardian";
        assert!(
            store
                .try_acquire_lease_blocking(
                    project_id,
                    provisional_holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        assert!(
            store
                .begin_stopped_session_interactive_blocking(
                    project_id,
                    "session-123",
                    provisional_holder,
                    None,
                )
                .unwrap()
        );
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    provisional_holder,
                    guardian_holder,
                    60,
                )
                .unwrap()
        );
        assert!(
            store
                .release_lease_blocking(project_id, guardian_holder)
                .unwrap()
        );
        let db_path = store.db_path().to_path_buf();
        drop(store);

        // Simulate an Interactive row adopted by a pre-gate CLT binary. A NULL
        // PID in that generation is not proof that Codex was never launched.
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let db = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
                .build()
                .await
                .unwrap();
            let conn = db.connect().unwrap();
            conn.execute(
                "UPDATE session_controls
                    SET interactive_launch_token = NULL
                  WHERE project_id = ?1 AND codex_session_id = ?2",
                turso::params![project_id, "session-123"],
            )
            .await
            .unwrap();
        });

        reconcile_stale_agent_session_controls(
            &state_dir,
            project_id,
            None,
            false,
            agent_timestamp_seconds()
                .saturating_add(TUI_SESSION_HANDOFF_TIMEOUT_SECONDS)
                .saturating_add(10),
        )
        .unwrap();

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let legacy = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(legacy.state, AgentSessionControlState::Interactive);
        assert_eq!(legacy.child_pid, None);
        assert_eq!(legacy.interactive_holder.as_deref(), Some(guardian_holder));
        assert!(legacy.interactive_launch_token.is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stale_automated_session_recovers_only_after_its_recorded_child_is_gone() {
        let root = temp_root("stale-automated-child-fence");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                std::process::id(),
                "run-live",
                &root.join("live.out"),
                &root.join("live.err"),
            )
            .unwrap();
        assert!(
            !store
                .recover_stale_automated_session_control_blocking(
                    project_id,
                    "session-123",
                    AgentSessionControlState::Running,
                    AgentSessionControlState::ResumeRequested,
                    std::process::id(),
                    Some("stale-run-token"),
                )
                .unwrap()
        );

        reconcile_stale_agent_session_controls(
            &state_dir,
            project_id,
            None,
            false,
            agent_timestamp_seconds(),
        )
        .unwrap();
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Running
        );
        assert!(session_controls_suspend_project(
            &store
                .session_controls_for_project_blocking(project_id)
                .unwrap(),
        ));

        store
            .mark_session_running_blocking(
                project_id,
                "session-123",
                u32::MAX,
                "run-gone",
                &root.join("gone.out"),
                &root.join("gone.err"),
            )
            .unwrap();
        reconcile_stale_agent_session_controls(
            &state_dir,
            project_id,
            None,
            false,
            agent_timestamp_seconds(),
        )
        .unwrap();
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::ResumeRequested
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn automated_codex_recovery_resumes_the_only_linked_task_session() {
        let root = temp_root("automated-codex-session-resume");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/doing.md"),
            "# Doing Tasks\n- waiting task — BLOCKED 2026-08-25: needs input codex:session-123\n",
        )
        .unwrap();
        let mut project = tui_agent_project_for_test(1, "resume-project").project;
        project.path = root.clone();
        let mut command = Command::new("codex");

        let session_id = configure_automated_codex_subcommand(
            &mut command,
            &project,
            AgentTaskSelection::RecoverBlocked,
            None,
        )
        .unwrap();
        let args: Vec<_> = command.get_args().collect();

        assert_eq!(session_id.as_deref(), Some("session-123"));
        assert_eq!(args[0], OsStr::new("exec"));
        assert_eq!(args[1], OsStr::new("resume"));
        assert_eq!(args[2], OsStr::new("--skip-git-repo-check"));
        assert_eq!(args[3], OsStr::new("session-123"));
        assert!(args[4].to_string_lossy().contains("Blocked-task monitor:"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_codex_session_is_attached_when_a_todo_task_enters_doing() {
        let root = temp_root("live-codex-session-link");
        add_task(&root, "existing todo", None).unwrap();
        let before = task_contents_for_status(&root, TaskStatus::Doing).unwrap();
        move_task(&root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();

        assert!(
            attach_codex_session_to_active_task(
                &root,
                AgentTaskSelection::NextTodo,
                &before,
                &[],
                "session-live",
            )
            .unwrap()
        );
        let task = read_task_entries(&get_tasks_dir(&root), TaskStatus::Doing)
            .unwrap()
            .remove(0);
        assert_eq!(
            codex_session_for_task(&task).as_deref(),
            Some("session-live")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_codex_session_is_not_reattached_after_its_task_leaves_doing() {
        let root = temp_root("live-codex-session-already-moved");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/doing.md"),
            "# Doing Tasks\n- unrelated active task\n",
        )
        .unwrap();
        fs::write(
            root.join("tasks/done.md"),
            "# Done Tasks\n- handed-back task codex:session-live\n",
        )
        .unwrap();

        assert!(
            attach_codex_session_to_active_task(
                &root,
                AgentTaskSelection::ResumeSession,
                &[],
                &[],
                "session-live",
            )
            .unwrap()
        );
        let doing = read_task_entries(&get_tasks_dir(&root), TaskStatus::Doing).unwrap();
        assert_eq!(doing.len(), 1);
        assert!(codex_session_for_task(&doing[0]).is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_session_completion_accepts_a_displaced_marker_and_leaves_todo_ready() {
        let root = temp_root("exact-session-displaced-completion-marker");
        init_tasks(&root, false).unwrap();
        let session_id = "01a03e61-21a8-7da1-956a-f50e4565123b";
        fs::write(
            root.join("tasks/done.md"),
            format!(
                "# Done Tasks\n- handed-back task codex:{session_id} \
— COMPLETED 2026-08-26: done\n"
            ),
        )
        .unwrap();
        fs::write(
            root.join("tasks/todo.md"),
            "# Todo Tasks\n- next automated task\n",
        )
        .unwrap();

        let mut project = tui_agent_project_for_test(1, "project").project;
        project.path = root.clone();
        let job = AgentRunJob {
            state_dir: root.join("state/clt"),
            project,
            holder: "holder".to_string(),
            worker_token: None,
            max_global_jobs: 12,
            task_selection: AgentTaskSelection::ResumeSession,
            resume_session_id: Some(session_id.to_string()),
            blocked_task_count_before: 0,
            done_task_contents_before: Vec::new(),
            blocked_task_snapshots_before: Vec::new(),
        };

        let done = read_task_entries(&get_tasks_dir(&root), TaskStatus::Done)
            .unwrap()
            .remove(0);
        assert_eq!(codex_session_for_task(&done).as_deref(), Some(session_id));
        attach_codex_session_after_run(&job, session_id, "success").unwrap();
        assert_eq!(
            task_status_for_codex_session(&root, session_id).unwrap(),
            Some(TaskStatus::Done)
        );
        let scan = scan_agent_project(&root);
        assert_eq!(scan.available_todo_count(), 1);
        assert!(scan.has_pending_task());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_session_without_its_marker_does_not_guess_an_unrelated_doing_task() {
        let root = temp_root("live-exact-session-missing-marker");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/doing.md"),
            "# Doing Tasks\n- unrelated active task\n",
        )
        .unwrap();

        assert!(
            !attach_codex_session_to_active_task(
                &root,
                AgentTaskSelection::ResumeSession,
                &[],
                &[],
                "session-live",
            )
            .unwrap()
        );
        let doing = read_task_entries(&get_tasks_dir(&root), TaskStatus::Doing).unwrap();
        assert!(codex_session_for_task(&doing[0]).is_none());

        let mut project = tui_agent_project_for_test(1, "project").project;
        project.path = root.clone();
        let job = AgentRunJob {
            state_dir: root.join("state/clt"),
            project,
            holder: "holder".to_string(),
            worker_token: None,
            max_global_jobs: 12,
            task_selection: AgentTaskSelection::ResumeSession,
            resume_session_id: Some("session-live".to_string()),
            blocked_task_count_before: 0,
            done_task_contents_before: Vec::new(),
            blocked_task_snapshots_before: Vec::new(),
        };
        assert!(attach_codex_session_after_run(&job, "session-live", "success").is_err());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_session_marker_prevents_top_level_reattachment() {
        let root = temp_root("nested-live-session-marker");
        init_tasks(&root, true).unwrap();
        let epic = root.join("tasks/doing/0001-epic");
        fs::create_dir_all(&epic).unwrap();
        fs::write(epic.join("task.md"), "Parent epic.\n").unwrap();
        fs::write(
            epic.join("done.md"),
            "# Done Tasks\n- nested task codex:session-live\n",
        )
        .unwrap();
        fs::write(
            root.join("tasks/doing/0002-unrelated.md"),
            "Unrelated active task.\n",
        )
        .unwrap();

        assert_eq!(
            task_status_for_codex_session(&root, "session-live").unwrap(),
            Some(TaskStatus::Done)
        );
        assert!(
            attach_codex_session_to_active_task(
                &root,
                AgentTaskSelection::ResumeSession,
                &[],
                &[],
                "session-live",
            )
            .unwrap()
        );
        let unrelated = fs::read_to_string(root.join("tasks/doing/0002-unrelated.md")).unwrap();
        assert!(!unrelated.contains("codex:session-live"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn blocked_recovery_does_not_guess_among_multiple_pre_run_candidates() {
        let root = temp_root("blocked-live-session-ambiguous");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/doing.md"),
            "# Doing Tasks\n- blocked doing — BLOCKED 2026-08-25: waiting\n",
        )
        .unwrap();
        fs::write(
            root.join("tasks/todo.md"),
            "# Todo Tasks\n- blocked todo — BLOCKED 2026-08-25: waiting\n",
        )
        .unwrap();
        let doing_before = task_contents_for_status(&root, TaskStatus::Doing).unwrap();
        let blocked_before = blocked_task_snapshots(&root).unwrap();

        assert!(
            !attach_codex_session_to_active_task(
                &root,
                AgentTaskSelection::RecoverBlocked,
                &doing_before,
                &blocked_before,
                "session-live",
            )
            .unwrap()
        );
        let doing = read_task_entries(&get_tasks_dir(&root), TaskStatus::Doing).unwrap();
        assert!(codex_session_for_task(&doing[0]).is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_session_attachment_does_not_guess_between_multiple_new_doing_tasks() {
        let root = temp_root("live-session-multiple-doing");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/doing.md"),
            "# Doing Tasks\n- first new task\n- second new task\n",
        )
        .unwrap();

        assert!(
            !attach_codex_session_to_active_task(
                &root,
                AgentTaskSelection::NextTodo,
                &[],
                &[],
                "session-live",
            )
            .unwrap()
        );
        assert!(
            !fs::read_to_string(root.join("tasks/doing.md"))
                .unwrap()
                .contains("codex:session-live")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interactive_codex_resume_accepts_doing_done_and_currently_blocked_todo_tasks() {
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

        assert!(task_supports_interactive_codex_resume(
            TaskStatus::Done,
            &done
        ));
        assert!(task_supports_interactive_codex_resume(
            TaskStatus::Todo,
            &blocked
        ));
        assert!(task_supports_interactive_codex_resume(
            TaskStatus::Doing,
            &blocked
        ));
        assert!(task_supports_interactive_codex_resume(
            TaskStatus::Doing,
            &unblocked
        ));
        assert!(!task_supports_interactive_codex_resume(
            TaskStatus::Todo,
            &unblocked
        ));
        assert!(!task_supports_interactive_codex_resume(
            TaskStatus::Backlog,
            &blocked
        ));
    }

    #[test]
    fn interactive_resume_revalidation_rejects_an_unblocked_or_duplicate_session_task() {
        let root = temp_root("interactive-session-task-revalidation");
        init_tasks(&root, false).unwrap();
        fs::write(
            root.join("tasks/done.md"),
            "# Done Tasks\n- resumable task codex:session-123\n",
        )
        .unwrap();
        assert!(codex_session_task_supports_interactive_resume(&root, "session-123").unwrap());

        fs::write(root.join("tasks/done.md"), "# Done Tasks\n").unwrap();
        fs::write(
            root.join("tasks/doing.md"),
            "# Doing Tasks\n- interrupted task codex:session-123\n",
        )
        .unwrap();
        assert!(codex_session_task_supports_interactive_resume(&root, "session-123").unwrap());

        fs::write(root.join("tasks/doing.md"), "# Doing Tasks\n").unwrap();
        fs::write(
            root.join("tasks/todo.md"),
            "# Todo Tasks\n- now unblocked codex:session-123\n",
        )
        .unwrap();
        assert!(!codex_session_task_supports_interactive_resume(&root, "session-123").unwrap());

        fs::write(
            root.join("tasks/done.md"),
            "# Done Tasks\n- duplicate codex:session-123\n",
        )
        .unwrap();
        assert!(!codex_session_task_supports_interactive_resume(&root, "session-123").unwrap());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interactive_c_busy_check_honors_persisted_session_fences_without_a_live_log() {
        let root = temp_root("interactive-c-session-fence");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let project_id = project.id;
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Interactive,
            )
            .unwrap();
        let mut panel = TuiAgentPanel {
            projects: vec![TuiAgentProject {
                project,
                scan: AgentProjectScan::empty(),
                runtime_state: TuiAgentRuntimeState::Idle,
                daemon_scan_problem: None,
                failure_problem: None,
            }],
            current_project_registration: None,
            daemon_status: "running".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));

        assert_eq!(
            tui_codex_session_availability_for_path_at(
                &mut panel,
                &project_root,
                "session-123",
                &state_dir,
            )
            .unwrap(),
            TuiCodexSessionAvailability::SelectedSessionBusy
        );
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        assert_eq!(
            tui_codex_session_availability_for_path_at(
                &mut panel,
                &project_root,
                "session-123",
                &state_dir,
            )
            .unwrap(),
            TuiCodexSessionAvailability::Idle
        );
        assert_eq!(
            tui_codex_session_availability_for_path_at(
                &mut panel,
                &project_root,
                "different-session",
                &state_dir,
            )
            .unwrap(),
            TuiCodexSessionAvailability::Idle
        );
        store
            .set_session_control_state_blocking(
                project_id,
                "session-other",
                AgentSessionControlState::Running,
            )
            .unwrap();
        assert_eq!(
            tui_codex_session_availability_for_path_at(
                &mut panel,
                &project_root,
                "session-123",
                &state_dir,
            )
            .unwrap(),
            TuiCodexSessionAvailability::ProjectBusy
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writable_shared_interactive_reservation_coexists_with_another_active_session() {
        let root = temp_root("writable-shared-interactive-reservation");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let project_id = project.id;
        let active_holder = "clt-worker-active";
        assert!(
            store
                .try_acquire_lease_blocking(project_id, active_holder, "100", "9999999999")
                .unwrap()
        );
        store
            .set_session_control_state_blocking(
                project_id,
                "session-active",
                AgentSessionControlState::Running,
            )
            .unwrap();

        let requester = InteractiveAgentLease::holder_for_shared_session(false);
        assert!(
            store
                .reserve_shared_session_interactive_blocking(
                    project_id,
                    "session-blocked",
                    &requester,
                    None,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .unwrap()
                .holder,
            active_holder
        );

        let guardian =
            interactive_guardian_holder(InteractiveGuardianDisposition::PreserveSharedSession);
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-blocked"),
                    &requester,
                    &guardian,
                    60,
                )
                .unwrap()
        );
        assert!(
            store
                .register_interactive_guardian_child_blocking(
                    project_id,
                    "session-blocked",
                    &guardian,
                    std::process::id(),
                    60,
                )
                .unwrap()
        );
        let message =
            toggle_tui_codex_session_stop_at(&state_dir, project_id, "session-blocked").unwrap();
        assert!(message.starts_with("Stopping this interactive Codex session safely"));
        let stopping = store
            .session_control_blocking(project_id, "session-blocked")
            .unwrap()
            .unwrap();
        assert_eq!(stopping.state, AgentSessionControlState::StopRequested);
        assert!(interactive_guardian_stop_requested(
            &stopping,
            std::process::id()
        ));
        let mut panel = TuiAgentPanel {
            projects: vec![TuiAgentProject {
                project,
                scan: AgentProjectScan::empty(),
                runtime_state: TuiAgentRuntimeState::Interactive,
                daemon_scan_problem: None,
                failure_problem: None,
            }],
            current_project_registration: None,
            daemon_status: "running".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));
        assert_eq!(
            selected_tui_agent_session_target_at(&panel, &state_dir)
                .unwrap()
                .session_id,
            "session-blocked"
        );
        assert!(
            store
                .finish_interactive_guardian_blocking(
                    project_id,
                    "session-blocked",
                    &guardian,
                    InteractiveGuardianDisposition::PreserveSharedSession,
                )
                .unwrap()
        );
        let preserved = store
            .session_control_blocking(project_id, "session-blocked")
            .unwrap()
            .unwrap();
        assert_eq!(preserved.state, AgentSessionControlState::Stopped);
        assert!(preserved.interactive_holder.is_none());
        assert_eq!(
            store
                .session_control_blocking(project_id, "session-active")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Running
        );
        assert_eq!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .unwrap()
                .holder,
            active_holder
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writable_shared_interactive_reservation_rejects_durable_git_boundaries() {
        let root = temp_root("writable-shared-interactive-git-boundaries");
        let state_dir = root.join("state/clt");
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let requester = InteractiveAgentLease::holder_for_shared_session(false);

        let launch_root = root.join("launch-project");
        fs::create_dir_all(&launch_root).unwrap();
        let launch_root = fs::canonicalize(launch_root).unwrap();
        store
            .register_project_blocking(&launch_root, "launch-project")
            .unwrap();
        let launch_project = store
            .list_projects_blocking()
            .unwrap()
            .into_iter()
            .find(|project| project.path == launch_root)
            .unwrap();
        store
            .set_session_control_state_blocking(
                launch_project.id,
                "session-active-launch",
                AgentSessionControlState::Running,
            )
            .unwrap();
        assert!(
            store
                .record_git_launch_state_blocking(
                    launch_project.id,
                    "launch-boundary-token",
                    AgentGitMode::Commit,
                    &AgentGitStartState {
                        starting_head: "1111111111111111111111111111111111111111".to_string(),
                        branch_ref: Some("refs/heads/master".to_string()),
                        upstream_ref: None,
                        worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#.to_string(),
                    },
                    "100",
                )
                .unwrap()
        );
        assert!(
            !store
                .reserve_shared_session_interactive_blocking(
                    launch_project.id,
                    "session-shared-launch",
                    &requester,
                    None,
                )
                .unwrap()
        );

        let finalization_root = root.join("finalization-project");
        fs::create_dir_all(&finalization_root).unwrap();
        let finalization_root = fs::canonicalize(finalization_root).unwrap();
        store
            .register_project_blocking(&finalization_root, "finalization-project")
            .unwrap();
        let finalization_project = store
            .list_projects_blocking()
            .unwrap()
            .into_iter()
            .find(|project| project.path == finalization_root)
            .unwrap();
        store
            .set_session_control_state_blocking(
                finalization_project.id,
                "session-active-finalization",
                AgentSessionControlState::Running,
            )
            .unwrap();
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: finalization_project.id,
                    codex_session_id: "session-working-finalization",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/master"),
                    upstream_ref: None,
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: None,
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
        assert!(
            !store
                .reserve_shared_session_interactive_blocking(
                    finalization_project.id,
                    "session-shared-finalization",
                    &requester,
                    None,
                )
                .unwrap()
        );

        let lease_root = root.join("lease-project");
        fs::create_dir_all(&lease_root).unwrap();
        let lease_root = fs::canonicalize(lease_root).unwrap();
        store
            .register_project_blocking(&lease_root, "lease-project")
            .unwrap();
        assert!(
            store
                .set_project_git_mode_for_path_blocking(&lease_root, AgentGitMode::Commit)
                .unwrap()
        );
        let lease_project = store
            .list_projects_blocking()
            .unwrap()
            .into_iter()
            .find(|project| project.path == lease_root)
            .unwrap();
        assert!(
            store
                .try_acquire_lease_blocking(lease_project.id, "git-owner", "100", "999")
                .unwrap()
        );
        assert!(
            !store
                .reserve_shared_session_interactive_blocking(
                    lease_project.id,
                    "session-shared-lease",
                    &requester,
                    None,
                )
                .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelling_shared_resume_restores_a_stopped_session() {
        let root = temp_root("shared-stopped-reservation");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .set_session_control_state_blocking(
                project_id,
                "session-active",
                AgentSessionControlState::Running,
            )
            .unwrap();
        store
            .set_session_control_state_blocking(
                project_id,
                "session-stopped",
                AgentSessionControlState::Stopped,
            )
            .unwrap();

        let requester = InteractiveAgentLease::holder_for_shared_session(true);
        assert!(
            store
                .reserve_shared_session_interactive_blocking(
                    project_id,
                    "session-stopped",
                    &requester,
                    None,
                )
                .unwrap()
        );
        assert!(
            store
                .cancel_idle_session_interactive_blocking(
                    project_id,
                    "session-stopped",
                    &requester,
                )
                .unwrap()
        );
        let stopped = store
            .session_control_blocking(project_id, "session-stopped")
            .unwrap()
            .unwrap();
        assert_eq!(stopped.state, AgentSessionControlState::Stopped);
        assert!(stopped.interactive_holder.is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn newly_added_task_is_not_guessed_when_duplicate_content_is_ambiguous() {
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

        assert!(newly_added_task_entry(&before, &after).is_none());
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
        let run = agent::AgentRunRecord {
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
            codex_session_id: Some("session-recorded".to_string()),
        };

        assert_eq!(
            preferred_recorded_agent_output_path(&run),
            Some(stderr_path)
        );

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
        fs::write(&stderr_path, "session id: session-live\nstarted\n").unwrap();

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
        assert!(tui_agent_log_title(&log_view).contains("s/i/c controls"));
        assert_eq!(log_view.content, "session id: session-live\nstarted\n");
        assert_eq!(
            viewed_tui_codex_session_target(Some(&log_view)).unwrap(),
            TuiCodexSessionTarget {
                project_id: 1,
                project_path: panel.projects[0].project.path.clone(),
                session_id: "session-live".to_string(),
            }
        );

        append_agent_log_line(&stderr_path, "still working").unwrap();
        log_view.refresh().unwrap();
        assert!(log_view.content.contains("still working"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fenced_agent_log_view_keeps_the_orphaned_session_controllable() {
        let root = temp_root("agent-fenced-output");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "fenced-project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let stdout_path = root.join("fenced.out");
        let stderr_path = root.join("fenced.err");
        fs::write(&stdout_path, "").unwrap();
        fs::write(
            &stderr_path,
            "session id: session-fenced\nwork survived its supervisor\n",
        )
        .unwrap();
        store
            .mark_session_running_blocking(
                project.id,
                "session-fenced",
                4242,
                "orphaned-run-token",
                &stdout_path,
                &stderr_path,
            )
            .unwrap();

        assert_eq!(
            store.suspending_session_project_ids_blocking().unwrap(),
            HashSet::from([project.id])
        );
        let mut panel = TuiAgentPanel {
            projects: vec![TuiAgentProject {
                project,
                scan: AgentProjectScan::empty(),
                runtime_state: TuiAgentRuntimeState::Fenced,
                daemon_scan_problem: None,
                failure_problem: None,
            }],
            current_project_registration: None,
            daemon_status: "running".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));

        let log_view = selected_tui_agent_log_view_at(&panel, &state_dir)
            .unwrap()
            .unwrap();
        assert!(log_view.is_live);
        assert!(log_view.content.contains("work survived its supervisor"));
        let target = viewed_tui_codex_session_target(Some(&log_view)).unwrap();
        assert_eq!(target.session_id, "session-fenced");
        assert!(tui_agent_log_title(&log_view).contains("s/i/c controls"));

        let message =
            toggle_tui_codex_session_stop_at(&state_dir, target.project_id, &target.session_id)
                .unwrap();
        assert!(message.starts_with("Stopping this Codex task session"));
        assert_eq!(
            store
                .session_control_blocking(target.project_id, &target.session_id)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::StopRequested
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn kanban_agent_log_view_uses_the_active_project_for_selected_doing_task() {
        let root = temp_root("kanban-agent-log");
        let state_dir = root.join("state/clt");
        let mut alpha = tui_agent_project_for_test(1, "alpha");
        alpha.runtime_state = TuiAgentRuntimeState::Running;
        let active_path = alpha.project.path.clone();
        let log_dir = agent_project_run_log_dir(&state_dir, &alpha.project).unwrap();
        fs::create_dir_all(&log_dir).unwrap();
        fs::write(
            log_dir.join("200-000-p1-1.err"),
            "session id: session-live\nalpha is working\n",
        )
        .unwrap();

        let mut panel = TuiAgentPanel {
            projects: vec![alpha, tui_agent_project_for_test(2, "beta")],
            current_project_registration: None,
            daemon_status: "running".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(1));
        let task = task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 1 },
            "Current task",
            "Current task codex:session-live",
            false,
        );

        let log_view = selected_tui_task_log_view_for_path_at(
            &mut panel,
            &active_path,
            TaskStatus::Doing,
            &task,
            &state_dir,
        )
        .unwrap()
        .unwrap();

        assert_eq!(panel.selected_project().unwrap().project.name, "alpha");
        assert_eq!(log_view.project_name, "alpha");
        assert!(log_view.content.contains("alpha is working"));
        assert!(log_view.is_live);

        panel.select_next();
        let project_log_view = selected_tui_task_or_project_log_view_for_path_at(
            &mut panel,
            &active_path,
            TaskStatus::Doing,
            None,
            &state_dir,
        )
        .unwrap()
        .unwrap();
        assert_eq!(panel.selected_project().unwrap().project.name, "alpha");
        assert!(project_log_view.content.contains("alpha is working"));
        assert!(project_log_view.is_live);

        let completed_view = selected_tui_task_log_view_for_path_at(
            &mut panel,
            &active_path,
            TaskStatus::Done,
            &task,
            &state_dir,
        )
        .unwrap()
        .unwrap();
        assert!(completed_view.is_live);
        assert_eq!(
            tui_codex_session_availability_for_path_at(
                &mut panel,
                &active_path,
                "session-live",
                &state_dir,
            )
            .unwrap(),
            TuiCodexSessionAvailability::SelectedSessionBusy
        );
        assert_eq!(
            tui_codex_session_availability_for_path_at(
                &mut panel,
                &active_path,
                "different-session",
                &state_dir,
            )
            .unwrap(),
            TuiCodexSessionAvailability::ProjectBusy
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn open_kanban_agent_log_follows_the_selected_task() {
        let root = temp_root("kanban-agent-log-follows-task");
        let state_dir = root.join("state/clt");
        let project_root = root.join("alpha");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "alpha")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        let first_stdout = root.join("first.out");
        let second_stdout = root.join("second.out");
        fs::write(&first_stdout, "first task output").unwrap();
        fs::write(&second_stdout, "second task output").unwrap();
        for (started_at, session_id, stdout_path) in [
            ("100", "session-one", &first_stdout),
            ("200", "session-two", &second_stdout),
        ] {
            store
                .record_run_outcome_blocking(agent::AgentRunOutcome {
                    project_id: project.id,
                    status: "success",
                    started_at,
                    finished_at: Some(started_at),
                    exit_code: Some(0),
                    log_dir: Some(root.to_str().unwrap()),
                    stdout_path: Some(stdout_path.to_str().unwrap()),
                    stderr_path: None,
                    summary: Some("completed"),
                    codex_session_id: Some(session_id),
                })
                .unwrap();
        }

        let mut panel = TuiAgentPanel {
            projects: vec![TuiAgentProject {
                project,
                scan: AgentProjectScan::empty(),
                runtime_state: TuiAgentRuntimeState::Idle,
                daemon_scan_problem: None,
                failure_problem: None,
            }],
            current_project_registration: None,
            daemon_status: "not-installed".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));
        let first_task = task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 1 },
            "First task",
            "First task codex:session-one",
            false,
        );
        let second_task = task_entry_from_text(
            TaskSource::MarkdownLine { line_index: 2 },
            "Second task",
            "Second task codex:session-two",
            false,
        );

        let mut log_view = selected_tui_task_log_view_for_path_at(
            &mut panel,
            &project_root,
            TaskStatus::Done,
            &first_task,
            &state_dir,
        )
        .unwrap();
        assert_eq!(log_view.as_ref().unwrap().content, "first task output");

        sync_open_tui_task_log_view_at(
            &mut panel,
            &project_root,
            TaskStatus::Done,
            Some(&second_task),
            &mut log_view,
            &state_dir,
        );

        assert_eq!(log_view.as_ref().unwrap().content, "second task output");

        sync_open_tui_task_log_view_at(
            &mut panel,
            &project_root,
            TaskStatus::Done,
            None,
            &mut log_view,
            &state_dir,
        );

        let project_log_view = log_view.unwrap();
        assert_eq!(project_log_view.content, "second task output");
        assert!(!project_log_view.is_live);
        assert_eq!(
            project_log_view
                .session_target
                .as_ref()
                .map(|target| target.session_id.as_str()),
            Some("session-two")
        );

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&alpha_root, "alpha")
            .unwrap();
        store.register_project_blocking(&beta_root, "beta").unwrap();

        let projects = store.list_projects_blocking().unwrap();
        for project in &projects {
            let stdout_path = root.join(format!("{}.out", project.name));
            fs::write(&stdout_path, format!("{} output", project.name)).unwrap();
            store
                .record_run_outcome_blocking(agent::AgentRunOutcome {
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
                    daemon_scan_problem: None,
                    failure_problem: None,
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

        let active_lease = agent::AgentLeaseRecord {
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

        let interactive_lease = agent::AgentLeaseRecord {
            project_id: 1,
            project_name: "alpha".to_string(),
            project_path: PathBuf::from("/tmp/alpha"),
            holder: InteractiveAgentLease::holder_for_idle_session(),
            acquired_at: "100".to_string(),
            expires_at: "200".to_string(),
        };
        assert_eq!(
            tui_agent_runtime_state(1, &[interactive_lease]),
            TuiAgentRuntimeState::Fenced
        );

        let stale_interactive_lease = agent::AgentLeaseRecord {
            project_id: 1,
            project_name: "alpha".to_string(),
            project_path: PathBuf::from("/tmp/alpha"),
            holder: format!("clt-idle-interactive-worker-{}-1-1", u32::MAX),
            acquired_at: "100".to_string(),
            expires_at: "9999999999".to_string(),
        };
        assert_eq!(
            tui_agent_runtime_state(1, &[stale_interactive_lease]),
            TuiAgentRuntimeState::Stale
        );
    }

    #[test]
    fn agent_project_table_surfaces_external_daemon_scan_errors() {
        let mut item = tui_agent_project_for_test(1, "fishdome");
        item.project.path = PathBuf::from("/Volumes/External/FISHDOME");
        item.project.last_daemon_scan_status = Some("unavailable".to_string());
        item.project.last_daemon_scan_error =
            Some("Operation not permitted (os error 1)".to_string());
        item.daemon_scan_problem = tui_agent_daemon_scan_problem(&item.project);
        item.runtime_state = TuiAgentRuntimeState::Error;

        let codex_width = agent_codex_column_width(std::slice::from_ref(&item), false);
        let project_width =
            agent_project_column_width(std::slice::from_ref(&item), None, 160, codex_width);
        let row = format_agent_project_table_row(0, &item, 160, project_width, codex_width, false);
        let mut panel = TuiAgentPanel {
            projects: vec![item],
            current_project_registration: None,
            daemon_status: "service active".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));
        let (console, color) = tui_console_content(true, &panel, None, "instructions");

        assert!(row.contains("ERROR"));
        assert!(row.contains("External project scan failed"));
        assert!(console.contains("Full Disk Access"));
        assert!(console.contains("restart the agent"));
        assert_eq!(color, Color::LightRed);
    }

    #[test]
    fn agent_project_table_surfaces_failed_run_reason_and_retry_guidance() {
        let mut item = tui_agent_project_for_test(13, "chitty");
        item.project.git_mode = AgentGitMode::CommitAndPush;
        item.project.last_failure_at = Some("100".to_string());
        item.project.failure_count = 1;
        item.scan = AgentProjectScan::pending(2);
        let latest_run = agent::AgentRunRecord {
            id: 2049,
            project_id: item.project.id,
            project_name: item.project.name.clone(),
            project_path: item.project.path.clone(),
            status: "failure".to_string(),
            started_at: "99".to_string(),
            finished_at: Some("100".to_string()),
            exit_code: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some(
                "Codex runner failed before completion: Todo candidate is not committed exactly once at the frozen task boundary"
                    .to_string(),
            ),
            codex_session_id: None,
        };
        item.failure_problem = tui_agent_failure_problem(
            &item.project,
            Some(&latest_run),
            250,
            Duration::from_secs(300),
        );
        item.runtime_state = TuiAgentRuntimeState::Error;

        let codex_width = agent_codex_column_width(std::slice::from_ref(&item), false);
        let project_width =
            agent_project_column_width(std::slice::from_ref(&item), None, 180, codex_width);
        let row = format_agent_project_table_row(0, &item, 180, project_width, codex_width, false);
        let mut panel = TuiAgentPanel {
            projects: vec![item],
            current_project_registration: None,
            daemon_status: "service active".to_string(),
            state: ListState::default(),
            scroll_offset: 0,
            last_error: None,
        };
        panel.state.select(Some(0));
        let (console, color) = tui_console_content(true, &panel, None, "instructions");

        assert!(row.contains("ERROR"));
        assert!(row.contains("Last agent run failed"));
        assert!(console.contains("Automatic retry in 150s"));
        assert!(console.contains("checkpoints dirty Todo definitions automatically"));
        assert!(console.contains("Press r"));
        assert_eq!(color, Color::LightRed);
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
        let entries = read_task_entries(&tasks_dir, TaskStatus::Todo).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].summary, "first task");
        assert_eq!(entries[1].summary, "second task");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expand_tasks_without_status_expands_all_statuses() {
        let root = temp_root("expand-all");
        add_task(&root, "todo task", None).unwrap();
        move_task(&root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();

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
    fn insert_subtask_expands_markdown_parent_and_reuses_nested_board() {
        let root = temp_root("insert-subtask-markdown");
        add_task(&root, "Ship dashboard", Some("FEATURE".to_string())).unwrap();
        add_task(&root, "Keep sibling", None).unwrap();
        let tasks_dir = root.join("tasks");
        let expected_parent = task_entry_at(&tasks_dir, TaskStatus::Todo, 1).unwrap();

        let subtask_board = insert_subtask_in_board(
            &tasks_dir,
            TaskStatus::Todo,
            1,
            &expected_parent,
            "Draft dashboard spec",
            Some("DOCS".to_string()),
        )
        .unwrap();

        assert!(tasks_dir.join("todo").is_dir());
        assert!(tasks_dir.join("todo.md.bak").is_file());
        let parent_entries = read_task_entries(&tasks_dir, TaskStatus::Todo).unwrap();
        assert_eq!(parent_entries.len(), 2);
        assert_eq!(parent_entries[0].summary, "Ship dashboard");
        assert_eq!(parent_entries[0].metadata.as_deref(), Some("FEATURE"));
        assert!(parent_entries[0].has_subtasks);
        assert_eq!(parent_entries[1].summary, "Keep sibling");
        assert_eq!(
            fs::read_to_string(subtask_board.join("task.md")).unwrap(),
            "Ship dashboard (FEATURE)\n"
        );
        assert_eq!(
            read_tasks_in_board(&subtask_board, TaskStatus::Todo).unwrap(),
            vec!["- Draft dashboard spec (DOCS)"]
        );

        let expected_parent = task_entry_at(&tasks_dir, TaskStatus::Todo, 1).unwrap();
        let reused_board = insert_subtask_in_board(
            &tasks_dir,
            TaskStatus::Todo,
            1,
            &expected_parent,
            "Build dashboard",
            None,
        )
        .unwrap();

        assert_eq!(reused_board, subtask_board);
        assert_eq!(
            read_tasks_in_board(&subtask_board, TaskStatus::Todo).unwrap(),
            vec!["- Draft dashboard spec (DOCS)", "- Build dashboard"]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn insert_subtask_preserves_folder_backed_parent_detail() {
        let root = temp_root("insert-subtask-folder");
        init_tasks(&root, true).unwrap();
        let tasks_dir = root.join("tasks");
        let parent_path = tasks_dir.join("doing/0001-research-api.md");
        let parent_content =
            "Research the API. Keep detailed notes.\n\n- Audit callers\n- Draft rollout\n";
        fs::write(&parent_path, parent_content).unwrap();
        let expected_parent = task_entry_at(&tasks_dir, TaskStatus::Doing, 1).unwrap();

        let subtask_board = insert_subtask_in_board(
            &tasks_dir,
            TaskStatus::Doing,
            1,
            &expected_parent,
            "Audit callers",
            None,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(subtask_board.join("task.md")).unwrap(),
            parent_content
        );
        assert_eq!(
            read_tasks_in_board(&subtask_board, TaskStatus::Todo).unwrap(),
            vec!["- Audit callers"]
        );
        assert!(!tasks_dir.join("doing.md.bak").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn insert_subtask_rejects_parent_changed_while_prompt_was_open() {
        let root = temp_root("insert-subtask-stale-parent");
        add_task(&root, "Original parent", None).unwrap();
        let tasks_dir = root.join("tasks");
        let expected_parent = task_entry_at(&tasks_dir, TaskStatus::Todo, 1).unwrap();
        update_task_in_board(&tasks_dir, TaskStatus::Todo, 1, "Changed parent").unwrap();

        let error = insert_subtask_in_board(
            &tasks_dir,
            TaskStatus::Todo,
            1,
            &expected_parent,
            "Must not attach",
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("Parent task changed"));
        assert!(tasks_dir.join("todo.md").is_file());
        assert!(!tasks_dir.join("todo").exists());

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
        assert_eq!(state.feedback_buffer, tui_task_board_instructions());
    }

    #[test]
    fn tui_task_board_instructions_only_describe_task_page_controls() {
        let instructions = tui_task_board_instructions();

        assert!(instructions.contains("Space creates a task"));
        assert!(instructions.contains("n or + creates a subtask under the selected task"));
        assert!(instructions.contains("e edits"));
        assert!(instructions.contains("Codex: s stops/resumes"));
        assert!(instructions.contains("i interrupts for interaction"));
        assert!(instructions.contains("c opens linked idle Doing, completed, or blocked sessions"));
        assert!(instructions.contains("l shows logs"));
        assert!(instructions.contains("Press r to reorganize"));
        assert!(instructions.contains("Tab opens Agent Projects"));
        assert!(!instructions.contains("toggles ON/OFF"));
        assert!(!instructions.contains("cycles Git"));
        assert!(!instructions.contains("cycles the selected target"));
        assert!(!instructions.contains("toggles fast"));
        assert!(!instructions.contains("cycles thinking"));
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
    fn unit_test_agent_state_dir_is_isolated_from_user_defaults() {
        let state_dir = agent_state_dir().unwrap();

        assert_eq!(state_dir, isolated_unit_test_agent_state_dir());
        assert!(state_dir.starts_with(std::env::temp_dir()));
        assert_ne!(
            state_dir,
            resolve_agent_state_dir(
                AgentPlatform::Linux,
                None,
                None,
                Some(PathBuf::from("/home/alex")),
            )
            .unwrap()
        );
        assert_ne!(
            state_dir,
            resolve_agent_state_dir(
                AgentPlatform::Macos,
                None,
                None,
                Some(PathBuf::from("/Users/alex")),
            )
            .unwrap()
        );
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
    fn automated_agent_lease_renewal_interval_is_bounded_and_frequent() {
        assert_eq!(
            agent_lease_renew_interval(Duration::from_secs(90)),
            Duration::from_millis(AGENT_LEASE_RENEW_MAX_INTERVAL_MILLIS)
        );
        assert_eq!(
            agent_lease_renew_interval(Duration::from_secs(3)),
            Duration::from_secs(1)
        );
        assert_eq!(
            agent_lease_renew_interval(Duration::from_millis(300)),
            Duration::from_millis(250)
        );
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
        let project = agent::AgentProject {
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
            last_daemon_scan_status: None,
            last_daemon_scan_error: None,
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
    fn agent_daemon_rebuilds_a_damaged_active_worker_index_once_and_retries() {
        let operation_calls = Cell::new(0);
        let rebuild_calls = Cell::new(0);

        let result = run_agent_daemon_database_operation_with_recovery(
            || {
                operation_calls.set(operation_calls.get() + 1);
                if operation_calls.get() == 1 {
                    anyhow::bail!(
                        "Failed to abandon worker token: IdxDelete: no matching index entry found for key"
                    );
                }
                Ok("recovered")
            },
            || {
                rebuild_calls.set(rebuild_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(result, "recovered");
        assert_eq!(operation_calls.get(), 2);
        assert_eq!(rebuild_calls.get(), 1);
    }

    #[test]
    fn agent_daemon_does_not_rebuild_indexes_for_unrelated_errors() {
        let rebuild_calls = Cell::new(0);

        let result: Result<()> = run_agent_daemon_database_operation_with_recovery(
            || anyhow::bail!("Failed to scan project"),
            || {
                rebuild_calls.set(rebuild_calls.get() + 1);
                Ok(())
            },
        );
        let error = result.unwrap_err();

        assert!(error.to_string().contains("Failed to scan project"));
        assert_eq!(rebuild_calls.get(), 0);
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
        let interactive_holder = format!("clt-interactive-{}", std::process::id());
        assert_eq!(
            agent_lease_holder_pid(&interactive_holder),
            Some(std::process::id())
        );
        assert_eq!(
            agent_lease_holder_liveness(&agent_scheduler_lease_holder()),
            AgentLeaseHolderLiveness::CurrentProcess
        );
        assert_eq!(agent_lease_holder_pid("external-agent"), None);
    }

    #[cfg(unix)]
    #[test]
    fn local_process_probe_handles_current_and_unrepresentable_pids() {
        assert_eq!(local_process_is_running(std::process::id()), Some(true));
        assert_eq!(local_process_is_running(u32::MAX), Some(false));
    }

    #[test]
    fn generated_interactive_holders_are_reclaimed_by_full_token_ttl_not_pid() {
        for holder in [
            InteractiveAgentLease::holder_for_current_process(),
            InteractiveAgentLease::holder_for_idle_session(),
            InteractiveAgentLease::holder_for_stopped_session(),
            interactive_guardian_holder(InteractiveGuardianDisposition::ResumeExec),
            interactive_guardian_holder(InteractiveGuardianDisposition::PreserveIdleSession),
            interactive_guardian_holder(InteractiveGuardianDisposition::RestoreStopped),
        ] {
            assert_eq!(agent_lease_holder_pid(&holder), None);
            assert_eq!(
                interactive_lease_holder_pid(&holder),
                Some(std::process::id())
            );
            assert_eq!(
                interactive_lease_holder_liveness(&holder),
                Some(AgentLeaseHolderLiveness::CurrentProcess)
            );
            let lease = agent::AgentLeaseRecord {
                project_id: 1,
                project_name: "project".to_string(),
                project_path: PathBuf::from("/tmp/project"),
                holder,
                acquired_at: "100".to_string(),
                expires_at: "200".to_string(),
            };

            assert!(!agent_lease_is_reclaimable(&lease, true, 199));
            assert!(agent_lease_is_reclaimable(&lease, false, 200));
        }
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
        let mut project = agent::AgentProject {
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
            last_daemon_scan_status: None,
            last_daemon_scan_error: None,
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
        let project = agent::AgentProject {
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
            last_daemon_scan_status: None,
            last_daemon_scan_error: None,
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

    #[cfg(unix)]
    #[test]
    fn agent_service_binary_snapshot_is_executable_and_immutable() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("agent-service-binary-snapshot");
        let state_dir = root.join("state/clt");
        let source = root.join("installed clt");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(&source, b"generation one").unwrap();
        let mut permissions = fs::metadata(&source).unwrap().permissions();
        permissions.set_mode(0o751);
        fs::set_permissions(&source, permissions).unwrap();

        let snapshot = snapshot_agent_service_binary(&state_dir, &source).unwrap();

        assert!(snapshot.starts_with(state_dir.join("worker-generations")));
        assert_eq!(snapshot.file_name(), Some(OsStr::new("clt")));
        assert_eq!(fs::read(&snapshot).unwrap(), b"generation one");
        assert_eq!(
            fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
            0o751
        );
        assert!(!snapshot.with_file_name("clt.partial").exists());

        fs::write(&source, b"generation two").unwrap();
        assert_eq!(fs::read(&snapshot).unwrap(), b"generation one");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_service_restart_refuses_a_live_legacy_owned_run() {
        let root = temp_root("agent-legacy-restart-fence");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let holder = format!("clt-agent-{}", std::process::id());
        assert!(
            store
                .try_acquire_lease_blocking(
                    project.id,
                    &holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );

        let error = ensure_no_live_legacy_agent_runs(&store).unwrap_err();
        assert!(error.to_string().contains("legacy in-process run"));
        assert!(store.release_lease_blocking(project.id, &holder).unwrap());
        ensure_no_live_legacy_agent_runs(&store).unwrap();

        let scheduler_holder = agent_scheduler_lease_holder();
        assert!(
            store
                .try_acquire_lease_blocking(
                    project.id,
                    &scheduler_holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );
        ensure_no_live_legacy_agent_runs(&store).unwrap();

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_binary_generation_gc_preserves_scheduler_and_active_worker_snapshots() {
        let root = temp_root("agent-generation-gc");
        let state_dir = root.join("state/clt");
        let generation_root = state_dir.join("worker-generations");
        let scheduler_dir = generation_root.join("scheduler");
        let worker_dir = generation_root.join("active-worker");
        let stale_dir = generation_root.join("stale");
        for directory in [&scheduler_dir, &worker_dir, &stale_dir] {
            fs::create_dir_all(directory).unwrap();
            fs::write(directory.join("clt"), b"snapshot").unwrap();
        }
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "999")
                .unwrap()
        );
        assert!(
            store
                .reserve_worker_blocking(agent::AgentWorkerReservation {
                    project_id: project.id,
                    worker_token: "generation-token",
                    expected_lease_holder: "scheduler",
                    max_active_workers: 12,
                    protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                    service_label: "clt-worker-generation-token",
                    binary_path: &worker_dir.join("clt"),
                    command_arguments: "[]",
                    path_env: OsStr::new("/usr/bin:/bin"),
                    codex_path: None,
                    task_selection: "next_todo",
                    resume_session_id: None,
                    created_at: "101",
                })
                .unwrap()
        );

        garbage_collect_agent_binary_generations(&state_dir, &store, &scheduler_dir.join("clt"))
            .unwrap();
        assert!(scheduler_dir.exists());
        assert!(worker_dir.exists());
        assert!(!stale_dir.exists());

        assert!(
            store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: "generation-token",
                    expected_state: AGENT_WORKER_STATE_DISPATCHING,
                    expected_worker_pid: None,
                    expected_heartbeat_at: Some("101"),
                    finished_at: "102",
                    error: "test cleanup",
                    permitted_successor_holder: None,
                })
                .unwrap()
        );
        garbage_collect_agent_binary_generations(&state_dir, &store, &scheduler_dir.join("clt"))
            .unwrap();
        assert!(scheduler_dir.exists());
        assert!(!worker_dir.exists());

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
    fn launchd_worker_plist_runs_one_pinned_worker_generation() {
        let service_env = AgentServiceEnvironment {
            codex_path_override: Some(PathBuf::from("/Users/alex/Codex & Tools/codex")),
            path: OsString::from("/Users/alex/bin & tools:/usr/bin:/bin"),
        };
        let spec = AgentWorkerLaunchSpec {
            state_dir: PathBuf::from("/Users/alex/Library/Application Support/clt & worker state"),
            executable: PathBuf::from("/Users/alex/CLT & Tools/generations/one/clt"),
            worker_token: "1234567890-000000001-p42-s7".to_string(),
            project_id: 42,
            task_selection: AgentTaskSelection::ResumeSession,
            resume_session_id: Some("01234567-89ab-cdef-0123-456789abcdef".to_string()),
            service_label: "com.alpinevibrations.clt.agent.worker.1234567890-000000001-p42-s7"
                .to_string(),
            command_arguments: None,
            service_env: service_env.clone(),
        };

        let plist = launchd_worker_plist_content(&spec, &service_env);

        assert!(plist.contains(&format!("<string>{}</string>", spec.service_label)));
        assert!(plist.contains("<string>/Users/alex/CLT &amp; Tools/generations/one/clt</string>"));
        for argument in [
            "--local",
            "agent",
            "worker",
            "--state-dir",
            "--project-id",
            "42",
            "--worker-token",
            "1234567890-000000001-p42-s7",
            "--task-selection",
            "resume_session",
            "--resume-session-id",
            "01234567-89ab-cdef-0123-456789abcdef",
        ] {
            assert!(
                plist.contains(&format!("<string>{argument}</string>")),
                "missing worker argument {argument}"
            );
        }
        assert!(plist.contains(
            "<string>/Users/alex/Library/Application Support/clt &amp; worker state</string>"
        ));
        assert!(plist.contains("<key>CLT_AGENT_CODEX_PATH</key>"));
        assert!(plist.contains("<string>/Users/alex/Codex &amp; Tools/codex</string>"));
        assert!(plist.contains("<string>/Users/alex/bin &amp; tools:/usr/bin:/bin</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <false/>"));
        assert!(plist.contains("<key>ProcessType</key>\n  <string>Standard</string>"));
        assert!(plist.contains(
            "<string>/Users/alex/Library/Application Support/clt &amp; worker state/workers/1234567890-000000001-p42-s7/worker.out</string>"
        ));
        assert!(plist.contains(
            "<string>/Users/alex/Library/Application Support/clt &amp; worker state/workers/1234567890-000000001-p42-s7/worker.err</string>"
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
    fn systemd_worker_run_uses_a_separate_one_shot_user_service() {
        let service_env = AgentServiceEnvironment {
            codex_path_override: Some(PathBuf::from("/home/alex/Codex Tools/codex")),
            path: OsString::from("/home/alex/bin with spaces:/usr/bin:/bin"),
        };
        let spec = AgentWorkerLaunchSpec {
            state_dir: PathBuf::from("/home/alex/.local/state/clt worker"),
            executable: PathBuf::from("/home/alex/.local/state/clt generations/one/clt"),
            worker_token: "1234567890-000000001-p42-s7".to_string(),
            project_id: 42,
            task_selection: AgentTaskSelection::ResumeSession,
            resume_session_id: Some("01234567-89ab-cdef-0123-456789abcdef".to_string()),
            service_label: "clt-agent-worker-1234567890-000000001-p42-s7.service".to_string(),
            command_arguments: None,
            service_env: service_env.clone(),
        };

        let arguments = systemd_worker_run_args(&spec, &service_env)
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            arguments,
            vec![
                "--user",
                "--unit=clt-agent-worker-1234567890-000000001-p42-s7.service",
                "--collect",
                "--service-type=exec",
                "--property=Restart=no",
                "--property=KillMode=control-group",
                "--setenv=CLT_AGENT_STATE_DIR=/home/alex/.local/state/clt worker",
                "--setenv=PATH=/home/alex/bin with spaces:/usr/bin:/bin",
                "--setenv=CLT_AGENT_CODEX_PATH=/home/alex/Codex Tools/codex",
                "--",
                "/home/alex/.local/state/clt generations/one/clt",
                "--local",
                "agent",
                "worker",
                "--state-dir",
                "/home/alex/.local/state/clt worker",
                "--project-id",
                "42",
                "--worker-token",
                "1234567890-000000001-p42-s7",
                "--task-selection",
                "resume_session",
                "--resume-session-id",
                "01234567-89ab-cdef-0123-456789abcdef",
            ]
        );
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

    #[test]
    fn systemd_run_user_service_command_receives_runtime_dir() {
        let command = service_command_with_systemd_user_configurer(
            "systemd-run",
            &[
                "--user",
                "--unit=clt-agent-worker-test.service",
                "--",
                "/bin/true",
            ],
            |command| configure_systemd_user_command_with_runtime_dir(command, None, "1000"),
        )
        .unwrap();

        assert_eq!(command.get_program(), OsStr::new("systemd-run"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("--user"),
                OsStr::new("--unit=clt-agent-worker-test.service"),
                OsStr::new("--"),
                OsStr::new("/bin/true"),
            ]
        );
        let configured_runtime_dir = command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new(XDG_RUNTIME_DIR_ENV))
            .and_then(|(_, value)| value);
        assert_eq!(configured_runtime_dir, Some(OsStr::new("/run/user/1000")));
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

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
            "agent_workers",
            "git_finalizations",
        ] {
            assert!(
                store.table_exists_blocking(table).unwrap(),
                "missing table {table}"
            );
        }
        assert!(!store.runs_has_task_content_column_blocking().unwrap());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_rebuilds_the_derived_active_worker_index() {
        let root = temp_root("agent-store-rebuild-active-worker-index");
        let state_dir = root.join("state/clt");
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert!(store.active_worker_project_index_exists_blocking().unwrap());
        store.drop_active_worker_project_index_blocking().unwrap();
        assert!(!store.active_worker_project_index_exists_blocking().unwrap());

        store
            .rebuild_active_worker_project_index_blocking()
            .unwrap();

        assert!(store.active_worker_project_index_exists_blocking().unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    const AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV: &str =
        "CLT_TEST_AGENT_STORE_MULTIPROCESS_STATE_DIR";
    const AGENT_STORE_MULTIPROCESS_GATE_ENV: &str = "CLT_TEST_AGENT_STORE_MULTIPROCESS_GATE";
    const AGENT_STORE_MULTIPROCESS_READY_ENV: &str = "CLT_TEST_AGENT_STORE_MULTIPROCESS_READY";
    const AGENT_STORE_MULTIPROCESS_HOLD_GATE_ENV: &str =
        "CLT_TEST_AGENT_STORE_MULTIPROCESS_HOLD_GATE";
    const AGENT_STORE_MULTIPROCESS_HOLD_READY_ENV: &str =
        "CLT_TEST_AGENT_STORE_MULTIPROCESS_HOLD_READY";

    #[cfg(unix)]
    #[test]
    fn agent_store_marks_a_versioned_turso_frame_index_for_rebuild() {
        let root = temp_root("agent-store-shared-wal-rebuild-bit");
        fs::create_dir_all(&root).unwrap();
        let db_path = root.join(AGENT_DB_FILE);
        let shared_wal_path = agent::shared_wal_path(&db_path);
        let mut bytes = vec![0u8; 4096];
        bytes[..TURSO_SHARED_WAL_MAGIC.len()].copy_from_slice(TURSO_SHARED_WAL_MAGIC);
        bytes[8..12].copy_from_slice(&TURSO_SHARED_WAL_VERSION.to_le_bytes());
        fs::write(&shared_wal_path, bytes).unwrap();

        assert!(agent::request_shared_wal_index_rebuild(&db_path).unwrap());
        let bytes = fs::read(&shared_wal_path).unwrap();
        assert_eq!(
            u32::from_le_bytes(
                bytes[TURSO_SHARED_WAL_INDEX_OVERFLOW_OFFSET
                    ..TURSO_SHARED_WAL_INDEX_OVERFLOW_OFFSET + 4]
                    .try_into()
                    .unwrap()
            ),
            1
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_allows_a_second_process_to_open_the_database() {
        let root = temp_root("agent-store-multiprocess");
        let state_dir = root.join("state/clt");
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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

    #[cfg(unix)]
    #[test]
    fn agent_store_long_lived_peer_pins_the_wal_before_auto_checkpoint_restart() {
        const WAL_HEADER_BYTES: usize = 32;
        const WAL_FRAME_HEADER_BYTES: usize = 24;
        const CHECKPOINT_PRESSURE_WRITES: usize = 1_100;

        let root = temp_root("agent-store-checkpoint-pin");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "checkpoint-pin-project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap()[0].id;
        store
            .write_checkpoint_pressure_blocking(project_id, CHECKPOINT_PRESSURE_WRITES)
            .unwrap();

        let mut wal_path = store.db_path().as_os_str().to_os_string();
        wal_path.push("-wal");
        let wal = fs::read(PathBuf::from(wal_path)).unwrap();
        let page_size = u32::from_be_bytes(wal[8..12].try_into().unwrap()) as usize;
        let frame_count = (wal.len() - WAL_HEADER_BYTES) / (WAL_FRAME_HEADER_BYTES + page_size);
        assert!(
            frame_count > 1_000,
            "the WAL restarted despite the long-lived store's checkpoint pin: {frame_count} frames"
        );

        let child_output = Command::new(std::env::current_exe().unwrap())
            .arg("tests::agent_store_multiprocess_child_opens_database")
            .arg("--exact")
            .arg("--nocapture")
            .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
            .output()
            .unwrap();
        assert!(
            child_output.status.success(),
            "second process failed after checkpoint pressure: stdout={}; stderr={}",
            String::from_utf8_lossy(&child_output.stdout),
            String::from_utf8_lossy(&child_output.stderr)
        );

        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    fn corrupt_shared_wal_page_one_mapping(db_path: &Path) {
        use std::os::unix::fs::FileExt;

        const WAL_HEADER_BYTES: usize = 32;
        const WAL_FRAME_HEADER_BYTES: usize = 24;
        const SHARED_WAL_BASE_BYTES: usize = 4096;
        const FRAME_INDEX_BLOCK_CAPACITY: usize = 4096;
        const FRAME_INDEX_HASH_SLOTS: usize = FRAME_INDEX_BLOCK_CAPACITY * 2;
        const FRAME_INDEX_ENTRY_BYTES: usize = 16;
        const FRAME_INDEX_ENTRY_REGION_BYTES: usize =
            FRAME_INDEX_BLOCK_CAPACITY * FRAME_INDEX_ENTRY_BYTES;
        const FRAME_INDEX_HASH_REGION_BYTES: usize = FRAME_INDEX_HASH_SLOTS * 2;
        const FRAME_INDEX_BLOCK_BYTES: usize =
            FRAME_INDEX_ENTRY_REGION_BYTES + FRAME_INDEX_HASH_REGION_BYTES;

        let mut wal_path = db_path.as_os_str().to_os_string();
        wal_path.push("-wal");
        let wal = fs::read(PathBuf::from(wal_path)).unwrap();
        let page_size = u32::from_be_bytes(wal[8..12].try_into().unwrap()) as usize;
        let frame_size = WAL_FRAME_HEADER_BYTES + page_size;
        let frame_count = (wal.len() - WAL_HEADER_BYTES) / frame_size;
        let target_frame = (0..frame_count)
            .rev()
            .find(|frame_index| {
                let frame_offset = WAL_HEADER_BYTES + frame_index * frame_size;
                let page_id =
                    u32::from_be_bytes(wal[frame_offset..frame_offset + 4].try_into().unwrap());
                page_id != 1
                    && wal[frame_offset + WAL_FRAME_HEADER_BYTES + 100] == 0
                    && (*frame_index + 1..frame_count).all(|later_frame_index| {
                        let later_offset = WAL_HEADER_BYTES + later_frame_index * frame_size;
                        u32::from_be_bytes(wal[later_offset..later_offset + 4].try_into().unwrap())
                            != 1
                    })
            })
            .map(|frame_index| frame_index as u64 + 1)
            .expect("test WAL needs a non-page-one tail frame with an invalid page-one header");

        let shared_wal_path = agent::shared_wal_path(db_path);
        let mut shared_wal = fs::read(&shared_wal_path).unwrap();
        let frame_index_blocks =
            u32::from_le_bytes(shared_wal[32..36].try_into().unwrap()) as usize;
        let frame_index_len = u32::from_le_bytes(shared_wal[40..44].try_into().unwrap()) as usize;
        let target_slot = (0..frame_index_len)
            .find(|slot| {
                let block = slot / FRAME_INDEX_BLOCK_CAPACITY;
                let local_slot = slot % FRAME_INDEX_BLOCK_CAPACITY;
                let entry_offset = SHARED_WAL_BASE_BYTES
                    + block * FRAME_INDEX_BLOCK_BYTES
                    + local_slot * FRAME_INDEX_ENTRY_BYTES;
                u64::from_le_bytes(
                    shared_wal[entry_offset + 8..entry_offset + 16]
                        .try_into()
                        .unwrap(),
                ) == target_frame
            })
            .expect("test WAL frame must be represented in the shared index");
        let target_block = target_slot / FRAME_INDEX_BLOCK_CAPACITY;
        let target_local_slot = target_slot % FRAME_INDEX_BLOCK_CAPACITY;
        let target_entry_offset = SHARED_WAL_BASE_BYTES
            + target_block * FRAME_INDEX_BLOCK_BYTES
            + target_local_slot * FRAME_INDEX_ENTRY_BYTES;
        shared_wal[target_entry_offset..target_entry_offset + 8]
            .copy_from_slice(&1u64.to_le_bytes());

        for block in 0..frame_index_blocks {
            let hash_offset = SHARED_WAL_BASE_BYTES
                + block * FRAME_INDEX_BLOCK_BYTES
                + FRAME_INDEX_ENTRY_REGION_BYTES;
            shared_wal[hash_offset..hash_offset + FRAME_INDEX_HASH_REGION_BYTES].fill(0);
        }
        for slot in 0..frame_index_len {
            let block = slot / FRAME_INDEX_BLOCK_CAPACITY;
            let local_slot = slot % FRAME_INDEX_BLOCK_CAPACITY;
            let block_offset = SHARED_WAL_BASE_BYTES + block * FRAME_INDEX_BLOCK_BYTES;
            let entry_offset = block_offset + local_slot * FRAME_INDEX_ENTRY_BYTES;
            let page_id = u64::from_le_bytes(
                shared_wal[entry_offset..entry_offset + 8]
                    .try_into()
                    .unwrap(),
            );
            let hash_offset = block_offset + FRAME_INDEX_ENTRY_REGION_BYTES;
            let mut hash_slot = page_id.wrapping_mul(383) as usize % FRAME_INDEX_HASH_SLOTS;
            loop {
                let value_offset = hash_offset + hash_slot * 2;
                if u16::from_le_bytes(
                    shared_wal[value_offset..value_offset + 2]
                        .try_into()
                        .unwrap(),
                ) == 0
                {
                    shared_wal[value_offset..value_offset + 2]
                        .copy_from_slice(&((local_slot + 1) as u16).to_le_bytes());
                    break;
                }
                hash_slot = (hash_slot + 1) % FRAME_INDEX_HASH_SLOTS;
            }
        }
        shared_wal
            [TURSO_SHARED_WAL_INDEX_OVERFLOW_OFFSET..TURSO_SHARED_WAL_INDEX_OVERFLOW_OFFSET + 4]
            .copy_from_slice(&0u32.to_le_bytes());

        let file = fs::OpenOptions::new()
            .write(true)
            .open(&shared_wal_path)
            .unwrap();
        file.write_all_at(&shared_wal, 0).unwrap();
        file.sync_data().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn agent_store_defers_stale_index_recovery_until_the_live_peer_exits() {
        let root = temp_root("agent-store-stale-shared-wal-index");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        let recovery_marker = root.join("recovery-requested");
        let holder_ready = root.join("holder-ready");
        let holder_gate = root.join("holder-gate");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "stale-index-project")
            .unwrap();

        let mut holder = Command::new(std::env::current_exe().unwrap())
            .arg("tests::agent_store_multiprocess_child_opens_database")
            .arg("--exact")
            .arg("--nocapture")
            .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
            .env(AGENT_STORE_MULTIPROCESS_HOLD_READY_ENV, &holder_ready)
            .env(AGENT_STORE_MULTIPROCESS_HOLD_GATE_ENV, &holder_gate)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let holder_deadline = Instant::now() + Duration::from_secs(5);
        while !holder_ready.exists() && Instant::now() < holder_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if !holder_ready.exists() {
            let _ = holder.kill();
            let holder_output = holder.wait_with_output().unwrap();
            panic!(
                "peer process did not open the agent store: stdout={}; stderr={}",
                String::from_utf8_lossy(&holder_output.stdout),
                String::from_utf8_lossy(&holder_output.stderr)
            );
        }
        corrupt_shared_wal_page_one_mapping(store.db_path());

        let blocked_output = Command::new(std::env::current_exe().unwrap())
            .arg("tests::agent_store_multiprocess_child_opens_database")
            .arg("--exact")
            .arg("--nocapture")
            .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
            .env(TEST_AGENT_SHARED_WAL_REBUILD_MARKER_ENV, &recovery_marker)
            .output()
            .unwrap();

        assert!(
            !blocked_output.status.success(),
            "stale-index recovery must wait for the live peer: stdout={}; stderr={}",
            String::from_utf8_lossy(&blocked_output.stdout),
            String::from_utf8_lossy(&blocked_output.stderr)
        );
        assert!(
            !recovery_marker.exists(),
            "recovery was requested while a peer still held the Turso lifetime lock"
        );

        drop(store);
        holder.kill().unwrap();
        let _ = holder.wait_with_output().unwrap();

        let recovered_output = Command::new(std::env::current_exe().unwrap())
            .arg("tests::agent_store_multiprocess_child_opens_database")
            .arg("--exact")
            .arg("--nocapture")
            .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
            .env(TEST_AGENT_SHARED_WAL_REBUILD_MARKER_ENV, &recovery_marker)
            .output()
            .unwrap();
        assert!(
            recovered_output.status.success(),
            "agent store did not recover after its peer exited: stdout={}; stderr={}",
            String::from_utf8_lossy(&recovered_output.stdout),
            String::from_utf8_lossy(&recovered_output.stderr)
        );
        assert!(recovery_marker.is_file());

        let recovered_store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert!(recovered_store.table_exists_blocking("projects").unwrap());
        drop(recovered_store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_multiprocess_child_opens_database() {
        let Some(state_dir) = std::env::var_os(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV) else {
            return;
        };

        if let Some(ready_path) = std::env::var_os(AGENT_STORE_MULTIPROCESS_READY_ENV) {
            fs::write(ready_path, b"ready").unwrap();
        }
        if let Some(gate_path) = std::env::var_os(AGENT_STORE_MULTIPROCESS_GATE_ENV) {
            let gate_path = PathBuf::from(gate_path);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !gate_path.exists() {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for concurrent agent-store open gate"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }

        let store = agent::TursoAgentStore::open_blocking(Path::new(&state_dir)).unwrap();
        assert!(store.table_exists_blocking("projects").unwrap());
        if let Some(ready_path) = std::env::var_os(AGENT_STORE_MULTIPROCESS_HOLD_READY_ENV) {
            fs::write(ready_path, b"ready").unwrap();
        }
        if let Some(gate_path) = std::env::var_os(AGENT_STORE_MULTIPROCESS_HOLD_GATE_ENV) {
            let gate_path = PathBuf::from(gate_path);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !gate_path.exists() {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for multiprocess hold gate"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn agent_store_concurrent_virgin_opens_apply_each_migration_once() {
        let root = temp_root("agent-store-concurrent-virgin-open");
        let state_dir = root.join("state/clt");
        let gate_path = root.join("open-gate");
        let first_ready_path = root.join("first-ready");
        let second_ready_path = root.join("second-ready");
        fs::create_dir_all(&root).unwrap();

        let spawn_open = |ready_path: &Path| {
            Command::new(std::env::current_exe().unwrap())
                .arg("tests::agent_store_multiprocess_child_opens_database")
                .arg("--exact")
                .arg("--nocapture")
                .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
                .env(AGENT_STORE_MULTIPROCESS_GATE_ENV, &gate_path)
                .env(AGENT_STORE_MULTIPROCESS_READY_ENV, ready_path)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        };
        let mut first = spawn_open(&first_ready_path);
        let mut second = spawn_open(&second_ready_path);
        let ready_deadline = Instant::now() + Duration::from_secs(5);
        while !(first_ready_path.exists() && second_ready_path.exists())
            && Instant::now() < ready_deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        if !(first_ready_path.exists() && second_ready_path.exists()) {
            let _ = first.kill();
            let _ = second.kill();
            let _ = first.wait();
            let _ = second.wait();
            panic!("concurrent agent-store children did not reach their open barrier");
        }
        fs::write(&gate_path, b"open").unwrap();

        let first_output = first.wait_with_output().unwrap();
        let second_output = second.wait_with_output().unwrap();
        assert!(
            first_output.status.success(),
            "first virgin open failed: stdout={}; stderr={}",
            String::from_utf8_lossy(&first_output.stdout),
            String::from_utf8_lossy(&first_output.stderr)
        );
        assert!(
            second_output.status.success(),
            "second virgin open failed: stdout={}; stderr={}",
            String::from_utf8_lossy(&second_output.stdout),
            String::from_utf8_lossy(&second_output.stderr)
        );

        let db_path = state_dir.join(AGENT_DB_FILE);
        let migration_count = tokio::runtime::Runtime::new().unwrap().block_on(async {
            let db = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
                .build()
                .await
                .unwrap();
            let conn = db.connect().unwrap();
            let mut rows = conn
                .query("SELECT COUNT(*) FROM schema_migrations", ())
                .await
                .unwrap();
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get_value(0)
                .unwrap()
                .as_integer()
                .copied()
                .unwrap()
        });
        assert_eq!(migration_count, 17);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_rolls_back_partial_migration_when_a_later_statement_fails() {
        let root = temp_root("agent-store-migration-rollback");
        let state_dir = root.join("state/clt");
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let db_path = store.db_path().to_path_buf();
        drop(store);

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let db = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
                .build()
                .await
                .unwrap();
            let conn = db.connect().unwrap();
            conn.execute("DELETE FROM schema_migrations WHERE version = 13", ())
                .await
                .unwrap();
            conn.execute("ALTER TABLE session_controls DROP COLUMN run_token", ())
                .await
                .unwrap();
        });

        let migration_error = match agent::TursoAgentStore::open_blocking(&state_dir) {
            Ok(_) => panic!("migration unexpectedly succeeded with a duplicate stdout_path column"),
            Err(error) => error,
        };
        assert!(migration_error.to_string().contains("migration 13"));

        let (run_token_columns, stdout_path_columns, migration_rows) =
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let db = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
                    .build()
                    .await
                    .unwrap();
                let conn = db.connect().unwrap();
                let mut run_token_rows = conn
                    .query(
                        "SELECT COUNT(*) FROM pragma_table_info('session_controls') WHERE name = 'run_token'",
                        (),
                    )
                    .await
                    .unwrap();
                let run_token_columns = run_token_rows
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get_value(0)
                    .unwrap()
                    .as_integer()
                    .copied()
                    .unwrap();
                let mut stdout_path_rows = conn
                    .query(
                        "SELECT COUNT(*) FROM pragma_table_info('session_controls') WHERE name = 'stdout_path'",
                        (),
                    )
                    .await
                    .unwrap();
                let stdout_path_columns = stdout_path_rows
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get_value(0)
                    .unwrap()
                    .as_integer()
                    .copied()
                    .unwrap();
                let mut migration_rows = conn
                    .query(
                        "SELECT COUNT(*) FROM schema_migrations WHERE version = 13",
                        (),
                    )
                    .await
                    .unwrap();
                let migration_rows = migration_rows
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get_value(0)
                    .unwrap()
                    .as_integer()
                    .copied()
                    .unwrap();
                (run_token_columns, stdout_path_columns, migration_rows)
            });
        assert_eq!(run_token_columns, 0);
        assert_eq!(stdout_path_columns, 1);
        assert_eq!(migration_rows, 0);

        fs::remove_dir_all(root).unwrap();
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

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        assert_eq!(project.git_mode, AgentGitMode::Commit);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_git_finalization_crud_is_idempotent_and_generation_fenced() {
        let root = temp_root("agent-store-git-finalization");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-one",
                123,
                "run-one",
                &root.join("session-one.out"),
                &root.join("session-one.err"),
            )
            .unwrap();
        store
            .mark_session_running_blocking(
                project.id,
                "session-two",
                124,
                "run-two",
                &root.join("session-two.out"),
                &root.join("session-two.err"),
            )
            .unwrap();
        let new_finalization = |codex_session_id: &'static str, owner_run_token: &'static str| {
            agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id,
                git_mode: AgentGitMode::Commit,
                starting_head: Some("1111111111111111111111111111111111111111"),
                branch_ref: Some("refs/heads/master"),
                upstream_ref: Some("refs/remotes/origin/master"),
                worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                task_identity: None,
                owner_run_token: Some(owner_run_token),
                created_at: "100",
            }
        };

        assert!(
            !store
                .create_git_finalization_blocking(new_finalization("session-one", "stale-run",))
                .unwrap()
        );
        assert!(
            store
                .create_git_finalization_blocking(new_finalization("session-one", "run-one"))
                .unwrap()
        );
        assert!(
            !store
                .create_git_finalization_blocking(new_finalization("session-one", "run-one"))
                .unwrap()
        );
        assert!(
            store
                .create_git_finalization_blocking(new_finalization("session-two", "run-two"))
                .unwrap()
        );

        let working = store
            .git_finalization_blocking(project.id, "session-one")
            .unwrap()
            .unwrap();
        assert_eq!(working.state, GitFinalizationState::Working);
        assert_eq!(working.git_mode, AgentGitMode::Commit);
        assert_eq!(working.owner_run_token.as_deref(), Some("run-one"));
        assert_eq!(working.generation, 0);
        assert_eq!(working.created_at, "100");
        assert_eq!(working.updated_at, "100");
        assert!(working.completed_at.is_none());
        assert!(working.task_identity.is_none());
        let working_two = store
            .git_finalization_blocking(project.id, "session-two")
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .list_pending_git_finalizations_blocking(Some(project.id))
                .unwrap(),
            vec![working.clone(), working_two.clone()]
        );
        assert!(
            store
                .compare_and_set_git_finalization_with_identity_blocking(
                    project.id,
                    "session-one",
                    0,
                    GitFinalizationState::Tracking,
                    "first task",
                    Some("run-one"),
                    "101",
                )
                .unwrap()
        );
        assert!(
            store
                .compare_and_set_git_finalization_with_identity_blocking(
                    project.id,
                    "session-two",
                    0,
                    GitFinalizationState::Tracking,
                    "second task",
                    Some("run-two"),
                    "101",
                )
                .is_err()
        );
        let tracking = store
            .git_finalization_blocking(project.id, "session-one")
            .unwrap()
            .unwrap();
        assert_eq!(tracking.state, GitFinalizationState::Tracking);
        assert_eq!(tracking.task_identity.as_deref(), Some("first task"));
        assert_eq!(
            store
                .list_pending_git_finalizations_blocking(Some(project.id))
                .unwrap(),
            vec![working_two.clone(), tracking]
        );

        assert!(
            !store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-one",
                    9,
                    GitFinalizationState::CommitPending,
                    Some("run-one"),
                    None,
                    None,
                    "101",
                )
                .unwrap()
        );
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-one",
                    1,
                    GitFinalizationState::CommitPending,
                    Some("run-one"),
                    None,
                    Some("commit not attempted yet"),
                    "101",
                )
                .unwrap()
        );
        assert!(
            !store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-one",
                    1,
                    GitFinalizationState::CommitPending,
                    Some("run-one"),
                    None,
                    None,
                    "102",
                )
                .unwrap()
        );
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-one",
                    2,
                    GitFinalizationState::CommitPending,
                    Some("run-one"),
                    Some("2222222222222222222222222222222222222222"),
                    None,
                    "102",
                )
                .unwrap()
        );
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-one",
                    3,
                    GitFinalizationState::Completed,
                    None,
                    None,
                    None,
                    "103",
                )
                .unwrap()
        );

        let completed = store
            .git_finalization_blocking(project.id, "session-one")
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, GitFinalizationState::Completed);
        assert_eq!(completed.generation, 4);
        assert_eq!(
            completed.commit_oid.as_deref(),
            Some("2222222222222222222222222222222222222222")
        );
        assert_eq!(completed.completed_at.as_deref(), Some("103"));
        assert_eq!(
            store
                .list_pending_git_finalizations_blocking(Some(project.id))
                .unwrap(),
            vec![working_two]
        );

        assert!(
            !store
                .create_git_finalization_blocking(new_finalization("session-two", "run-two"))
                .unwrap()
        );
        assert!(
            !store
                .delete_terminal_git_finalization_blocking(project.id, "session-two")
                .unwrap()
        );
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-two",
                    0,
                    GitFinalizationState::Cancelled,
                    None,
                    None,
                    None,
                    "104",
                )
                .unwrap()
        );
        assert!(
            store
                .delete_terminal_git_finalization_blocking(project.id, "session-one")
                .unwrap()
        );
        assert!(
            store
                .delete_terminal_git_finalization_blocking(project.id, "session-two")
                .unwrap()
        );
        assert!(
            store
                .git_finalization_blocking(project.id, "session-one")
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn push_pending_commit_oid_cannot_be_replaced() {
        let root = temp_root("agent-store-push-pending-immutable-oid");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-immutable-push",
                    git_mode: AgentGitMode::CommitAndPush,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/master"),
                    upstream_ref: Some("refs/remotes/origin/master"),
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: Some("immutable push"),
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
        for (generation, state, commit_oid) in [
            (0, GitFinalizationState::Tracking, None),
            (1, GitFinalizationState::CommitPending, None),
            (
                2,
                GitFinalizationState::PushPending,
                Some("2222222222222222222222222222222222222222"),
            ),
        ] {
            assert!(
                store
                    .compare_and_set_git_finalization_blocking(
                        project.id,
                        "session-immutable-push",
                        generation,
                        state,
                        None,
                        commit_oid,
                        None,
                        "101",
                    )
                    .unwrap()
            );
        }

        let error = store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-immutable-push",
                3,
                GitFinalizationState::PushPending,
                None,
                Some("3333333333333333333333333333333333333333"),
                Some("retry must retain the sealed commit"),
                "102",
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("commit OID cannot change once recorded"));
        let pending = store
            .git_finalization_blocking(project.id, "session-immutable-push")
            .unwrap()
            .unwrap();
        assert_eq!(pending.state, GitFinalizationState::PushPending);
        assert_eq!(pending.generation, 3);
        assert_eq!(
            pending.commit_oid.as_deref(),
            Some("2222222222222222222222222222222222222222")
        );
        assert!(pending.last_error.is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn automated_done_is_provisional_until_the_exact_task_commit_is_proven() {
        let root = temp_root("automated-git-finalization-proof");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Ship feature — COMPLETED 2026-09-02: cargo test passed codex:session-proof\n",
        )
        .unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        assert!(
            store
                .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit,)
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-proof",
                123,
                "run-proof",
                &root.join("proof.out"),
                &root.join("proof.err"),
            )
            .unwrap();
        let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-proof",
            "run-proof",
            Some(&git_start),
        )
        .unwrap();
        assert!(
            bind_agent_git_working_task_identity(&store, &project, "session-proof", "run-proof",)
                .unwrap()
        );
        fs::write(project_root.join("feature.txt"), "implemented\n").unwrap();
        run_test_git(&project_root, &["add", "feature.txt"]);

        move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-proof".to_string(),
            },
            &store,
        )
        .unwrap();

        assert!(read_tasks(&project_root, "doing").unwrap().is_empty());
        assert_eq!(read_tasks(&project_root, "done").unwrap().len(), 1);
        let pending = store
            .git_finalization_blocking(project.id, "session-proof")
            .unwrap()
            .unwrap();
        assert_eq!(pending.state, GitFinalizationState::CommitPending);
        assert_eq!(
            pending.starting_head.as_deref(),
            Some(starting_head.as_str())
        );

        run_test_git(&project_root, &["add", "--all"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Ship feature",
                "-m",
                "CLT-Task: codex:session-proof",
            ],
        );
        let committed_head = run_test_git(&project_root, &["rev-parse", "HEAD"]);
        let completed = reconcile_agent_git_finalization(
            &store,
            &project_root,
            pending,
            Some("run-proof"),
            None,
        )
        .unwrap();
        assert_eq!(completed.state, GitFinalizationState::Completed);
        assert_eq!(
            completed.commit_oid.as_deref(),
            Some(committed_head.as_str())
        );

        let acknowledged_again = reconcile_agent_git_finalization(
            &store,
            &project_root,
            completed,
            Some("run-proof"),
            None,
        )
        .unwrap();
        assert_eq!(acknowledged_again.state, GitFinalizationState::Completed);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_start_journal_is_not_reconstructed_from_completed_evidence() {
        let root = temp_root("automated-git-missing-start-journal");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Already finished — COMPLETED 2026-09-02: checked codex:session-no-journal\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-no-journal",
                123,
                "run-no-journal",
                &root.join("missing.out"),
                &root.join("missing.err"),
            )
            .unwrap();
        let late_snapshot =
            capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();

        let error = ensure_agent_git_working_record(
            &store,
            &project,
            "session-no-journal",
            "run-no-journal",
            Some(&late_snapshot),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("cannot safely reconstruct"));
        assert!(
            store
                .git_finalization_blocking(project.id, "session-no-journal")
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_enabled_task_must_exist_in_the_frozen_commit() {
        let root = temp_root("automated-git-untracked-task");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        fs::write(project_root.join("README.md"), "tracked\n").unwrap();
        initialize_test_git_repository(&project_root);
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Untracked task\n",
        )
        .unwrap();
        let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();

        let error = require_agent_git_start_task_identity(
            &project_root,
            &start.starting_head,
            &durable_task_identity("Untracked task").unwrap(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("committed exactly once"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prelaunch_git_snapshot_rejects_work_before_task_activation() {
        let root = temp_root("automated-git-prelaunch-snapshot");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Start first\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        fs::write(project_root.join("too-early.txt"), "implementation\n").unwrap();

        let error =
            verify_agent_git_start_state_unchanged(&project_root, AgentGitMode::Commit, &start)
                .unwrap_err();
        assert!(format!("{error:#}").contains("changed after CLT froze"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_owned_git_launch_state_is_required_and_consumed_at_activation() {
        let root = temp_root("automated-git-server-launch-state");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Start from durable launch state\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-launch-state",
                123,
                "run-launch-state",
                &root.join("launch.out"),
                &root.join("launch.err"),
            )
            .unwrap();
        let context = AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run-launch-state".to_string(),
        };

        let error = move_task_to_doing_with_agent_git_journal(
            &project_root,
            "1",
            &context,
            &project,
            &store,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("no atomically registered Working Git journal"));
        assert_eq!(read_tasks(&project_root, "todo").unwrap().len(), 1);
        assert!(read_tasks(&project_root, "doing").unwrap().is_empty());
        assert!(
            store
                .git_finalization_blocking(project.id, "session-launch-state")
                .unwrap()
                .is_none()
        );

        let launch = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        store
            .record_git_launch_state_blocking(
                project.id,
                "run-launch-state",
                AgentGitMode::Commit,
                &launch,
                "100",
            )
            .unwrap();
        assert_eq!(
            store
                .git_launch_state_blocking(project.id, "run-launch-state")
                .unwrap(),
            Some((AgentGitMode::Commit, launch.clone()))
        );
        store
            .mark_session_running_with_git_finalization_blocking(
                project.id,
                "session-launch-state",
                123,
                "run-launch-state",
                &root.join("launch.out"),
                &root.join("launch.err"),
                AgentGitMode::Commit,
            )
            .unwrap();
        let unbound = store
            .git_finalization_blocking(project.id, "session-launch-state")
            .unwrap()
            .unwrap();
        assert_eq!(unbound.state, GitFinalizationState::Working);
        assert_eq!(unbound.task_identity, None);
        assert_eq!(
            unbound.starting_head.as_deref(),
            Some(launch.starting_head.as_str())
        );
        assert!(
            store
                .git_launch_state_blocking(project.id, "run-launch-state")
                .unwrap()
                .is_none()
        );

        move_task_to_doing_with_agent_git_journal(&project_root, "1", &context, &project, &store)
            .unwrap();
        assert!(read_tasks(&project_root, "todo").unwrap().is_empty());
        assert_eq!(read_tasks(&project_root, "doing").unwrap().len(), 1);
        assert!(
            store
                .git_launch_state_blocking(project.id, "run-launch-state")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .git_finalization_blocking(project.id, "session-launch-state")
                .unwrap()
                .unwrap()
                .state,
            GitFinalizationState::Working
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unconsumed_git_launch_boundary_cannot_be_overwritten_after_release() {
        let root = temp_root("automated-git-unconsumed-launch");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Preserve the first launch boundary\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let first = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        assert!(
            store
                .record_git_launch_state_blocking(
                    project.id,
                    "released-run-a",
                    AgentGitMode::Commit,
                    &first,
                    "100",
                )
                .unwrap()
        );

        fs::write(
            project_root.join("unregistered-work.txt"),
            "made after release\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "unregistered-work.txt"]);
        run_test_git(&project_root, &["commit", "-m", "Work from ambiguous run"]);
        let later = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        assert_ne!(later.starting_head, first.starting_head);

        let error = prepare_agent_git_start_state_for_run(
            &store,
            &project,
            AgentTaskSelection::NextTodo,
            false,
            false,
            "new-run-b",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unconsumed launch boundary"));
        let record_error = store
            .record_git_launch_state_blocking(
                project.id,
                "new-run-b",
                AgentGitMode::Commit,
                &later,
                "101",
            )
            .unwrap_err();
        assert!(format!("{record_error:#}").contains("refusing to replace"));
        assert_eq!(
            store
                .git_launch_state_blocking(project.id, "released-run-a")
                .unwrap(),
            Some((AgentGitMode::Commit, first))
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unchanged_launch_boundary_is_reclaimed_only_after_its_worker_is_dead() {
        let root = temp_root("automated-git-unchanged-launch-reclaim");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Retry after a pre-registration crash\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            "dead-launch-worker",
            "scheduler",
            "101",
            12,
        ));
        assert!(
            store
                .claim_worker_blocking("dead-launch-worker", 123, "102")
                .unwrap()
        );
        let launch = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        store
            .record_git_launch_state_blocking(
                project.id,
                "dead-launch-worker",
                AgentGitMode::Commit,
                &launch,
                "103",
            )
            .unwrap();
        assert!(
            store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: "dead-launch-worker",
                    expected_state: "running",
                    expected_worker_pid: Some(123),
                    expected_heartbeat_at: Some("102"),
                    finished_at: "104",
                    error: "simulated crash before session registration",
                    permitted_successor_holder: None,
                })
                .unwrap()
        );

        let recovered = prepare_agent_git_start_state_for_run(
            &store,
            &project,
            AgentTaskSelection::NextTodo,
            false,
            false,
            "replacement-worker",
        )
        .unwrap()
        .unwrap();
        assert_eq!(recovered, launch);
        assert!(
            store
                .git_launch_state_blocking(project.id, "dead-launch-worker")
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_worker_launch_reclaim_waits_for_every_exact_session_row() {
        let root = temp_root("automated-git-launch-session-fence");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Preserve a session-bound launch\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            "session-bound-launch-worker",
            "scheduler",
            "101",
            12,
        ));
        assert!(
            store
                .claim_worker_blocking("session-bound-launch-worker", 123, "102")
                .unwrap()
        );
        let launch = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        assert!(
            store
                .record_git_launch_state_blocking(
                    project.id,
                    "session-bound-launch-worker",
                    AgentGitMode::Commit,
                    &launch,
                    "103",
                )
                .unwrap()
        );
        store
            .set_session_control_recovery_token_blocking(
                project.id,
                "session-survives-reap",
                "session-bound-launch-worker",
            )
            .unwrap();
        assert!(
            store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: "session-bound-launch-worker",
                    expected_state: "running",
                    expected_worker_pid: Some(123),
                    expected_heartbeat_at: Some("102"),
                    finished_at: "104",
                    error: "simulated reap with a durable session row",
                    permitted_successor_holder: None,
                })
                .unwrap()
        );

        assert!(
            !store
                .reclaim_unchanged_git_launch_state_blocking(
                    project.id,
                    "session-bound-launch-worker",
                    AgentGitMode::Commit,
                    &launch,
                )
                .unwrap()
        );
        assert!(
            store
                .git_launch_state_blocking(project.id, "session-bound-launch-worker")
                .unwrap()
                .is_some()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_git_session_registration_rolls_back_without_a_launch_boundary() {
        let root = temp_root("automated-git-session-registration-rollback");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        let error = store
            .mark_session_running_with_git_finalization_blocking(
                project.id,
                "session-without-launch",
                123,
                "run-without-launch",
                &root.join("missing.out"),
                &root.join("missing.err"),
                AgentGitMode::Commit,
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("no compatible scheduler-owned Git launch state"));
        assert!(
            store
                .session_control_blocking(project.id, "session-without-launch")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .git_finalization_blocking(project.id, "session-without-launch")
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_start_capture_rejects_detached_head() {
        let root = temp_root("automated-git-detached-head");
        init_tasks(&root, false).unwrap();
        initialize_test_git_repository(&root);
        run_test_git(&root, &["checkout", "--detach"]);

        let error = capture_agent_git_start_state(&root, AgentGitMode::Commit).unwrap_err();
        assert!(format!("{error:#}").contains("attached branch"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_start_preflight_rejects_unsafe_mixed_board_storage() {
        let first = temp_root("automated-git-mixed-doing-done");
        init_tasks(&first, false).unwrap();
        fs::remove_file(first.join("tasks/doing.md")).unwrap();
        fs::create_dir(first.join("tasks/doing")).unwrap();
        let error = require_agent_git_board_storage_compatible(&first).unwrap_err();
        assert!(format!("{error:#}").contains("folder-backed Done"));

        let second = temp_root("automated-git-mixed-todo-doing");
        init_tasks(&second, true).unwrap();
        fs::remove_dir(second.join("tasks/doing")).unwrap();
        fs::write(second.join("tasks/doing.md"), "# Doing Tasks\n").unwrap();
        let error = require_agent_git_board_storage_compatible(&second).unwrap_err();
        assert!(format!("{error:#}").contains("folder-backed Doing"));

        fs::remove_dir_all(first).unwrap();
        fs::remove_dir_all(second).unwrap();
    }

    #[test]
    fn git_start_preflight_checkpoints_uncommitted_todos_before_launch() {
        let root = temp_root("automated-git-uncommitted-todo");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(project_root.join("source.txt"), "before\n").unwrap();
        initialize_test_git_repository(&project_root);
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- This task exists only in the worktree\n",
        )
        .unwrap();
        fs::write(project_root.join("source.txt"), "after\n").unwrap();
        fs::write(project_root.join("unrelated.txt"), "preserve me\n").unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let previous_head = run_test_git(&project_root, &["rev-parse", "HEAD"]);

        let start = prepare_agent_git_start_state_for_run(
            &store,
            &project,
            AgentTaskSelection::NextTodo,
            false,
            false,
            "uncommitted-run",
        )
        .unwrap()
        .unwrap();
        assert_ne!(start.starting_head, previous_head);
        assert_eq!(
            run_test_git(&project_root, &["show", "-s", "--format=%s", "HEAD"]),
            "Record CLT task board"
        );
        assert!(git_commit_uses_agent_identity(&project_root, &start.starting_head).unwrap());
        require_agent_git_start_task_identity(
            &project_root,
            &start.starting_head,
            &durable_task_identity("This task exists only in the worktree").unwrap(),
        )
        .unwrap();
        assert!(run_test_git(&project_root, &["status", "--short", "--", "tasks"]).is_empty());
        assert_eq!(
            run_test_git(&project_root, &["status", "--short", "--", "unrelated.txt"]),
            "?? unrelated.txt"
        );
        assert_eq!(
            run_test_git(&project_root, &["status", "--short", "--", "source.txt"]),
            "M source.txt"
        );
        assert!(run_test_git(&project_root, &["diff", "--cached", "--name-only"]).is_empty());
        assert!(
            store
                .git_launch_state_blocking(project.id, "uncommitted-run")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .list_pending_git_finalizations_blocking(Some(project.id))
                .unwrap()
                .is_empty()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_git_sync_fast_forwards_before_the_launch_snapshot() {
        let root = temp_root("automated-git-startup-sync");
        let project_root = root.join("project");
        let peer_root = root.join("peer");
        let remote_root = root.join("remote.git");
        init_tasks(&project_root, false).unwrap();
        initialize_test_git_repository(&project_root);
        fs::create_dir_all(&remote_root).unwrap();
        run_test_git(&remote_root, &["init", "--bare"]);
        run_test_git(
            &project_root,
            &["remote", "add", "origin", remote_root.to_str().unwrap()],
        );
        run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
        run_test_git(
            &root,
            &[
                "clone",
                remote_root.to_str().unwrap(),
                peer_root.to_str().unwrap(),
            ],
        );
        run_test_git(&peer_root, &["config", "user.name", "CLT Peer"]);
        run_test_git(
            &peer_root,
            &["config", "user.email", "clt-peer@example.invalid"],
        );
        fs::write(peer_root.join("upstream.txt"), "upstream\n").unwrap();
        run_test_git(&peer_root, &["add", "upstream.txt"]);
        run_test_git(&peer_root, &["commit", "-m", "Advance upstream"]);
        run_test_git(&peer_root, &["push"]);
        let expected_head = run_test_git(&peer_root, &["rev-parse", "HEAD"]);
        fs::write(project_root.join("untracked-local.txt"), "preserve me\n").unwrap();

        synchronize_agent_git_checkout_before_launch(&project_root).unwrap();

        assert_eq!(
            run_test_git(&project_root, &["rev-parse", "HEAD"]),
            expected_head
        );
        assert_eq!(
            fs::read_to_string(project_root.join("untracked-local.txt")).unwrap(),
            "preserve me\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_git_sync_rechecks_board_storage_after_fast_forward() {
        let root = temp_root("automated-git-startup-storage-change");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        let peer_root = root.join("peer");
        let remote_root = root.join("remote.git");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Refuse an unsafe upstream board layout\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        fs::create_dir_all(&remote_root).unwrap();
        run_test_git(&remote_root, &["init", "--bare"]);
        run_test_git(
            &project_root,
            &["remote", "add", "origin", remote_root.to_str().unwrap()],
        );
        run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
        run_test_git(
            &root,
            &[
                "clone",
                remote_root.to_str().unwrap(),
                peer_root.to_str().unwrap(),
            ],
        );
        run_test_git(&peer_root, &["config", "user.name", "CLT Peer"]);
        run_test_git(
            &peer_root,
            &["config", "user.email", "clt-peer@example.invalid"],
        );
        run_test_git(&peer_root, &["rm", "tasks/doing.md"]);
        fs::create_dir(peer_root.join("tasks/doing")).unwrap();
        fs::write(peer_root.join("tasks/doing/.gitkeep"), "").unwrap();
        run_test_git(&peer_root, &["add", "tasks/doing/.gitkeep"]);
        run_test_git(
            &peer_root,
            &["commit", "-m", "Make only Doing folder-backed"],
        );
        run_test_git(&peer_root, &["push"]);
        let upstream_head = run_test_git(&peer_root, &["rev-parse", "HEAD"]);

        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let error = prepare_agent_git_start_state_for_run(
            &store,
            &project,
            AgentTaskSelection::NextTodo,
            false,
            false,
            "storage-change-run",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("folder-backed Done"));
        assert_eq!(
            run_test_git(&project_root, &["rev-parse", "HEAD"]),
            upstream_head
        );
        assert!(
            store
                .git_launch_state_blocking(project.id, "storage-change-run")
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_git_sync_rejects_divergence_without_rewriting_local_history() {
        let root = temp_root("automated-git-startup-sync-diverged");
        let project_root = root.join("project");
        let peer_root = root.join("peer");
        let remote_root = root.join("remote.git");
        init_tasks(&project_root, false).unwrap();
        initialize_test_git_repository(&project_root);
        fs::create_dir_all(&remote_root).unwrap();
        run_test_git(&remote_root, &["init", "--bare"]);
        run_test_git(
            &project_root,
            &["remote", "add", "origin", remote_root.to_str().unwrap()],
        );
        run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
        run_test_git(
            &root,
            &[
                "clone",
                remote_root.to_str().unwrap(),
                peer_root.to_str().unwrap(),
            ],
        );
        run_test_git(&peer_root, &["config", "user.name", "CLT Peer"]);
        run_test_git(
            &peer_root,
            &["config", "user.email", "clt-peer@example.invalid"],
        );
        fs::write(peer_root.join("upstream.txt"), "upstream\n").unwrap();
        run_test_git(&peer_root, &["add", "upstream.txt"]);
        run_test_git(&peer_root, &["commit", "-m", "Advance upstream"]);
        run_test_git(&peer_root, &["push"]);
        fs::write(project_root.join("local.txt"), "local\n").unwrap();
        run_test_git(&project_root, &["add", "local.txt"]);
        run_test_git(&project_root, &["commit", "-m", "Advance locally"]);
        let local_head = run_test_git(&project_root, &["rev-parse", "HEAD"]);

        let error = synchronize_agent_git_checkout_before_launch(&project_root).unwrap_err();

        assert!(format!("{error:#}").contains("Failed to fast-forward"));
        assert_eq!(
            run_test_git(&project_root, &["rev-parse", "HEAD"]),
            local_head
        );
        assert_eq!(
            fs::read_to_string(project_root.join("local.txt")).unwrap(),
            "local\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    fn working_git_link_race_fixture(
        name: &str,
        task: &str,
        session_id: &str,
        run_token: &str,
    ) -> (
        PathBuf,
        PathBuf,
        agent::TursoAgentStore,
        agent::GitFinalizationRecord,
    ) {
        let root = temp_root(name);
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            format!("# Todo Tasks\n- {task}\n"),
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                session_id,
                123,
                run_token,
                &root.join("race.out"),
                &root.join("race.err"),
            )
            .unwrap();
        let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        let task_identity = durable_task_identity(task).unwrap();
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: session_id,
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some(&start.starting_head),
                    branch_ref: start.branch_ref.as_deref(),
                    upstream_ref: start.upstream_ref.as_deref(),
                    worktree_baseline: &start.worktree_baseline,
                    task_identity: Some(&task_identity),
                    owner_run_token: Some(run_token),
                    created_at: "100",
                })
                .unwrap()
        );
        let finalization = store
            .git_finalization_blocking(project.id, session_id)
            .unwrap()
            .unwrap();
        (root, project_root, store, finalization)
    }

    #[test]
    fn working_cancellation_wins_before_link_repair_without_mutating_the_board() {
        let task = "Cancel before repairing the link";
        let session_id = "session-cancel-first";
        let run_token = "run-cancel-first";
        let (root, project_root, store, finalization) = working_git_link_race_fixture(
            "automated-git-working-cancel-first",
            task,
            session_id,
            run_token,
        );
        let state_dir = root.join("state/clt");
        let (cancel_holding_tx, cancel_holding_rx) = mpsc::channel();
        let (release_cancel_tx, release_cancel_rx) = mpsc::channel();
        let cancel_state_dir = state_dir.clone();
        let cancel_project_root = project_root.clone();
        let cancel_finalization = finalization.clone();
        let cancel_thread = thread::spawn(move || {
            let store = agent::TursoAgentStore::open_blocking(&cancel_state_dir).unwrap();
            cancel_unlinked_working_git_finalization_with_lock_callbacks(
                &store,
                &cancel_project_root,
                &cancel_finalization,
                run_token,
                || {},
                move || {
                    cancel_holding_tx.send(()).unwrap();
                    release_cancel_rx
                        .recv_timeout(Duration::from_secs(2))
                        .expect("cancellation was not released");
                },
                || {},
            )
            .unwrap()
        });
        cancel_holding_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("cancellation did not acquire the board lock");

        let (repair_contended_tx, repair_contended_rx) = mpsc::channel();
        let repair_state_dir = state_dir.clone();
        let repair_project_root = project_root.clone();
        let repair_finalization = finalization.clone();
        let repair_thread = thread::spawn(move || {
            let store = agent::TursoAgentStore::open_blocking(&repair_state_dir).unwrap();
            repair_working_git_task_link_with_lock_callbacks(
                &store,
                &repair_project_root,
                &repair_finalization,
                || {},
                || {},
                move || repair_contended_tx.send(()).unwrap(),
            )
            .unwrap()
        });
        repair_contended_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("repair did not wait for cancellation's board lock");
        release_cancel_tx.send(()).unwrap();

        assert!(cancel_thread.join().unwrap());
        assert!(!repair_thread.join().unwrap());
        assert_eq!(
            read_tasks(&project_root, "todo").unwrap(),
            vec![format!("- {task}")]
        );
        assert!(read_tasks(&project_root, "doing").unwrap().is_empty());
        let current = store
            .git_finalization_blocking(finalization.project_id, session_id)
            .unwrap()
            .unwrap();
        assert_eq!(current.state, GitFinalizationState::Cancelled);
        assert_eq!(current.generation, finalization.generation + 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn working_link_repair_wins_before_idle_cancellation() {
        let task = "Repair before cancelling the journal";
        let session_id = "session-repair-first";
        let run_token = "run-repair-first";
        let (root, project_root, store, finalization) = working_git_link_race_fixture(
            "automated-git-working-repair-first",
            task,
            session_id,
            run_token,
        );
        let state_dir = root.join("state/clt");
        let (repair_holding_tx, repair_holding_rx) = mpsc::channel();
        let (release_repair_tx, release_repair_rx) = mpsc::channel();
        let repair_state_dir = state_dir.clone();
        let repair_project_root = project_root.clone();
        let repair_finalization = finalization.clone();
        let repair_thread = thread::spawn(move || {
            let store = agent::TursoAgentStore::open_blocking(&repair_state_dir).unwrap();
            repair_working_git_task_link_with_lock_callbacks(
                &store,
                &repair_project_root,
                &repair_finalization,
                || {},
                move || {
                    repair_holding_tx.send(()).unwrap();
                    release_repair_rx
                        .recv_timeout(Duration::from_secs(2))
                        .expect("repair was not released");
                },
                || {},
            )
            .unwrap()
        });
        repair_holding_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("repair did not acquire the board lock");

        let (cancel_contended_tx, cancel_contended_rx) = mpsc::channel();
        let cancel_state_dir = state_dir.clone();
        let cancel_project_root = project_root.clone();
        let cancel_finalization = finalization.clone();
        let cancel_thread = thread::spawn(move || {
            let store = agent::TursoAgentStore::open_blocking(&cancel_state_dir).unwrap();
            cancel_unlinked_working_git_finalization_with_lock_callbacks(
                &store,
                &cancel_project_root,
                &cancel_finalization,
                run_token,
                || {},
                || {},
                move || cancel_contended_tx.send(()).unwrap(),
            )
            .unwrap()
        });
        cancel_contended_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("cancellation did not wait for repair's board lock");
        release_repair_tx.send(()).unwrap();

        assert!(repair_thread.join().unwrap());
        assert!(!cancel_thread.join().unwrap());
        assert!(read_tasks(&project_root, "todo").unwrap().is_empty());
        let doing = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing).unwrap();
        assert_eq!(doing.len(), 1);
        assert_eq!(doing[0].content, format!("{task} codex:{session_id}"));
        assert_eq!(
            store
                .git_finalization_blocking(finalization.project_id, session_id)
                .unwrap()
                .unwrap(),
            finalization
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn working_link_repair_refuses_a_different_branch() {
        let root = temp_root("automated-git-working-repair-branch");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Repair only on frozen branch\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-working-repair",
                123,
                "run-working-repair",
                &root.join("working.out"),
                &root.join("working.err"),
            )
            .unwrap();
        let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        let task_identity = durable_task_identity("Repair only on frozen branch").unwrap();
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-working-repair",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some(&start.starting_head),
                    branch_ref: start.branch_ref.as_deref(),
                    upstream_ref: start.upstream_ref.as_deref(),
                    worktree_baseline: &start.worktree_baseline,
                    task_identity: Some(&task_identity),
                    owner_run_token: Some("run-working-repair"),
                    created_at: "100",
                })
                .unwrap()
        );
        let finalization = store
            .git_finalization_blocking(project.id, "session-working-repair")
            .unwrap()
            .unwrap();
        run_test_git(&project_root, &["checkout", "-b", "wrong-repair-branch"]);

        assert!(!repair_working_git_task_link(&store, &project_root, &finalization).unwrap());
        let todo = read_tasks(&project_root, "todo").unwrap();
        assert_eq!(todo, vec!["- Repair only on frozen branch"]);
        assert!(read_tasks(&project_root, "doing").unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn working_link_repair_rechecks_history_after_waiting_for_the_board_lock() {
        let root = temp_root("automated-git-working-repair-history-race");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Recheck repair history\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-repair-race",
                123,
                "run-repair-race",
                &root.join("race.out"),
                &root.join("race.err"),
            )
            .unwrap();
        let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        let task_identity = durable_task_identity("Recheck repair history").unwrap();
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-repair-race",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some(&start.starting_head),
                    branch_ref: start.branch_ref.as_deref(),
                    upstream_ref: start.upstream_ref.as_deref(),
                    worktree_baseline: &start.worktree_baseline,
                    task_identity: Some(&task_identity),
                    owner_run_token: Some("run-repair-race"),
                    created_at: "100",
                })
                .unwrap()
        );
        let finalization = store
            .git_finalization_blocking(project.id, "session-repair-race")
            .unwrap()
            .unwrap();

        assert!(
            !repair_working_git_task_link_with_before_lock(
                &store,
                &project_root,
                &finalization,
                || {
                    fs::write(project_root.join("racing-commit.txt"), "unproven\n").unwrap();
                    run_test_git(&project_root, &["add", "racing-commit.txt"]);
                    run_test_git(&project_root, &["commit", "-m", "Unproven racing commit"]);
                },
            )
            .unwrap()
        );
        assert_eq!(
            read_tasks(&project_root, "todo").unwrap(),
            vec!["- Recheck repair history"]
        );
        assert!(read_tasks(&project_root, "doing").unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn working_link_repair_finishes_marker_and_duplicate_activation_crashes() {
        let root = temp_root("automated-git-working-activation-repair");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Repair activation crash\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-activation-repair",
                123,
                "run-activation-repair",
                &root.join("activation.out"),
                &root.join("activation.err"),
            )
            .unwrap();
        let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        let task_identity = durable_task_identity("Repair activation crash").unwrap();
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-activation-repair",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some(&start.starting_head),
                    branch_ref: start.branch_ref.as_deref(),
                    upstream_ref: start.upstream_ref.as_deref(),
                    worktree_baseline: &start.worktree_baseline,
                    task_identity: Some(&task_identity),
                    owner_run_token: Some("run-activation-repair"),
                    created_at: "100",
                })
                .unwrap()
        );
        let linked = "Repair activation crash codex:session-activation-repair";
        fs::write(
            project_root.join("tasks/todo.md"),
            format!("# Todo Tasks\n- {linked}\n"),
        )
        .unwrap();
        let finalization = store
            .git_finalization_blocking(project.id, "session-activation-repair")
            .unwrap()
            .unwrap();

        assert!(repair_working_git_task_link(&store, &project_root, &finalization).unwrap());
        assert!(read_tasks(&project_root, "todo").unwrap().is_empty());
        let doing = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing).unwrap();
        assert_eq!(doing.len(), 1);
        assert_eq!(doing[0].content, linked);

        fs::write(
            project_root.join("tasks/todo.md"),
            format!("# Todo Tasks\n- {linked}\n"),
        )
        .unwrap();
        assert!(repair_working_git_task_link(&store, &project_root, &finalization).unwrap());
        assert!(read_tasks(&project_root, "todo").unwrap().is_empty());
        let doing = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing).unwrap();
        assert_eq!(doing.len(), 1);
        assert_eq!(doing[0].content, linked);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sealed_git_finalization_rejects_a_same_path_rewrite() {
        let root = temp_root("automated-git-finalization-same-path-rewrite");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Seal exact bytes — COMPLETED 2026-09-02: checked codex:session-exact-bytes\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-exact-bytes",
                123,
                "run-exact-bytes",
                &root.join("exact.out"),
                &root.join("exact.err"),
            )
            .unwrap();
        let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-exact-bytes",
            "run-exact-bytes",
            Some(&git_start),
        )
        .unwrap();
        bind_agent_git_working_task_identity(
            &store,
            &project,
            "session-exact-bytes",
            "run-exact-bytes",
        )
        .unwrap();
        fs::write(project_root.join("feature.txt"), "sealed\n").unwrap();
        run_test_git(&project_root, &["add", "feature.txt"]);
        move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-exact-bytes".to_string(),
            },
            &store,
        )
        .unwrap();

        fs::write(project_root.join("feature.txt"), "rewritten after seal\n").unwrap();
        run_test_git(&project_root, &["add", "--all"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Rewrite sealed bytes",
                "-m",
                "CLT-Task: codex:session-exact-bytes",
            ],
        );
        let pending = store
            .git_finalization_blocking(project.id, "session-exact-bytes")
            .unwrap()
            .unwrap();
        let unchanged = reconcile_agent_git_finalization(
            &store,
            &project_root,
            pending,
            Some("run-exact-bytes"),
            None,
        )
        .unwrap();
        assert_eq!(unchanged.state, GitFinalizationState::CommitPending);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provisional_done_can_reseal_a_hook_mutation_before_commit() {
        let root = temp_root("automated-git-finalization-reseal");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Reseal hook output — COMPLETED 2026-09-02: checked codex:session-reseal\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-reseal",
                123,
                "run-reseal",
                &root.join("reseal.out"),
                &root.join("reseal.err"),
            )
            .unwrap();
        let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-reseal",
            "run-reseal",
            Some(&git_start),
        )
        .unwrap();
        bind_agent_git_working_task_identity(&store, &project, "session-reseal", "run-reseal")
            .unwrap();
        fs::write(project_root.join("formatted.txt"), "before hook\n").unwrap();
        run_test_git(&project_root, &["add", "formatted.txt"]);
        move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-reseal".to_string(),
            },
            &store,
        )
        .unwrap();

        fs::write(project_root.join("formatted.txt"), "after hook\n").unwrap();
        run_test_git(&project_root, &["add", "--all"]);
        let pending = store
            .git_finalization_blocking(project.id, "session-reseal")
            .unwrap()
            .unwrap();
        let task_identity = pending.task_identity.as_deref().unwrap();
        let resealed_manifest = capture_agent_git_resealed_manifest(
            AgentGitProofContext {
                store: &store,
                project_id: project.id,
            },
            &project_root,
            &pending.worktree_baseline,
            "session-reseal",
            task_identity,
            pending.starting_head.as_deref().unwrap(),
            pending.branch_ref.as_deref(),
        )
        .unwrap();
        assert!(
            store
                .reseal_git_finalization_manifest_blocking(
                    project.id,
                    "session-reseal",
                    pending.generation,
                    task_identity,
                    &resealed_manifest,
                    "run-reseal",
                    "200",
                )
                .unwrap()
        );
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Reseal hook output",
                "-m",
                "CLT-Task: codex:session-reseal",
            ],
        );
        let resealed = store
            .git_finalization_blocking(project.id, "session-reseal")
            .unwrap()
            .unwrap();
        let completed = reconcile_agent_git_finalization(
            &store,
            &project_root,
            resealed,
            Some("run-reseal"),
            None,
        )
        .unwrap();
        assert_eq!(completed.state, GitFinalizationState::Completed);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_finalization_preserves_a_concurrent_unstaged_todo() {
        let root = temp_root("automated-git-finalization-concurrent-todo");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Finish feature — COMPLETED 2026-09-02: checked codex:session-concurrent-todo\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-concurrent-todo",
                123,
                "run-concurrent-todo",
                &root.join("concurrent.out"),
                &root.join("concurrent.err"),
            )
            .unwrap();
        let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-concurrent-todo",
            "run-concurrent-todo",
            Some(&git_start),
        )
        .unwrap();
        bind_agent_git_working_task_identity(
            &store,
            &project,
            "session-concurrent-todo",
            "run-concurrent-todo",
        )
        .unwrap();

        fs::write(project_root.join("feature.txt"), "implemented\n").unwrap();
        run_test_git(&project_root, &["add", "feature.txt"]);
        fs::write(
            project_root.join("tasks/todo.md"),
            "# To Do Tasks\n- Added by a person while the agent was working\n",
        )
        .unwrap();

        move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-concurrent-todo".to_string(),
            },
            &store,
        )
        .unwrap();
        run_test_git(&project_root, &["add", "tasks/doing.md", "tasks/done.md"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Finish feature",
                "-m",
                "CLT-Task: codex:session-concurrent-todo",
            ],
        );

        let pending = store
            .git_finalization_blocking(project.id, "session-concurrent-todo")
            .unwrap()
            .unwrap();
        let completed = reconcile_agent_git_finalization(
            &store,
            &project_root,
            pending,
            Some("run-concurrent-todo"),
            None,
        )
        .unwrap();
        assert_eq!(completed.state, GitFinalizationState::Completed);
        assert_eq!(
            fs::read_to_string(project_root.join("tasks/todo.md")).unwrap(),
            "# To Do Tasks\n- Added by a person while the agent was working\n"
        );
        assert_eq!(
            run_test_git(&project_root, &["show", "HEAD:tasks/todo.md"]),
            "# To Do Tasks"
        );
        assert_eq!(
            run_test_git(&project_root, &["status", "--short", "--", "tasks/todo.md"]),
            "M tasks/todo.md"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_finalization_rejects_raw_changes_to_another_board_scope() {
        let root = temp_root("automated-git-finalization-raw-board-scope");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Protect board scope — COMPLETED 2026-09-02: checked codex:session-board-scope\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-board-scope",
                123,
                "run-board-scope",
                &root.join("scope.out"),
                &root.join("scope.err"),
            )
            .unwrap();
        let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-board-scope",
            "run-board-scope",
            Some(&git_start),
        )
        .unwrap();
        bind_agent_git_working_task_identity(
            &store,
            &project,
            "session-board-scope",
            "run-board-scope",
        )
        .unwrap();
        fs::write(project_root.join("feature.txt"), "implemented\n").unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Unrelated header rewrite\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "feature.txt", "tasks/todo.md"]);
        let error = move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-board-scope".to_string(),
            },
            &store,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("raw changes outside the selected task"));
        assert_eq!(read_tasks(&project_root, "doing").unwrap().len(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tracking_repair_deduplicates_an_interrupted_markdown_move() {
        let root = temp_root("automated-git-finalization-move-repair");
        init_tasks(&root, false).unwrap();
        let content = "Repair move — COMPLETED 2026-09-02: checked codex:session-move-repair";
        fs::write(
            root.join("tasks/doing.md"),
            format!("# Doing Tasks\n- {content}\n"),
        )
        .unwrap();
        fs::write(
            root.join("tasks/done.md"),
            format!("# Done Tasks\n- {content}\n"),
        )
        .unwrap();

        assert!(
            repair_tracking_agent_git_board(
                &root,
                "session-move-repair",
                &durable_task_identity(content).unwrap(),
            )
            .unwrap()
        );
        assert!(read_tasks(&root, "doing").unwrap().is_empty());
        let done = read_task_entries(&root.join("tasks"), TaskStatus::Done).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].content, content);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tracking_repair_deduplicates_identical_markdown_and_plain_file_entries() {
        let root = temp_root("automated-git-finalization-mixed-move-repair");
        init_tasks(&root, false).unwrap();
        let content =
            "Repair mixed move — COMPLETED 2026-09-02: checked codex:session-mixed-repair";
        fs::write(
            root.join("tasks/doing.md"),
            format!("# Doing Tasks\n- {content}\n"),
        )
        .unwrap();
        fs::remove_file(root.join("tasks/done.md")).unwrap();
        fs::create_dir_all(root.join("tasks/done")).unwrap();
        fs::write(
            root.join("tasks/done/0001-repair-mixed-move.md"),
            format!("{content}\n"),
        )
        .unwrap();

        assert!(
            repair_tracking_agent_git_board(
                &root,
                "session-mixed-repair",
                &durable_task_identity(content).unwrap(),
            )
            .unwrap()
        );
        assert!(read_tasks(&root, "doing").unwrap().is_empty());
        let done = read_task_entries(&root.join("tasks"), TaskStatus::Done).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].content.trim_end(), content);
        assert!(matches!(
            done[0].source,
            TaskSource::Path { is_dir: false, .. }
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_directory_move_crash_never_reorders_or_hides_unrelated_tasks() {
        let root = temp_root("automated-git-directory-move-crash");
        init_tasks(&root, false).unwrap();
        let content =
            "Publish atomically — COMPLETED 2026-09-02: checked codex:session-directory-crash";
        fs::write(
            root.join("tasks/doing.md"),
            format!("# Doing Tasks\n- {content}\n"),
        )
        .unwrap();
        fs::remove_file(root.join("tasks/done.md")).unwrap();
        fs::create_dir(root.join("tasks/done")).unwrap();
        let unrelated = root.join("tasks/done/0007-unrelated.md");
        fs::write(&unrelated, "Unrelated completed task\n").unwrap();

        let error = move_task_without_reordering_with_after_destination(
            &root.join("tasks"),
            TaskStatus::Doing,
            TaskStatus::Done,
            1,
            || anyhow::bail!("simulated crash after destination publication"),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("simulated crash"));
        assert!(unrelated.is_file());
        assert_eq!(read_tasks(&root, "doing").unwrap().len(), 1);
        assert_eq!(read_tasks(&root, "done").unwrap().len(), 2);
        assert!(
            fs::read_dir(root.join("tasks/done"))
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".clt-reorder-"))
        );

        assert!(
            repair_tracking_agent_git_board(
                &root,
                "session-directory-crash",
                &durable_task_identity(content).unwrap(),
            )
            .unwrap()
        );
        assert!(read_tasks(&root, "doing").unwrap().is_empty());
        assert_eq!(read_tasks(&root, "done").unwrap().len(), 2);
        assert!(unrelated.is_file());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn managed_git_recovery_sweeps_only_exact_orphaned_atomic_temp_files() {
        let root = temp_root("automated-git-orphaned-board-temps");
        init_tasks(&root, false).unwrap();
        let board_dir = root.join("tasks");
        let todo_file = board_dir.join("todo.md");
        let original_todo = fs::read_to_string(&todo_file).unwrap();
        let markdown_crash = std::panic::catch_unwind(|| {
            replace_file_atomically_with_before_publish(
                &todo_file,
                b"# Todo Tasks\n- unpublished\n",
                |_| panic!("simulated power loss before Markdown rename"),
            )
            .unwrap();
        });
        assert!(markdown_crash.is_err());
        assert!(
            fs::read_dir(&board_dir)
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| is_clt_atomic_task_temporary_name(
                    &entry.file_name().to_string_lossy()
                ))
        );

        fs::remove_file(board_dir.join("done.md")).unwrap();
        fs::create_dir(board_dir.join("done")).unwrap();
        let target = board_dir.join("done/0001-unpublished.md");
        let directory_crash = std::panic::catch_unwind(|| {
            write_new_task_file_atomically(&target, b"unpublished\n", |_| {
                panic!("simulated power loss before task-file rename")
            })
            .unwrap();
        });
        assert!(directory_crash.is_err());
        fs::write(board_dir.join("done/.keep-me"), "user file\n").unwrap();

        assert_eq!(cleanup_clt_atomic_task_temporaries(&board_dir).unwrap(), 2);
        assert_eq!(fs::read_to_string(&todo_file).unwrap(), original_todo);
        assert!(!target.exists());
        assert!(board_dir.join("done/.keep-me").is_file());
        let done_dir = board_dir.join("done");
        for directory in [&board_dir, &done_dir] {
            assert!(
                fs::read_dir(directory)
                    .unwrap()
                    .filter_map(Result::ok)
                    .all(|entry| !is_clt_atomic_task_temporary_name(
                        &entry.file_name().to_string_lossy()
                    ))
            );
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn push_mode_uses_the_frozen_push_remote_and_one_exact_refspec() {
        let root = temp_root("automated-git-exact-push-finalization");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        let origin_root = root.join("origin.git");
        let publish_root = root.join("publish.git");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Publish feature — COMPLETED 2026-09-02: cargo test passed codex:session-push\n",
        )
        .unwrap();
        let initial_head = initialize_test_git_repository(&project_root);
        fs::create_dir_all(&origin_root).unwrap();
        fs::create_dir_all(&publish_root).unwrap();
        run_test_git(&origin_root, &["init", "--bare"]);
        run_test_git(&publish_root, &["init", "--bare"]);
        run_test_git(
            &project_root,
            &["remote", "add", "origin", origin_root.to_str().unwrap()],
        );
        run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
        run_test_git(
            &project_root,
            &["remote", "add", "publish", publish_root.to_str().unwrap()],
        );
        let branch = run_test_git(&project_root, &["branch", "--show-current"]);
        let branch_ref = format!("refs/heads/{branch}");
        run_test_git(&project_root, &["branch", "side"]);
        run_test_git(
            &project_root,
            &["push", "publish", &format!("HEAD:{branch_ref}")],
        );
        run_test_git(&project_root, &["push", "publish", "side:refs/heads/side"]);
        run_test_git(
            &project_root,
            &["config", &format!("branch.{branch}.pushRemote"), "publish"],
        );
        run_test_git(
            &project_root,
            &["config", "remote.publish.push", "refs/heads/*:refs/heads/*"],
        );

        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        assert!(
            store
                .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::CommitAndPush,)
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-push",
                123,
                "run-push",
                &root.join("push.out"),
                &root.join("push.err"),
            )
            .unwrap();
        let git_start =
            capture_agent_git_start_state(&project_root, AgentGitMode::CommitAndPush).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-push",
            "run-push",
            Some(&git_start),
        )
        .unwrap();
        assert!(
            bind_agent_git_working_task_identity(&store, &project, "session-push", "run-push",)
                .unwrap()
        );
        fs::write(project_root.join("publish.txt"), "implemented\n").unwrap();
        run_test_git(&project_root, &["add", "publish.txt"]);
        move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-push".to_string(),
            },
            &store,
        )
        .unwrap();

        run_test_git(&project_root, &["add", "--all"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Publish feature",
                "-m",
                "CLT-Task: codex:session-push",
            ],
        );
        let task_commit = run_test_git(&project_root, &["rev-parse", "HEAD"]);
        run_test_git(&project_root, &["branch", "-f", "side", &task_commit]);
        let pending = store
            .git_finalization_blocking(project.id, "session-push")
            .unwrap()
            .unwrap();
        let completed = reconcile_agent_git_finalization(
            &store,
            &project_root,
            pending,
            Some("run-push"),
            None,
        )
        .unwrap();
        assert_eq!(completed.state, GitFinalizationState::Completed);
        assert_eq!(completed.commit_oid.as_deref(), Some(task_commit.as_str()));
        assert_eq!(
            run_test_git(&publish_root, &["rev-parse", &branch_ref]),
            task_commit
        );
        assert_eq!(
            run_test_git(&publish_root, &["rev-parse", "refs/heads/side"]),
            initial_head
        );
        assert_eq!(
            run_test_git(&origin_root, &["rev-parse", &branch_ref]),
            initial_head
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn sealed_commit_push_runs_the_repository_pre_push_hook() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("automated-git-pre-push-hook");
        let project_root = root.join("project");
        let remote_root = root.join("remote.git");
        let hook_marker = root.join("pre-push-ran");
        init_tasks(&project_root, false).unwrap();
        let initial_head = initialize_test_git_repository(&project_root);
        fs::create_dir_all(&remote_root).unwrap();
        run_test_git(&remote_root, &["init", "--bare"]);
        run_test_git(
            &project_root,
            &["remote", "add", "origin", remote_root.to_str().unwrap()],
        );
        run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);

        let start =
            capture_agent_git_start_state(&project_root, AgentGitMode::CommitAndPush).unwrap();
        let baseline = AgentGitWorktreeBaseline::from_json(&start.worktree_baseline).unwrap();
        fs::write(project_root.join("hooked.txt"), "must pass policy\n").unwrap();
        run_test_git(&project_root, &["add", "hooked.txt"]);
        run_test_git(&project_root, &["commit", "-m", "Exercise pre-push hook"]);
        let commit_oid = run_test_git(&project_root, &["rev-parse", "HEAD"]);

        let hook = project_root.join(".git/hooks/pre-push");
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\nprintf invoked > '{}'\nexit 1\n",
                hook_marker.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).unwrap();

        let error = push_agent_git_commit_to_frozen_destination(
            &project_root,
            start.branch_ref.as_deref(),
            start.upstream_ref.as_deref(),
            &baseline,
            &commit_oid,
            None,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("Failed to push sealed commit"));
        assert!(hook_marker.is_file());
        let branch_ref = start.branch_ref.unwrap();
        assert_eq!(
            run_test_git(&remote_root, &["rev-parse", &branch_ref]),
            initial_head
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn push_mode_keeps_a_non_fast_forward_publication_pending() {
        let root = temp_root("automated-git-non-fast-forward-push");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        let peer_root = root.join("peer");
        let remote_root = root.join("remote.git");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Preserve rejected publication — COMPLETED 2026-09-02: checked codex:session-push-reject\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        fs::create_dir_all(&remote_root).unwrap();
        run_test_git(&remote_root, &["init", "--bare"]);
        run_test_git(
            &project_root,
            &["remote", "add", "origin", remote_root.to_str().unwrap()],
        );
        run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);

        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::CommitAndPush)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-push-reject",
                123,
                "run-push-reject",
                &root.join("reject.out"),
                &root.join("reject.err"),
            )
            .unwrap();
        let git_start =
            capture_agent_git_start_state(&project_root, AgentGitMode::CommitAndPush).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-push-reject",
            "run-push-reject",
            Some(&git_start),
        )
        .unwrap();
        bind_agent_git_working_task_identity(
            &store,
            &project,
            "session-push-reject",
            "run-push-reject",
        )
        .unwrap();

        run_test_git(
            &root,
            &[
                "clone",
                remote_root.to_str().unwrap(),
                peer_root.to_str().unwrap(),
            ],
        );
        run_test_git(&peer_root, &["config", "user.name", "CLT Peer"]);
        run_test_git(
            &peer_root,
            &["config", "user.email", "clt-peer@example.invalid"],
        );
        fs::write(peer_root.join("remote-only.txt"), "remote advance\n").unwrap();
        run_test_git(&peer_root, &["add", "remote-only.txt"]);
        run_test_git(&peer_root, &["commit", "-m", "Advance remote"]);
        run_test_git(&peer_root, &["push"]);
        let remote_tip = run_test_git(&peer_root, &["rev-parse", "HEAD"]);

        fs::write(project_root.join("local-only.txt"), "local task\n").unwrap();
        run_test_git(&project_root, &["add", "local-only.txt"]);
        move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-push-reject".to_string(),
            },
            &store,
        )
        .unwrap();
        run_test_git(&project_root, &["add", "--all"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Preserve rejected publication",
                "-m",
                "CLT-Task: codex:session-push-reject",
            ],
        );
        let pending = store
            .git_finalization_blocking(project.id, "session-push-reject")
            .unwrap()
            .unwrap();
        let error = reconcile_agent_git_finalization(
            &store,
            &project_root,
            pending,
            Some("run-push-reject"),
            None,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("Failed to push sealed commit"));
        assert_eq!(
            store
                .git_finalization_blocking(project.id, "session-push-reject")
                .unwrap()
                .unwrap()
                .state,
            GitFinalizationState::PushPending
        );
        let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);
        assert_eq!(
            run_test_git(&remote_root, &["rev-parse", &branch_ref]),
            remote_tip
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_commit_proof_rejects_a_duplicate_session_marker() {
        let root = temp_root("git-finalization-duplicate-marker");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Finished — COMPLETED 2026-09-02: checked codex:session-duplicate\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Duplicate marker codex:session-duplicate\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "--all"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Duplicate marker fixture",
                "-m",
                "CLT-Task: codex:session-duplicate",
            ],
        );
        let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);

        assert!(
            !git_ref_contains_completed_task(&project_root, &branch_ref, "session-duplicate")
                .unwrap()
        );
        assert_eq!(
            find_agent_git_task_commit(
                &project_root,
                &starting_head,
                Some(&branch_ref),
                "session-duplicate",
                &durable_task_identity("Finished").unwrap(),
            )
            .unwrap(),
            None
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_commit_proof_rejects_a_partially_staged_board_move() {
        let root = temp_root("git-finalization-partial-board");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Move me\n",
        )
        .unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Move me — COMPLETED 2026-09-02: checked codex:session-partial\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "tasks/done.md"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Stage only Done",
                "-m",
                "CLT-Task: codex:session-partial",
            ],
        );
        let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);

        assert_eq!(
            find_agent_git_task_commit(
                &project_root,
                &starting_head,
                Some(&branch_ref),
                "session-partial",
                &durable_task_identity("Move me").unwrap(),
            )
            .unwrap(),
            None
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_finalization_rejects_a_board_only_commit_with_source_left_uncommitted() {
        let root = temp_root("git-finalization-leftover-source");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Implement source — COMPLETED 2026-09-02: checked codex:session-source\n",
        )
        .unwrap();
        fs::write(project_root.join("source.txt"), "before\n").unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-source",
                123,
                "run-source",
                &root.join("source.out"),
                &root.join("source.err"),
            )
            .unwrap();
        let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-source",
            "run-source",
            Some(&git_start),
        )
        .unwrap();
        bind_agent_git_working_task_identity(&store, &project, "session-source", "run-source")
            .unwrap();
        fs::write(project_root.join("source.txt"), "after\n").unwrap();
        let error = move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-source".to_string(),
            },
            &store,
        )
        .unwrap_err();
        assert!(error.to_string().contains("remaining unstaged"));

        let pending = store
            .git_finalization_blocking(project.id, "session-source")
            .unwrap()
            .unwrap();
        assert_eq!(pending.state, GitFinalizationState::Working);
        assert_eq!(read_tasks(&project_root, "doing").unwrap().len(), 1);
        assert_eq!(
            fs::read_to_string(project_root.join("source.txt")).unwrap(),
            "after\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_commit_proof_rejects_an_extra_untrailed_agent_commit() {
        let root = temp_root("git-finalization-extra-agent-commit");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- One commit task\n",
        )
        .unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        fs::write(project_root.join("implementation.txt"), "implemented\n").unwrap();
        run_test_git(&project_root, &["add", "implementation.txt"]);
        run_test_agent_git(&project_root, &["commit", "-m", "Implement first"]);
        fs::write(project_root.join("tasks/todo.md"), "# Todo Tasks\n").unwrap();
        fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- One commit task — COMPLETED 2026-09-02: checked codex:session-one-commit\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "--all", "--", "tasks"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Finish task",
                "-m",
                "CLT-Task: codex:session-one-commit",
            ],
        );
        let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);

        assert_eq!(
            find_agent_git_task_commit(
                &project_root,
                &starting_head,
                Some(&branch_ref),
                "session-one-commit",
                &durable_task_identity("One commit task").unwrap(),
            )
            .unwrap(),
            None
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn premanifest_audit_rejects_an_agent_commit_with_an_overridden_author() {
        let root = temp_root("git-finalization-overridden-author");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        fs::write(project_root.join("implementation.txt"), "premature\n").unwrap();
        run_test_git(&project_root, &["add", "implementation.txt"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "--author=Different Author <different@example.invalid>",
                "-m",
                "Premature implementation",
            ],
        );
        let manifest_parent = run_test_git(&project_root, &["rev-parse", "HEAD"]);
        let store = agent::TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();

        assert!(
            !agent_git_range_is_safe_before_manifest(
                AgentGitProofContext {
                    store: &store,
                    project_id: 1,
                },
                &project_root,
                &starting_head,
                &manifest_parent,
                "session-current",
            )
            .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn premanifest_audit_rejects_a_commit_with_no_agent_identity_or_journal() {
        let root = temp_root("git-finalization-unproven-non-agent");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        fs::write(project_root.join("premature.txt"), "premature\n").unwrap();
        run_test_git(&project_root, &["add", "premature.txt"]);
        run_test_git(&project_root, &["commit", "-m", "Unproven premature work"]);
        let manifest_parent = run_test_git(&project_root, &["rev-parse", "HEAD"]);
        let store = agent::TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();

        assert!(
            !agent_git_range_is_safe_before_manifest(
                AgentGitProofContext {
                    store: &store,
                    project_id: 1,
                },
                &project_root,
                &starting_head,
                &manifest_parent,
                "session-current",
            )
            .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn premanifest_audit_accepts_another_sessions_exact_completed_commit() {
        let root = temp_root("git-finalization-proven-other-task");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        let peer_root = root.join("peer");
        let remote_root = root.join("remote.git");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Task A — BLOCKED 2026-09-02: waiting codex:session-a\n- Task B — COMPLETED 2026-09-02: checked\n",
        )
        .unwrap();
        fs::write(project_root.join("tasks/doing.md"), "# Doing Tasks\n").unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        fs::create_dir_all(&remote_root).unwrap();
        run_test_git(&remote_root, &["init", "--bare"]);
        run_test_git(
            &project_root,
            &["remote", "add", "origin", remote_root.to_str().unwrap()],
        );
        run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
        run_test_git(
            &root,
            &[
                "clone",
                remote_root.to_str().unwrap(),
                peer_root.to_str().unwrap(),
            ],
        );
        run_test_git(&peer_root, &["config", "user.name", "CLT Peer"]);
        run_test_git(
            &peer_root,
            &["config", "user.email", "clt-peer@example.invalid"],
        );
        fs::write(peer_root.join("upstream.txt"), "upstream\n").unwrap();
        run_test_git(&peer_root, &["add", "upstream.txt"]);
        run_test_git(&peer_root, &["commit", "-m", "Advance upstream"]);
        run_test_git(&peer_root, &["push"]);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-a",
                122,
                "run-a",
                &root.join("a.out"),
                &root.join("a.err"),
            )
            .unwrap();
        let task_a_start =
            capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-a",
            "run-a",
            Some(&task_a_start),
        )
        .unwrap();
        assert!(
            bind_agent_git_working_task_identity(&store, &project, "session-a", "run-a").unwrap()
        );
        store
            .set_session_control_state_blocking(
                project.id,
                "session-a",
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap();
        let git_start = prepare_agent_git_start_state_for_run(
            &store,
            &project,
            AgentTaskSelection::NextTodo,
            false,
            false,
            "run-b",
        )
        .unwrap()
        .unwrap();
        assert_eq!(git_start.starting_head, starting_head);
        assert_eq!(
            run_test_git(&project_root, &["rev-parse", "HEAD"]),
            starting_head
        );
        move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "2").unwrap();
        let task_b = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing)
            .unwrap()
            .remove(0);
        attach_codex_session_to_task_after_lock(
            &project_root,
            TaskStatus::Doing,
            &task_b,
            "session-b",
            || {},
        )
        .unwrap();
        store
            .mark_session_running_blocking(
                project.id,
                "session-b",
                123,
                "run-b",
                &root.join("b.out"),
                &root.join("b.err"),
            )
            .unwrap();
        ensure_agent_git_working_record(&store, &project, "session-b", "run-b", Some(&git_start))
            .unwrap();
        assert!(
            bind_agent_git_working_task_identity(&store, &project, "session-b", "run-b").unwrap()
        );
        fs::write(project_root.join("task-b.txt"), "implemented\n").unwrap();
        run_test_git(&project_root, &["add", "--all"]);
        move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-b".to_string(),
            },
            &store,
        )
        .unwrap();
        run_test_git(&project_root, &["add", "--all"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Finish task B",
                "-m",
                "CLT-Task: codex:session-b",
            ],
        );
        let task_b_commit = run_test_git(&project_root, &["rev-parse", "HEAD"]);
        let pending = store
            .git_finalization_blocking(project.id, "session-b")
            .unwrap()
            .unwrap();
        let completed =
            reconcile_agent_git_finalization(&store, &project_root, pending, Some("run-b"), None)
                .unwrap();
        assert_eq!(completed.state, GitFinalizationState::Completed);

        assert!(
            agent_git_range_is_safe_before_manifest(
                AgentGitProofContext {
                    store: &store,
                    project_id: project.id,
                },
                &project_root,
                &starting_head,
                &task_b_commit,
                "session-a",
            )
            .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn premanifest_audit_rejects_a_merge_with_extra_resolution_content() {
        let root = temp_root("git-finalization-mutated-merge");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        let primary_branch = run_test_git(&project_root, &["branch", "--show-current"]);
        run_test_git(&project_root, &["checkout", "-b", "upstream-fixture"]);
        fs::write(project_root.join("upstream.txt"), "upstream\n").unwrap();
        run_test_git(&project_root, &["add", "upstream.txt"]);
        run_test_git(&project_root, &["commit", "-m", "Upstream change"]);
        run_test_git(&project_root, &["checkout", &primary_branch]);
        fs::write(project_root.join("local.txt"), "local\n").unwrap();
        run_test_git(&project_root, &["add", "local.txt"]);
        run_test_git(&project_root, &["commit", "-m", "Local change"]);
        run_test_git(&project_root, &["merge", "--no-commit", "upstream-fixture"]);
        fs::write(project_root.join("smuggled.txt"), "implementation\n").unwrap();
        run_test_git(&project_root, &["add", "smuggled.txt"]);
        run_test_agent_git(&project_root, &["commit", "-m", "Synchronization merge"]);
        let manifest_parent = run_test_git(&project_root, &["rev-parse", "HEAD"]);
        let store = agent::TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();

        assert!(
            !agent_git_range_is_safe_before_manifest(
                AgentGitProofContext {
                    store: &store,
                    project_id: 1,
                },
                &project_root,
                &starting_head,
                &manifest_parent,
                "session-current",
            )
            .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn premanifest_audit_rejects_a_clean_merge_with_an_unproven_side_commit() {
        let root = temp_root("git-finalization-clean-sync-merge");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        let primary_branch = run_test_git(&project_root, &["branch", "--show-current"]);
        run_test_git(&project_root, &["checkout", "-b", "upstream-fixture"]);
        fs::write(project_root.join("upstream.txt"), "upstream\n").unwrap();
        run_test_git(&project_root, &["add", "upstream.txt"]);
        run_test_git(&project_root, &["commit", "-m", "Upstream change"]);
        run_test_git(&project_root, &["checkout", &primary_branch]);
        run_test_git(
            &project_root,
            &[
                "merge",
                "--no-ff",
                "upstream-fixture",
                "-m",
                "Synchronization merge",
            ],
        );
        let manifest_parent = run_test_git(&project_root, &["rev-parse", "HEAD"]);
        let store = agent::TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();

        assert!(
            !agent_git_range_is_safe_before_manifest(
                AgentGitProofContext {
                    store: &store,
                    project_id: 1,
                },
                &project_root,
                &starting_head,
                &manifest_parent,
                "session-current",
            )
            .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_commit_proof_rejects_a_divergent_starting_head() {
        let root = temp_root("git-finalization-divergent-head");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Divergent task — COMPLETED 2026-09-02: checked codex:session-divergent\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "tasks/done.md"]);
        let tree = run_test_git(&project_root, &["write-tree"]);
        let divergent = run_test_agent_git(
            &project_root,
            &[
                "commit-tree",
                &tree,
                "-m",
                "Divergent task",
                "-m",
                "CLT-Task: codex:session-divergent",
            ],
        );
        let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);
        run_test_git(&project_root, &["update-ref", &branch_ref, &divergent]);

        assert_eq!(
            find_agent_git_task_commit(
                &project_root,
                &starting_head,
                Some(&branch_ref),
                "session-divergent",
                &durable_task_identity("Divergent task").unwrap(),
            )
            .unwrap(),
            None
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_commit_proof_rejects_a_distinct_agent_task_in_the_same_range() {
        let root = temp_root("git-finalization-distinct-agent-task");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Task A\n- Task B\n",
        )
        .unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Task A\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Task B — COMPLETED 2026-09-02: checked codex:session-b\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "--all", "--", "tasks"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Finish task B",
                "-m",
                "CLT-Task: codex:session-b",
            ],
        );
        fs::write(project_root.join("tasks/todo.md"), "# Todo Tasks\n").unwrap();
        fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Task A — COMPLETED 2026-09-02: checked codex:session-a\n- Task B — COMPLETED 2026-09-02: checked codex:session-b\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "--all", "--", "tasks"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Finish task A",
                "-m",
                "CLT-Task: codex:session-a",
            ],
        );
        let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);

        assert_eq!(
            find_agent_git_task_commit(
                &project_root,
                &starting_head,
                Some(&branch_ref),
                "session-a",
                &durable_task_identity("Task A").unwrap(),
            )
            .unwrap(),
            None
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_commit_proof_rejects_two_commits_with_the_same_task_trailer() {
        let root = temp_root("git-finalization-two-task-trailers");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Trailer task\n",
        )
        .unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        fs::write(project_root.join("implementation.txt"), "implemented\n").unwrap();
        run_test_git(&project_root, &["add", "implementation.txt"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "First task commit",
                "-m",
                "CLT-Task: codex:session-two-trailers",
            ],
        );
        fs::write(project_root.join("tasks/todo.md"), "# Todo Tasks\n").unwrap();
        fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Trailer task — COMPLETED 2026-09-02: checked codex:session-two-trailers\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "--all", "--", "tasks"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Second task commit",
                "-m",
                "CLT-Task: codex:session-two-trailers",
            ],
        );
        let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);

        assert_eq!(
            find_agent_git_task_commit(
                &project_root,
                &starting_head,
                Some(&branch_ref),
                "session-two-trailers",
                &durable_task_identity("Trailer task").unwrap(),
            )
            .unwrap(),
            None
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn committed_task_parser_matches_folder_and_nested_board_grammar() {
        let root = temp_root("git-finalization-folder-parser");
        let project_root = root.join("project");
        fs::create_dir_all(project_root.join("tasks/doing/0001-parent/todo")).unwrap();
        fs::create_dir_all(project_root.join("tasks/doing/0002-real/assets/done")).unwrap();
        fs::write(
            project_root.join("tasks/doing/0001-parent/task.md"),
            "Parent task\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/doing/0001-parent/todo/0001-child.md"),
            "Nested child\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/doing/0002-real/task.md"),
            "Real task codex:session-real\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/doing/0002-real/assets/done/fake.md"),
            "Fake attachment codex:session-fake\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/doing/README.md"),
            "Direct README task\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let entries = git_ref_task_entries(&project_root, "HEAD").unwrap();
        let contents = entries
            .iter()
            .map(|entry| entry.content.trim().to_string())
            .collect::<Vec<_>>();

        assert!(contents.contains(&"Parent task".to_string()));
        assert!(contents.contains(&"Nested child".to_string()));
        assert!(contents.contains(&"Real task codex:session-real".to_string()));
        assert!(contents.contains(&"Direct README task".to_string()));
        assert!(
            !contents
                .iter()
                .any(|content| content.contains("Fake attachment"))
        );
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.content.contains("Nested child"))
                .map(|entry| entry.status.as_str()),
            Some("todo")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_finalizer_does_not_steal_an_expired_lease_from_a_running_session() {
        let root = temp_root("git-finalizer-live-control-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-live-control",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/master"),
                    upstream_ref: None,
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: None,
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
        store
            .mark_session_running_blocking(
                project.id,
                "session-live-control",
                std::process::id(),
                "run-live-control",
                &root.join("live.out"),
                &root.join("live.err"),
            )
            .unwrap();
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "live-session-holder", "100", "101")
                .unwrap()
        );

        assert!(
            !store
                .try_acquire_git_finalization_lease_blocking(
                    project.id,
                    "git-finalizer-contender",
                    "200",
                    "500",
                    None,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .holder,
            "live-session-holder"
        );
        assert_eq!(
            store
                .session_control_blocking(project.id, "session-live-control")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Running
        );
        assert!(
            store
                .release_lease_blocking(project.id, "live-session-holder")
                .unwrap()
        );
        store
            .set_session_control_recovery_token_blocking(
                project.id,
                "session-live-control",
                "ordinary-resume-token",
            )
            .unwrap();
        assert!(
            !store
                .try_acquire_git_finalization_lease_blocking(
                    project.id,
                    "git-finalizer-contender",
                    "200",
                    "500",
                    None,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project.id, "session-live-control")
                .unwrap()
                .unwrap()
                .run_token
                .as_deref(),
            Some("ordinary-resume-token")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_finalizer_gate_accepts_each_pending_sessions_exact_recovery_generation() {
        let root = temp_root("git-finalizer-multiple-recovery-controls");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        for (session, task_identity) in [
            ("session-working", None),
            ("session-finalizing", Some("finalizing task")),
        ] {
            assert!(
                store
                    .create_git_finalization_blocking(agent::NewGitFinalization {
                        project_id: project.id,
                        codex_session_id: session,
                        git_mode: AgentGitMode::Commit,
                        starting_head: Some("1111111111111111111111111111111111111111"),
                        branch_ref: Some("refs/heads/master"),
                        upstream_ref: None,
                        worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                        task_identity,
                        owner_run_token: None,
                        created_at: "100",
                    })
                    .unwrap()
            );
        }
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-finalizing",
                    0,
                    GitFinalizationState::Tracking,
                    None,
                    None,
                    None,
                    "101",
                )
                .unwrap()
        );
        store
            .set_session_control_recovery_token_blocking(
                project.id,
                "session-working",
                "clt-git-finalization:999",
            )
            .unwrap();
        assert!(
            store
                .ensure_pending_git_finalization_resume_requested_blocking(
                    project.id,
                    "session-finalizing",
                )
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project.id, "session-working")
                .unwrap()
                .unwrap()
                .run_token
                .as_deref(),
            Some("clt-git-finalization:999")
        );
        assert_eq!(
            store
                .session_control_blocking(project.id, "session-finalizing")
                .unwrap()
                .unwrap()
                .run_token
                .as_deref(),
            Some("clt-git-finalization:1")
        );
        assert!(
            store
                .try_acquire_git_finalization_lease_blocking(
                    project.id,
                    "multi-session-finalizer",
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                    None,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project.id, "session-working")
                .unwrap()
                .unwrap()
                .run_token
                .as_deref(),
            Some("clt-git-finalization:0")
        );
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-finalizing",
                    1,
                    GitFinalizationState::CommitPending,
                    None,
                    None,
                    None,
                    "102",
                )
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project.id, "session-finalizing")
                .unwrap()
                .unwrap()
                .run_token
                .as_deref(),
            Some("clt-git-finalization:2")
        );
        assert!(
            store
                .git_finalization_lease_is_owned_blocking(
                    project.id,
                    "multi-session-finalizer",
                    &agent_timestamp(),
                )
                .unwrap()
        );
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-finalizing",
                    2,
                    GitFinalizationState::Completed,
                    None,
                    Some("2222222222222222222222222222222222222222"),
                    None,
                    "103",
                )
                .unwrap()
        );
        assert!(
            store
                .session_control_blocking(project.id, "session-finalizing")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .git_finalization_lease_is_owned_blocking(
                    project.id,
                    "multi-session-finalizer",
                    &agent_timestamp(),
                )
                .unwrap()
        );
        assert!(
            store
                .release_lease_blocking(project.id, "multi-session-finalizer")
                .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_finalizer_heartbeat_renews_and_exact_holder_loss_fences_it() {
        let root = temp_root("git-finalizer-heartbeat-fence");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-heartbeat",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/master"),
                    upstream_ref: None,
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: None,
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
        drop(store);

        let lease = try_acquire_agent_git_finalization_lease_with_timeout(
            &state_dir,
            &project,
            false,
            Duration::from_secs(2),
        )
        .unwrap()
        .unwrap();
        thread::sleep(Duration::from_secs(3));
        lease.ensure_owned().unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert!(
            !store
                .try_acquire_git_finalization_lease_blocking(
                    project.id,
                    "competing-finalizer",
                    &agent_timestamp(),
                    &agent_timestamp_after(1),
                    None,
                )
                .unwrap()
        );
        let exact_holder = lease.holder.clone();
        assert!(
            store
                .release_lease_blocking(project.id, &exact_holder)
                .unwrap()
        );
        let fence_error = lease.ensure_owned().unwrap_err();
        assert!(format!("{fence_error:#}").contains("lost its exact project lease"));
        drop(lease);

        assert!(
            store
                .try_acquire_git_finalization_lease_blocking(
                    project.id,
                    "replacement-finalizer",
                    &agent_timestamp(),
                    &agent_timestamp_after(1),
                    None,
                )
                .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_prioritizes_pending_git_finalization_over_new_todo_work() {
        let root = temp_root("scheduler-git-finalization-priority");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Later task\n",
        )
        .unwrap();
        fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Provisional — COMPLETED 2026-09-02: checked codex:session-pending\n",
        )
        .unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        assert!(
            store
                .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit,)
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-pending",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some(&starting_head),
                    branch_ref: Some(&branch_ref),
                    upstream_ref: None,
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: None,
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
        assert!(
            store
                .recover_git_finalization_intent_blocking(
                    project.id,
                    "session-pending",
                    0,
                    "provisional",
                    None,
                    "101",
                )
                .unwrap()
        );
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-pending",
                    1,
                    GitFinalizationState::CommitPending,
                    None,
                    None,
                    None,
                    "102",
                )
                .unwrap()
        );
        store
            .set_session_control_state_blocking(
                project.id,
                "session-pending",
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap();
        drop(store);

        let start =
            run_agent_scheduler_pass_with_max_global_jobs(&state_dir, true, &[], 1, None).unwrap();
        assert_eq!(start.jobs.len(), 1);
        assert_eq!(
            start.jobs[0].task_selection,
            AgentTaskSelection::ResumeSession
        );
        assert_eq!(
            start.jobs[0].resume_session_id.as_deref(),
            Some("session-pending")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_keeps_push_pending_autonomous_and_does_not_resume_codex() {
        let root = temp_root("scheduler-autonomous-push-pending");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Do not start while publication is pending\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-autonomous-push",
                    git_mode: AgentGitMode::CommitAndPush,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/master"),
                    upstream_ref: Some("refs/remotes/origin/master"),
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: Some("autonomous push"),
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
        for (generation, state, commit_oid) in [
            (0, GitFinalizationState::Tracking, None),
            (1, GitFinalizationState::CommitPending, None),
            (
                2,
                GitFinalizationState::PushPending,
                Some("2222222222222222222222222222222222222222"),
            ),
        ] {
            assert!(
                store
                    .compare_and_set_git_finalization_blocking(
                        project.id,
                        "session-autonomous-push",
                        generation,
                        state,
                        None,
                        commit_oid,
                        None,
                        "101",
                    )
                    .unwrap()
            );
        }
        store
            .set_session_control_state_blocking(
                project.id,
                "session-autonomous-push",
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap();
        assert!(
            store
                .try_acquire_lease_blocking(
                    project.id,
                    "concurrent-finalizer",
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );
        drop(store);

        let fenced =
            run_agent_scheduler_pass_with_max_global_jobs(&state_dir, true, &[], 1, None).unwrap();
        assert!(fenced.jobs.is_empty());
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let untouched = store
            .git_finalization_blocking(project.id, "session-autonomous-push")
            .unwrap()
            .unwrap();
        assert_eq!(untouched.generation, 3);
        assert!(untouched.last_error.is_none());
        assert!(
            store
                .release_lease_blocking(project.id, "concurrent-finalizer")
                .unwrap()
        );
        drop(store);

        let pass =
            run_agent_scheduler_pass_with_max_global_jobs(&state_dir, true, &[], 1, None).unwrap();
        assert!(pass.jobs.is_empty());
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert!(
            store
                .session_control_blocking(project.id, "session-autonomous-push")
                .unwrap()
                .is_none()
        );
        let failed = store
            .git_finalization_blocking(project.id, "session-autonomous-push")
            .unwrap()
            .unwrap();
        assert_eq!(failed.state, GitFinalizationState::PushPending);
        assert_eq!(failed.generation, 4);
        assert!(failed.last_error.is_some());
        drop(store);

        let backed_off =
            run_agent_scheduler_pass_with_max_global_jobs(&state_dir, true, &[], 1, None).unwrap();
        assert!(backed_off.jobs.is_empty());
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let still_pending = store
            .git_finalization_blocking(project.id, "session-autonomous-push")
            .unwrap()
            .unwrap();
        assert_eq!(still_pending.state, GitFinalizationState::PushPending);
        assert_eq!(still_pending.generation, failed.generation);
        assert_eq!(still_pending.last_error, failed.last_error);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_job_preserves_pending_git_finalization_then_accepts_the_proven_commit() {
        let root = temp_root("agent-job-git-finalization-lifecycle");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- Durable finish — COMPLETED 2026-09-02: cargo test passed codex:session-lifecycle\n",
        )
        .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        assert!(
            store
                .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit,)
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-lifecycle",
                123,
                "run-before-commit",
                &root.join("before.out"),
                &root.join("before.err"),
            )
            .unwrap();
        let git_start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-lifecycle",
            "run-before-commit",
            Some(&git_start),
        )
        .unwrap();
        assert!(
            bind_agent_git_working_task_identity(
                &store,
                &project,
                "session-lifecycle",
                "run-before-commit",
            )
            .unwrap()
        );
        fs::write(project_root.join("durable.txt"), "implemented\n").unwrap();
        run_test_git(&project_root, &["add", "durable.txt"]);
        move_task_to_done_with_agent_store(
            &project_root,
            TaskStatus::Doing,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-before-commit".to_string(),
            },
            &store,
        )
        .unwrap();

        let first_holder = "git-finalization-first-holder";
        assert!(
            store
                .try_acquire_lease_blocking(project.id, first_holder, "100", "9999999999")
                .unwrap()
        );
        let mut timed_out_runner = FakeAgentRunner::new(&state_dir, "timeout");
        timed_out_runner.result.codex_session_id = Some("session-lifecycle".to_string());
        timed_out_runner.result.session_run_token = Some("run-before-commit".to_string());
        timed_out_runner.result.exit_code = None;
        let pending_completion = run_agent_job(
            AgentRunJob {
                state_dir: state_dir.clone(),
                project: project.clone(),
                holder: first_holder.to_string(),
                worker_token: None,
                max_global_jobs: 12,
                task_selection: AgentTaskSelection::NextTodo,
                resume_session_id: None,
                blocked_task_count_before: 0,
                done_task_contents_before: Vec::new(),
                blocked_task_snapshots_before: Vec::new(),
            },
            &timed_out_runner,
            &new_agent_shutdown_signal(),
        )
        .unwrap();

        assert_eq!(pending_completion.status, "failure");
        assert!(pending_completion.summary.contains("FINALIZING"));
        assert_eq!(
            store
                .session_control_blocking(project.id, "session-lifecycle")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Running
        );
        assert_eq!(
            store
                .git_finalization_blocking(project.id, "session-lifecycle")
                .unwrap()
                .unwrap()
                .state,
            GitFinalizationState::CommitPending
        );

        run_test_git(&project_root, &["add", "--all"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Durable finish",
                "-m",
                "CLT-Task: codex:session-lifecycle",
            ],
        );
        store
            .mark_session_running_blocking(
                project.id,
                "session-lifecycle",
                124,
                "run-after-commit",
                &root.join("after.out"),
                &root.join("after.err"),
            )
            .unwrap();
        let second_holder = "git-finalization-second-holder";
        assert!(
            store
                .try_acquire_lease_blocking(project.id, second_holder, "200", "9999999999")
                .unwrap()
        );
        let mut resumed_runner = FakeAgentRunner::new(&state_dir, "timeout");
        resumed_runner.result.codex_session_id = Some("session-lifecycle".to_string());
        resumed_runner.result.session_run_token = Some("run-after-commit".to_string());
        resumed_runner.result.exit_code = None;
        let completed = run_agent_job(
            AgentRunJob {
                state_dir: state_dir.clone(),
                project: project.clone(),
                holder: second_holder.to_string(),
                worker_token: None,
                max_global_jobs: 12,
                task_selection: AgentTaskSelection::ResumeSession,
                resume_session_id: Some("session-lifecycle".to_string()),
                blocked_task_count_before: 0,
                done_task_contents_before: completed_task_contents(&project_root).unwrap(),
                blocked_task_snapshots_before: Vec::new(),
            },
            &resumed_runner,
            &new_agent_shutdown_signal(),
        )
        .unwrap();

        assert_eq!(completed.status, "success");
        assert!(completed.summary.contains("proved"));
        assert!(
            store
                .session_control_blocking(project.id, "session-lifecycle")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .git_finalization_blocking(project.id, "session-lifecycle")
                .unwrap()
                .unwrap()
                .state,
            GitFinalizationState::Completed
        );
        assert_eq!(
            store
                .latest_run_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .status,
            "success"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_rolls_forward_a_proven_commit_without_resuming_codex() {
        let root = temp_root("scheduler-git-finalization-roll-forward");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- Later task\n",
        )
        .unwrap();
        let starting_head = initialize_test_git_repository(&project_root);
        let branch_ref = run_test_git(&project_root, &["symbolic-ref", "HEAD"]);
        fs::write(
            project_root.join("tasks/done.md"),
            "# Done Tasks\n- Already committed — COMPLETED 2026-09-02: checked codex:session-roll-forward\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "--all"]);
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Already committed",
                "-m",
                "CLT-Task: codex:session-roll-forward",
            ],
        );

        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        assert!(
            store
                .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit,)
                .unwrap()
        );
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-roll-forward",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some(&starting_head),
                    branch_ref: Some(&branch_ref),
                    upstream_ref: None,
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: None,
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
        assert!(
            store
                .recover_git_finalization_intent_blocking(
                    project.id,
                    "session-roll-forward",
                    0,
                    &durable_task_identity("Already committed").unwrap(),
                    None,
                    "101",
                )
                .unwrap()
        );
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-roll-forward",
                    1,
                    GitFinalizationState::CommitPending,
                    None,
                    None,
                    None,
                    "102",
                )
                .unwrap()
        );
        store
            .set_session_control_state_blocking(
                project.id,
                "session-roll-forward",
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap();
        drop(store);

        let mut start =
            run_agent_scheduler_pass_with_max_global_jobs(&state_dir, true, &[], 1, None).unwrap();
        assert_eq!(start.jobs.len(), 1);
        assert_eq!(start.jobs[0].task_selection, AgentTaskSelection::NextTodo);

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store
                .git_finalization_blocking(project.id, "session-roll-forward")
                .unwrap()
                .unwrap()
                .state,
            GitFinalizationState::Completed
        );
        assert!(
            store
                .session_control_blocking(project.id, "session-roll-forward")
                .unwrap()
                .is_none()
        );
        let job = start.jobs.pop().unwrap();
        assert!(
            store
                .release_lease_blocking(project.id, &job.holder)
                .unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_register_is_idempotent_and_lists_projects() {
        let root = temp_root("agent-register");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

        store
            .register_project_blocking(&project_root, "project")
            .unwrap();

        assert!(store.unregister_project_blocking(&project_root).unwrap());
        assert!(!store.unregister_project_blocking(&project_root).unwrap());
        assert!(store.list_projects_blocking().unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_unregister_and_clean_preserve_an_unconsumed_git_launch_boundary() {
        let root = temp_root("agent-unregister-unconsumed-git-launch");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let launch = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        store
            .record_git_launch_state_blocking(
                project.id,
                "orphaned-release",
                AgentGitMode::Commit,
                &launch,
                "100",
            )
            .unwrap();

        let unregister_error = store
            .unregister_project_blocking(&project_root)
            .unwrap_err();
        assert!(format!("{unregister_error:#}").contains("launch boundary"));
        let clean_error = store.clean_agent_history_blocking("200").unwrap_err();
        assert!(format!("{clean_error:#}").contains("launch boundary"));
        assert_eq!(store.list_projects_blocking().unwrap().len(), 1);
        assert_eq!(
            store
                .git_launch_state_blocking(project.id, "orphaned-release")
                .unwrap(),
            Some((AgentGitMode::Commit, launch))
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_unregister_preserves_push_pending_finalization() {
        let root = temp_root("agent-unregister-push-pending");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-pending-unregister",
                123,
                "run-pending-unregister",
                &root.join("pending.out"),
                &root.join("pending.err"),
            )
            .unwrap();
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-pending-unregister",
                    git_mode: AgentGitMode::CommitAndPush,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/master"),
                    upstream_ref: Some("refs/remotes/origin/master"),
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: Some("pending task"),
                    owner_run_token: Some("run-pending-unregister"),
                    created_at: "100",
                })
                .unwrap()
        );
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-pending-unregister",
                    0,
                    GitFinalizationState::Tracking,
                    Some("run-pending-unregister"),
                    None,
                    None,
                    "101",
                )
                .unwrap()
        );
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-pending-unregister",
                    1,
                    GitFinalizationState::CommitPending,
                    Some("run-pending-unregister"),
                    None,
                    None,
                    "102",
                )
                .unwrap()
        );
        assert!(
            store
                .compare_and_set_git_finalization_blocking(
                    project.id,
                    "session-pending-unregister",
                    2,
                    GitFinalizationState::PushPending,
                    Some("run-pending-unregister"),
                    Some("2222222222222222222222222222222222222222"),
                    None,
                    "103",
                )
                .unwrap()
        );

        let error = store
            .unregister_project_blocking(&project_root)
            .unwrap_err();
        assert!(format!("{error:#}").contains("nonterminal"));
        assert_eq!(
            store
                .git_finalization_blocking(project.id, "session-pending-unregister")
                .unwrap()
                .unwrap()
                .state,
            GitFinalizationState::PushPending
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn agent_store_unregister_reclaims_dead_process_lease() {
        let root = temp_root("agent-unregister-dead-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(
                    project.id,
                    "clt-agent-4294967295",
                    "100",
                    "9999999999",
                )
                .unwrap()
        );

        assert!(store.unregister_project_blocking(&project_root).unwrap());
        assert!(store.list_projects_blocking().unwrap().is_empty());
        assert_eq!(store.lease_count_blocking().unwrap(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_unregister_reclaims_expired_lease() {
        let root = temp_root("agent-unregister-expired-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "unknown-holder", "100", "101")
                .unwrap()
        );

        assert!(store.unregister_project_blocking(&project_root).unwrap());
        assert!(store.list_projects_blocking().unwrap().is_empty());
        assert_eq!(store.lease_count_blocking().unwrap(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_unregister_rejects_live_or_unknown_lease() {
        let root = temp_root("agent-unregister-active-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "unknown-holder", "100", "9999999999",)
                .unwrap()
        );

        let error = store
            .unregister_project_blocking(&project_root)
            .unwrap_err();
        assert!(error.to_string().contains("agent lease is active"));
        assert_eq!(store.list_projects_blocking().unwrap().len(), 1);
        assert_eq!(store.lease_count_blocking().unwrap(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_unregister_removes_project_run_history() {
        let root = temp_root("agent-unregister-run-history");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
                project_id: project.id,
                status: "failed",
                started_at: "100",
                finished_at: Some("101"),
                exit_code: Some(1),
                log_dir: None,
                stdout_path: None,
                stderr_path: None,
                summary: Some("Codex rejected an untrusted directory"),
                codex_session_id: Some("session-123"),
            })
            .unwrap();
        assert_eq!(store.run_count_blocking().unwrap(), 1);

        assert!(store.unregister_project_blocking(&project_root).unwrap());
        assert!(store.list_projects_blocking().unwrap().is_empty());
        assert_eq!(store.run_count_blocking().unwrap(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_unregister_cascades_session_controls_on_its_fresh_connection() {
        let root = temp_root("agent-unregister-session-control-cascade");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        assert!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .is_some()
        );

        assert!(store.unregister_project_blocking(&project_root).unwrap());
        assert!(
            store
                .session_control_blocking(project_id, "session-123")
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_can_pause_and_resume_registered_project() {
        let root = temp_root("agent-pause-resume");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
    fn agent_store_immediate_retry_clears_only_the_selected_project_failure_count() {
        let root = temp_root("agent-immediate-retry");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap()[0].id;
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
                project_id,
                status: "failure",
                started_at: "100",
                finished_at: Some("101"),
                exit_code: Some(1),
                log_dir: None,
                stdout_path: None,
                stderr_path: None,
                summary: Some("failed"),
                codex_session_id: None,
            })
            .unwrap();
        assert_eq!(store.list_projects_blocking().unwrap()[0].failure_count, 1);
        assert!(
            store
                .try_acquire_lease_blocking(project_id, "expired-holder", "98", "99")
                .unwrap()
        );

        assert!(
            store
                .clear_project_failure_backoff_for_path_blocking(&project_root)
                .unwrap()
        );
        assert_eq!(store.list_projects_blocking().unwrap()[0].failure_count, 0);
        assert_eq!(store.run_count_blocking().unwrap(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_persists_all_git_modes_for_registered_project() {
        let root = temp_root("agent-git-commit");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
            agent::AgentModelDefaults::default()
        );

        let provider = agent::AgentModelProvider {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            env_key: Some("OPENROUTER_API_KEY".to_string()),
            built_in: false,
            enabled: true,
        };
        store.upsert_model_provider_blocking(&provider).unwrap();
        let target = agent::AgentModelTarget {
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
            agent::AgentModelDefaults {
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let provider = agent::AgentModelProvider {
            id: "local-delete".to_string(),
            name: "Local Delete".to_string(),
            base_url: Some("http://localhost:9090/v1".to_string()),
            env_key: None,
            built_in: false,
            enabled: true,
        };
        let model = agent::AgentModelTarget {
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
            agent::AgentModelDefaults::default()
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .upsert_model_target_blocking(&agent::AgentModelTarget {
                provider_id: "openai".to_string(),
                model_id: "gpt-5.6".to_string(),
                label: "GPT-5.6".to_string(),
                enabled: true,
                favorite: true,
                reasoning_effort: None,
            })
            .unwrap();
        store
            .upsert_model_target_blocking(&agent::AgentModelTarget {
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

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
    fn tui_models_navigation_pages_and_searches_visible_models() {
        let mut panel = TuiModelsPanel {
            providers: Vec::new(),
            models: [
                ("alpha", "Alpha"),
                ("beta-v1", "Beta"),
                ("delta-v2", "Delta"),
                ("g-3", "Gamma Preview"),
            ]
            .into_iter()
            .map(|(model_id, label)| agent::AgentModelTarget {
                provider_id: "test".to_string(),
                model_id: model_id.to_string(),
                label: label.to_string(),
                enabled: true,
                favorite: false,
                reasoning_effort: None,
            })
            .collect(),
            defaults: agent::AgentModelDefaults::default(),
            codex_default: "not explicitly set".to_string(),
            codex_default_provider: None,
            codex_default_model: None,
            focus: TuiModelsFocus::Models,
            provider_state: ListState::default(),
            model_state: ListState::default().with_selected(Some(0)),
            model_search: String::new(),
            provider_viewport_height: 0,
            model_viewport_height: 2,
            last_error: None,
        };

        panel.select_page_down();
        assert_eq!(panel.model_state.selected(), Some(2));
        panel.select_page_down();
        assert_eq!(panel.model_state.selected(), Some(3));
        panel.select_page_up();
        assert_eq!(panel.model_state.selected(), Some(1));
        panel.select_first();
        assert_eq!(panel.model_state.selected(), Some(0));
        panel.select_last();
        assert_eq!(panel.model_state.selected(), Some(3));

        let mut search = TuiModelInput::search_models("TA".to_string());
        let message = submit_tui_model_input(&mut search, &mut panel)
            .unwrap()
            .unwrap();
        assert!(message.contains("2 matches"));
        assert_eq!(panel.visible_model_indices(), [1, 2]);
        assert_eq!(panel.model_state.selected(), Some(1));
        panel.select_next();
        assert_eq!(panel.model_state.selected(), Some(2));
        panel.select_next();
        assert_eq!(panel.model_state.selected(), Some(1));
        panel.select_previous();
        assert_eq!(panel.model_state.selected(), Some(2));
        panel.select_first();
        assert_eq!(panel.model_state.selected(), Some(1));
        panel.select_last();
        assert_eq!(panel.model_state.selected(), Some(2));

        assert_eq!(panel.set_model_search("PREVIEW".to_string()), 1);
        assert_eq!(panel.selected_model().unwrap().model_id, "g-3");
        assert_eq!(panel.set_model_search("missing".to_string()), 0);
        assert!(panel.selected_model().is_none());
        assert_eq!(panel.set_model_search(String::new()), 4);
        assert_eq!(panel.model_state.selected(), Some(0));
    }

    #[test]
    fn tui_models_rows_have_labeled_columns_and_independent_defaults() {
        let provider = agent::AgentModelProvider {
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

        let model = agent::AgentModelTarget {
            provider_id: "openai".to_string(),
            model_id: "gpt-5.6".to_string(),
            label: "GPT-5.6".to_string(),
            enabled: true,
            favorite: true,
            reasoning_effort: None,
        };
        let defaults = agent::AgentModelDefaults {
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
            &agent::AgentModelDefaults::default(),
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

        let same_id_on_openrouter = agent::AgentModelTarget {
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
        let store = agent::TursoAgentStore::open_blocking(&root).unwrap();
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
        let providers = vec![agent::AgentModelProvider {
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
        assert_eq!(tui_models_add_provider_hint(), "[n] Add provider");
        assert!(tui_models_provider_choice_prompt().contains("[3] Ollama"));
        assert!(tui_models_provider_choice_prompt().contains("[5] Local/custom"));
        assert!(!tui_models_instructions().contains("1 OpenAI"));
        assert!(tui_models_instructions().contains("r refreshes"));
        assert!(tui_models_instructions().contains("/ searches"));
        assert!(tui_models_instructions().contains("PageUp/PageDown"));
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
        let store = agent::TursoAgentStore::open_blocking(&root).unwrap();
        let provider = agent::AgentModelProvider {
            id: "local-test".to_string(),
            name: "Local Test".to_string(),
            base_url: Some("http://localhost:8080/v1".to_string()),
            env_key: None,
            built_in: false,
            enabled: true,
        };
        store.upsert_model_provider_blocking(&provider).unwrap();
        store
            .upsert_model_target_blocking(&agent::AgentModelTarget {
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
        move_task(&root, TaskStatus::Todo, TaskStatus::Doing, "2").unwrap();

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
        move_task(&root, TaskStatus::Todo, TaskStatus::Backlog, "1").unwrap();

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
    fn agent_store_records_and_clears_daemon_scan_errors() {
        let root = temp_root("agent-daemon-scan-store");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap()[0].id;

        store
            .record_project_daemon_scan_blocking(
                project_id,
                "unavailable",
                Some("Operation not permitted (os error 1)"),
            )
            .unwrap();
        let failed = store.list_projects_blocking().unwrap().remove(0);

        assert_eq!(
            failed.last_daemon_scan_status.as_deref(),
            Some("unavailable")
        );
        assert_eq!(
            failed.last_daemon_scan_error.as_deref(),
            Some("Operation not permitted (os error 1)")
        );

        store
            .record_project_daemon_scan_blocking(project_id, "pending", None)
            .unwrap();
        let recovered = store.list_projects_blocking().unwrap().remove(0);

        assert_eq!(
            recovered.last_daemon_scan_status.as_deref(),
            Some("pending")
        );
        assert_eq!(recovered.last_daemon_scan_error, None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scheduler_persists_unavailable_scan_for_the_ui() {
        let root = temp_root("agent-scheduler-scan-error");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        let tasks_dir = project_root.join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("todo.md"), [0xff, 0xfe]).unwrap();
        fs::write(tasks_dir.join("doing.md"), "# Doing Tasks\n").unwrap();
        fs::write(tasks_dir.join("done.md"), "# Done Tasks\n").unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();

        let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        assert_eq!(start.pass.pending_projects, 0);
        assert_eq!(
            project.last_daemon_scan_status.as_deref(),
            Some("unavailable")
        );
        assert!(project.last_daemon_scan_error.is_some());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_records_and_clears_daemon_checkins() {
        let root = temp_root("agent-daemon-checkin-store");
        let state_dir = root.join("state/clt");
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        let fresh_cli = agent::AgentDaemonCheckin {
            holder: "clt-agent-1".to_string(),
            mode: "cli".to_string(),
            started_at: "100".to_string(),
            checked_in_at: "120".to_string(),
            expires_at: "200".to_string(),
        };
        let fresh_service = agent::AgentDaemonCheckin {
            holder: "clt-agent-2".to_string(),
            mode: "service".to_string(),
            started_at: "100".to_string(),
            checked_in_at: "120".to_string(),
            expires_at: "200".to_string(),
        };
        let stale_cli = agent::AgentDaemonCheckin {
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
        let stale_service = agent::AgentDaemonCheckin {
            holder: "clt-agent-1".to_string(),
            mode: "service".to_string(),
            started_at: "100".to_string(),
            checked_in_at: "120".to_string(),
            expires_at: "150".to_string(),
        };
        let fresh_service = agent::AgentDaemonCheckin {
            holder: "clt-agent-2".to_string(),
            expires_at: "200".to_string(),
            ..stale_service.clone()
        };
        let stale_cli = agent::AgentDaemonCheckin {
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&first_project, "alpha")
            .unwrap();
        store
            .register_project_blocking(&second_project, "beta")
            .unwrap();
        let runner = FakeAgentRunner::new(&state_dir, "success");
        drop(store);

        let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let runner = Arc::new(FakeAgentRunner::new(&state_dir, "success"));
        drop(store);

        let daemon_runner: Arc<dyn AgentRunner> = runner.clone();
        run_agent_daemon_loop(&state_dir, daemon_runner, Duration::ZERO, Some(2)).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(store.run_count_blocking().unwrap(), 1);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
        assert_eq!(runner.ran_project_count(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_daemon_dispatches_independent_worker_outside_async_runtime() {
        let root = temp_root("agent-daemon-independent-dispatch");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "independent daemon task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        drop(store);

        run_agent_daemon_loop_with_executor(
            &state_dir,
            AgentDaemonExecutor::Independent {
                executable: PathBuf::from("/tmp/pinned-clt-generation"),
                dispatch: dispatch_independent_agent_worker_without_service,
            },
            Duration::ZERO,
            Some(1),
            new_agent_shutdown_signal(),
        )
        .unwrap();

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let workers = store.list_active_workers_blocking().unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].project_id, project_id);
        assert_eq!(workers[0].state, AGENT_WORKER_STATE_DISPATCHING);
        assert_eq!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .unwrap()
                .holder,
            workers[0].lease_holder
        );
        assert_eq!(store.run_count_blocking().unwrap(), 0);

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let mut start = run_agent_daemon_scheduler_pass(&state_dir).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        add_task(&project_root, "scheduled task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let checkin = AgentDaemonCheckinSource {
            holder: "clt-scheduler-test".to_string(),
            mode: "cli".to_string(),
            started_at: "100".to_string(),
        };
        let start = run_agent_daemon_scheduler_pass_with_active_and_checkin(
            &state_dir,
            Vec::new(),
            Some(&checkin),
        )
        .unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let checkins = store.list_daemon_checkins_blocking().unwrap();

        assert_eq!(start.pass.scanned_projects, 1);
        assert_eq!(start.jobs.len(), 1);
        assert_eq!(start.jobs[0].holder, checkin.holder);
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
    fn agent_run_once_skips_disabled_projects_but_reconciles_abandoned_handoffs() {
        let root = temp_root("agent-run-disabled");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "disabled task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .set_session_control_state_blocking(
                project.id,
                "session-123",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        assert!(
            store
                .begin_stopped_session_interactive_blocking(
                    project.id,
                    "session-123",
                    "clt-interactive-4294967295",
                    None,
                )
                .unwrap()
        );
        store
            .set_project_enabled_blocking(project.id, false)
            .unwrap();
        let runner = FakeAgentRunner::new(&state_dir, "success");
        drop(store);

        let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        assert_eq!(
            store
                .session_control_blocking(project.id, "session-123")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::ResumeRequested
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn agent_run_once_reclaims_dead_lease_for_disabled_project() {
        let root = temp_root("agent-run-disabled-dead-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "disabled task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(
                    project.id,
                    "clt-agent-4294967295",
                    "100",
                    "9999999999",
                )
                .unwrap()
        );
        store
            .set_project_enabled_blocking(project.id, false)
            .unwrap();
        let runner = FakeAgentRunner::new(&state_dir, "success");
        drop(store);

        let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(pass.scanned_projects, 0);
        assert_eq!(pass.runs_started, 0);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        assert_eq!(runner.ran_project_count(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn agent_run_once_reclaims_dead_orphaned_interactive_lease_before_expiry() {
        let root = temp_root("agent-run-disabled-dead-interactive-lease");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "disabled task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let holder = format!("clt-idle-interactive-worker-{}-1-1", u32::MAX);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, &holder, "100", "9999999999",)
                .unwrap()
        );
        store
            .set_project_enabled_blocking(project.id, false)
            .unwrap();
        let runner = FakeAgentRunner::new(&state_dir, "success");
        drop(store);

        let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

        assert_eq!(pass.scanned_projects, 0);
        assert_eq!(pass.runs_started, 0);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let runner = FakeAgentRunner::new(&state_dir, "success");
        drop(store);

        let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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
        move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
                .contains("left 3 blocked task(s) unresolved across todo and doing")
        );

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
    fn agent_scheduler_rechecks_blocked_work_before_todo_then_uses_backoff() {
        let root = temp_root("agent-blocked-before-todo");
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

        assert_eq!(start.jobs.len(), 1);
        assert_eq!(
            start.jobs[0].task_selection,
            AgentTaskSelection::RecoverBlocked
        );
        assert_eq!(start.jobs[0].blocked_task_count_before, 2);

        let runner = FakeAgentRunner::new(&state_dir, "success");
        let shutdown = new_agent_shutdown_signal();
        let completion = run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();
        assert_eq!(completion.status, "blocked");
        assert!(
            completion
                .summary
                .contains("left 2 blocked task(s) unresolved")
        );

        let during_backoff = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(during_backoff.jobs.len(), 1);
        assert_eq!(
            during_backoff.jobs[0].task_selection,
            AgentTaskSelection::NextTodo
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_scheduler_recovers_blocked_work_without_taking_unblocked_doing() {
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

        assert_eq!(start.jobs.len(), 1);
        assert_eq!(start.pass.pending_projects, 1);
        assert_eq!(start.pass.runs_started, 1);
        assert_eq!(
            start.jobs[0].task_selection,
            AgentTaskSelection::RecoverBlocked
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_agent_run_records_its_codex_session_for_the_done_task() {
        let root = temp_root("agent-task-codex-session");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        add_task(&project_root, "resumable task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(start.jobs.len(), 1);
        move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
        move_task(&project_root, TaskStatus::Doing, TaskStatus::Done, "1").unwrap();

        let mut runner = FakeAgentRunner::new(&state_dir, "success");
        runner.result.codex_session_id = Some("session-for-task".to_string());
        let shutdown = new_agent_shutdown_signal();
        run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();

        let done_task = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Done)
            .unwrap()
            .remove(0);
        assert_eq!(done_task.content, "resumable task codex:session-for-task");
        assert_eq!(task_display_text(&done_task), "resumable task");
        assert_eq!(
            codex_session_id_from_task_content(&done_task.content),
            Some("session-for-task")
        );
        assert_eq!(
            codex_session_for_task(&done_task).as_deref(),
            Some("session-for-task")
        );

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store.latest_codex_session_id_blocking().unwrap().as_deref(),
            Some("session-for-task")
        );

        let mut markerless_task = done_task;
        markerless_task.content = "resumable task".to_string();
        assert_eq!(codex_session_for_task(&markerless_task), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn completed_agent_run_reports_a_session_marker_write_failure() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("agent-task-codex-session-write-failure");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        add_task(&project_root, "resumable task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
        move_task(&project_root, TaskStatus::Doing, TaskStatus::Done, "1").unwrap();
        let done_path = project_root.join("tasks/done.md");
        let mut permissions = fs::metadata(&done_path).unwrap().permissions();
        permissions.set_mode(0o444);
        fs::set_permissions(&done_path, permissions).unwrap();

        let mut runner = FakeAgentRunner::new(&state_dir, "success");
        runner.result.codex_session_id = Some("session-for-task".to_string());
        let shutdown = new_agent_shutdown_signal();
        let completion = run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();

        let mut permissions = fs::metadata(&done_path).unwrap().permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&done_path, permissions).unwrap();
        assert_eq!(completion.status, "failure");
        assert!(
            completion
                .summary
                .contains("Failed to persist the Codex session marker on the task")
        );
        assert_eq!(
            fs::read_to_string(&done_path).unwrap(),
            "# Done Tasks\n- resumable task\n"
        );

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store.latest_codex_session_id_blocking().unwrap().as_deref(),
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);

        let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(start.jobs.len(), 1);
        move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
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

        let blocked_task = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing)
            .unwrap()
            .remove(0);
        assert_eq!(
            blocked_task.content,
            format!("{blocked_content} codex:session-for-blocked-task")
        );
        assert_eq!(task_display_text(&blocked_task), blocked_content);
        assert_eq!(
            codex_session_id_from_task_content(&blocked_task.content),
            Some("session-for-blocked-task")
        );

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            store.latest_codex_session_id_blocking().unwrap().as_deref(),
            Some("session-for-blocked-task")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn foreground_git_run_uses_one_durable_inline_generation() {
        let root = temp_root("foreground-inline-generation");
        let (state_dir, job) = prepare_inline_git_job(&root);
        let runner = InspectInlineOwnershipRunner {
            state_dir: state_dir.clone(),
            break_lease_after_observation: false,
            observed_run_token: Mutex::new(None),
        };

        let completion = run_agent_job(job, &runner, &new_agent_shutdown_signal()).unwrap();
        assert_eq!(completion.status, "idle");
        let worker_token = runner.observed_run_token.lock().unwrap().clone().unwrap();
        assert!(!inline_agent_worker_generation_is_registered(&worker_token));
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert!(store.list_active_workers_blocking().unwrap().is_empty());
        let worker = store
            .list_terminal_workers_blocking()
            .unwrap()
            .into_iter()
            .find(|worker| worker.worker_token == worker_token)
            .unwrap();
        assert_eq!(worker.state, "completed");
        assert_eq!(worker.run_id, Some(completion.run_id));
        assert!(
            store
                .lease_for_project_blocking(worker.project_id)
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn foreground_git_run_error_abandons_its_exact_inline_generation() {
        let root = temp_root("foreground-inline-error");
        let (state_dir, job) = prepare_inline_git_job(&root);
        let runner = InspectInlineOwnershipRunner {
            state_dir: state_dir.clone(),
            break_lease_after_observation: true,
            observed_run_token: Mutex::new(None),
        };

        let error = run_agent_job(job, &runner, &new_agent_shutdown_signal())
            .err()
            .expect("broken inline lease must fail the run");
        assert!(
            error
                .to_string()
                .contains("lost its durable ownership fence")
        );
        let worker_token = runner.observed_run_token.lock().unwrap().clone().unwrap();
        assert!(!inline_agent_worker_generation_is_registered(&worker_token));
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert!(store.list_active_workers_blocking().unwrap().is_empty());
        let worker = store
            .list_terminal_workers_blocking()
            .unwrap()
            .into_iter()
            .find(|worker| worker.worker_token == worker_token)
            .unwrap();
        assert_eq!(worker.state, "abandoned");
        assert!(
            worker
                .error
                .unwrap()
                .contains("lost its durable ownership fence")
        );
        assert_eq!(store.run_count_blocking().unwrap(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn independent_worker_dispatch_survives_scheduler_state_and_consumes_capacity() {
        let root = temp_root("independent-worker-dispatch");
        let state_dir = root.join("state/clt");
        let first_root = root.join("a-project");
        let second_root = root.join("b-project");
        add_task(&first_root, "first task", None).unwrap();
        add_task(&second_root, "second task", None).unwrap();
        let first_root = fs::canonicalize(first_root).unwrap();
        let second_root = fs::canonicalize(second_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&first_root, "a-project")
            .unwrap();
        store
            .register_project_blocking(&second_root, "b-project")
            .unwrap();
        drop(store);

        let mut first =
            run_agent_scheduler_pass_with_max_global_jobs(&state_dir, false, &[], 1, None).unwrap();
        assert_eq!(first.jobs.len(), 1);
        assert_eq!(first.pass.deferred_projects, 1);
        let launched = Cell::new(false);
        let mut observed = None;
        dispatch_independent_agent_worker_with(
            &state_dir,
            Path::new("/tmp/pinned-clt-generation"),
            first.jobs.pop().unwrap(),
            AgentServiceEnvironment {
                codex_path_override: None,
                path: OsString::from("/usr/bin:/bin"),
            },
            |spec| {
                observed = Some(spec.clone());
                Ok(())
            },
            |_| {
                launched.set(true);
                Ok(())
            },
        )
        .unwrap();
        assert!(launched.get());
        let observed = observed.unwrap();

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let workers = store.list_active_workers_blocking().unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].worker_token, observed.worker_token);
        assert_eq!(workers[0].state, AGENT_WORKER_STATE_DISPATCHING);
        assert_eq!(
            workers[0].binary_path,
            PathBuf::from("/tmp/pinned-clt-generation")
        );
        assert_eq!(
            store
                .lease_for_project_blocking(workers[0].project_id)
                .unwrap()
                .unwrap()
                .holder,
            agent_worker_lease_holder(&observed.worker_token)
        );
        drop(store);

        let restarted =
            run_agent_scheduler_pass_with_max_global_jobs(&state_dir, false, &[], 1, None).unwrap();
        assert_eq!(restarted.pass.active_agent_jobs, 1);
        assert_eq!(restarted.pass.runs_started, 0);
        assert_eq!(restarted.pass.deferred_projects, 1);
        assert!(restarted.jobs.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_independent_worker_launch_releases_only_its_fenced_lease() {
        let root = temp_root("independent-worker-launch-failure");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        add_task(&project_root, "task", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        drop(store);
        let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

        let error = dispatch_independent_agent_worker_with(
            &state_dir,
            Path::new("/tmp/pinned-clt-generation"),
            start.jobs.pop().unwrap(),
            AgentServiceEnvironment {
                codex_path_override: None,
                path: OsString::from("/usr/bin:/bin"),
            },
            |_| Ok(()),
            |_| anyhow::bail!("synthetic launch failure"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("independent agent worker"));

        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert!(store.list_active_workers_blocking().unwrap().is_empty());
        let terminal = store.list_terminal_workers_blocking().unwrap();
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].state, "abandoned");
        assert!(
            store
                .lease_for_project_blocking(terminal[0].project_id)
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_resumes_doing_task_after_independent_worker_dies() {
        let root = temp_root("independent-worker-crash-recovery");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        add_task(&project_root, "interrupted task", None).unwrap();
        move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "9999999999")
                .unwrap()
        );
        assert!(
            store
                .reserve_worker_blocking(agent::AgentWorkerReservation {
                    project_id: project.id,
                    worker_token: "crashed-worker",
                    expected_lease_holder: "scheduler",
                    max_active_workers: 12,
                    protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                    command_arguments: "[]",
                    path_env: OsStr::new("/usr/bin:/bin"),
                    codex_path: None,
                    service_label: "clt-worker-crashed-worker",
                    binary_path: Path::new("/tmp/old-clt-generation"),
                    task_selection: "next_todo",
                    resume_session_id: None,
                    created_at: "101",
                })
                .unwrap()
        );
        assert!(
            store
                .claim_worker_blocking("crashed-worker", u32::MAX, "102")
                .unwrap()
        );
        drop(store);

        let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
        assert_eq!(start.jobs.len(), 1);
        assert_eq!(
            start.jobs[0].task_selection,
            AgentTaskSelection::ResumeDoing
        );
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert!(store.list_active_workers_blocking().unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_worker_reservation_claim_and_heartbeat_are_fenced() {
        let root = temp_root("agent-worker-lifecycle");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "200")
                .unwrap()
        );

        assert!(
            store
                .reserve_worker_blocking(agent::AgentWorkerReservation {
                    project_id: project.id,
                    worker_token: "token-one",
                    expected_lease_holder: "scheduler",
                    max_active_workers: 12,
                    protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                    command_arguments: "[]",
                    path_env: OsStr::new("/usr/bin:/bin"),
                    codex_path: None,
                    service_label: "clt-worker-token-one",
                    binary_path: Path::new("/tmp/clt-generation-one"),
                    task_selection: "next_todo",
                    resume_session_id: None,
                    created_at: "101",
                })
                .unwrap()
        );
        let lease = store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap();
        assert_eq!(lease.holder, agent::worker_lease_holder("token-one"));
        let workers = store.list_active_workers_blocking().unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].state, "dispatching");
        assert_eq!(workers[0].heartbeat_at.as_deref(), Some("101"));
        assert_eq!(workers[0].worker_pid, None);
        assert!(
            !store
                .try_acquire_lease_blocking(project.id, "new-scheduler", "201", "400")
                .unwrap()
        );
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .holder,
            agent::worker_lease_holder("token-one")
        );

        assert!(
            store
                .claim_worker_blocking("token-one", 123, "102")
                .unwrap()
        );
        assert!(
            !store
                .claim_worker_blocking("token-one", 456, "102")
                .unwrap()
        );
        assert!(
            store
                .claim_worker_blocking("token-one", 123, "102")
                .unwrap()
        );
        assert!(
            !store
                .renew_worker_blocking("token-one", 456, "103", "300")
                .unwrap()
        );
        let unchanged = store.list_active_workers_blocking().unwrap().remove(0);
        assert_eq!(unchanged.heartbeat_at.as_deref(), Some("102"));
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .expires_at,
            "200"
        );
        assert!(
            store
                .renew_worker_blocking("token-one", 123, "104", "300")
                .unwrap()
        );
        let renewed = store.list_active_workers_blocking().unwrap().remove(0);
        assert_eq!(renewed.state, "running");
        assert_eq!(renewed.worker_pid, Some(123));
        assert_eq!(renewed.started_at.as_deref(), Some("102"));
        assert_eq!(renewed.heartbeat_at.as_deref(), Some("104"));
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .expires_at,
            "300"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inline_worker_reservation_and_claim_commit_as_one_lease_transfer() {
        let root = temp_root("inline-worker-atomic-claim");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "200")
                .unwrap()
        );
        let reservation = |expected_lease_holder| agent::AgentWorkerReservation {
            project_id: project.id,
            worker_token: "inline-token",
            expected_lease_holder,
            max_active_workers: 12,
            protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
            service_label: "clt-inline-worker-inline-token",
            binary_path: Path::new("/tmp/clt-inline-generation"),
            command_arguments: "[]",
            path_env: OsStr::new("/usr/bin:/bin"),
            codex_path: None,
            task_selection: "next_todo",
            resume_session_id: None,
            created_at: "101",
        };

        assert!(
            !store
                .reserve_and_claim_worker_blocking(
                    reservation("wrong-holder"),
                    std::process::id(),
                    "102",
                )
                .unwrap()
        );
        assert!(store.list_active_workers_blocking().unwrap().is_empty());
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .holder,
            "scheduler"
        );

        assert!(
            store
                .reserve_and_claim_worker_blocking(
                    reservation("scheduler"),
                    std::process::id(),
                    "102",
                )
                .unwrap()
        );
        let worker = store.list_active_workers_blocking().unwrap().remove(0);
        assert_eq!(worker.state, AGENT_WORKER_STATE_RUNNING);
        assert_eq!(worker.worker_pid, Some(std::process::id()));
        assert_eq!(worker.started_at.as_deref(), Some("102"));
        assert_eq!(worker.heartbeat_at.as_deref(), Some("102"));
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .holder,
            agent_worker_lease_holder("inline-token")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_worker_reservation_rolls_back_failed_lease_transfer() {
        let root = temp_root("agent-worker-reservation-rollback");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "200")
                .unwrap()
        );

        let reserve = |expected_lease_holder| agent::AgentWorkerReservation {
            project_id: project.id,
            worker_token: "token-one",
            expected_lease_holder,
            max_active_workers: 12,
            protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
            command_arguments: "[]",
            path_env: OsStr::new("/usr/bin:/bin"),
            codex_path: None,
            service_label: "clt-worker-token-one",
            binary_path: Path::new("/tmp/clt-generation-one"),
            task_selection: "next_todo",
            resume_session_id: None,
            created_at: "101",
        };
        assert!(
            !store
                .reserve_worker_blocking(reserve("wrong-holder"))
                .unwrap()
        );
        assert!(store.list_active_workers_blocking().unwrap().is_empty());
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .holder,
            "scheduler"
        );
        assert!(store.reserve_worker_blocking(reserve("scheduler")).unwrap());

        assert!(
            !store
                .reserve_worker_blocking(agent::AgentWorkerReservation {
                    project_id: project.id,
                    worker_token: "token-two",
                    expected_lease_holder: &agent::worker_lease_holder("token-one"),
                    max_active_workers: 12,
                    protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                    command_arguments: "[]",
                    path_env: OsStr::new("/usr/bin:/bin"),
                    codex_path: None,
                    service_label: "clt-worker-token-two",
                    binary_path: Path::new("/tmp/clt-generation-two"),
                    task_selection: "next_todo",
                    resume_session_id: None,
                    created_at: "102",
                })
                .unwrap()
        );
        assert_eq!(store.list_active_workers_blocking().unwrap().len(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_worker_abandonment_is_observation_and_lease_fenced() {
        let root = temp_root("agent-worker-abandonment");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "200")
                .unwrap()
        );
        assert!(
            store
                .reserve_worker_blocking(agent::AgentWorkerReservation {
                    project_id: project.id,
                    worker_token: "token-one",
                    expected_lease_holder: "scheduler",
                    max_active_workers: 12,
                    protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                    command_arguments: "[]",
                    path_env: OsStr::new("/usr/bin:/bin"),
                    codex_path: None,
                    service_label: "clt-worker-token-one",
                    binary_path: Path::new("/tmp/clt-generation-one"),
                    task_selection: "next_todo",
                    resume_session_id: None,
                    created_at: "101",
                })
                .unwrap()
        );
        assert!(
            store
                .claim_worker_blocking("token-one", 123, "102")
                .unwrap()
        );
        assert!(
            !store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: "token-one",
                    expected_state: "running",
                    expected_worker_pid: Some(123),
                    expected_heartbeat_at: Some("stale-observation"),
                    finished_at: "103",
                    error: "worker disappeared",
                    permitted_successor_holder: None,
                })
                .unwrap()
        );
        assert_eq!(store.list_active_workers_blocking().unwrap().len(), 1);
        assert!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .is_some()
        );

        let worker_holder = agent::worker_lease_holder("token-one");
        assert!(
            store
                .release_lease_blocking(project.id, &worker_holder)
                .unwrap()
        );
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "successor-holder", "103", "300")
                .unwrap()
        );
        assert!(
            !store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: "token-one",
                    expected_state: "running",
                    expected_worker_pid: Some(123),
                    expected_heartbeat_at: Some("102"),
                    finished_at: "104",
                    error: "worker disappeared",
                    permitted_successor_holder: Some("different-successor"),
                })
                .unwrap()
        );
        assert_eq!(store.list_active_workers_blocking().unwrap().len(), 1);
        assert!(
            store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: "token-one",
                    expected_state: "running",
                    expected_worker_pid: Some(123),
                    expected_heartbeat_at: Some("102"),
                    finished_at: "104",
                    error: "worker disappeared",
                    permitted_successor_holder: Some("successor-holder"),
                })
                .unwrap()
        );
        assert!(store.list_active_workers_blocking().unwrap().is_empty());
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .holder,
            "successor-holder"
        );
        let terminal = store.list_terminal_workers_blocking().unwrap();
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].state, "abandoned");
        assert_eq!(terminal[0].finished_at.as_deref(), Some("104"));
        assert_eq!(terminal[0].error.as_deref(), Some("worker disappeared"));
        assert!(terminal[0].run_id.is_some());
        assert_eq!(store.run_count_blocking().unwrap(), 1);
        assert_eq!(
            store
                .list_projects_blocking()
                .unwrap()
                .remove(0)
                .failure_count,
            1
        );
        assert!(
            !store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: "token-one",
                    expected_state: "running",
                    expected_worker_pid: Some(123),
                    expected_heartbeat_at: Some("102"),
                    finished_at: "106",
                    error: "duplicate observation",
                    permitted_successor_holder: None,
                })
                .unwrap()
        );
        assert_eq!(store.run_count_blocking().unwrap(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_store_worker_finalization_is_idempotent_and_transactional() {
        let root = temp_root("agent-worker-finalization");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "200")
                .unwrap()
        );
        assert!(
            store
                .reserve_worker_blocking(agent::AgentWorkerReservation {
                    project_id: project.id,
                    worker_token: "token-one",
                    expected_lease_holder: "scheduler",
                    max_active_workers: 12,
                    protocol_version: AGENT_WORKER_PROTOCOL_VERSION,
                    command_arguments: "[]",
                    path_env: OsStr::new("/usr/bin:/bin"),
                    codex_path: None,
                    service_label: "clt-worker-token-one",
                    binary_path: Path::new("/tmp/clt-generation-one"),
                    task_selection: "next_todo",
                    resume_session_id: None,
                    created_at: "101",
                })
                .unwrap()
        );
        assert!(
            store
                .claim_worker_blocking("token-one", 123, "102")
                .unwrap()
        );
        let worker_holder = agent::worker_lease_holder("token-one");
        let finalize = |expected_worker_pid| agent::AgentWorkerFinalization {
            worker_token: "token-one",
            expected_worker_pid,
            expected_lease_holder: &worker_holder,
            status: "failure",
            finished_at: "110",
            exit_code: Some(1),
            log_dir: Some("/tmp/logs"),
            stdout_path: Some("/tmp/logs/run.out"),
            stderr_path: Some("/tmp/logs/run.err"),
            summary: Some("failed once"),
            codex_session_id: Some("session-one"),
            error: Some("worker reported failure"),
        };
        assert_eq!(
            store.finalize_worker_blocking(finalize(Some(456))).unwrap(),
            None
        );
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        assert!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .is_some()
        );

        let run_id = store
            .finalize_worker_blocking(finalize(Some(123)))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.finalize_worker_blocking(finalize(Some(123))).unwrap(),
            Some(run_id)
        );
        assert_eq!(store.run_count_blocking().unwrap(), 1);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
        assert!(store.list_active_workers_blocking().unwrap().is_empty());
        let terminal = store.list_terminal_workers_blocking().unwrap();
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].state, "completed");
        assert_eq!(terminal[0].run_id, Some(run_id));
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.failure_count, 1);
        assert_eq!(project.last_failure_at.as_deref(), Some("110"));
        let run = store
            .latest_run_for_project_blocking(project.id)
            .unwrap()
            .unwrap();
        assert_eq!(run.id, run_id);
        assert_eq!(run.status, "failure");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dispatching_worker_requires_a_verified_drain_after_its_startup_deadline() {
        let root = temp_root("agent-worker-dispatch-timeout");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "99", "999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            "dispatch-token",
            "scheduler",
            "100",
            12,
        ));

        let launch_count = Cell::new(0);
        let workers = reconcile_independent_agent_workers_with(
            &state_dir,
            &store,
            159,
            |spec| {
                launch_count.set(launch_count.get() + 1);
                assert_eq!(
                    spec.command_arguments.as_ref().unwrap(),
                    &vec![
                        OsString::from("--local"),
                        OsString::from("agent"),
                        OsString::from("worker"),
                        OsString::from("--worker-token"),
                        OsString::from("dispatch-token"),
                    ]
                );
                assert_eq!(spec.service_env.path, OsString::from("/usr/bin:/bin"));
                Ok(())
            },
            |_| panic!("a fresh dispatch must not be drained"),
        )
        .unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(launch_count.get(), 1);

        let workers = reconcile_independent_agent_workers_with(
            &state_dir,
            &store,
            160,
            |_| panic!("a timed-out dispatch must not be relaunched before draining"),
            |_| Ok(false),
        )
        .unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        assert!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .is_some()
        );

        let workers = reconcile_independent_agent_workers_with(
            &state_dir,
            &store,
            161,
            |_| panic!("a timed-out dispatch must not be relaunched"),
            |_| Ok(true),
        )
        .unwrap();
        assert!(workers.is_empty());
        assert!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.run_count_blocking().unwrap(), 1);
        let terminal = store.list_terminal_workers_blocking().unwrap();
        assert_eq!(terminal[0].state, "abandoned");
        assert!(terminal[0].run_id.is_some());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn newer_worker_protocol_is_opaque_to_an_older_scheduler() {
        let root = temp_root("agent-worker-newer-protocol");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "1", "999")
                .unwrap()
        );
        assert!(
            store
                .reserve_worker_blocking(agent::AgentWorkerReservation {
                    project_id: project.id,
                    worker_token: "future-token",
                    expected_lease_holder: "scheduler",
                    max_active_workers: 12,
                    protocol_version: AGENT_WORKER_PROTOCOL_VERSION + 1,
                    service_label: "future-service",
                    binary_path: Path::new("/tmp/future-clt"),
                    command_arguments: "[]",
                    path_env: OsStr::new("/usr/bin:/bin"),
                    codex_path: None,
                    task_selection: "next_todo",
                    resume_session_id: None,
                    created_at: "2",
                })
                .unwrap()
        );

        let workers = reconcile_independent_agent_workers_with(
            &state_dir,
            &store,
            10_000,
            |_| panic!("an older scheduler must not launch a newer worker protocol"),
            |_| panic!("an older scheduler must not drain a newer worker protocol"),
        )
        .unwrap();

        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].worker_token, "future-token");
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .holder,
            agent::worker_lease_holder("future-token")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dead_outer_worker_keeps_its_fence_until_the_service_is_drained() {
        let root = temp_root("agent-worker-drain-fence");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            "dead-token",
            "scheduler",
            "101",
            12,
        ));
        assert!(
            store
                .claim_worker_blocking("dead-token", u32::MAX, "102")
                .unwrap()
        );

        let still_fenced = reconcile_independent_agent_workers_with(
            &state_dir,
            &store,
            103,
            |_| Ok(()),
            |_| Ok(false),
        )
        .unwrap();
        assert_eq!(still_fenced.len(), 1);
        assert!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .is_some()
        );
        assert_eq!(store.run_count_blocking().unwrap(), 0);

        let recovered = reconcile_independent_agent_workers_with(
            &state_dir,
            &store,
            104,
            |_| Ok(()),
            |_| Ok(true),
        )
        .unwrap();
        assert!(recovered.is_empty());
        assert!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.run_count_blocking().unwrap(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_heartbeat_is_recovered_even_when_the_pid_was_reused() {
        let root = temp_root("agent-worker-stale-heartbeat");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "99", "999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            "stale-token",
            "scheduler",
            "100",
            12,
        ));
        assert!(
            store
                .claim_worker_blocking("stale-token", std::process::id(), "100")
                .unwrap()
        );

        let drain_count = Cell::new(0);
        let workers = reconcile_independent_agent_workers_with(
            &state_dir,
            &store,
            160,
            |_| Ok(()),
            |_| {
                drain_count.set(drain_count.get() + 1);
                Ok(true)
            },
        )
        .unwrap();
        assert!(workers.is_empty());
        assert_eq!(drain_count.get(), 1);
        assert_eq!(store.run_count_blocking().unwrap(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inline_worker_liveness_uses_the_registered_generation_not_a_reused_pid() {
        let root = temp_root("inline-worker-generation-liveness");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "99", "999")
                .unwrap()
        );
        let generation = InlineAgentWorkerGeneration::register("inline-live-token");
        assert!(reserve_test_inline_worker(
            &store,
            project.id,
            "inline-live-token",
            "scheduler",
            std::process::id(),
            "100",
        ));

        let drain_count = Cell::new(0);
        let workers = reconcile_independent_agent_workers_with(
            &state_dir,
            &store,
            160,
            |_| panic!("a running inline generation must not be dispatched"),
            |_| {
                drain_count.set(drain_count.get() + 1);
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(drain_count.get(), 0);

        drop(generation);
        let workers = reconcile_independent_agent_workers_with(
            &state_dir,
            &store,
            161,
            |_| panic!("an inline generation must not be dispatched"),
            |_| {
                drain_count.set(drain_count.get() + 1);
                Ok(true)
            },
        )
        .unwrap();
        assert!(workers.is_empty());
        assert_eq!(drain_count.get(), 1);
        assert_eq!(store.run_count_blocking().unwrap(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inline_worker_recovery_waits_for_exact_launch_reap_fence() {
        let root = temp_root("inline-worker-launch-reap-fence");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "99", "999")
                .unwrap()
        );
        assert!(reserve_test_inline_worker(
            &store,
            project.id,
            "inline-reap-token",
            "scheduler",
            u32::MAX,
            "100",
        ));
        let git_start = AgentGitStartState {
            starting_head: "abc123".to_string(),
            branch_ref: Some("refs/heads/main".to_string()),
            upstream_ref: None,
            worktree_baseline: "[]".to_string(),
        };
        assert!(
            store
                .record_git_launch_state_blocking(
                    project.id,
                    "inline-reap-token",
                    AgentGitMode::Commit,
                    &git_start,
                    "101",
                )
                .unwrap()
        );

        let drain_count = Cell::new(0);
        let workers = reconcile_independent_agent_workers_with(
            &state_dir,
            &store,
            160,
            |_| panic!("an inline worker must not be dispatched"),
            |_| {
                drain_count.set(drain_count.get() + 1);
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(drain_count.get(), 0);
        assert_eq!(store.run_count_blocking().unwrap(), 0);

        assert!(
            store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: "inline-reap-token",
                    expected_state: AGENT_WORKER_STATE_RUNNING,
                    expected_worker_pid: Some(u32::MAX),
                    expected_heartbeat_at: Some("100"),
                    finished_at: "161",
                    error: "simulated supervisor finalization after reap",
                    permitted_successor_holder: None,
                })
                .unwrap()
        );
        assert!(
            store
                .reclaim_unchanged_git_launch_state_blocking(
                    project.id,
                    "inline-reap-token",
                    AgentGitMode::Commit,
                    &git_start,
                )
                .unwrap()
        );
        assert!(
            store
                .git_launch_state_blocking(project.id, "inline-reap-token")
                .unwrap()
                .is_none()
        );
        assert_eq!(drain_count.get(), 0);
        assert_eq!(store.run_count_blocking().unwrap(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_finalization_requires_the_exact_current_lease_holder() {
        let root = temp_root("agent-worker-finalization-fence");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            "fenced-token",
            "scheduler",
            "101",
            12,
        ));
        assert!(
            store
                .claim_worker_blocking("fenced-token", 123, "102")
                .unwrap()
        );
        store
            .mark_session_running_blocking(
                project.id,
                "session-fenced",
                456,
                "fenced-token",
                &root.join("session.out"),
                &root.join("session.err"),
            )
            .unwrap();
        let worker_holder = agent::worker_lease_holder("fenced-token");
        assert!(
            store
                .release_lease_blocking(project.id, &worker_holder)
                .unwrap()
        );
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "unrelated-successor", "103", "999")
                .unwrap()
        );
        assert!(
            store
                .mark_session_running_blocking(
                    project.id,
                    "session-fenced",
                    789,
                    "fenced-token",
                    &root.join("new-session.out"),
                    &root.join("new-session.err"),
                )
                .is_err()
        );
        assert!(
            !store
                .clear_running_session_control_blocking(
                    project.id,
                    "session-fenced",
                    Some("fenced-token"),
                )
                .unwrap()
        );
        assert_eq!(
            store
                .session_control_blocking(project.id, "session-fenced")
                .unwrap()
                .unwrap()
                .child_pid,
            Some(456)
        );

        assert_eq!(
            store
                .finalize_worker_blocking(agent::AgentWorkerFinalization {
                    worker_token: "fenced-token",
                    expected_worker_pid: Some(123),
                    expected_lease_holder: &worker_holder,
                    status: "success",
                    finished_at: "104",
                    exit_code: Some(0),
                    log_dir: None,
                    stdout_path: None,
                    stderr_path: None,
                    summary: Some("must not commit"),
                    codex_session_id: None,
                    error: None,
                })
                .unwrap(),
            None
        );
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        assert_eq!(store.list_active_workers_blocking().unwrap().len(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn abandoned_worker_preserves_a_generation_verified_interactive_successor() {
        let root = temp_root("agent-worker-interactive-successor");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "9999999999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            "handoff-token",
            "scheduler",
            "101",
            12,
        ));
        assert!(
            store
                .claim_worker_blocking("handoff-token", 123, "102")
                .unwrap()
        );
        store
            .mark_session_running_blocking(
                project.id,
                "session-handoff",
                456,
                "handoff-token",
                &root.join("session.out"),
                &root.join("session.err"),
            )
            .unwrap();
        let interactive_holder = "clt-interactive-999";
        assert!(
            store
                .request_session_interrupt_blocking(
                    project.id,
                    "session-handoff",
                    456,
                    "handoff-token",
                    interactive_holder,
                )
                .unwrap()
        );
        let worker_holder = agent::worker_lease_holder("handoff-token");
        assert_eq!(
            store
                .complete_session_interrupt_handoff_blocking(
                    project.id,
                    "session-handoff",
                    "handoff-token",
                    &worker_holder,
                    60,
                )
                .unwrap()
                .as_deref(),
            Some(interactive_holder)
        );
        assert!(
            store
                .transition_session_control_state_blocking(
                    project.id,
                    "session-handoff",
                    AgentSessionControlState::ReadyInteractive,
                    AgentSessionControlState::Interactive,
                )
                .unwrap()
        );

        assert!(
            store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: "handoff-token",
                    expected_state: "running",
                    expected_worker_pid: Some(123),
                    expected_heartbeat_at: Some("102"),
                    finished_at: "104",
                    error: "outer worker exited after handoff",
                    permitted_successor_holder: Some(interactive_holder),
                })
                .unwrap()
        );
        assert!(store.list_active_workers_blocking().unwrap().is_empty());
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .holder,
            interactive_holder
        );
        assert_eq!(store.run_count_blocking().unwrap(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_reservation_serializes_the_global_capacity_limit() {
        let root = temp_root("agent-worker-global-capacity");
        let state_dir = root.join("state/clt");
        let first_root = root.join("first");
        let second_root = root.join("second");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        let first_root = fs::canonicalize(first_root).unwrap();
        let second_root = fs::canonicalize(second_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&first_root, "first")
            .unwrap();
        store
            .register_project_blocking(&second_root, "second")
            .unwrap();
        let projects = store.list_projects_blocking().unwrap();
        let first_id = projects
            .iter()
            .find(|project| project.name == "first")
            .unwrap()
            .id;
        let second_id = projects
            .iter()
            .find(|project| project.name == "second")
            .unwrap()
            .id;
        assert!(
            store
                .try_acquire_lease_blocking(first_id, "scheduler-one", "100", "999")
                .unwrap()
        );
        assert!(
            store
                .try_acquire_lease_blocking(second_id, "scheduler-two", "100", "999")
                .unwrap()
        );
        drop(store);

        let barrier = Arc::new(Barrier::new(3));
        let handles = [
            (first_id, "capacity-one", "scheduler-one"),
            (second_id, "capacity-two", "scheduler-two"),
        ]
        .into_iter()
        .map(|(project_id, token, holder)| {
            let state_dir = state_dir.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
                barrier.wait();
                reserve_test_worker(&store, project_id, token, holder, "101", 1)
            })
        })
        .collect::<Vec<_>>();
        barrier.wait();
        let reserved = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|reserved| *reserved)
            .count();
        assert_eq!(reserved, 1);
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(store.list_active_workers_blocking().unwrap().len(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn successful_worker_reservation_atomically_supersedes_old_abandonment() {
        let root = temp_root("agent-worker-supersede-atomic");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler-old", "100", "999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            "old-token",
            "scheduler-old",
            "101",
            12,
        ));
        assert!(
            store
                .claim_worker_blocking("old-token", 123, "102")
                .unwrap()
        );
        assert!(
            store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: "old-token",
                    expected_state: "running",
                    expected_worker_pid: Some(123),
                    expected_heartbeat_at: Some("102"),
                    finished_at: "103",
                    error: "old worker exited",
                    permitted_successor_holder: None,
                })
                .unwrap()
        );
        assert_eq!(
            store.list_terminal_workers_blocking().unwrap()[0].state,
            "abandoned"
        );
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler-new", "104", "999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            "new-token",
            "scheduler-new",
            "105",
            12,
        ));
        let terminal = store.list_terminal_workers_blocking().unwrap();
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].worker_token, "old-token");
        assert_eq!(terminal[0].state, "superseded");
        assert_eq!(
            store.list_active_workers_blocking().unwrap()[0].worker_token,
            "new-token"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incompatible_schema_migration_is_deferred_for_pinned_workers() {
        let future_migration_version = 18;
        let root = temp_root("agent-worker-migration-barrier");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            "migration-token",
            "scheduler",
            "101",
            12,
        ));
        assert!(
            !store
                .worker_schema_migration_deferred_blocking(AGENT_WORKER_SHARED_SCHEMA_VERSION)
                .unwrap()
        );
        assert!(
            store
                .worker_schema_migration_deferred_blocking(future_migration_version)
                .unwrap()
        );
        store
            .mark_session_running_blocking(
                project.id,
                "migration-session",
                123,
                "migration-token",
                &root.join("migration.out"),
                &root.join("migration.err"),
            )
            .unwrap();
        let compatibility_store = agent::TursoAgentStore::open_blocking_with_test_migration(
            &state_dir,
            future_migration_version,
            "CREATE TABLE deferred_worker_migration_probe (id INTEGER PRIMARY KEY)",
        )
        .unwrap();
        assert_eq!(
            compatibility_store.pending_migration_version(),
            Some(future_migration_version)
        );
        assert!(
            !compatibility_store
                .table_exists_blocking("deferred_worker_migration_probe")
                .unwrap()
        );
        assert!(
            compatibility_store
                .request_session_stop_blocking(
                    project.id,
                    "migration-session",
                    123,
                    "migration-token",
                )
                .unwrap()
        );
        assert!(
            store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: "migration-token",
                    expected_state: AGENT_WORKER_STATE_DISPATCHING,
                    expected_worker_pid: None,
                    expected_heartbeat_at: Some("101"),
                    finished_at: "102",
                    error: "test completion",
                    permitted_successor_holder: None,
                })
                .unwrap()
        );
        assert!(
            !store
                .worker_schema_migration_deferred_blocking(future_migration_version)
                .unwrap()
        );
        let migrated_store = agent::TursoAgentStore::open_blocking_with_test_migration(
            &state_dir,
            future_migration_version,
            "CREATE TABLE deferred_worker_migration_probe (id INTEGER PRIMARY KEY)",
        )
        .unwrap();
        assert_eq!(migrated_store.pending_migration_version(), None);
        assert!(
            migrated_store
                .table_exists_blocking("deferred_worker_migration_probe")
                .unwrap()
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
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
            store.latest_codex_session_id_blocking().unwrap().as_deref(),
            Some("session-123")
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
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
                .record_run_outcome_blocking(agent::AgentRunOutcome {
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
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
            })
            .unwrap();
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
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
            })
            .unwrap();
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);

        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
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
            })
            .unwrap();
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
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
            })
            .unwrap();
        store
            .record_daemon_checkin_blocking("stale-daemon", "service", "90", "95", "99")
            .unwrap();
        fs::create_dir_all(state_dir.join("runs/project/run-1")).unwrap();
        fs::write(state_dir.join("runs/project/run-1/stdout.log"), "old run").unwrap();
        fs::write(state_dir.join("agent-service.out"), "service out").unwrap();
        fs::write(state_dir.join("agent-service.err"), "service err").unwrap();

        assert!(
            store
                .try_acquire_lease_blocking(project.id, "concurrent-scheduler", "104", "999")
                .unwrap()
        );
        let error = store.clean_agent_history_blocking("105").unwrap_err();
        assert!(error.to_string().contains("project leases are active"));
        assert_eq!(store.run_count_blocking().unwrap(), 2);
        assert!(
            store
                .release_lease_blocking(project.id, "concurrent-scheduler")
                .unwrap()
        );
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "stale-scheduler", "90", "100")
                .unwrap()
        );

        clean_agent_state(&store, &state_dir).unwrap();

        let project = store.list_projects_blocking().unwrap().remove(0);
        assert_eq!(project.failure_count, 0);
        assert_eq!(project.last_failure_at, None);
        assert_eq!(project.last_blocked_recovery_at, None);
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        assert_eq!(store.lease_count_blocking().unwrap(), 0);
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
        let mut project = agent::AgentProject {
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
            last_daemon_scan_status: None,
            last_daemon_scan_error: None,
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
        assert!(base_prompt.contains("A dirty worktree is expected"));
        assert!(base_prompt.contains("it is not a blocker by itself"));
        assert!(base_prompt.contains("same file is not automatically a blocker"));
        assert!(base_prompt.contains("the first non-whitespace token is exactly `/goal`"));
        assert!(base_prompt.contains("without including `/goal` in the goal objective"));
        assert!(base_prompt.contains("do not create a goal when `/goal` appears anywhere except"));
        assert!(base_prompt.contains("the goal objective is missing"));
        assert!(base_prompt.contains("skip tasks whose latest dated state note is `BLOCKED"));
        assert!(!base_prompt.contains("Embedded skill fallback:"));
        assert!(!base_prompt.contains("Interrupted task recovery:"));
        assert!(!base_prompt.contains("$git-commit"));
        assert!(!base_prompt.contains("Git push:"));

        project.git_mode = AgentGitMode::Commit;
        let commit_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, true, true);
        assert!(commit_prompt.contains("$git-commit"));
        assert!(commit_prompt.contains("CLT Agent <clt-agent@localhost>"));
        assert!(commit_prompt.contains("do not change Git configuration"));
        assert!(commit_prompt.contains("Pre-existing unstaged changes do not prevent a commit"));
        assert!(commit_prompt.contains("Do not require the worktree to be clean"));
        assert!(commit_prompt.contains("Do not commit when there are no tasks left"));
        assert!(commit_prompt.contains("CLT completed the scheduler-owned startup preparation"));
        assert!(commit_prompt.contains("Do not pull, fetch or otherwise synchronize"));
        assert!(!commit_prompt.contains("Git push:"));

        project.git_mode = AgentGitMode::CommitAndPush;
        let push_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::NextTodo, true, true);
        assert!(push_prompt.contains("Git commit:"));
        assert!(push_prompt.contains("Git push:"));
        assert!(!push_prompt.contains("git pull --no-rebase --autostash"));
        assert!(push_prompt.contains("Do not run `git push`"));
        assert!(push_prompt.contains("CLT proves the sealed local commit"));
        assert!(push_prompt.contains("single intended push URL"));
        assert!(push_prompt.contains("remains PUSH-PENDING"));
        assert!(push_prompt.contains("Never force-push"));

        let recovery_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::ResumeDoing, true, true);
        assert!(recovery_prompt.contains("Interrupted task recovery:"));
        assert!(recovery_prompt.contains("Resume and finish exactly one existing doing task."));
        assert!(recovery_prompt.contains("Do not pick or move a TODO task"));

        let exact_recovery_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::ResumeSession, true, true);
        assert!(exact_recovery_prompt.contains("Interactive handoff recovery:"));
        assert!(exact_recovery_prompt.contains("next unfinished substantive step"));
        assert!(exact_recovery_prompt.contains("claimed completion is not proof"));
        assert!(exact_recovery_prompt.contains("durable changes actually exist"));
        assert!(exact_recovery_prompt.contains("response-only task"));
        assert!(exact_recovery_prompt.contains("any requested durable output"));
        assert!(exact_recovery_prompt.contains("Do not select another Todo or Backlog task"));

        let blocked_prompt =
            build_agent_codex_prompt(&project, AgentTaskSelection::RecoverBlocked, true, true);
        assert!(blocked_prompt.contains("Blocked-task monitor:"));
        assert!(blocked_prompt.contains("at least one blocked task in todo or doing"));
        assert!(blocked_prompt.contains("before starting fresh Todo work"));
        assert!(blocked_prompt.contains("whether the recorded blocking conditions still exist"));
        assert!(blocked_prompt.contains("blocked task from todo or doing"));
        assert!(blocked_prompt.contains("Update the existing task; do not create a replacement"));
        assert!(blocked_prompt.contains("`UNBLOCKED YYYY-MM-DD:` note"));
        assert!(blocked_prompt.contains("Stop after handling that one blocked task"));
        assert!(!blocked_prompt.contains("Interrupted task recovery:"));
    }

    #[test]
    fn agent_git_identity_is_scoped_to_commit_modes() {
        let mut off_command = Command::new("codex");
        configure_agent_git_identity(&mut off_command, AgentGitMode::Off);
        assert!(off_command.get_envs().next().is_none());

        for mode in [AgentGitMode::Commit, AgentGitMode::CommitAndPush] {
            let mut command = Command::new("codex");
            configure_agent_git_identity(&mut command, mode);

            for (key, expected) in [
                ("GIT_AUTHOR_NAME", AGENT_GIT_IDENTITY_NAME),
                ("GIT_AUTHOR_EMAIL", AGENT_GIT_IDENTITY_EMAIL),
                ("GIT_COMMITTER_NAME", AGENT_GIT_IDENTITY_NAME),
                ("GIT_COMMITTER_EMAIL", AGENT_GIT_IDENTITY_EMAIL),
            ] {
                let value = command
                    .get_envs()
                    .find(|(name, _)| *name == OsStr::new(key))
                    .and_then(|(_, value)| value)
                    .and_then(OsStr::to_str);
                assert_eq!(value, Some(expected), "unexpected value for {key}");
            }
        }
    }

    #[test]
    fn agent_codex_prompt_embeds_only_missing_required_skills() {
        let mut project = agent::AgentProject {
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
            last_daemon_scan_status: None,
            last_daemon_scan_error: None,
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
            || Ok(()),
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
            || Ok(()),
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
    fn stop_agent_child_process_kills_term_resistant_group_descendant() {
        let root = temp_root("agent-process-group-shutdown");
        fs::create_dir_all(&root).unwrap();
        let descendant_pid_path = root.join("descendant.pid");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(
                r#"
(
    trap '' TERM
    count=0
    while [ "$count" -lt 20 ]; do
        sleep 1
        count=$((count + 1))
    done
) &
printf '%s\n' "$!" > "$1"
wait
"#,
            )
            .arg("sh")
            .arg(&descendant_pid_path);
        configure_agent_child_command(&mut command);
        let mut child = command.spawn().unwrap();
        let process_group = i32::try_from(child.id()).unwrap();

        let started = Instant::now();
        let descendant_pid = loop {
            if let Ok(raw_pid) = fs::read_to_string(&descendant_pid_path)
                && let Ok(pid) = raw_pid.trim().parse::<libc::pid_t>()
            {
                break pid;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "TERM-resistant descendant did not start"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert_ne!(descendant_pid, process_group);

        let status = stop_agent_child_process(&mut child).unwrap();

        assert!(status.is_some());
        assert!(!agent_process_group_exists(process_group).unwrap());
        // SAFETY: signal zero only checks whether the recorded descendant PID exists.
        assert_eq!(unsafe { libc::kill(descendant_pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn stop_agent_child_process_accepts_darwin_zombie_only_group_signal_rejection() {
        let mut command = Command::new("/usr/bin/true");
        configure_agent_child_command(&mut command);
        let mut child = command.spawn().unwrap();
        let process_group = i32::try_from(child.id()).unwrap();

        let started = Instant::now();
        while !interactive_child_exited_without_reaping(&child).unwrap() {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "process-group leader did not exit"
            );
            thread::sleep(Duration::from_millis(10));
        }

        // Darwin reports EPERM for a group containing only its unreaped zombie
        // leader. This is the exact rejection that cleanup must disambiguate.
        // SAFETY: the retained direct child anchors this positive process-group ID.
        assert_eq!(unsafe { libc::kill(-process_group, libc::SIGTERM) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));

        let status = stop_agent_child_process(&mut child).unwrap();

        assert!(status.is_some_and(|status| status.success()));
        assert!(!agent_process_group_exists(process_group).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn rejected_group_signal_is_accepted_only_after_reap_and_absence_proof() {
        let mut command = Command::new("/usr/bin/true");
        configure_agent_child_command(&mut command);
        let mut child = command.spawn().unwrap();
        let process_group = i32::try_from(child.id()).unwrap();

        let started = Instant::now();
        while !interactive_child_exited_without_reaping(&child).unwrap() {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "process-group leader did not exit"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let status = recover_rejected_agent_process_group_signal(
            &mut child,
            process_group,
            anyhow::anyhow!("synthetic group signal rejection"),
            |_| Ok(false),
        )
        .unwrap();

        assert!(status.is_some_and(|status| status.success()));
    }

    #[cfg(unix)]
    #[test]
    fn rejected_group_signal_stays_fenced_while_leader_is_live() {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30");
        configure_agent_child_command(&mut command);
        let mut child = command.spawn().unwrap();
        let process_group = i32::try_from(child.id()).unwrap();
        let probe_called = std::cell::Cell::new(false);

        let result = recover_rejected_agent_process_group_signal(
            &mut child,
            process_group,
            anyhow::anyhow!("synthetic group signal rejection"),
            |_| {
                probe_called.set(true);
                Ok(false)
            },
        );

        assert!(result.is_err());
        assert!(!probe_called.get());
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejected_group_signal_stays_fenced_when_group_is_live_or_unknown() {
        for group_probe in [Ok(true), Err(anyhow::anyhow!("synthetic probe failure"))] {
            let mut command = Command::new("/usr/bin/true");
            configure_agent_child_command(&mut command);
            let mut child = command.spawn().unwrap();
            let process_group = i32::try_from(child.id()).unwrap();

            let started = Instant::now();
            while !interactive_child_exited_without_reaping(&child).unwrap() {
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "process-group leader did not exit"
                );
                thread::sleep(Duration::from_millis(10));
            }

            let result = recover_rejected_agent_process_group_signal(
                &mut child,
                process_group,
                anyhow::anyhow!("synthetic group signal rejection"),
                |_| group_probe,
            );

            assert!(result.is_err());
            assert!(child.try_wait().unwrap().is_some());
        }
    }

    #[cfg(unix)]
    #[test]
    fn interactive_exit_observation_keeps_the_group_anchored_until_descendants_stop() {
        let root = temp_root("interactive-exit-group-drain");
        fs::create_dir_all(&root).unwrap();
        let descendant_pid_path = root.join("descendant.pid");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(
                r#"
(
    trap '' TERM
    sleep 30
) &
printf '%s\n' "$!" > "$1"
exit 0
"#,
            )
            .arg("sh")
            .arg(&descendant_pid_path);
        configure_interactive_child_command(&mut command);
        let mut child = command.spawn().unwrap();
        let process_group = i32::try_from(child.id()).unwrap();

        let started = Instant::now();
        let descendant_pid = loop {
            if let Ok(raw_pid) = fs::read_to_string(&descendant_pid_path)
                && let Ok(pid) = raw_pid.trim().parse::<libc::pid_t>()
            {
                break pid;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "interactive descendant did not start"
            );
            thread::sleep(Duration::from_millis(10));
        };
        while !interactive_child_exited_without_reaping(&child).unwrap() {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "interactive leader did not exit"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(agent_process_group_exists(process_group).unwrap());

        let status = stop_agent_child_process(&mut child).unwrap();

        assert!(status.is_some_and(|status| status.success()));
        assert!(!agent_process_group_exists(process_group).unwrap());
        // SAFETY: signal zero only checks whether the recorded descendant PID exists.
        assert_eq!(unsafe { libc::kill(descendant_pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn interactive_exec_gate_preserves_inherited_terminal_for_crossterm() {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        // SAFETY: openpty initializes both integer descriptors; null termios,
        // winsize, and name pointers request platform defaults.
        let open_result = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(
            open_result,
            0,
            "openpty failed: {}",
            io::Error::last_os_error()
        );
        // SAFETY: successful openpty returned two newly owned descriptors.
        let master = unsafe { fs::File::from_raw_fd(master_fd) };
        // SAFETY: successful openpty returned two newly owned descriptors.
        let slave = unsafe { fs::File::from_raw_fd(slave_fd) };

        let executable = std::env::current_exe().unwrap();
        let mut target = Command::new(executable);
        target
            .arg("--exact")
            .arg("tests::interactive_terminal_event_source_process_entry")
            .arg("--nocapture")
            .env("CLT_TEST_INTERACTIVE_TERMINAL_SOURCE", "1");
        let mut gate = interactive_exec_gate_command(&target).unwrap();
        gate.command_mut()
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        configure_interactive_child_command(gate.command_mut());
        let (mut child, mut launch_gate) = gate.spawn().unwrap();
        launch_gate.write_all(b"x").unwrap();
        launch_gate.flush().unwrap();
        drop(launch_gate);

        let status = child.wait().unwrap();
        drop(master);
        assert!(
            status.success(),
            "terminal source probe exited with {status}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn interactive_exec_gate_parent_drop_prevents_target_exec() {
        let root = temp_root("interactive-exec-gate-parent-drop");
        let launched_marker = root.join("target-launched");
        fs::create_dir_all(&root).unwrap();

        let mut target = Command::new("/bin/sh");
        target
            .arg("-c")
            .arg("printf launched > \"$1\"")
            .arg("sh")
            .arg(&launched_marker);
        let mut gate = interactive_exec_gate_command(&target).unwrap();
        configure_interactive_child_command(gate.command_mut());
        let (mut child, launch_gate) = gate.spawn().unwrap();

        // A hard guardian death closes its only writer. The gate must exit
        // without ever replacing itself with the interactive Codex target.
        drop(launch_gate);
        assert!(child.wait().unwrap().success());
        assert!(!launched_marker.exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn registered_interactive_gate_dying_before_release_recovers_exact_session() {
        let root = temp_root("interactive-gate-registered-before-release");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        let launched_marker = root.join("target-launched");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let provisional_holder = "clt-interactive-pre-release-requester";
        let guardian_holder = format!("clt-interactive-worker-{}-pre-release-guardian", u32::MAX);
        assert!(
            store
                .try_acquire_lease_blocking(
                    project_id,
                    provisional_holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        assert!(
            store
                .begin_stopped_session_interactive_blocking(
                    project_id,
                    "session-123",
                    provisional_holder,
                    None,
                )
                .unwrap()
        );
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    provisional_holder,
                    &guardian_holder,
                    60,
                )
                .unwrap()
        );

        let mut target = Command::new("/bin/sh");
        target
            .arg("-c")
            .arg("printf launched > \"$1\"")
            .arg("sh")
            .arg(&launched_marker);
        let mut gate = interactive_exec_gate_command(&target).unwrap();
        configure_interactive_child_command(gate.command_mut());
        let (mut child, launch_gate) = gate.spawn().unwrap();
        let child_pid = child.id();
        assert!(
            store
                .register_interactive_guardian_child_blocking(
                    project_id,
                    "session-123",
                    &guardian_holder,
                    child_pid,
                    60,
                )
                .unwrap()
        );
        let registered = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(registered.state, AgentSessionControlState::Interactive);
        assert_eq!(registered.child_pid, Some(child_pid));
        assert_eq!(
            registered.interactive_launch_token.as_deref(),
            Some(guardian_holder.as_str())
        );
        assert!(!launched_marker.exists());

        // Simulate SIGKILL after registration but before the one-byte release.
        let orphaned_lease = store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap();
        assert_eq!(orphaned_lease.holder, guardian_holder);
        drop(launch_gate);
        assert!(child.wait().unwrap().success());
        assert!(!launched_marker.exists());
        drop(store);

        reconcile_stale_agent_session_controls(
            &state_dir,
            project_id,
            Some(&orphaned_lease),
            false,
            agent_timestamp_seconds(),
        )
        .unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let recovered = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(recovered.state, AgentSessionControlState::ResumeRequested);
        assert_eq!(recovered.child_pid, None);
        assert!(recovered.interactive_holder.is_none());
        assert!(recovered.interactive_launch_token.is_none());
        assert!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stale_guardian_keeps_a_live_registered_group_fenced_then_recovers_when_gone() {
        let root = temp_root("interactive-gate-live-child-fence");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        let launched_marker = root.join("target-launched");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let provisional_holder = "clt-interactive-live-requester";
        let guardian_holder = format!("clt-interactive-worker-{}-live-guardian", u32::MAX);
        assert!(
            store
                .try_acquire_lease_blocking(
                    project_id,
                    provisional_holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );
        store
            .set_session_control_state_blocking(
                project_id,
                "session-123",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        assert!(
            store
                .begin_stopped_session_interactive_blocking(
                    project_id,
                    "session-123",
                    provisional_holder,
                    None,
                )
                .unwrap()
        );
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    provisional_holder,
                    &guardian_holder,
                    60,
                )
                .unwrap()
        );

        let mut target = Command::new("/bin/sh");
        target
            .arg("-c")
            .arg("printf launched > \"$1\"; sleep 30")
            .arg("sh")
            .arg(&launched_marker);
        let mut gate = interactive_exec_gate_command(&target).unwrap();
        configure_interactive_child_command(gate.command_mut());
        let (mut child, mut launch_gate) = gate.spawn().unwrap();
        let child_pid = child.id();
        assert!(
            store
                .register_interactive_guardian_child_blocking(
                    project_id,
                    "session-123",
                    &guardian_holder,
                    child_pid,
                    60,
                )
                .unwrap()
        );
        assert!(
            !store
                .recover_stale_interactive_guardian_blocking(
                    project_id,
                    "session-123",
                    &guardian_holder,
                    Some(child_pid.checked_add(1).unwrap()),
                    InteractiveGuardianDisposition::ResumeExec,
                )
                .unwrap()
        );
        launch_gate.write_all(b"x").unwrap();
        launch_gate.flush().unwrap();
        drop(launch_gate);
        let launch_deadline = Instant::now() + Duration::from_secs(2);
        while !launched_marker.exists() && Instant::now() < launch_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(launched_marker.exists());
        assert!(child.try_wait().unwrap().is_none());
        let orphaned_lease = store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap();
        assert_eq!(orphaned_lease.holder, guardian_holder);
        drop(store);

        let stale_now = agent_timestamp_seconds();
        reconcile_stale_agent_session_controls(
            &state_dir,
            project_id,
            Some(&orphaned_lease),
            false,
            stale_now,
        )
        .unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let fenced = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(fenced.state, AgentSessionControlState::Interactive);
        assert_eq!(fenced.child_pid, Some(child_pid));
        assert!(child.try_wait().unwrap().is_none());
        drop(store);

        assert!(
            stop_interactive_child_process(&mut child)
                .unwrap()
                .is_some()
        );
        reconcile_stale_agent_session_controls(
            &state_dir,
            project_id,
            Some(&orphaned_lease),
            false,
            stale_now,
        )
        .unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        let recovered = store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(recovered.state, AgentSessionControlState::ResumeRequested);
        assert_eq!(recovered.child_pid, None);
        assert!(recovered.interactive_holder.is_none());
        assert!(recovered.interactive_launch_token.is_none());
        assert!(
            store
                .lease_for_project_blocking(project_id)
                .unwrap()
                .is_none()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn automated_exec_gate_parent_drop_prevents_target_exec() {
        let root = temp_root("automated-exec-gate-parent-drop");
        let launched_marker = root.join("target-launched");
        fs::create_dir_all(&root).unwrap();

        let mut target = Command::new("/bin/sh");
        target
            .arg("-c")
            .arg("printf launched > \"$1\"")
            .arg("sh")
            .arg(&launched_marker);
        let mut gate = automated_exec_gate_command(&target).unwrap();
        let mut child = gate.spawn().unwrap();

        // Dropping the parent's only writer simulates its death before session
        // registration. The helper must observe EOF and never exec the target.
        drop(child.stdin.take().unwrap());
        assert!(child.wait().unwrap().success());
        assert!(!launched_marker.exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn exact_resume_runner_releases_gate_only_after_live_lease_registration() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("exact-resume-runner-launch-gate");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .set_session_control_state_blocking(
                project.id,
                "session-123",
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap();
        let lease_holder = "exact-resume-live-holder";
        assert!(
            store
                .try_acquire_lease_blocking(
                    project.id,
                    lease_holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );

        let launched_marker = root.join("fake-codex-launched");
        let fake_codex = root.join("fake-codex");
        fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\nprintf launched > \"{}\"\nprintf 'NO_TASKS_LEFT\\n'\n",
                launched_marker.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();
        let runner =
            CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);

        let wrong_holder_result = runner.run_project(
            &project,
            AgentTaskSelection::ResumeSession,
            Some("session-123"),
            "wrong-holder",
            None,
            &new_agent_shutdown_signal(),
        );
        assert!(wrong_holder_result.is_err());
        assert!(!launched_marker.exists());
        let unclaimed = store
            .session_control_blocking(project.id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(unclaimed.state, AgentSessionControlState::ResumeRequested);
        assert!(unclaimed.child_pid.is_none());
        assert!(unclaimed.run_token.is_none());

        let result = runner
            .run_project(
                &project,
                AgentTaskSelection::ResumeSession,
                Some("session-123"),
                lease_holder,
                None,
                &new_agent_shutdown_signal(),
            )
            .unwrap();
        assert_eq!(result.status, "idle");
        assert!(launched_marker.exists());
        let registered = store
            .session_control_blocking(project.id, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(registered.state, AgentSessionControlState::Running);
        assert!(registered.child_pid.is_some());
        assert_eq!(
            registered.run_token.as_deref(),
            result.session_run_token.as_deref()
        );
        assert_eq!(
            registered.stdout_path.as_deref(),
            Some(result.stdout_path.to_string_lossy().as_ref())
        );
        assert_eq!(
            registered.stderr_path.as_deref(),
            Some(result.stderr_path.to_string_lossy().as_ref())
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn codex_runner_writes_logs_and_treats_no_tasks_left_as_idle() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("agent-codex-runner");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .set_model_target_reasoning_blocking("openai", "gpt-5.6-terra", Some("low"))
            .unwrap();
        store
            .register_project_blocking(&project_root, "Project With Spaces")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;

        let fake_codex = root.join("fake-codex");
        fs::write(
            &fake_codex,
            "#!/bin/sh\nprintf 'session id: session-42\\n' >&2\nprintf 'arg=%s\\n' \"$@\" >&2\nprintf 'NO_TASKS_LEFT\\n'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();

        let project = agent::AgentProject {
            id: project_id,
            path: project_root.clone(),
            name: "Project With Spaces".to_string(),
            enabled: true,
            git_mode: AgentGitMode::Off,
            codex_provider: Some("openai".to_string()),
            codex_model: Some("gpt-5.6-terra".to_string()),
            codex_reasoning_effort: Some("high".to_string()),
            codex_fast_enabled: true,
            last_scan_at: None,
            last_daemon_scan_status: None,
            last_daemon_scan_error: None,
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
            .run_project(
                &project,
                AgentTaskSelection::NextTodo,
                None,
                "test-holder",
                None,
                &shutdown,
            )
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
            "arg=--sandbox\narg=danger-full-access\narg=--ask-for-approval\narg=never\narg=--enable\narg=goals\narg=--config\narg=model_provider=\"openai\"\narg=--model\narg=gpt-5.6-terra\narg=--config\narg=model_reasoning_effort=\"high\"\narg=--enable\narg=fast_mode\narg=--config\narg=service_tier=\"fast\"\narg=exec\narg=--skip-git-repo-check\narg=-C\n"
        ));
        assert!(!stderr.contains("arg=model_reasoning_effort=\"low\"\n"));
        assert!(stderr.contains(&format!("arg={}\n", project_root.display())));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    fn assert_connected_no_session_launch_recovery(mutate_checkout: bool) {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root(if mutate_checkout {
            "git-runner-no-session-mutated"
        } else {
            "git-runner-no-session-unchanged"
        });
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        add_task(&project_root, "committed task", None).unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let mut project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .set_project_git_mode_blocking(project.id, AgentGitMode::Commit)
                .unwrap()
        );
        project.git_mode = AgentGitMode::Commit;
        let scheduler_holder = "git-no-session-scheduler";
        assert!(
            store
                .try_acquire_lease_blocking(
                    project.id,
                    scheduler_holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );
        let fake_codex = root.join("fake-codex");
        fs::write(&fake_codex, "#!/bin/sh\nprintf 'NO_TASKS_LEFT\\n'\n").unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();
        let runner =
            CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);
        let run_token = "git-no-session-token";
        assert!(reserve_test_inline_worker(
            &store,
            project.id,
            run_token,
            scheduler_holder,
            std::process::id(),
            &agent_timestamp(),
        ));
        let lease_holder = agent_worker_lease_holder(run_token);

        let result = runner
            .run_project(
                &project,
                AgentTaskSelection::NextTodo,
                None,
                &lease_holder,
                Some(run_token),
                &new_agent_shutdown_signal(),
            )
            .unwrap();

        assert_eq!(result.status, "idle");
        assert!(result.codex_session_id.is_none());
        assert!(
            store
                .git_launch_state_blocking(project.id, run_token)
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .finalize_worker_blocking(agent::AgentWorkerFinalization {
                    worker_token: run_token,
                    expected_worker_pid: Some(std::process::id()),
                    expected_lease_holder: &lease_holder,
                    status: result.status,
                    finished_at: &agent_timestamp(),
                    exit_code: result.exit_code,
                    log_dir: Some(result.log_dir.to_string_lossy().as_ref()),
                    stdout_path: Some(result.stdout_path.to_string_lossy().as_ref()),
                    stderr_path: Some(result.stderr_path.to_string_lossy().as_ref()),
                    summary: Some(&result.summary),
                    codex_session_id: None,
                    error: None,
                })
                .unwrap()
                .is_some()
        );
        if mutate_checkout {
            fs::write(project_root.join("unexpected.txt"), "changed after reap\n").unwrap();
            let error = prepare_agent_git_start_state_for_run(
                &store,
                &project,
                AgentTaskSelection::NextTodo,
                false,
                false,
                "replacement-run",
            )
            .unwrap_err();
            assert!(format!("{error:#}").contains("unconsumed launch boundary"));
            assert!(
                store
                    .git_launch_state_blocking(project.id, run_token)
                    .unwrap()
                    .is_some()
            );
        } else {
            assert!(
                prepare_agent_git_start_state_for_run(
                    &store,
                    &project,
                    AgentTaskSelection::NextTodo,
                    false,
                    false,
                    "replacement-run",
                )
                .unwrap()
                .is_some()
            );
            assert!(
                store
                    .git_launch_state_blocking(project.id, run_token)
                    .unwrap()
                    .is_none()
            );
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn connected_no_session_reclaims_unchanged_launch_after_worker_finalization() {
        assert_connected_no_session_launch_recovery(false);
    }

    #[cfg(unix)]
    #[test]
    fn connected_no_session_preserves_launch_after_checkout_mutation() {
        assert_connected_no_session_launch_recovery(true);
    }

    #[cfg(unix)]
    #[test]
    fn codex_runner_registers_a_marker_derived_resume_without_a_session_banner() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("agent-known-session-without-banner");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- resumed task codex:session-known\n",
        )
        .unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let lease_holder = "known-session-live-holder";
        assert!(
            store
                .try_acquire_lease_blocking(
                    project.id,
                    lease_holder,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );

        let launched_marker = root.join("known-session-launched");
        let fake_codex = root.join("fake-codex");
        fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\nprintf launched > \"{}\"\nprintf 'NO_TASKS_LEFT\\n'\n",
                launched_marker.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();

        let runner =
            CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);
        assert!(
            runner
                .run_project(
                    &project,
                    AgentTaskSelection::ResumeDoing,
                    None,
                    "wrong-holder",
                    None,
                    &new_agent_shutdown_signal(),
                )
                .is_err()
        );
        assert!(!launched_marker.exists());
        assert!(
            store
                .session_control_blocking(project.id, "session-known")
                .unwrap()
                .is_none()
        );

        let result = runner
            .run_project(
                &project,
                AgentTaskSelection::ResumeDoing,
                None,
                lease_holder,
                None,
                &new_agent_shutdown_signal(),
            )
            .unwrap();

        assert_eq!(result.codex_session_id.as_deref(), Some("session-known"));
        assert!(launched_marker.exists());
        let control = store
            .session_control_blocking(project.id, "session-known")
            .unwrap()
            .unwrap();
        assert_eq!(control.state, AgentSessionControlState::Running);
        assert!(control.child_pid.is_some());
        assert!(control.run_token.is_some());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn codex_runner_renews_its_automated_project_lease_while_running() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("agent-codex-lease-renewal");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let lease_holder = "test-holder";
        assert!(
            store
                .try_acquire_lease_blocking(project.id, lease_holder, &agent_timestamp(), "1",)
                .unwrap()
        );

        let fake_codex = root.join("fake-codex");
        fs::write(
            &fake_codex,
            "#!/bin/sh\nsleep 0.2\nprintf 'NO_TASKS_LEFT\\n'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();

        let mut runner =
            CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);
        runner.heartbeat_interval = Duration::from_millis(20);
        runner.lease_timeout = Duration::from_secs(2);
        runner.lease_renew_interval = Duration::from_millis(50);

        let result = runner
            .run_project(
                &project,
                AgentTaskSelection::NextTodo,
                None,
                lease_holder,
                None,
                &new_agent_shutdown_signal(),
            )
            .unwrap();
        assert_eq!(result.status, "idle");

        let lease = store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap();
        assert_eq!(lease.holder, lease_holder);
        assert_ne!(lease.expires_at, "1");

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn codex_runner_links_the_session_while_the_task_is_doing() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("agent-live-session-link");
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        add_task(&project_root, "live task", None).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "live-link")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;

        let started_marker = root.join("fake-codex-started");
        let fake_codex = root.join("fake-codex");
        fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\nprintf 'session id: session-live\n' >&2\nprintf 'started\n' > \"{}\"\nsleep 1\nprintf 'finished\n'\n",
                started_marker.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).unwrap();

        let mut project = tui_agent_project_for_test(project_id, "live-link").project;
        project.path = project_root.clone();
        let move_root = project_root.clone();
        let move_thread = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !started_marker.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            move_task(&move_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
        });
        let runner =
            CodexAgentRunner::with_command(state_dir.clone(), Duration::from_secs(5), fake_codex);

        let result = runner
            .run_project(
                &project,
                AgentTaskSelection::NextTodo,
                None,
                "test-holder",
                None,
                &new_agent_shutdown_signal(),
            )
            .unwrap();
        move_thread.join().unwrap();

        assert_eq!(result.codex_session_id.as_deref(), Some("session-live"));
        let task = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing)
            .unwrap()
            .remove(0);
        assert_eq!(
            codex_session_for_task(&task).as_deref(),
            Some("session-live")
        );

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
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .upsert_model_provider_blocking(&agent::AgentModelProvider {
                id: "openrouter".to_string(),
                name: "OpenRouter".to_string(),
                base_url: Some("https://openrouter.ai/api/v1".to_string()),
                env_key: Some("OPENROUTER_API_KEY".to_string()),
                built_in: false,
                enabled: true,
            })
            .unwrap();
        store
            .upsert_model_target_blocking(&agent::AgentModelTarget {
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

        let project = agent::AgentProject {
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
            last_daemon_scan_status: None,
            last_daemon_scan_error: None,
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
                None,
                "test-holder",
                None,
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
        agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

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

        let project = agent::AgentProject {
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
            last_daemon_scan_status: None,
            last_daemon_scan_error: None,
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
            .run_project(
                &project,
                AgentTaskSelection::NextTodo,
                None,
                "test-holder",
                None,
                &shutdown,
            )
            .unwrap();

        assert_eq!(result.status, "interrupted");
        let stderr = fs::read_to_string(&result.stderr_path).unwrap();
        assert!(
            stderr.contains("arg=--enable\narg=goals\narg=--disable\narg=fast_mode\narg=exec\n")
        );
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
    fn tui_codex_handoff_status_renders_while_the_event_handler_is_blocked() {
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(100, 9);
        let mut terminal = Terminal::new(backend).unwrap();

        draw_tui_codex_handoff_status(&mut terminal, TuiCodexHandoffStage::WaitingForAutomatedExit)
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Codex handoff"));
        assert!(rendered.contains("Requesting the automated Codex process to stop..."));
        assert!(rendered.contains("Waiting for the current run to exit safely."));
    }

    #[test]
    fn tui_codex_handoff_status_is_printed_across_terminal_suspension() {
        let mut output = Vec::new();

        write_tui_codex_handoff_status(&mut output, TuiCodexHandoffStage::QueueingExecResume)
            .unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\nInteractive Codex exited.\nReturning the same session to automated exec mode...\n"
        );
    }

    #[test]
    fn move_task_writes_destination_and_removes_source() {
        let root = temp_root("move");

        add_task(&root, "ship the fix", None).unwrap();
        ManagedTaskWorkflow::new(&root)
            .move_task(TaskStatus::Todo, TaskStatus::Doing, "1")
            .unwrap();

        let todo = fs::read_to_string(root.join("tasks/todo.md")).unwrap();
        let doing = fs::read_to_string(root.join("tasks/doing.md")).unwrap();

        assert_eq!(todo, "# To Do Tasks\n");
        assert_eq!(doing, "# Doing Tasks\n- ship the fix\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_status_keeps_the_existing_serialized_names_and_order() {
        assert_eq!(
            TaskStatus::ALL.map(TaskStatus::as_str),
            ["todo", "doing", "done", "backlog"]
        );
        assert_eq!(TaskStatus::parse_arg("0").unwrap(), TaskStatus::Backlog);
        assert_eq!(TaskStatus::parse_arg("1").unwrap(), TaskStatus::Todo);
        assert_eq!(TaskStatus::parse_arg("2").unwrap(), TaskStatus::Doing);
        assert_eq!(TaskStatus::parse_arg("3").unwrap(), TaskStatus::Done);
        assert_eq!(TaskStatus::Todo.filename(), "todo.md");
        assert_eq!(TaskStatus::Doing.header(), "# Doing Tasks\n");
    }

    #[test]
    fn task_board_exposes_typed_storage_operations() {
        let root = temp_root("typed-task-board");
        init_tasks(&root, false).unwrap();
        let board = TaskBoard::for_project(&root);

        board
            .insert_content(TaskStatus::Todo, None, "typed task")
            .unwrap();
        let entry = board.entry(TaskStatus::Todo, 1).unwrap();

        assert_eq!(entry.summary, "typed task");
        assert!(board.entries(TaskStatus::Doing).unwrap().is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_module_has_no_agent_or_tui_dependencies() {
        let source = include_str!("task.rs");
        for forbidden in [
            "use super::*",
            "crate::agent",
            "super::agent",
            "agent::",
            "crate::tui",
            "super::tui",
            "tui::",
        ] {
            assert!(
                !source.contains(forbidden),
                "task.rs must not depend on {forbidden}"
            );
        }
    }

    #[test]
    fn move_task_supports_backlog_as_a_status() {
        let root = temp_root("move-backlog");

        add_task(&root, "consider this later", None).unwrap();
        move_task(&root, TaskStatus::Todo, TaskStatus::Backlog, "1").unwrap();

        assert!(read_tasks(&root, "todo").unwrap().is_empty());
        assert_eq!(
            read_tasks(&root, "backlog").unwrap(),
            vec!["- consider this later"]
        );
        assert_eq!(normalize_status_arg("0").unwrap(), TaskStatus::Backlog);

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
        move_task(&root, TaskStatus::Todo, TaskStatus::Done, "1").unwrap();
        move_task(&root, TaskStatus::Todo, TaskStatus::Done, "1").unwrap();

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

        move_task_to_archive_in_board(&tasks_dir, TaskStatus::Todo, "1").unwrap();

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

        move_task(&root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();

        assert!(directory_task_paths(&todo_dir).unwrap().is_empty());
        let doing_entries = read_task_entries(&root.join("tasks"), TaskStatus::Doing).unwrap();
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

        move_task(&root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();

        assert!(tasks_dir.join("doing").is_dir());
        assert!(tasks_dir.join("doing.md.bak").exists());
        let doing_entries = read_task_entries(&tasks_dir, TaskStatus::Doing).unwrap();
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

        let entries = read_task_entries(&root.join("tasks"), TaskStatus::Doing).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "Ship epic.");
        assert!(entries[0].has_subtasks);
        assert_eq!(
            read_tasks_in_board(&epic_dir, TaskStatus::Todo).unwrap(),
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
        insert_task_at_selection_in_board(
            &root.join("tasks"),
            TaskStatus::Todo,
            &state,
            "new task",
            None,
        )
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
        insert_task_at_selection_in_board(
            &root.join("tasks"),
            TaskStatus::Todo,
            &state,
            "new task",
            None,
        )
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
