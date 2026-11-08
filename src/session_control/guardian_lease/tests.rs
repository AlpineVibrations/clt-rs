use super::*;
use crate::test_support::prelude::*;

struct Fixture {
    store: TursoAgentStore,
    project_id: i64,
    holder: String,
    state_dir: PathBuf,
    _root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state_dir = root.path().join("state");
        let project_root = root.path().join("project");
        fs::create_dir_all(&project_root).unwrap();
        let store = TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap().remove(0).id;
        let requester = InteractiveAgentLease::holder_for_idle_session();
        let holder =
            interactive_guardian_holder(InteractiveGuardianDisposition::PreserveIdleSession);
        assert!(
            store
                .try_acquire_lease_blocking(
                    project_id,
                    &requester,
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                )
                .unwrap()
        );
        assert!(
            store
                .reserve_idle_session_interactive_blocking(
                    project_id,
                    "session-123",
                    &requester,
                    None,
                )
                .unwrap()
        );
        assert!(
            store
                .adopt_interactive_guardian_blocking(
                    project_id,
                    Some("session-123"),
                    &requester,
                    &holder,
                    60,
                )
                .unwrap()
        );
        Self {
            store,
            project_id,
            holder,
            state_dir,
            _root: root,
        }
    }

    fn renewal(&self) -> InteractiveGuardianLease {
        let mut renewal = InteractiveGuardianLease::new(Duration::from_secs(60));
        renewal.expires_at = self.lease().expires_at.parse().unwrap();
        renewal.next_renewal = Instant::now();
        renewal
    }

    fn lease(&self) -> AgentLeaseRecord {
        self.store
            .lease_for_project_blocking(self.project_id)
            .unwrap()
            .unwrap()
    }
}

#[test]
fn busy_writer_defers_renewal_without_mutating_or_extending_the_lease() {
    let fixture = Fixture::new();
    let mut renewal = fixture.renewal();
    let before = fixture.lease();
    let snapshot = fs::read(fixture.state_dir.join("registry.json")).unwrap();
    let writer = fixture.store.try_service_restart_guard().unwrap().unwrap();
    let start = Instant::now();
    for _ in 0..2 {
        renewal.next_renewal = Instant::now();
        renewal
            .renew_if_due(&fixture.store, fixture.project_id, &fixture.holder)
            .unwrap();
    }
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(renewal.expires_at.to_string(), before.expires_at);
    assert_eq!(fixture.lease().expires_at, before.expires_at);
    assert_eq!(
        fs::read(fixture.state_dir.join("registry.json")).unwrap(),
        snapshot
    );
    assert!(!fixture.state_dir.join("registry-dirty").exists());
    drop(writer);

    renewal.next_renewal = Instant::now();
    renewal
        .renew_if_due(&fixture.store, fixture.project_id, &fixture.holder)
        .unwrap();
    assert_eq!(fixture.lease().expires_at, renewal.expires_at.to_string());
    assert!(renewal.next_renewal > Instant::now());
}

#[test]
fn expired_reservation_is_not_renewed_even_when_the_writer_is_available() {
    let fixture = Fixture::new();
    let mut renewal = fixture.renewal();
    renewal.expires_at = agent_timestamp_seconds();
    fixture
        .store
        .renew_lease_blocking(
            fixture.project_id,
            &fixture.holder,
            &renewal.expires_at.to_string(),
        )
        .unwrap();
    let error = renewal
        .renew_if_due(&fixture.store, fixture.project_id, &fixture.holder)
        .unwrap_err();
    assert!(error.to_string().contains("lease expired"));
    assert_eq!(fixture.lease().expires_at, renewal.expires_at.to_string());
}

