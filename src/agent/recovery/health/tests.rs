use super::*;
use crate::agent::{self, recovery::tests::registered_store};
use crate::worker::tests::reserve_test_worker;

fn corrupt_leaf_indexes(state_dir: &Path, pages: &[usize]) {
    let path = state_dir.join("agent.db");
    let mut bytes = fs::read(&path).unwrap();
    let page_size = u16::from_be_bytes([bytes[16], bytes[17]]) as usize;
    assert!(page_size > 1);
    for page in pages {
        let offset = (page - 1) * page_size;
        assert_eq!(bytes[offset], 0x0a, "fixture index must have a leaf root");
        // Remove the derived entries while leaving the table records intact.
        bytes[offset..offset + page_size].fill(0);
        bytes[offset] = 0x0a;
        bytes[offset + 5..offset + 7].copy_from_slice(&(page_size as u16).to_be_bytes());
    }
    fs::write(path, bytes).unwrap();
    fs::remove_file(state_dir.join(CHECKED_AT)).unwrap();
}

fn damaged_registry(indexes: &[&str]) -> (std::path::PathBuf, std::path::PathBuf) {
    let (root, state_dir, mut store, project) = registered_store("registry-index-health");
    for token in ["worker-one", "worker-two"] {
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "9999999999")
                .unwrap()
        );
        assert!(reserve_test_worker(
            &store,
            project.id,
            token,
            "scheduler",
            "100",
            1
        ));
        assert!(
            store
                .abandon_worker_blocking(agent::AgentWorkerAbandonment {
                    worker_token: token,
                    expected_state: "dispatching",
                    expected_worker_pid: None,
                    expected_heartbeat_at: Some("100"),
                    finished_at: "101",
                    error: "fixture worker exited",
                    permitted_successor_holder: None,
                })
                .unwrap()
        );
    }
    let pin = store.checkpoint_pin.take().unwrap();
    store
        .blocking
        .block_on(async {
            pin.execute("ROLLBACK", ()).await?;
            Ok(())
        })
        .unwrap();
    drop(pin);
    let pages = store
        .blocking
        .block_on(async {
            let conn = store.recovery_db.connect()?;
            let mut pages = Vec::new();
            for name in indexes {
                let mut rows = conn
                    .query(
                        "SELECT rootpage FROM sqlite_schema WHERE name = ?1",
                        [*name],
                    )
                    .await?;
                pages.push(rows.next().await?.unwrap().get::<i64>(0)? as usize);
            }
            let mut rows = conn.query("PRAGMA wal_checkpoint(TRUNCATE)", ()).await?;
            assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 0);
            Ok(pages)
        })
        .unwrap();
    drop(store);
    corrupt_leaf_indexes(&state_dir, &pages);
    (root, state_dir)
}

#[test]
fn fresh_open_detects_and_automatically_repairs_worker_indexes_without_losing_history() {
    let (root, state_dir) = damaged_registry(&WORKER_INDEXES[1..]);
    let original = fs::read(state_dir.join("agent.db")).unwrap();
    // No panic or recovery marker is necessary: ordinary opens detect damage.
    let store = agent::open_agent_store_at(&state_dir).unwrap();
    assert_eq!(store.run_count_blocking().unwrap(), 2);
    assert_eq!(store.list_terminal_workers_blocking().unwrap().len(), 2);
    assert_eq!(store.list_projects_blocking().unwrap().len(), 1);
    store
        .blocking
        .block_on(integrity_check(&store.recovery_db))
        .unwrap();
    assert!(!state_dir.join("recovery-required").exists());
    let archives = fs::read_dir(state_dir.join("quarantine"))
        .unwrap()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(archives.len(), 1);
    assert_eq!(
        fs::read(archives[0].path().join("agent.db")).unwrap(),
        original
    );
    drop(store);
    // A later independent open must see the same repaired records.
    let reopened = agent::open_agent_store_at(&state_dir).unwrap();
    assert_eq!(reopened.run_count_blocking().unwrap(), 2);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn automatic_index_repair_refuses_damage_outside_worker_indexes() {
    let (root, state_dir) = damaged_registry(&["sqlite_autoindex_projects_1"]);
    let original = fs::read(state_dir.join("agent.db")).unwrap();
    assert!(agent::open_agent_store_at(&state_dir).is_err());
    assert!(state_dir.join("recovery-required").exists());
    assert_eq!(fs::read(state_dir.join("agent.db")).unwrap(), original);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn health_check_reports_damage_and_waits_for_existing_clients_to_exit() {
    let (root, state_dir) = damaged_registry(&WORKER_INDEXES[1..]);
    let access = agent::recovery::RegistryAccess::shared(&state_dir).unwrap();
    assert!(agent::open_agent_store_at(&state_dir).is_err());
    assert!(state_dir.join("recovery-required").exists());
    assert!(!state_dir.join("quarantine").exists());
    drop(access);
    let store = agent::open_agent_store_at(&state_dir).unwrap();
    assert_eq!(store.run_count_blocking().unwrap(), 2);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
