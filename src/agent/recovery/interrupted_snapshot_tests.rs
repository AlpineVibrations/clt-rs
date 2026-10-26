use std::fs;

use super::{publish_verified_snapshot, try_repair};
use crate::agent::{
    AGENT_DB_FILE,
    recovery::{
        DIRTY_FILE, INTERRUPTED_UPDATE_REASON, REQUIRED_FILE, SNAPSHOT_FILE, check_clean,
        mark_required, read_snapshot, tests::registered_store,
    },
};

#[test]
fn snapshot_repair_keeps_live_readers_and_committed_worker_state() {
    let (root, state_dir, store, project) = registered_store("snapshot-live-repair");
    let stale = fs::read(state_dir.join(SNAPSHOT_FILE)).unwrap();
    store
        .mark_session_running_blocking(
            project.id,
            "surviving-session",
            std::process::id(),
            "surviving-run",
            &root.join("out"),
            &root.join("err"),
        )
        .unwrap();
    store
        .set_project_enabled_blocking(project.id, false)
        .unwrap();
    fs::write(state_dir.join(SNAPSHOT_FILE), stale).unwrap();
    fs::write(state_dir.join(DIRTY_FILE), "interrupted export").unwrap();
    assert!(check_clean(&state_dir).is_err());
    let original_db = fs::read(state_dir.join(AGENT_DB_FILE)).unwrap();
    let original_wal = fs::read(state_dir.join("agent.db-wal")).unwrap();

    assert!(try_repair(&state_dir).unwrap());
    assert_eq!(
        fs::read(state_dir.join(AGENT_DB_FILE)).unwrap(),
        original_db
    );
    assert_eq!(
        fs::read(state_dir.join("agent.db-wal")).unwrap(),
        original_wal
    );
    assert!(!store.list_projects_blocking().unwrap()[0].enabled);
    let controls = store
        .session_controls_for_project_blocking(project.id)
        .unwrap();
    assert_eq!(controls[0].child_pid, Some(std::process::id()));
    let snapshot = read_snapshot(&state_dir).unwrap().unwrap();
    assert_eq!(
        snapshot["tables"]["session_controls"][0]["run_token"],
        "surviving-run"
    );
    assert_eq!(snapshot["tables"]["projects"][0]["enabled"], 0);
    assert!(!state_dir.join("quarantine").exists());
    assert!(!state_dir.join(DIRTY_FILE).exists());
    assert!(!state_dir.join(REQUIRED_FILE).exists());
    store
        .set_project_enabled_blocking(project.id, true)
        .unwrap();
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn snapshot_repair_does_not_clear_a_new_storage_failure_or_exclusive_recovery() {
    for progress in [false, true] {
        let (root, state_dir, store, _) = registered_store("snapshot-reason-changed");
        let snapshot = read_snapshot(&state_dir).unwrap().unwrap();
        let original = fs::read(state_dir.join(SNAPSHOT_FILE)).unwrap();
        fs::write(state_dir.join(DIRTY_FILE), "interrupted export").unwrap();
        assert!(check_clean(&state_dir).is_err());
        if progress {
            fs::write(
                state_dir.join("recovery-in-progress.json"),
                "exclusive repair",
            )
            .unwrap();
        } else {
            mark_required(&state_dir, "shared WAL ownership failure").unwrap();
        }
        let reason = fs::read(state_dir.join(REQUIRED_FILE)).unwrap();
        assert!(publish_verified_snapshot(&state_dir, &snapshot).is_err());
        assert!(!try_repair(&state_dir).unwrap());
        assert_eq!(fs::read(state_dir.join(REQUIRED_FILE)).unwrap(), reason);
        assert_eq!(fs::read(state_dir.join(SNAPSHOT_FILE)).unwrap(), original);
        assert!(state_dir.join(DIRTY_FILE).exists());
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn snapshot_publication_failure_retains_recovery_evidence_and_can_retry() {
    let (root, state_dir, store, _) = registered_store("snapshot-publish-retry");
    let snapshot = read_snapshot(&state_dir).unwrap().unwrap();
    let original = fs::read(state_dir.join(SNAPSHOT_FILE)).unwrap();
    fs::write(state_dir.join(DIRTY_FILE), "interrupted export").unwrap();
    assert!(check_clean(&state_dir).is_err());
    fs::remove_file(state_dir.join(SNAPSHOT_FILE)).unwrap();
    fs::create_dir(state_dir.join(SNAPSHOT_FILE)).unwrap();
    assert!(publish_verified_snapshot(&state_dir, &snapshot).is_err());
    assert!(state_dir.join(DIRTY_FILE).exists());
    assert!(state_dir.join(REQUIRED_FILE).exists());
    fs::remove_dir(state_dir.join(SNAPSHOT_FILE)).unwrap();
    fs::write(state_dir.join(SNAPSHOT_FILE), original).unwrap();
    assert!(try_repair(&state_dir).unwrap());
    assert!(!state_dir.join(DIRTY_FILE).exists());
    assert!(!state_dir.join(REQUIRED_FILE).exists());
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_storage_failure_always_survives_snapshot_publication() {
    let (root, state_dir, store, _) = registered_store("snapshot-concurrent-failure");
    let snapshot = read_snapshot(&state_dir).unwrap().unwrap();
    for _ in 0..8 {
        fs::write(state_dir.join(DIRTY_FILE), "interrupted export").unwrap();
        mark_required(&state_dir, INTERRUPTED_UPDATE_REASON).unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|threads| {
            let publish = threads.spawn(|| {
                barrier.wait();
                publish_verified_snapshot(&state_dir, &snapshot)
            });
            barrier.wait();
            mark_required(&state_dir, "shared WAL ownership failure").unwrap();
            // Either publication completes first or it observes the new failure.
            let _ = publish.join().unwrap();
        });
        assert_eq!(
            fs::read_to_string(state_dir.join(REQUIRED_FILE)).unwrap(),
            "shared WAL ownership failure"
        );
    }
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
