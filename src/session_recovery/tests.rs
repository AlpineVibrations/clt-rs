use super::*;
use crate::{
    agent::{AgentSessionControlState, TursoAgentStore},
    application::{AGENT_PROJECT_ID_ENV, AGENT_RUN_TOKEN_ENV},
    runner::agent_timestamp_seconds,
    scheduler::{agent_lease_holder_pid, reconcile_stale_agent_session_controls},
    session_control::prepare_tui_codex_session_interrupt_at,
    test_support::temp_root,
};
use std::{path::PathBuf, process::Child};

#[test]
fn recovered_lease_identifies_its_owner() {
    assert_eq!(
        agent_lease_holder_pid("clt-reattached-123-1788945300-p17-s7"),
        Some(123)
    );
    assert_eq!(recovered_supervisor_pid("clt-reattached-invalid"), None);
}

#[test]
fn supervisor_process_entry() {
    let Some(state_dir) = std::env::var_os("CLT_TEST_RECOVERED_STATE_DIR") else {
        return;
    };
    run_orphaned_session_supervisor(
        &PathBuf::from(state_dir),
        std::env::var("CLT_TEST_RECOVERED_PROJECT")
            .unwrap()
            .parse()
            .unwrap(),
        &std::env::var("CLT_TEST_RECOVERED_SESSION").unwrap(),
        std::env::var("CLT_TEST_RECOVERED_PID")
            .unwrap()
            .parse()
            .unwrap(),
        &std::env::var("CLT_TEST_RECOVERED_TOKEN").unwrap(),
    )
    .unwrap();
}

