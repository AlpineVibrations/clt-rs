use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
};

use super::{REQUIRED_FILE, registered_store};
use crate::agent::TursoAgentStore;

const WAL_TAIL_CHILD_STATE: &str = "CLT_REGISTRY_WAL_TAIL_TEST_STATE";
const WAL_TAIL_CHILD_PROJECT: &str = "CLT_REGISTRY_WAL_TAIL_TEST_PROJECT";

fn uncommitted_wal_frame_after(wal: &[u8]) -> Vec<u8> {
    use turso::core::storage::sqlite3_ondisk::{WalHeader, checksum_wal};

    let read_u32 = |bytes: &[u8]| u32::from_be_bytes(bytes.try_into().unwrap());
    assert!(wal.len() > 32, "fixture has no WAL frames");
    let magic = read_u32(&wal[..4]);
    assert!(matches!(magic, 0x377f0682 | 0x377f0683));
    let page_size = read_u32(&wal[8..12]) as usize;
    let frame_size = 24 + page_size;
    assert_eq!((wal.len() - 32) % frame_size, 0);
    let committed = &wal[wal.len() - frame_size..];
    assert_ne!(
        read_u32(&committed[4..8]),
        0,
        "fixture tail is not committed"
    );
    let previous_checksum = (read_u32(&committed[16..20]), read_u32(&committed[20..24]));
    let mut frame = committed.to_vec();
    // Model a spilled but uncommitted update to the last committed page. A
    // correct WAL scan must ignore it even though its rolling checksum is valid.
    frame[4..8].copy_from_slice(&0u32.to_be_bytes());
    let last_byte = frame.len() - 1;
    frame[last_byte] ^= 0x01;
    let native_checksum = cfg!(target_endian = "big") == ((magic & 1) != 0);
    let header = WalHeader::default();
    let checksum = checksum_wal(&frame[..8], &header, previous_checksum, native_checksum);
    let checksum = checksum_wal(&frame[24..], &header, checksum, native_checksum);
    frame[16..20].copy_from_slice(&checksum.0.to_be_bytes());
    frame[20..24].copy_from_slice(&checksum.1.to_be_bytes());
    frame
}

#[test]
fn registry_reopen_preserves_committed_rows_after_uncommitted_or_partial_wal_tail() {
    let mut failures = Vec::new();
    for partial in [false, true] {
        let label = if partial {
            "registry-partial-wal-tail"
        } else {
            "registry-uncommitted-wal-tail"
        };
        let (root, state_dir, store, project) = registered_store(label);
        store
            .set_project_enabled_blocking(project.id, false)
            .unwrap();
        drop(store);
        assert!(!state_dir.join(REQUIRED_FILE).exists());
        let wal_path = state_dir.join("agent.db-wal");
        let mut wal = fs::read(&wal_path).unwrap();
        let frame = uncommitted_wal_frame_after(&wal);
        if partial {
            // A power loss during the page payload leaves a valid frame header
            // followed by fewer bytes than the WAL's declared page size.
            wal.extend_from_slice(&frame[..frame.len() / 2]);
        } else {
            wal.extend_from_slice(&frame);
        }
        fs::write(&wal_path, wal).unwrap();
        File::open(&wal_path).unwrap().sync_all().unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent::recovery::tests::wal_tail_tests::registry_wal_tail_reader_child",
                "--nocapture",
            ])
            .env(WAL_TAIL_CHILD_STATE, &state_dir)
            .env(WAL_TAIL_CHILD_PROJECT, &project.path)
            .output()
            .unwrap();
        if !child.status.success() {
            failures.push(format!(
                "{label} failed; isolated fixture retained at {}: status={}\nstdout={}\nstderr={}",
                root.display(),
                child.status,
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr),
            ));
        } else {
            assert!(!state_dir.join(REQUIRED_FILE).exists());
            fs::remove_dir_all(root).unwrap();
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn registry_wal_tail_reader_child() {
    let Some(state_dir) = std::env::var_os(WAL_TAIL_CHILD_STATE) else {
        return;
    };
    let state_dir = Path::new(&state_dir);
    let expected_project = PathBuf::from(std::env::var_os(WAL_TAIL_CHILD_PROJECT).unwrap());
    for _ in 0..3 {
        let store = TursoAgentStore::open_blocking(state_dir).unwrap();
        let projects = store.list_projects_blocking().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, expected_project);
        assert!(!projects[0].enabled);
        drop(store);
        assert!(
            !state_dir.join(REQUIRED_FILE).exists(),
            "reader teardown required recovery"
        );
    }
}
