use std::{fs, path::Path, process::Command, time::Duration};

use super::{REQUIRED_FILE, registered_store};

const CHILD_STATE: &str = "CLT_REGISTRY_CONCURRENT_OPEN_TEST_STATE";

#[test]
fn registry_connection_opens_preserve_concurrent_writer_frames() {
    let (root, state_dir, store, project) = registered_store("registry-concurrent-open");
    store
        .write_checkpoint_pressure_blocking(project.id, 1_100)
        .unwrap();
    store
        .blocking
        .block_on(async {
            let conn = store.recovery_db.connect()?;
            let mut rows = conn.query("PRAGMA wal_checkpoint(PASSIVE)", ()).await?;
            let row = rows.next().await?.unwrap();
            let frames = row.get::<i64>(1)?;
            let backfilled = row.get::<i64>(2)?;
            // Positive partial backfill forces the peer to load a disk scan;
            // subsequent connections then exercise coordination reseeding.
            assert!(backfilled > 0 && backfilled < frames);
            Ok(())
        })
        .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "agent::recovery::tests::concurrent_open_tests::registry_concurrent_open_reader_child",
            "--nocapture",
        ])
        .env(CHILD_STATE, &state_dir)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !state_dir.join("reader-ready").exists() {
        assert!(child.try_wait().unwrap().is_none(), "reader exited early");
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("reader did not open the registry");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let writes = store.write_checkpoint_pressure_blocking(project.id, 2_000);
    fs::write(state_dir.join("writer-done"), b"").unwrap();
    let status = child.wait().unwrap();
    writes.unwrap();
    assert!(status.success(), "concurrent reader failed: {status}");
    assert_eq!(
        store.list_projects_blocking().unwrap()[0].failure_count,
        1_999
    );
    store
        .blocking
        .block_on(super::integrity_check(&store.recovery_db))
        .unwrap();
    drop(store);
    assert!(!state_dir.join(REQUIRED_FILE).exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_concurrent_open_reader_child() {
    let Some(state_dir) = std::env::var_os(CHILD_STATE) else {
        return;
    };
    let state_dir = Path::new(&state_dir);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let db = turso::Builder::new_local(state_dir.join("agent.db").to_str().unwrap())
            .experimental_multiprocess_wal(true)
            .build()
            .await
            .unwrap();
        fs::write(state_dir.join("reader-ready"), b"").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut reads = 0;
        while !state_dir.join("writer-done").exists() || reads < 100 {
            assert!(
                std::time::Instant::now() < deadline,
                "writer did not finish"
            );
            // Each connection constructs its own WAL coordination view while
            // a different process keeps committing frames to the same index.
            let conn = db.connect().unwrap();
            let mut rows = conn
                .query("SELECT failure_count FROM projects", ())
                .await
                .unwrap();
            assert!(rows.next().await.unwrap().is_some());
            reads += 1;
        }
    });
}
