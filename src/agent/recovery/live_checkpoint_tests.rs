use std::{fs, path::Path, process::Command, time::Duration};

use serde_json::json;

use super::{
    DIRTY_FILE, REQUIRED_FILE, WAL_MAINTENANCE_BYTES, WAL_WRITE_LIMIT_BYTES,
    checkpoint_live_registry_above, integrity_check, pin_store, read_snapshot, registered_store,
    write_lock,
};
use crate::agent::TursoAgentStore;

const CHILD_STATE: &str = "CLT_LIVE_CHECKPOINT_TEST_STATE";

struct TestChild(std::process::Child);

impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn live_stores_reclaim_large_wals_on_updates_and_opens() {
    let (root, state_dir, store, project) = registered_store("live-wal-checkpoint");
    let mut child = TestChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent::recovery::tests::live_checkpoint_tests::live_checkpoint_writer_child",
                "--nocapture",
            ])
            .env(CHILD_STATE, &state_dir)
            .spawn()
            .unwrap(),
    );
    for round in 0..2 {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !state_dir.join(format!("ready-{round}")).exists() {
            assert!(child.0.try_wait().unwrap().is_none(), "writer exited early");
            if std::time::Instant::now() > deadline {
                child.0.kill().unwrap();
                child.0.wait().unwrap();
                panic!("writer did not finish checkpoint pressure");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            fs::metadata(state_dir.join("agent.db-wal")).unwrap().len() >= WAL_WRITE_LIMIT_BYTES
        );
        // Both processes retain their original store across WAL generations.
        // The first round exercises an existing writer; the second a new open.
        let reopened = (round == 1).then(|| TursoAgentStore::open_blocking(&state_dir).unwrap());
        store
            .set_project_enabled_blocking(project.id, false)
            .unwrap();
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "parent", "100", "9999999999")
                .unwrap()
        );
        assert!(
            fs::metadata(state_dir.join("agent.db-wal")).unwrap().len() < WAL_MAINTENANCE_BYTES
        );
        if let Some(reopened) = reopened {
            assert!(!reopened.list_projects_blocking().unwrap()[0].enabled);
        }
        assert_eq!(
            store.list_projects_blocking().unwrap()[0].failure_count,
            round + 1
        );
        store
            .blocking
            .block_on(integrity_check(&store.recovery_db))
            .unwrap();
        assert!(!state_dir.join(DIRTY_FILE).exists());
        assert!(!state_dir.join(REQUIRED_FILE).exists());
        fs::write(state_dir.join(format!("checkpointed-{round}")), b"").unwrap();
    }
    assert!(child.0.wait().unwrap().success());
    assert!(store.list_projects_blocking().unwrap()[0].enabled);
    assert!(
        store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .is_none()
    );
    store
        .blocking
        .block_on(integrity_check(&store.recovery_db))
        .unwrap();
    let snapshot = read_snapshot(&state_dir).unwrap().unwrap();
    assert_eq!(snapshot["tables"]["projects"][0]["enabled"], json!(1));
    drop(store);
    let reopened = TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert!(reopened.list_projects_blocking().unwrap()[0].enabled);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_checkpoint_writer_child() {
    let Some(state_dir) = std::env::var_os(CHILD_STATE) else {
        return;
    };
    let state_dir = Path::new(&state_dir);
    let store = TursoAgentStore::open_blocking(state_dir).unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    for round in 0..2 {
        // One deliberately long operation produces a real, committed WAL over
        // the admission threshold without exposing synthetic bytes to Turso.
        store
            .blocking
            .block_on(async {
                let conn = store.recovery_db.connect()?;
                // Repeated large updates exercise the production byte threshold
                // without changing the schema or growing the final database.
                let payloads = ["x".repeat(1024 * 1024), "y".repeat(1024 * 1024)];
                for index in 0..130 {
                    conn.execute(
                        "UPDATE projects SET name = ?1 WHERE id = ?2",
                        clt_database::turso::params![payloads[index % 2].as_str(), project.id],
                    )
                    .await?;
                }
                conn.execute(
                    "UPDATE projects SET name = ?1 WHERE id = ?2",
                    clt_database::turso::params![project.name.as_str(), project.id],
                )
                .await?;
                conn.execute(
                    "UPDATE projects SET failure_count = ?1 WHERE id = ?2",
                    clt_database::turso::params![round + 1, project.id],
                )
                .await?;
                Ok(())
            })
            .unwrap();
        fs::write(state_dir.join(format!("ready-{round}")), b"").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !state_dir.join(format!("checkpointed-{round}")).exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "checkpoint did not finish"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!store.list_projects_blocking().unwrap()[0].enabled);
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .unwrap()
                .holder,
            "parent"
        );
        assert!(store.release_lease_blocking(project.id, "parent").unwrap());
        store
            .set_project_enabled_blocking(project.id, true)
            .unwrap();
    }
}

#[test]
fn live_checkpoint_defers_to_a_pinned_reader_then_reclaims_the_wal() {
    let (root, state_dir, mut reader, project) = registered_store("busy-live-checkpoint");
    pin_store(&mut reader);
    reader
        .write_checkpoint_pressure_blocking(project.id, 50)
        .unwrap();
    let writer = TursoAgentStore::open_blocking(&state_dir).unwrap();
    let original_len = fs::metadata(state_dir.join("agent.db-wal")).unwrap().len();
    {
        let _lock = write_lock(&state_dir).unwrap();
        writer
            .blocking
            .block_on_recovery(checkpoint_live_registry_above(
                &writer.recovery_db,
                &state_dir,
                1,
            ))
            .unwrap();
    }
    assert_eq!(
        fs::metadata(state_dir.join("agent.db-wal")).unwrap().len(),
        original_len
    );
    writer
        .set_project_enabled_blocking(project.id, false)
        .unwrap();
    assert!(!reader.list_projects_blocking().unwrap()[0].enabled);
    drop(reader);
    {
        let _lock = write_lock(&state_dir).unwrap();
        writer
            .blocking
            .block_on_recovery(checkpoint_live_registry_above(
                &writer.recovery_db,
                &state_dir,
                1,
            ))
            .unwrap();
    }
    assert!(fs::metadata(state_dir.join("agent.db-wal")).unwrap().len() < original_len);
    assert!(!writer.list_projects_blocking().unwrap()[0].enabled);
    writer
        .blocking
        .block_on(integrity_check(&writer.recovery_db))
        .unwrap();
    assert!(!state_dir.join(REQUIRED_FILE).exists());
    drop(writer);
    fs::remove_dir_all(root).unwrap();
}