#[test]
fn changed_lease_owner_is_not_treated_as_writer_contention() {
    let fixture = Fixture::new();
    let mut renewal = fixture.renewal();
    assert!(
        fixture
            .store
            .release_lease_blocking(fixture.project_id, &fixture.holder)
            .unwrap()
    );
    assert!(
        fixture
            .store
            .try_acquire_lease_blocking(
                fixture.project_id,
                "replacement-owner",
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap()
    );
    let error = renewal
        .renew_if_due(&fixture.store, fixture.project_id, &fixture.holder)
        .unwrap_err();
    assert!(error.to_string().contains("no longer holds"));
    assert_eq!(fixture.lease().holder, "replacement-owner");
}

#[test]
fn recovery_required_is_fatal_even_with_a_busy_writer() {
    let fixture = Fixture::new();
    let mut renewal = fixture.renewal();
    let _writer = fixture.store.try_service_restart_guard().unwrap().unwrap();
    fs::write(
        fixture.state_dir.join("recovery-required"),
        "storage failure",
    )
    .unwrap();
    let error = renewal
        .renew_if_due(&fixture.store, fixture.project_id, &fixture.holder)
        .unwrap_err();
    assert!(format!("{error:#}").contains("recovery required"));
}

#[test]
fn interrupted_update_is_fatal_after_the_writer_is_released() {
    let fixture = Fixture::new();
    let mut renewal = fixture.renewal();
    let writer = fixture.store.try_service_restart_guard().unwrap().unwrap();
    fs::write(
        fixture.state_dir.join("registry-dirty"),
        "update in progress",
    )
    .unwrap();
    // A dirty interval owned by a live writer is ordinary contention.
    renewal
        .renew_if_due(&fixture.store, fixture.project_id, &fixture.holder)
        .unwrap();
    assert!(!fixture.state_dir.join("recovery-required").exists());
    drop(writer);
    renewal.next_renewal = Instant::now();
    let error = renewal
        .renew_if_due(&fixture.store, fixture.project_id, &fixture.holder)
        .unwrap_err();
    assert!(format!("{error:#}").contains("recovery required"));
}

#[cfg(unix)]
#[test]
fn interactive_child_survives_writer_contention_longer_than_the_old_lock_timeout() {
    let fixture = Fixture::new();
    let mut command = Command::new("/bin/sleep");
    command.arg("7");
    configure_interactive_child_command(&mut command);
    let mut child = command.spawn().unwrap();
    let renewal = fixture.renewal();
    let before = fixture.lease().expires_at.parse::<u64>().unwrap();
    assert!(
        fixture
            .store
            .register_interactive_guardian_child_blocking(
                fixture.project_id,
                "session-123",
                &fixture.holder,
                child.id(),
                60,
            )
            .unwrap()
    );
    let writer = fixture.store.try_service_restart_guard().unwrap().unwrap();
    let parent = Arc::new(AtomicBool::new(true));
    let connected = Arc::clone(&parent);
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let result = super::super::wait_for_interactive_codex(
            &mut child,
            &fixture.store,
            fixture.project_id,
            &fixture.holder,
            Duration::from_secs(60),
            Some(renewal),
            &connected,
        );
        let lease = fixture.lease();
        let control = fixture
            .store
            .session_control_blocking(fixture.project_id, "session-123")
            .unwrap()
            .unwrap();
        tx.send(()).unwrap();
        (result, lease, control)
    });
    thread::sleep(Duration::from_millis(5500));
    let exited_while_busy = rx.try_recv().is_ok();
    drop(writer);
    let completed = exited_while_busy || rx.recv_timeout(Duration::from_secs(10)).is_ok();
    parent.store(false, Ordering::SeqCst);
    let (result, lease, control) = handle.join().unwrap();
    assert!(
        !exited_while_busy,
        "registry contention must not terminate the child"
    );
    assert!(completed);
    assert!(result.unwrap().unwrap().success());
    assert!(lease.expires_at.parse::<u64>().unwrap() > before);
    assert_eq!(control.state, AgentSessionControlState::Interactive);
    assert_eq!(
        control.interactive_holder.as_deref(),
        Some(lease.holder.as_str())
    );
}

#[cfg(unix)]
#[test]
fn permanent_renewal_failures_reap_the_child_and_preserve_the_cause() {
    for scenario in ["expired", "ownership", "recovery"] {
        let fixture = Fixture::new();
        let mut renewal = fixture.renewal();
        let expected = match scenario {
            "expired" => {
                renewal.expires_at = agent_timestamp_seconds();
                "lease expired"
            }
            "ownership" => {
                fixture
                    .store
                    .release_lease_blocking(fixture.project_id, &fixture.holder)
                    .unwrap();
                "no longer holds"
            }
            "recovery" => {
                fs::write(
                    fixture.state_dir.join("recovery-required"),
                    "storage failure",
                )
                .unwrap();
                "recovery required"
            }
            _ => unreachable!(),
        };
        let mut command = Command::new("/bin/sleep");
        command.arg("10");
        configure_interactive_child_command(&mut command);
        let mut child = command.spawn().unwrap();
        let result = super::super::wait_for_interactive_codex(
            &mut child,
            &fixture.store,
            fixture.project_id,
            &fixture.holder,
            Duration::from_secs(60),
            Some(renewal),
            &AtomicBool::new(true),
        );
        let status = child.try_wait().unwrap().expect("child must be reaped");
        assert!(!status.success());
        assert!(format!("{:#}", result.unwrap_err()).contains(expected));
    }
}

#[cfg(unix)]
#[test]
fn parent_disconnect_still_reaps_the_interactive_child_while_the_writer_is_busy() {
    let fixture = Fixture::new();
    let mut command = Command::new("/bin/sleep");
    command.arg("10");
    configure_interactive_child_command(&mut command);
    let mut child = command.spawn().unwrap();
    let renewal = fixture.renewal();
    let writer = fixture.store.try_service_restart_guard().unwrap().unwrap();
    let parent = Arc::new(AtomicBool::new(true));
    let connected = Arc::clone(&parent);
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let result = super::super::wait_for_interactive_codex(
            &mut child,
            &fixture.store,
            fixture.project_id,
            &fixture.holder,
            Duration::from_secs(60),
            Some(renewal),
            &connected,
        );
        tx.send(()).unwrap();
        (result, child.try_wait().unwrap())
    });
    thread::sleep(Duration::from_millis(500));
    parent.store(false, Ordering::SeqCst);
    let reaped_while_busy = rx.recv_timeout(Duration::from_secs(3)).is_ok();
    drop(writer);
    let (result, status) = handle.join().unwrap();
    assert!(
        reaped_while_busy,
        "lock contention must not block the parent lifeline"
    );
    assert!(!result.unwrap().unwrap().success());
    assert!(status.is_some());
}
