use clt_database::turso;
use std::{
    fs,
    path::Path,
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use super::{REQUIRED_FILE, registered_store};

const CHILD_STATE: &str = "CLT_REGISTRY_CONCURRENT_OPEN_TEST_STATE";
const PHASE_TIMEOUT: Duration = Duration::from_secs(60);
const READ_INTERVAL: Duration = Duration::from_millis(2);
const SEED_WRITES: usize = 1_100;
const PROBE_WRITES: usize = 2;

struct ReaderProcess(Child);

impl ReaderProcess {
    fn wait_for_marker(&mut self, marker: &Path) {
        let deadline = Instant::now() + PHASE_TIMEOUT;
        while !marker.exists() {
            if let Some(status) = self.0.try_wait().unwrap() {
                panic!("reader exited with {status} before creating {marker:?}");
            }
            assert!(Instant::now() < deadline, "reader did not create {marker:?}");
            thread::sleep(READ_INTERVAL);
        }
    }

    fn wait_for_success(&mut self) {
        let deadline = Instant::now() + PHASE_TIMEOUT;
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "concurrent reader failed: {status}");
                return;
            }
            assert!(Instant::now() < deadline, "reader did not finish");
            thread::sleep(READ_INTERVAL);
        }
    }
}

impl Drop for ReaderProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn registry_connection_opens_preserve_concurrent_writer_frames() {
    let (root, state_dir, mut store, project) = registered_store("registry-concurrent-open");
    super::pin_store(&mut store);
    store
        .write_checkpoint_pressure_blocking(project.id, SEED_WRITES)
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
    let mut reader = ReaderProcess(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent::recovery::tests::concurrent_open_tests::registry_concurrent_open_reader_child",
                "--nocapture",
            ])
            .env(CHILD_STATE, &state_dir)
            .spawn()
            .unwrap(),
    );
    reader.wait_for_marker(&state_dir.join("reader-ready"));
    // Confirm the peer can reopen against frames committed after its database
    // was opened, even if scheduling would otherwise let the full batch race by.
    store
        .write_checkpoint_pressure_blocking(project.id, PROBE_WRITES)
        .unwrap();
    reader.wait_for_marker(&state_dir.join("reader-saw-write"));
    store
        .write_checkpoint_pressure_blocking(project.id, 2_000)
        .unwrap();
    fs::write(state_dir.join("writer-done"), b"").unwrap();
    reader.wait_for_success();
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let db = turso::Builder::new_local(state_dir.join("agent.db").to_str().unwrap())
            .experimental_multiprocess_wal(true)
            .build()
            .await
            .unwrap();
        let mut deadline = Instant::now() + PHASE_TIMEOUT;
        let mut saw_write = false;
        let mut reads = 0;
        let writer_done = state_dir.join("writer-done");
        while reads < 100 || !writer_done.exists() {
            assert!(Instant::now() < deadline, "concurrent WAL phase timed out");
            // Each connection constructs its own WAL coordination view while
            // a different process keeps committing frames to the same index.
            let failure_count = {
                let conn = db.connect().unwrap();
                let mut rows = conn
                    .query("SELECT failure_count FROM projects", ())
                    .await
                    .unwrap();
                rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
            };
            if reads == 0 {
                assert_eq!(failure_count, SEED_WRITES as i64 - 1);
                fs::write(state_dir.join("reader-ready"), b"").unwrap();
            } else if !saw_write && failure_count == PROBE_WRITES as i64 - 1 {
                fs::write(state_dir.join("reader-saw-write"), b"").unwrap();
                saw_write = true;
                deadline = Instant::now() + PHASE_TIMEOUT;
            }
            reads += 1;
            // Leave a gap with no reader connection so repeated opens cannot
            // monopolize WAL coordination when the writer is slow to schedule.
            tokio::time::sleep(READ_INTERVAL).await;
        }
    });
}