#[test]
fn running_process_entry() {
    if std::env::var_os("CLT_TEST_RECOVERED_RUNNING_PROCESS").is_some() {
        thread::sleep(Duration::from_secs(60));
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Fixture {
    root: PathBuf,
    state_dir: PathBuf,
    project_id: i64,
    child: Child,
    token: String,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Fixture {
    fn new() -> Self {
        let root = temp_root("reattached-session");
        let state_dir = root.join("state");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let store = TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let token = next_agent_worker_token(project_id);
        // macOS redacts the environment of platform binaries such as sleep;
        // a test helper has the same inspectable run context as Codex itself.
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("session_recovery::tests::running_process_entry")
            .env("CLT_TEST_RECOVERED_RUNNING_PROCESS", "1")
            .env(AGENT_PROJECT_ID_ENV, project_id.to_string())
            .env(AGENT_RUN_TOKEN_ENV, &token)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_agent_child_command(&mut command);
        let child = command.spawn().unwrap();
        fs::write(
            root.join("run.err"),
            "session id: retained-session\nexisting work\n",
        )
        .unwrap();
        store
            .mark_session_running_blocking(
                project_id,
                "retained-session",
                child.id(),
                &token,
                &root.join("run.out"),
                &root.join("run.err"),
            )
            .unwrap();
        Self {
            root,
            state_dir,
            project_id,
            child,
            token,
        }
    }

    fn control(&self) -> AgentSessionControlRecord {
        with_agent_store_at(&self.state_dir, |store| {
            store.session_control_blocking(self.project_id, "retained-session")
        })
        .unwrap()
        .unwrap()
    }

    fn wait_for(&mut self, state: AgentSessionControlState) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.control().state != state {
            // Reap our fixture child just as PID 1 reaps an orphan in production.
            let _ = self.child.try_wait().unwrap();
            assert!(
                Instant::now() < deadline,
                "session did not enter {state:?}; current={:?}; supervisor logs={}",
                self.control(),
                fs::read_dir(self.state_dir.join("session-supervisors"))
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| entry.ok())
                    .filter_map(|entry| fs::read_to_string(entry.path()).ok())
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn attach(&self) -> String {
        assert!(
            ensure_orphaned_session_supervision(
                &self.state_dir,
                self.project_id,
                "retained-session"
            )
            .unwrap()
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(lease) = with_agent_store_at(&self.state_dir, |store| {
                store.lease_for_project_blocking(self.project_id)
            })
            .unwrap()
            {
                assert!(recovered_supervisor_pid(&lease.holder).is_some());
                return lease.holder;
            }
            assert!(Instant::now() < deadline, "supervisor failed to attach");
            thread::sleep(Duration::from_millis(25));
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let deadline = Instant::now() + Duration::from_secs(5);
        while with_agent_store_at(&self.state_dir, |store| {
            store.lease_for_project_blocking(self.project_id)
        })
        .ok()
        .flatten()
        .is_some()
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(25));
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn scheduler_reattaches_without_restarting_work_and_recovers_after_exit() {
    let mut fixture = Fixture::new();
    let original = fixture.control();
    reconcile_stale_agent_session_controls(
        &fixture.state_dir,
        fixture.project_id,
        None,
        false,
        agent_timestamp_seconds(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while with_agent_store_at(&fixture.state_dir, |store| {
        store.lease_for_project_blocking(fixture.project_id)
    })
    .unwrap()
    .is_none()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(25));
    }
    assert!(fixture.child.try_wait().unwrap().is_none());
    assert_eq!(fixture.control(), original);
    assert!(
        !ensure_orphaned_session_supervision(
            &fixture.state_dir,
            fixture.project_id,
            "retained-session"
        )
        .unwrap()
    );
    fixture.child.kill().unwrap();
    fixture.child.wait().unwrap();
    fixture.wait_for(AgentSessionControlState::ResumeRequested);
    assert_eq!(fixture.control().run_token, original.run_token);
    assert_eq!(fixture.control().stderr_path, original.stderr_path);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn reattached_supervisor_delivers_stop_to_existing_process() {
    let mut fixture = Fixture::new();
    fixture.attach();
    with_agent_store_at(&fixture.state_dir, |store| {
        store.request_session_stop_blocking(
            fixture.project_id,
            "retained-session",
            fixture.child.id(),
            &fixture.token,
        )
    })
    .unwrap();
    fixture.wait_for(AgentSessionControlState::Stopped);
    assert!(fixture.child.try_wait().unwrap().is_some());
    assert_eq!(fixture.control().child_pid, None);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn takeover_reattaches_missing_supervisor_and_transfers_lease_after_exit() {
    let mut fixture = Fixture::new();
    let state_dir = fixture.state_dir.clone();
    let project_id = fixture.project_id;
    let handoff = thread::spawn(move || {
        prepare_tui_codex_session_interrupt_at(
            &state_dir,
            project_id,
            "retained-session",
            60,
            Duration::from_secs(10),
        )
    });
    fixture.wait_for(AgentSessionControlState::ReadyInteractive);
    let lease = handoff.join().unwrap().unwrap();
    assert_eq!(
        fixture.control().interactive_holder.as_deref(),
        Some(lease.holder.as_str())
    );
    lease.release().unwrap();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn replacement_supervisor_can_itself_be_replaced() {
    let mut fixture = Fixture::new();
    let first_holder = fixture.attach();
    let first_pid = recovered_supervisor_pid(&first_holder).unwrap();
    // The helper is our own spawned test process. Production takeover uses the
    // platform's generation-bound handles instead of this test-only signal.
    assert_eq!(unsafe { libc::kill(first_pid as i32, libc::SIGKILL) }, 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    while crate::platform::local_process_is_running(first_pid) != Some(false) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(25));
    }
    assert!(fixture.child.try_wait().unwrap().is_none());
    let second_holder = fixture.attach();
    assert_ne!(first_holder, second_holder);
    with_agent_store_at(&fixture.state_dir, |store| {
        store.request_session_stop_blocking(
            fixture.project_id,
            "retained-session",
            fixture.child.id(),
            &fixture.token,
        )
    })
    .unwrap();
    fixture.wait_for(AgentSessionControlState::Stopped);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn takeover_of_exited_orphan_does_not_require_a_daemon() {
    let mut fixture = Fixture::new();
    fixture.child.kill().unwrap();
    fixture.child.wait().unwrap();
    let lease = prepare_tui_codex_session_interrupt_at(
        &fixture.state_dir,
        fixture.project_id,
        "retained-session",
        60,
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(
        fixture.control().state,
        AgentSessionControlState::ReadyInteractive
    );
    assert_eq!(fixture.control().child_pid, None);
    lease.release().unwrap();
}
