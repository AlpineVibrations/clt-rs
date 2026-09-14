use std::{fs, path::Path, process::Command};

use super::{DIRTY_FILE, REQUIRED_FILE, integrity_check, registered_store};
use crate::agent::TursoAgentStore;

const IDLE_CHILD_STATE: &str = "CLT_REGISTRY_IDLE_WRITER_TEST_STATE";

#[test]
fn idle_registry_observes_peer_writes_and_preserves_new_leases() {
    let (root, state_dir, store, project) = registered_store("registry-idle-peer");
    store
        .try_acquire_lease_blocking(project.id, "old-owner", "100", "9999999999")
        .unwrap();
    store
        .write_checkpoint_pressure_blocking(project.id, 1100)
        .unwrap();
    store
        .blocking
        .block_on(async {
            let conn = store.recovery_db.connect()?;
            let mut rows = conn.query("PRAGMA wal_checkpoint(PASSIVE)", ()).await?;
            let row = rows.next().await?.unwrap();
            assert!(row.get::<i64>(2)? > 0);
            Ok(())
        })
        .unwrap();
    drop(store);
    // An exclusive reopen after a partial checkpoint loads a disk scan. Keep
    // this process idle while a peer advances the WAL; subsequent connections
    // must not republish the original scan under the peer's newer metadata.
    let store = TursoAgentStore::open_blocking(&state_dir).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "agent::recovery::tests::idle_tests::registry_idle_writer_child",
            "--nocapture",
        ])
        .env(IDLE_CHILD_STATE, &state_dir)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    for _ in 0..3 {
        assert_eq!(
            store.list_projects_blocking().unwrap()[0].failure_count,
            1199
        );
        assert!(
            store
                .release_lease_blocking(project.id, "old-owner")
                .unwrap()
        );
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "new-owner", "200", "9999999999")
                .unwrap()
        );
        let peer = TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            peer.lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .holder,
            "new-owner"
        );
        assert!(
            peer.release_lease_blocking(project.id, "new-owner")
                .unwrap()
        );
        assert!(
            peer.try_acquire_lease_blocking(project.id, "old-owner", "300", "9999999999")
                .unwrap()
        );
    }
    assert!(
        store
            .set_project_enabled_blocking(project.id, false)
            .unwrap()
    );
    store
        .blocking
        .block_on(integrity_check(&store.recovery_db))
        .unwrap();
    drop(store);
    let reopened = crate::agent::open_agent_store_at(&state_dir).unwrap();
    let actual = &reopened.list_projects_blocking().unwrap()[0];
    assert!(!actual.enabled);
    assert_eq!(actual.failure_count, 1199);
    assert!(!state_dir.join(REQUIRED_FILE).exists());
    assert!(!state_dir.join(DIRTY_FILE).exists());
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_idle_writer_child() {
    let Some(state_dir) = std::env::var_os(IDLE_CHILD_STATE) else {
        return;
    };
    let store = TursoAgentStore::open_blocking(Path::new(&state_dir)).unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .write_checkpoint_pressure_blocking(project.id, 1200)
        .unwrap();
}
