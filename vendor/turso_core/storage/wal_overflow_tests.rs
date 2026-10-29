use super::*;
use crate::{Connection, Database, DatabaseOpts, OpenFlags};

fn test_io() -> Arc<dyn IO> {
    #[cfg(all(target_os = "windows", clt_turso_feature = "experimental_win_iocp"))]
    {
        Arc::new(crate::WindowsIOCP::new().unwrap())
    }
    #[cfg(not(all(target_os = "windows", clt_turso_feature = "experimental_win_iocp")))]
    {
        Arc::new(crate::io::PlatformIO::new().unwrap())
    }
}

fn open(path: &std::path::Path) -> Arc<Database> {
    Database::open_file_with_flags(
        test_io(),
        path.to_str().unwrap(),
        OpenFlags::default(),
        DatabaseOpts::new().with_multiprocess_wal(true),
        None,
    )
    .unwrap()
}

/// Use an ordinary disk scan, then force the index's capacity flag so the
/// production overflow path can be exercised without a gigabyte-sized WAL.
fn scanned_overflow_database() -> (tempfile::TempDir, Arc<Database>, Arc<Connection>) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let path = directory.path().join("scanned.db");
    {
        let db = open(&source);
        let conn = db.connect().unwrap();
        conn.wal_auto_actions_disable();
        conn.execute("CREATE TABLE entries (id INTEGER PRIMARY KEY, value BLOB)")
            .unwrap();
        conn.execute("INSERT INTO entries VALUES (1, zeroblob(32))")
            .unwrap();
        // Copy a quiescent committed bundle without a coordination map. The
        // reopened database must derive its frame-cache proof from disk.
        std::fs::copy(&source, &path).unwrap();
        std::fs::copy(
            directory.path().join("source.db-wal"),
            directory.path().join("scanned.db-wal"),
        )
        .unwrap();
    }
    let db = open(&path);
    let conn = db.connect().unwrap();
    conn.wal_auto_actions_disable();
    let authority = db.shared_wal_coordination().unwrap().unwrap();
    let snapshot = authority.snapshot();
    assert!(db
        .shared_wal
        .read()
        .runtime
        .overflow_fallback_coverage
        .lock()
        .covers(snapshot, snapshot.max_frame));
    authority.mark_frame_index_overflowed_for_tests();
    (directory, db, conn)
}

fn assert_covered(db: &Database) {
    let snapshot = db.shared_wal_coordination().unwrap().unwrap().snapshot();
    assert!(
        db.shared_wal
            .read()
            .runtime
            .overflow_fallback_coverage
            .lock()
            .covers(snapshot, snapshot.max_frame),
        "local WAL operations lost complete coverage of the committed snapshot"
    );
}

fn rows(conn: &Arc<Connection>, sql: &str) -> Vec<Vec<crate::Value>> {
    let mut result = Vec::new();
    conn.query(sql)
        .unwrap()
        .unwrap()
        .run_with_row_callback(|row| {
            result.push(row.get_values().cloned().collect());
            Ok(())
        })
        .unwrap();
    result
}

#[test]
fn scanned_overflow_survives_multi_page_commit_and_checkpoint() {
    let (_directory, db, conn) = scanned_overflow_database();
    // Registry opens run no-op migration transactions before pinning a fresh
    // reader; newer schemas can then append multiple pages in one commit.
    conn.execute("BEGIN IMMEDIATE; SELECT id FROM entries; COMMIT")
        .unwrap();
    assert_covered(&db);
    let before = db.shared_wal_coordination().unwrap().unwrap().snapshot();
    conn.execute("INSERT INTO entries VALUES (2, zeroblob(20000))")
        .unwrap();
    let after = db.shared_wal_coordination().unwrap().unwrap().snapshot();
    assert!(after.max_frame > before.max_frame + 1);
    assert_covered(&db);
    let reader = db.connect().unwrap();
    assert_eq!(
        rows(&reader, "SELECT id, length(value) FROM entries ORDER BY id"),
        vec![
            vec![crate::Value::from_i64(1), crate::Value::from_i64(32)],
            vec![crate::Value::from_i64(2), crate::Value::from_i64(20000)],
        ]
    );
    assert_eq!(
        rows(&reader, "PRAGMA integrity_check"),
        vec![vec![crate::Value::build_text("ok")]]
    );
    reader
        .checkpoint(crate::CheckpointMode::Truncate {
            upper_bound_inclusive: None,
        })
        .unwrap();
    assert_eq!(
        rows(&reader, "SELECT count(*) FROM entries"),
        vec![vec![crate::Value::from_i64(2)]]
    );
}

#[test]
fn scanned_overflow_preserves_committed_prefix_after_rollback() {
    let (_directory, db, conn) = scanned_overflow_database();
    conn.execute("BEGIN IMMEDIATE; INSERT INTO entries VALUES (2, zeroblob(20000)); ROLLBACK")
        .unwrap();
    assert_covered(&db);
    let reader = db.connect().unwrap();
    assert_eq!(
        rows(&reader, "SELECT count(*) FROM entries"),
        vec![vec![crate::Value::from_i64(1)]]
    );
}

#[test]
fn scanned_overflow_reads_spilled_pages_and_preserves_rollback_prefix() {
    let (_directory, db, conn) = scanned_overflow_database();
    conn.execute("BEGIN IMMEDIATE").unwrap();
    let committed = db.shared_wal_coordination().unwrap().unwrap().snapshot();
    conn.execute("INSERT INTO entries VALUES (2, zeroblob(300000))")
        .unwrap();
    // Explicitly flush dirty pages without committing, so this regression
    // does not depend on the cache's evolving automatic spill thresholds.
    for completion in conn.cacheflush().unwrap() {
        db.io.wait_for_completion(completion).unwrap();
    }
    assert!(
        conn.pager.load().wal.as_ref().unwrap().get_max_frame() > committed.max_frame,
        "the flush must append uncommitted pages to the WAL"
    );
    assert_eq!(
        db.shared_wal_coordination()
            .unwrap()
            .unwrap()
            .snapshot()
            .max_frame,
        committed.max_frame
    );
    // Read an overflow page directly from the uncommitted WAL, bypassing
    // cached dirty pages which would otherwise mask a missing coverage proof.
    let (_page, completion) = conn
        .pager
        .load()
        .read_page_no_cache(3, None, false)
        .unwrap();
    db.io.wait_for_completion(completion).unwrap();
    assert_eq!(
        rows(
            &conn,
            "SELECT hex(substr(value, 200000, 4)) FROM entries WHERE id = 2"
        ),
        vec![vec![crate::Value::build_text("00000000")]]
    );
    conn.execute("ROLLBACK").unwrap();
    assert_covered(&db);
    let reader = db.connect().unwrap();
    assert_eq!(
        rows(&reader, "SELECT count(*) FROM entries"),
        vec![vec![crate::Value::from_i64(1)]]
    );
}

fn coordination_fixture() -> (tempfile::TempDir, ShmWalCoordination) {
    let directory = tempfile::tempdir().unwrap();
    let authority = Arc::new(
        MappedSharedWalCoordination::create_or_open(
            &test_io(),
            &directory.path().join("coverage.db-tshm"),
            64,
        )
        .unwrap(),
    );
    let shared = WalFileShared::new_noop();
    let coordination = ShmWalCoordination::new(shared, authority);
    let snapshot = SharedWalCoordinationHeader {
        max_frame: 3,
        checkpoint_seq: 7,
        page_size: 4096,
        salt_1: 17,
        salt_2: 23,
        ..coordination.authority.snapshot()
    };
    coordination.authority.install_snapshot(snapshot);
    coordination.sync_local_from_authority(snapshot);
    for frame in 1..=3 {
        coordination.cache_frame(frame, frame);
    }
    coordination
        .shared
        .read()
        .runtime
        .overflow_fallback_coverage
        .lock()
        .record_snapshot(snapshot, 3);
    coordination
        .authority
        .mark_frame_index_overflowed_for_tests();
    (directory, coordination)
}

#[test]
fn overflow_coverage_rejects_foreign_gaps_and_other_generations() {
    for changed in ["peer frames", "checkpoint", "salt1", "salt2", "no proof"] {
        let (_directory, coordination) = coordination_fixture();
        let mut snapshot = coordination.authority.snapshot();
        let next = match changed {
            "peer frames" => {
                snapshot.max_frame = 5;
                6
            }
            "checkpoint" => {
                snapshot.checkpoint_seq += 1;
                4
            }
            "salt1" => {
                snapshot.salt_1 += 1;
                4
            }
            "salt2" => {
                snapshot.salt_2 += 1;
                4
            }
            "no proof" => {
                coordination
                    .shared
                    .read()
                    .runtime
                    .overflow_fallback_coverage
                    .lock()
                    .clear();
                4
            }
            _ => unreachable!(),
        };
        coordination.authority.install_snapshot(snapshot);
        coordination.cache_frame(99, next);
        coordination.publish_commit(WalCommitState {
            max_frame: next,
            last_checksum: (31, 37),
            transaction_count: 1,
        });
        coordination.rollback_cache(next);
        assert!(
            matches!(
                coordination.ensure_local_frame_cache_covers_snapshot(
                    &test_io(),
                    coordination.load_snapshot(),
                ),
                Err(LimboError::Busy)
            ),
            "{changed} must not be promoted into proven fallback coverage"
        );
    }
}

#[test]
fn overflow_coverage_truncates_reused_slots_and_rolled_back_suffixes() {
    let (_directory, coordination) = coordination_fixture();
    for frame in 4..=6 {
        coordination.cache_frame(frame, frame);
    }
    let snapshot = coordination.authority.snapshot();
    assert!(coordination.overflow_fallback_covers(snapshot, 6));
    // The shared append position rewound, while this process still caches the
    // old uncommitted suffix. Reusing its first slot must trim local proof.
    coordination.authority.rollback_frames(3);
    coordination.cache_frame(99, 4);
    assert!(coordination.overflow_fallback_covers(snapshot, 4));
    assert!(!coordination.overflow_fallback_covers(snapshot, 5));
    assert_eq!(coordination.fallback.find_frame(5, 0, 6, None), None);
    assert_eq!(coordination.fallback.find_frame(99, 0, 6, None), Some(4));
    coordination.rollback_cache(3);
    assert!(coordination.overflow_fallback_covers(snapshot, 3));
    assert!(!coordination.overflow_fallback_covers(snapshot, 4));
    coordination.rollback_cache(9);
    assert!(!coordination.overflow_fallback_covers(snapshot, 4));
}
