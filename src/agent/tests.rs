use crate::test_support::prelude::*;
use crate::test_support::*;

#[test]
fn agent_state_dir_uses_explicit_override() {
    let override_dir = PathBuf::from("/tmp/custom-clt-state");

    let state_dir = resolve_agent_state_dir(
        AgentPlatform::Linux,
        Some(override_dir.clone()),
        Some(PathBuf::from("/tmp/xdg-state")),
        Some(PathBuf::from("/home/alex")),
    )
    .unwrap();

    assert_eq!(state_dir, override_dir);
}

#[test]
fn unit_test_agent_state_dir_is_isolated_from_user_defaults() {
    let state_dir = agent_state_dir().unwrap();

    assert_eq!(state_dir, isolated_unit_test_agent_state_dir());
    assert!(state_dir.starts_with(std::env::temp_dir()));
    assert_ne!(
        state_dir,
        resolve_agent_state_dir(
            AgentPlatform::Linux,
            None,
            None,
            Some(PathBuf::from("/home/alex")),
        )
        .unwrap()
    );
    assert_ne!(
        state_dir,
        resolve_agent_state_dir(
            AgentPlatform::Macos,
            None,
            None,
            Some(PathBuf::from("/Users/alex")),
        )
        .unwrap()
    );
}

#[test]
fn agent_state_dir_uses_macos_application_support() {
    let state_dir = resolve_agent_state_dir(
        AgentPlatform::Macos,
        None,
        None,
        Some(PathBuf::from("/Users/alex")),
    )
    .unwrap();

    assert_eq!(
        state_dir,
        PathBuf::from("/Users/alex/Library/Application Support/clt")
    );
}

#[test]
fn agent_state_dir_uses_xdg_state_home_on_linux() {
    let state_dir = resolve_agent_state_dir(
        AgentPlatform::Linux,
        None,
        Some(PathBuf::from("/var/state/alex")),
        Some(PathBuf::from("/home/alex")),
    )
    .unwrap();

    assert_eq!(state_dir, PathBuf::from("/var/state/alex/clt"));
}

#[test]
fn agent_state_dir_uses_local_state_fallback_on_linux() {
    let state_dir = resolve_agent_state_dir(
        AgentPlatform::Linux,
        None,
        None,
        Some(PathBuf::from("/home/alex")),
    )
    .unwrap();

    assert_eq!(state_dir, PathBuf::from("/home/alex/.local/state/clt"));
}

#[test]
fn agent_timeout_duration_requires_positive_integer() {
    assert_eq!(
        parse_agent_timeout_duration(AGENT_LEASE_TIMEOUT_SECONDS_ENV, "90").unwrap(),
        90
    );
    assert!(parse_agent_timeout_duration(AGENT_LEASE_TIMEOUT_SECONDS_ENV, "0").is_err());
    assert!(parse_agent_timeout_duration(AGENT_LEASE_TIMEOUT_SECONDS_ENV, "soon").is_err());
}

#[test]
fn automated_agent_lease_renewal_interval_is_bounded_and_frequent() {
    assert_eq!(
        agent_lease_renew_interval(Duration::from_secs(90)),
        Duration::from_millis(AGENT_LEASE_RENEW_MAX_INTERVAL_MILLIS)
    );
    assert_eq!(
        agent_lease_renew_interval(Duration::from_secs(3)),
        Duration::from_secs(1)
    );
    assert_eq!(
        agent_lease_renew_interval(Duration::from_millis(300)),
        Duration::from_millis(250)
    );
}

#[test]
fn agent_timestamp_display_is_human_readable_but_preserves_invalid_values() {
    let formatted = format_agent_timestamp("1700000000");

    assert_ne!(formatted, "1700000000");
    assert!(formatted.contains('-'));
    assert!(formatted.contains(':'));
    assert_eq!(format_agent_timestamp("not-a-timestamp"), "not-a-timestamp");
    assert_eq!(format_optional_agent_timestamp(None), "-");
}

#[test]
fn agent_project_summary_formats_readable_block() {
    let project = agent::AgentProject {
        id: 7,
        path: PathBuf::from("/tmp/demo-project"),
        name: "demo-project".to_string(),
        enabled: true,
        git_mode: AgentGitMode::Off,
        codex_provider: None,
        codex_model: None,
        codex_reasoning_effort: None,
        codex_fast_enabled: false,
        last_scan_at: None,
        last_daemon_scan_status: None,
        last_daemon_scan_error: None,
        last_run_at: Some("last-run".to_string()),
        last_success_at: Some("last-success".to_string()),
        last_failure_at: None,
        last_blocked_recovery_at: None,
        failure_count: 3,
    };
    let scan = AgentProjectScan::pending(2);

    let summary = format_agent_project_summary(&project, &scan, Some("last-scan"));

    let expected = [
            "7. demo-project [enabled]",
            "   queue     pending: yes todo: 2   todo-ready: 2   todo-blocked: 0   doing: 0   doing-blocked: 0   scan: pending",
            "   activity  last scan: last-scan",
            "             last run:  last-run",
            "             success:   last-success",
            "             failure:   -",
            "             blocked:   -",
            "   settings  git: off",
            "             target: CLT default  reasoning: default  fast: disabled",
            "   health    failures: 3",
            "   path      /tmp/demo-project",
        ]
        .join("\n");
    assert_eq!(summary, expected);
    assert!(!summary.contains("last_scan="));
    assert!(!summary.contains("path="));
}

#[test]
fn agent_poll_interval_duration_requires_positive_integer() {
    assert_eq!(AGENT_DEFAULT_POLL_INTERVAL_SECONDS, 15);
    assert_eq!(
        parse_agent_timeout_duration(AGENT_POLL_INTERVAL_SECONDS_ENV, "15").unwrap(),
        15
    );
    assert!(parse_agent_timeout_duration(AGENT_POLL_INTERVAL_SECONDS_ENV, "0").is_err());
    assert!(parse_agent_timeout_duration(AGENT_POLL_INTERVAL_SECONDS_ENV, "soon").is_err());
}

#[test]
fn agent_daemon_rebuilds_a_damaged_active_worker_index_once_and_retries() {
    let operation_calls = Cell::new(0);
    let rebuild_calls = Cell::new(0);

    let result = run_agent_daemon_database_operation_with_recovery(
            || {
                operation_calls.set(operation_calls.get() + 1);
                if operation_calls.get() == 1 {
                    anyhow::bail!(
                        "Failed to abandon worker token: IdxDelete: no matching index entry found for key"
                    );
                }
                Ok("recovered")
            },
            || {
                rebuild_calls.set(rebuild_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(result, "recovered");
    assert_eq!(operation_calls.get(), 2);
    assert_eq!(rebuild_calls.get(), 1);
}

#[test]
fn agent_daemon_does_not_rebuild_indexes_for_unrelated_errors() {
    let rebuild_calls = Cell::new(0);

    let result: Result<()> = run_agent_daemon_database_operation_with_recovery(
        || anyhow::bail!("Failed to scan project"),
        || {
            rebuild_calls.set(rebuild_calls.get() + 1);
            Ok(())
        },
    );
    let error = result.unwrap_err();

    assert!(error.to_string().contains("Failed to scan project"));
    assert_eq!(rebuild_calls.get(), 0);
}

#[test]
fn agent_max_global_jobs_defaults_to_twelve_and_requires_positive_integer() {
    assert_eq!(AGENT_DEFAULT_MAX_GLOBAL_JOBS, 12);
    assert_eq!(
        parse_agent_positive_usize(AGENT_MAX_GLOBAL_JOBS_ENV, "12").unwrap(),
        12
    );
    assert!(parse_agent_positive_usize(AGENT_MAX_GLOBAL_JOBS_ENV, "0").is_err());
    assert!(parse_agent_positive_usize(AGENT_MAX_GLOBAL_JOBS_ENV, "many").is_err());
}

#[test]
fn agent_bool_env_accepts_common_true_and_false_values() {
    assert!(parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "1").unwrap());
    assert!(parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "true").unwrap());
    assert!(parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "YES").unwrap());
    assert!(!parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "0").unwrap());
    assert!(!parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "false").unwrap());
    assert!(!parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "off").unwrap());
    assert!(parse_agent_bool(AGENT_HEARTBEAT_TAIL_ENV, "sometimes").is_err());
}

#[test]
fn agent_daemon_uses_short_poll_when_no_projects_are_enabled() {
    let empty_pass = AgentSchedulerPass {
        scanned_projects: 0,
        pending_projects: 0,
        active_agent_jobs: 0,
        skipped_active_lease: 0,
        deferred_projects: 0,
        runs_started: 0,
        runs_recorded: 0,
    };
    let active_pass = AgentSchedulerPass {
        scanned_projects: 1,
        pending_projects: 0,
        active_agent_jobs: 0,
        skipped_active_lease: 0,
        deferred_projects: 0,
        runs_started: 0,
        runs_recorded: 0,
    };

    assert_eq!(
        agent_daemon_sleep_interval(
            &empty_pass,
            Duration::from_secs(AGENT_DEFAULT_POLL_INTERVAL_SECONDS)
        ),
        Duration::from_secs(AGENT_EMPTY_REGISTRY_POLL_INTERVAL_SECONDS)
    );
    assert_eq!(
        agent_daemon_sleep_interval(&empty_pass, Duration::from_secs(2)),
        Duration::from_secs(2)
    );
    assert_eq!(
        agent_daemon_sleep_interval(
            &active_pass,
            Duration::from_secs(AGENT_DEFAULT_POLL_INTERVAL_SECONDS)
        ),
        Duration::from_secs(AGENT_DEFAULT_POLL_INTERVAL_SECONDS)
    );
}

#[test]
fn agent_store_initializes_database_and_tables() {
    let root = temp_root("agent-store");
    let state_dir = root.join("state/clt");

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(store.db_path(), state_dir.join(AGENT_DB_FILE));
    assert!(store.db_path().is_file());
    for table in [
        "schema_migrations",
        "projects",
        "runs",
        "leases",
        "daemon_checkins",
        "model_providers",
        "model_targets",
        "agent_settings",
        "agent_workers",
        "git_finalizations",
    ] {
        assert!(
            store.table_exists_blocking(table).unwrap(),
            "missing table {table}"
        );
    }
    assert!(!store.runs_has_task_content_column_blocking().unwrap());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_rebuilds_the_derived_active_worker_index() {
    let root = temp_root("agent-store-rebuild-active-worker-index");
    let state_dir = root.join("state/clt");
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert!(store.active_worker_project_index_exists_blocking().unwrap());
    store.drop_active_worker_project_index_blocking().unwrap();
    assert!(!store.active_worker_project_index_exists_blocking().unwrap());

    store
        .rebuild_active_worker_project_index_blocking()
        .unwrap();

    assert!(store.active_worker_project_index_exists_blocking().unwrap());
    fs::remove_dir_all(root).unwrap();
}

const AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV: &str = "CLT_TEST_AGENT_STORE_MULTIPROCESS_STATE_DIR";
const AGENT_STORE_MULTIPROCESS_GATE_ENV: &str = "CLT_TEST_AGENT_STORE_MULTIPROCESS_GATE";
const AGENT_STORE_MULTIPROCESS_READY_ENV: &str = "CLT_TEST_AGENT_STORE_MULTIPROCESS_READY";
const AGENT_STORE_MULTIPROCESS_HOLD_GATE_ENV: &str = "CLT_TEST_AGENT_STORE_MULTIPROCESS_HOLD_GATE";
const AGENT_STORE_MULTIPROCESS_HOLD_READY_ENV: &str =
    "CLT_TEST_AGENT_STORE_MULTIPROCESS_HOLD_READY";

#[cfg(unix)]
#[test]
fn agent_store_marks_a_versioned_turso_frame_index_for_rebuild() {
    let root = temp_root("agent-store-shared-wal-rebuild-bit");
    fs::create_dir_all(&root).unwrap();
    let db_path = root.join(AGENT_DB_FILE);
    let shared_wal_path = agent::shared_wal_path(&db_path);
    let mut bytes = vec![0u8; 4096];
    bytes[..TURSO_SHARED_WAL_MAGIC.len()].copy_from_slice(TURSO_SHARED_WAL_MAGIC);
    bytes[8..12].copy_from_slice(&TURSO_SHARED_WAL_VERSION.to_le_bytes());
    fs::write(&shared_wal_path, bytes).unwrap();

    assert!(agent::request_shared_wal_index_rebuild(&db_path).unwrap());
    let bytes = fs::read(&shared_wal_path).unwrap();
    assert_eq!(
        u32::from_le_bytes(
            bytes[TURSO_SHARED_WAL_INDEX_OVERFLOW_OFFSET
                ..TURSO_SHARED_WAL_INDEX_OVERFLOW_OFFSET + 4]
                .try_into()
                .unwrap()
        ),
        1
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_allows_a_second_process_to_open_the_database() {
    let root = temp_root("agent-store-multiprocess");
    let state_dir = root.join("state/clt");
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    let child_output = Command::new(std::env::current_exe().unwrap())
        .arg("agent::tests::agent_store_multiprocess_child_opens_database")
        .arg("--exact")
        .arg("--nocapture")
        .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
        .output()
        .unwrap();

    assert!(
        child_output.status.success(),
        "second process failed to open agent store: stdout={}; stderr={}",
        String::from_utf8_lossy(&child_output.stdout),
        String::from_utf8_lossy(&child_output.stderr)
    );
    assert!(store.table_exists_blocking("projects").unwrap());

    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn agent_store_long_lived_peer_pins_the_wal_before_auto_checkpoint_restart() {
    const WAL_HEADER_BYTES: usize = 32;
    const WAL_FRAME_HEADER_BYTES: usize = 24;
    const CHECKPOINT_PRESSURE_WRITES: usize = 1_100;

    let root = temp_root("agent-store-checkpoint-pin");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "checkpoint-pin-project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap()[0].id;
    store
        .write_checkpoint_pressure_blocking(project_id, CHECKPOINT_PRESSURE_WRITES)
        .unwrap();

    let mut wal_path = store.db_path().as_os_str().to_os_string();
    wal_path.push("-wal");
    let wal = fs::read(PathBuf::from(wal_path)).unwrap();
    let page_size = u32::from_be_bytes(wal[8..12].try_into().unwrap()) as usize;
    let frame_count = (wal.len() - WAL_HEADER_BYTES) / (WAL_FRAME_HEADER_BYTES + page_size);
    assert!(
        frame_count > 1_000,
        "the WAL restarted despite the long-lived store's checkpoint pin: {frame_count} frames"
    );

    let child_output = Command::new(std::env::current_exe().unwrap())
        .arg("agent::tests::agent_store_multiprocess_child_opens_database")
        .arg("--exact")
        .arg("--nocapture")
        .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
        .output()
        .unwrap();
    assert!(
        child_output.status.success(),
        "second process failed after checkpoint pressure: stdout={}; stderr={}",
        String::from_utf8_lossy(&child_output.stdout),
        String::from_utf8_lossy(&child_output.stderr)
    );

    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
fn corrupt_shared_wal_page_one_mapping(db_path: &Path) {
    use std::os::unix::fs::FileExt;

    const WAL_HEADER_BYTES: usize = 32;
    const WAL_FRAME_HEADER_BYTES: usize = 24;
    const SHARED_WAL_BASE_BYTES: usize = 4096;
    const FRAME_INDEX_BLOCK_CAPACITY: usize = 4096;
    const FRAME_INDEX_HASH_SLOTS: usize = FRAME_INDEX_BLOCK_CAPACITY * 2;
    const FRAME_INDEX_ENTRY_BYTES: usize = 16;
    const FRAME_INDEX_ENTRY_REGION_BYTES: usize =
        FRAME_INDEX_BLOCK_CAPACITY * FRAME_INDEX_ENTRY_BYTES;
    const FRAME_INDEX_HASH_REGION_BYTES: usize = FRAME_INDEX_HASH_SLOTS * 2;
    const FRAME_INDEX_BLOCK_BYTES: usize =
        FRAME_INDEX_ENTRY_REGION_BYTES + FRAME_INDEX_HASH_REGION_BYTES;

    let mut wal_path = db_path.as_os_str().to_os_string();
    wal_path.push("-wal");
    let wal = fs::read(PathBuf::from(wal_path)).unwrap();
    let page_size = u32::from_be_bytes(wal[8..12].try_into().unwrap()) as usize;
    let frame_size = WAL_FRAME_HEADER_BYTES + page_size;
    let frame_count = (wal.len() - WAL_HEADER_BYTES) / frame_size;
    let target_frame = (0..frame_count)
        .rev()
        .find(|frame_index| {
            let frame_offset = WAL_HEADER_BYTES + frame_index * frame_size;
            let page_id =
                u32::from_be_bytes(wal[frame_offset..frame_offset + 4].try_into().unwrap());
            page_id != 1
                && wal[frame_offset + WAL_FRAME_HEADER_BYTES + 100] == 0
                && (*frame_index + 1..frame_count).all(|later_frame_index| {
                    let later_offset = WAL_HEADER_BYTES + later_frame_index * frame_size;
                    u32::from_be_bytes(wal[later_offset..later_offset + 4].try_into().unwrap()) != 1
                })
        })
        .map(|frame_index| frame_index as u64 + 1)
        .expect("test WAL needs a non-page-one tail frame with an invalid page-one header");

    let shared_wal_path = agent::shared_wal_path(db_path);
    let mut shared_wal = fs::read(&shared_wal_path).unwrap();
    let frame_index_blocks = u32::from_le_bytes(shared_wal[32..36].try_into().unwrap()) as usize;
    let frame_index_len = u32::from_le_bytes(shared_wal[40..44].try_into().unwrap()) as usize;
    let target_slot = (0..frame_index_len)
        .find(|slot| {
            let block = slot / FRAME_INDEX_BLOCK_CAPACITY;
            let local_slot = slot % FRAME_INDEX_BLOCK_CAPACITY;
            let entry_offset = SHARED_WAL_BASE_BYTES
                + block * FRAME_INDEX_BLOCK_BYTES
                + local_slot * FRAME_INDEX_ENTRY_BYTES;
            u64::from_le_bytes(
                shared_wal[entry_offset + 8..entry_offset + 16]
                    .try_into()
                    .unwrap(),
            ) == target_frame
        })
        .expect("test WAL frame must be represented in the shared index");
    let target_block = target_slot / FRAME_INDEX_BLOCK_CAPACITY;
    let target_local_slot = target_slot % FRAME_INDEX_BLOCK_CAPACITY;
    let target_entry_offset = SHARED_WAL_BASE_BYTES
        + target_block * FRAME_INDEX_BLOCK_BYTES
        + target_local_slot * FRAME_INDEX_ENTRY_BYTES;
    shared_wal[target_entry_offset..target_entry_offset + 8].copy_from_slice(&1u64.to_le_bytes());

    for block in 0..frame_index_blocks {
        let hash_offset = SHARED_WAL_BASE_BYTES
            + block * FRAME_INDEX_BLOCK_BYTES
            + FRAME_INDEX_ENTRY_REGION_BYTES;
        shared_wal[hash_offset..hash_offset + FRAME_INDEX_HASH_REGION_BYTES].fill(0);
    }
    for slot in 0..frame_index_len {
        let block = slot / FRAME_INDEX_BLOCK_CAPACITY;
        let local_slot = slot % FRAME_INDEX_BLOCK_CAPACITY;
        let block_offset = SHARED_WAL_BASE_BYTES + block * FRAME_INDEX_BLOCK_BYTES;
        let entry_offset = block_offset + local_slot * FRAME_INDEX_ENTRY_BYTES;
        let page_id = u64::from_le_bytes(
            shared_wal[entry_offset..entry_offset + 8]
                .try_into()
                .unwrap(),
        );
        let hash_offset = block_offset + FRAME_INDEX_ENTRY_REGION_BYTES;
        let mut hash_slot = page_id.wrapping_mul(383) as usize % FRAME_INDEX_HASH_SLOTS;
        loop {
            let value_offset = hash_offset + hash_slot * 2;
            if u16::from_le_bytes(
                shared_wal[value_offset..value_offset + 2]
                    .try_into()
                    .unwrap(),
            ) == 0
            {
                shared_wal[value_offset..value_offset + 2]
                    .copy_from_slice(&((local_slot + 1) as u16).to_le_bytes());
                break;
            }
            hash_slot = (hash_slot + 1) % FRAME_INDEX_HASH_SLOTS;
        }
    }
    shared_wal[TURSO_SHARED_WAL_INDEX_OVERFLOW_OFFSET..TURSO_SHARED_WAL_INDEX_OVERFLOW_OFFSET + 4]
        .copy_from_slice(&0u32.to_le_bytes());

    let file = fs::OpenOptions::new()
        .write(true)
        .open(&shared_wal_path)
        .unwrap();
    file.write_all_at(&shared_wal, 0).unwrap();
    file.sync_data().unwrap();
}

#[cfg(unix)]
#[test]
fn agent_store_defers_stale_index_recovery_until_the_live_peer_exits() {
    let root = temp_root("agent-store-stale-shared-wal-index");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    let recovery_marker = root.join("recovery-requested");
    let holder_ready = root.join("holder-ready");
    let holder_gate = root.join("holder-gate");
    fs::create_dir_all(&project_root).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "stale-index-project")
        .unwrap();

    let mut holder = Command::new(std::env::current_exe().unwrap())
        .arg("agent::tests::agent_store_multiprocess_child_opens_database")
        .arg("--exact")
        .arg("--nocapture")
        .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
        .env(AGENT_STORE_MULTIPROCESS_HOLD_READY_ENV, &holder_ready)
        .env(AGENT_STORE_MULTIPROCESS_HOLD_GATE_ENV, &holder_gate)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let holder_deadline = Instant::now() + Duration::from_secs(5);
    while !holder_ready.exists() && Instant::now() < holder_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !holder_ready.exists() {
        let _ = holder.kill();
        let holder_output = holder.wait_with_output().unwrap();
        panic!(
            "peer process did not open the agent store: stdout={}; stderr={}",
            String::from_utf8_lossy(&holder_output.stdout),
            String::from_utf8_lossy(&holder_output.stderr)
        );
    }
    corrupt_shared_wal_page_one_mapping(store.db_path());

    let blocked_output = Command::new(std::env::current_exe().unwrap())
        .arg("agent::tests::agent_store_multiprocess_child_opens_database")
        .arg("--exact")
        .arg("--nocapture")
        .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
        .env(TEST_AGENT_SHARED_WAL_REBUILD_MARKER_ENV, &recovery_marker)
        .output()
        .unwrap();

    assert!(
        !blocked_output.status.success(),
        "stale-index recovery must wait for the live peer: stdout={}; stderr={}",
        String::from_utf8_lossy(&blocked_output.stdout),
        String::from_utf8_lossy(&blocked_output.stderr)
    );
    assert!(
        !recovery_marker.exists(),
        "recovery was requested while a peer still held the Turso lifetime lock"
    );

    drop(store);
    holder.kill().unwrap();
    let _ = holder.wait_with_output().unwrap();

    let recovered_output = Command::new(std::env::current_exe().unwrap())
        .arg("agent::tests::agent_store_multiprocess_child_opens_database")
        .arg("--exact")
        .arg("--nocapture")
        .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
        .env(TEST_AGENT_SHARED_WAL_REBUILD_MARKER_ENV, &recovery_marker)
        .output()
        .unwrap();
    assert!(
        recovered_output.status.success(),
        "agent store did not recover after its peer exited: stdout={}; stderr={}",
        String::from_utf8_lossy(&recovered_output.stdout),
        String::from_utf8_lossy(&recovered_output.stderr)
    );
    assert!(recovery_marker.is_file());

    let recovered_store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert!(recovered_store.table_exists_blocking("projects").unwrap());
    drop(recovered_store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_multiprocess_child_opens_database() {
    let Some(state_dir) = std::env::var_os(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV) else {
        return;
    };

    if let Some(ready_path) = std::env::var_os(AGENT_STORE_MULTIPROCESS_READY_ENV) {
        fs::write(ready_path, b"ready").unwrap();
    }
    if let Some(gate_path) = std::env::var_os(AGENT_STORE_MULTIPROCESS_GATE_ENV) {
        let gate_path = PathBuf::from(gate_path);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !gate_path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for concurrent agent-store open gate"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    let store = agent::TursoAgentStore::open_blocking(Path::new(&state_dir)).unwrap();
    assert!(store.table_exists_blocking("projects").unwrap());
    if let Some(ready_path) = std::env::var_os(AGENT_STORE_MULTIPROCESS_HOLD_READY_ENV) {
        fs::write(ready_path, b"ready").unwrap();
    }
    if let Some(gate_path) = std::env::var_os(AGENT_STORE_MULTIPROCESS_HOLD_GATE_ENV) {
        let gate_path = PathBuf::from(gate_path);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !gate_path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for multiprocess hold gate"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn agent_store_concurrent_virgin_opens_apply_each_migration_once() {
    let root = temp_root("agent-store-concurrent-virgin-open");
    let state_dir = root.join("state/clt");
    let gate_path = root.join("open-gate");
    let first_ready_path = root.join("first-ready");
    let second_ready_path = root.join("second-ready");
    fs::create_dir_all(&root).unwrap();

    let spawn_open = |ready_path: &Path| {
        Command::new(std::env::current_exe().unwrap())
            .arg("agent::tests::agent_store_multiprocess_child_opens_database")
            .arg("--exact")
            .arg("--nocapture")
            .env(AGENT_STORE_MULTIPROCESS_STATE_DIR_ENV, &state_dir)
            .env(AGENT_STORE_MULTIPROCESS_GATE_ENV, &gate_path)
            .env(AGENT_STORE_MULTIPROCESS_READY_ENV, ready_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let mut first = spawn_open(&first_ready_path);
    let mut second = spawn_open(&second_ready_path);
    let ready_deadline = Instant::now() + Duration::from_secs(5);
    while !(first_ready_path.exists() && second_ready_path.exists())
        && Instant::now() < ready_deadline
    {
        thread::sleep(Duration::from_millis(10));
    }
    if !(first_ready_path.exists() && second_ready_path.exists()) {
        let _ = first.kill();
        let _ = second.kill();
        let _ = first.wait();
        let _ = second.wait();
        panic!("concurrent agent-store children did not reach their open barrier");
    }
    fs::write(&gate_path, b"open").unwrap();

    let first_output = first.wait_with_output().unwrap();
    let second_output = second.wait_with_output().unwrap();
    assert!(
        first_output.status.success(),
        "first virgin open failed: stdout={}; stderr={}",
        String::from_utf8_lossy(&first_output.stdout),
        String::from_utf8_lossy(&first_output.stderr)
    );
    assert!(
        second_output.status.success(),
        "second virgin open failed: stdout={}; stderr={}",
        String::from_utf8_lossy(&second_output.stdout),
        String::from_utf8_lossy(&second_output.stderr)
    );

    let db_path = state_dir.join(AGENT_DB_FILE);
    let migration_count = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        let mut rows = conn
            .query("SELECT COUNT(*) FROM schema_migrations", ())
            .await
            .unwrap();
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get_value(0)
            .unwrap()
            .as_integer()
            .copied()
            .unwrap()
    });
    assert_eq!(migration_count, 17);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_rolls_back_partial_migration_when_a_later_statement_fails() {
    let root = temp_root("agent-store-migration-rollback");
    let state_dir = root.join("state/clt");
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let db_path = store.db_path().to_path_buf();
    drop(store);

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute("DELETE FROM schema_migrations WHERE version = 13", ())
            .await
            .unwrap();
        conn.execute("ALTER TABLE session_controls DROP COLUMN run_token", ())
            .await
            .unwrap();
    });

    let migration_error = match agent::TursoAgentStore::open_blocking(&state_dir) {
        Ok(_) => panic!("migration unexpectedly succeeded with a duplicate stdout_path column"),
        Err(error) => error,
    };
    assert!(migration_error.to_string().contains("migration 13"));

    let (run_token_columns, stdout_path_columns, migration_rows) =
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let db = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
                    .build()
                    .await
                    .unwrap();
                let conn = db.connect().unwrap();
                let mut run_token_rows = conn
                    .query(
                        "SELECT COUNT(*) FROM pragma_table_info('session_controls') WHERE name = 'run_token'",
                        (),
                    )
                    .await
                    .unwrap();
                let run_token_columns = run_token_rows
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get_value(0)
                    .unwrap()
                    .as_integer()
                    .copied()
                    .unwrap();
                let mut stdout_path_rows = conn
                    .query(
                        "SELECT COUNT(*) FROM pragma_table_info('session_controls') WHERE name = 'stdout_path'",
                        (),
                    )
                    .await
                    .unwrap();
                let stdout_path_columns = stdout_path_rows
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get_value(0)
                    .unwrap()
                    .as_integer()
                    .copied()
                    .unwrap();
                let mut migration_rows = conn
                    .query(
                        "SELECT COUNT(*) FROM schema_migrations WHERE version = 13",
                        (),
                    )
                    .await
                    .unwrap();
                let migration_rows = migration_rows
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get_value(0)
                    .unwrap()
                    .as_integer()
                    .copied()
                    .unwrap();
                (run_token_columns, stdout_path_columns, migration_rows)
            });
    assert_eq!(run_token_columns, 0);
    assert_eq!(stdout_path_columns, 1);
    assert_eq!(migration_rows, 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_migrates_enabled_git_commits_to_commit_mode() {
    let root = temp_root("agent-store-git-mode-migration");
    let state_dir = root.join("state/clt");
    let db_path = state_dir.join(AGENT_DB_FILE);
    fs::create_dir_all(&state_dir).unwrap();

    tokio::runtime::Runtime::new().unwrap().block_on(async {
            let db = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
                .build()
                .await
                .unwrap();
            let conn = db.connect().unwrap();
            conn.execute(
                "CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                )",
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "CREATE TABLE projects (
                    id INTEGER PRIMARY KEY,
                    path TEXT NOT NULL UNIQUE,
                    name TEXT NOT NULL,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    registered_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    last_scan_at TEXT,
                    last_run_at TEXT,
                    last_success_at TEXT,
                    last_failure_at TEXT,
                    failure_count INTEGER NOT NULL DEFAULT 0,
                    git_commit_enabled INTEGER NOT NULL DEFAULT 0,
                    codex_model TEXT,
                    codex_reasoning_effort TEXT,
                    codex_fast_enabled INTEGER NOT NULL DEFAULT 0
                )",
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "CREATE TABLE runs (
                    id INTEGER PRIMARY KEY,
                    project_id INTEGER NOT NULL REFERENCES projects(id),
                    status TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    finished_at TEXT,
                    exit_code INTEGER,
                    log_dir TEXT,
                    stdout_path TEXT,
                    stderr_path TEXT,
                    summary TEXT
                )",
                (),
            )
            .await
            .unwrap();
            for version in 1..=4 {
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at)
                     VALUES (?1, datetime('now'))",
                    [version],
                )
                .await
                .unwrap();
            }
            conn.execute(
                "INSERT INTO projects (
                    path, name, registered_at, updated_at, git_commit_enabled
                 ) VALUES ('/tmp/legacy-project', 'legacy-project', datetime('now'), datetime('now'), 1)",
                (),
            )
            .await
            .unwrap();
        });

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);

    assert_eq!(project.git_mode, AgentGitMode::Commit);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_register_is_idempotent_and_lists_projects() {
    let root = temp_root("agent-register");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert!(
        store
            .register_project_blocking(&project_root, "project")
            .unwrap()
    );
    assert!(
        !store
            .register_project_blocking(&project_root, "renamed")
            .unwrap()
    );

    let projects = store.list_projects_blocking().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].path, project_root);
    assert_eq!(projects[0].name, "renamed");
    assert!(projects[0].enabled);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_register_without_path_uses_default_project_root() {
    let root = temp_root("agent-register-default-root");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = open_agent_store_at(&state_dir).unwrap();

    register_agent_project(&store, None, false, &project_root).unwrap();

    let projects = store.list_projects_blocking().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].path, project_root);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_unregister_removes_registered_project() {
    let root = temp_root("agent-unregister");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    store
        .register_project_blocking(&project_root, "project")
        .unwrap();

    assert!(store.unregister_project_blocking(&project_root).unwrap());
    assert!(!store.unregister_project_blocking(&project_root).unwrap());
    assert!(store.list_projects_blocking().unwrap().is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_unregister_and_clean_preserve_an_unconsumed_git_launch_boundary() {
    let root = temp_root("agent-unregister-unconsumed-git-launch");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    initialize_test_git_repository(&project_root);
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let launch = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    store
        .record_git_launch_state_blocking(
            project.id,
            "orphaned-release",
            AgentGitMode::Commit,
            &launch,
            "100",
        )
        .unwrap();

    let unregister_error = store
        .unregister_project_blocking(&project_root)
        .unwrap_err();
    assert!(format!("{unregister_error:#}").contains("launch boundary"));
    let clean_error = store.clean_agent_history_blocking("200").unwrap_err();
    assert!(format!("{clean_error:#}").contains("launch boundary"));
    assert_eq!(store.list_projects_blocking().unwrap().len(), 1);
    assert_eq!(
        store
            .git_launch_state_blocking(project.id, "orphaned-release")
            .unwrap(),
        Some((AgentGitMode::Commit, launch))
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_unregister_preserves_push_pending_finalization() {
    let root = temp_root("agent-unregister-push-pending");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .mark_session_running_blocking(
            project.id,
            "session-pending-unregister",
            123,
            "run-pending-unregister",
            &root.join("pending.out"),
            &root.join("pending.err"),
        )
        .unwrap();
    assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "session-pending-unregister",
                    git_mode: AgentGitMode::CommitAndPush,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/master"),
                    upstream_ref: Some("refs/remotes/origin/master"),
                    worktree_baseline: r#"{"version":1,"tracked_patch_ids":{},"untracked_blob_ids":{},"require_clean":false}"#,
                    task_identity: Some("pending task"),
                    owner_run_token: Some("run-pending-unregister"),
                    created_at: "100",
                })
                .unwrap()
        );
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-pending-unregister",
                0,
                GitFinalizationState::Tracking,
                Some("run-pending-unregister"),
                None,
                None,
                "101",
            )
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-pending-unregister",
                1,
                GitFinalizationState::CommitPending,
                Some("run-pending-unregister"),
                None,
                None,
                "102",
            )
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_git_finalization_blocking(
                project.id,
                "session-pending-unregister",
                2,
                GitFinalizationState::PushPending,
                Some("run-pending-unregister"),
                Some("2222222222222222222222222222222222222222"),
                None,
                "103",
            )
            .unwrap()
    );

    let error = store
        .unregister_project_blocking(&project_root)
        .unwrap_err();
    assert!(format!("{error:#}").contains("nonterminal"));
    assert_eq!(
        store
            .git_finalization_blocking(project.id, "session-pending-unregister")
            .unwrap()
            .unwrap()
            .state,
        GitFinalizationState::PushPending
    );

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn agent_store_unregister_reclaims_dead_process_lease() {
    let root = temp_root("agent-unregister-dead-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "clt-agent-4294967295", "100", "9999999999",)
            .unwrap()
    );

    assert!(store.unregister_project_blocking(&project_root).unwrap());
    assert!(store.list_projects_blocking().unwrap().is_empty());
    assert_eq!(store.lease_count_blocking().unwrap(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_unregister_reclaims_expired_lease() {
    let root = temp_root("agent-unregister-expired-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "unknown-holder", "100", "101")
            .unwrap()
    );

    assert!(store.unregister_project_blocking(&project_root).unwrap());
    assert!(store.list_projects_blocking().unwrap().is_empty());
    assert_eq!(store.lease_count_blocking().unwrap(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_unregister_rejects_live_or_unknown_lease() {
    let root = temp_root("agent-unregister-active-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "unknown-holder", "100", "9999999999",)
            .unwrap()
    );

    let error = store
        .unregister_project_blocking(&project_root)
        .unwrap_err();
    assert!(error.to_string().contains("agent lease is active"));
    assert_eq!(store.list_projects_blocking().unwrap().len(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_unregister_removes_project_run_history() {
    let root = temp_root("agent-unregister-run-history");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .record_run_outcome_blocking(agent::AgentRunOutcome {
            project_id: project.id,
            status: "failed",
            started_at: "100",
            finished_at: Some("101"),
            exit_code: Some(1),
            log_dir: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some("Codex rejected an untrusted directory"),
            codex_session_id: Some("session-123"),
        })
        .unwrap();
    assert_eq!(store.run_count_blocking().unwrap(), 1);

    assert!(store.unregister_project_blocking(&project_root).unwrap());
    assert!(store.list_projects_blocking().unwrap().is_empty());
    assert_eq!(store.run_count_blocking().unwrap(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_unregister_cascades_session_controls_on_its_fresh_connection() {
    let root = temp_root("agent-unregister-session-control-cascade");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .set_session_control_state_blocking(
            project_id,
            "session-123",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    assert!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .is_some()
    );

    assert!(store.unregister_project_blocking(&project_root).unwrap());
    assert!(
        store
            .session_control_blocking(project_id, "session-123")
            .unwrap()
            .is_none()
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_can_pause_and_resume_registered_project() {
    let root = temp_root("agent-pause-resume");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    store
        .register_project_blocking(&project_root, "project")
        .unwrap();

    assert!(
        store
            .set_project_enabled_for_path_blocking(&project_root, false)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(!project.enabled);

    assert!(
        store
            .set_project_enabled_blocking(project.id, true)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(project.enabled);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_immediate_retry_clears_only_the_selected_project_failure_count() {
    let root = temp_root("agent-immediate-retry");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap()[0].id;
    store
        .record_run_outcome_blocking(agent::AgentRunOutcome {
            project_id,
            status: "failure",
            started_at: "100",
            finished_at: Some("101"),
            exit_code: Some(1),
            log_dir: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some("failed"),
            codex_session_id: None,
        })
        .unwrap();
    assert_eq!(store.list_projects_blocking().unwrap()[0].failure_count, 1);
    assert!(
        store
            .try_acquire_lease_blocking(project_id, "expired-holder", "98", "99")
            .unwrap()
    );

    assert!(
        store
            .clear_project_failure_backoff_for_path_blocking(&project_root)
            .unwrap()
    );
    assert_eq!(store.list_projects_blocking().unwrap()[0].failure_count, 0);
    assert_eq!(store.run_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_persists_all_git_modes_for_registered_project() {
    let root = temp_root("agent-git-commit");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.git_mode, AgentGitMode::Off);

    assert!(
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.git_mode, AgentGitMode::Commit);

    assert!(
        store
            .set_project_git_mode_blocking(project.id, AgentGitMode::CommitAndPush)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.git_mode, AgentGitMode::CommitAndPush);

    assert!(
        store
            .set_project_git_mode_blocking(project.id, AgentGitMode::Off)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.git_mode, AgentGitMode::Off);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_persists_per_project_codex_settings() {
    let root = temp_root("agent-codex-settings");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.codex_model, None);
    assert_eq!(project.codex_reasoning_effort, None);
    assert!(!project.codex_fast_enabled);

    assert!(
        store
            .set_project_codex_settings_blocking(
                project.id,
                Some("openai"),
                Some("gpt-5.6-terra"),
                Some("high"),
                true,
            )
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.codex_provider.as_deref(), Some("openai"));
    assert_eq!(project.codex_model.as_deref(), Some("gpt-5.6-terra"));
    assert_eq!(project.codex_reasoning_effort.as_deref(), Some("high"));
    assert!(project.codex_fast_enabled);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_persists_provider_catalog_favorites_and_clt_default() {
    let root = temp_root("agent-model-catalog");
    let state_dir = root.join("state/clt");
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    let providers = store.list_model_providers_blocking().unwrap();
    assert_eq!(providers[0].id, "openai");
    assert!(providers[0].enabled);
    let openai_models = store.list_model_targets_blocking(Some("openai")).unwrap();
    assert!(
        openai_models
            .iter()
            .any(|model| { model.model_id == "gpt-5.6-sol" && model.enabled && model.favorite })
    );
    assert!(
        !openai_models
            .iter()
            .any(|model| model.model_id == "gpt-5.6")
    );
    let gpt_5_6_models = openai_models
        .iter()
        .filter(|model| model.model_id.starts_with("gpt-5.6"))
        .map(|model| model.model_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        gpt_5_6_models,
        std::collections::BTreeSet::from(["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra"])
    );
    assert_eq!(
        store.model_defaults_blocking().unwrap(),
        agent::AgentModelDefaults::default()
    );

    let provider = agent::AgentModelProvider {
        id: "openrouter".to_string(),
        name: "OpenRouter".to_string(),
        base_url: Some("https://openrouter.ai/api/v1".to_string()),
        env_key: Some("OPENROUTER_API_KEY".to_string()),
        built_in: false,
        enabled: true,
    };
    store.upsert_model_provider_blocking(&provider).unwrap();
    let target = agent::AgentModelTarget {
        provider_id: "openrouter".to_string(),
        model_id: "anthropic/claude-sonnet-4".to_string(),
        label: "Claude Sonnet 4".to_string(),
        enabled: true,
        favorite: true,
        reasoning_effort: Some("high".to_string()),
    };
    store.upsert_model_target_blocking(&target).unwrap();
    assert_eq!(
        store
            .model_target_reasoning_blocking(&target.provider_id, &target.model_id)
            .unwrap()
            .as_deref(),
        Some("high")
    );
    store
        .set_model_default_blocking(&target.provider_id, &target.model_id)
        .unwrap();

    assert_eq!(
        store.model_defaults_blocking().unwrap(),
        agent::AgentModelDefaults {
            provider_id: Some("openrouter".to_string()),
            model_id: Some("anthropic/claude-sonnet-4".to_string()),
        }
    );
    assert!(
        store
            .list_enabled_model_targets_blocking()
            .unwrap()
            .contains(&target)
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_deletes_provider_models_and_dependent_selections() {
    let root = temp_root("agent-model-provider-delete");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let provider = agent::AgentModelProvider {
        id: "local-delete".to_string(),
        name: "Local Delete".to_string(),
        base_url: Some("http://localhost:9090/v1".to_string()),
        env_key: None,
        built_in: false,
        enabled: true,
    };
    let model = agent::AgentModelTarget {
        provider_id: provider.id.clone(),
        model_id: "local-model".to_string(),
        label: "Local Model".to_string(),
        enabled: true,
        favorite: true,
        reasoning_effort: None,
    };
    store.upsert_model_provider_blocking(&provider).unwrap();
    store.upsert_model_target_blocking(&model).unwrap();
    store
        .set_project_codex_settings_blocking(
            project.id,
            Some(&provider.id),
            Some(&model.model_id),
            Some("high"),
            true,
        )
        .unwrap();
    store
        .set_model_default_blocking(&provider.id, &model.model_id)
        .unwrap();

    assert!(store.delete_model_provider_blocking(&provider.id).unwrap());
    assert!(!store.delete_model_provider_blocking(&provider.id).unwrap());
    assert!(
        !store
            .list_model_providers_blocking()
            .unwrap()
            .iter()
            .any(|candidate| candidate.id == provider.id)
    );
    assert!(
        store
            .list_model_targets_blocking(Some(&provider.id))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.model_defaults_blocking().unwrap(),
        agent::AgentModelDefaults::default()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.codex_provider, None);
    assert_eq!(project.codex_model, None);
    assert_eq!(project.codex_reasoning_effort.as_deref(), Some("high"));
    assert!(project.codex_fast_enabled);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_migrates_plain_gpt_5_6_selections_to_sol() {
    let root = temp_root("agent-model-gpt-5-6-sol-migration");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .upsert_model_target_blocking(&agent::AgentModelTarget {
            provider_id: "openai".to_string(),
            model_id: "gpt-5.6".to_string(),
            label: "GPT-5.6".to_string(),
            enabled: true,
            favorite: true,
            reasoning_effort: None,
        })
        .unwrap();
    store
        .upsert_model_target_blocking(&agent::AgentModelTarget {
            provider_id: "openai".to_string(),
            model_id: "gpt-5.6-sol".to_string(),
            label: "GPT-5.6 Sol".to_string(),
            enabled: false,
            favorite: false,
            reasoning_effort: None,
        })
        .unwrap();
    store
        .set_project_codex_settings_blocking(
            project.id,
            Some("openai"),
            Some("gpt-5.6"),
            None,
            false,
        )
        .unwrap();
    store
        .set_model_default_blocking("openai", "gpt-5.6")
        .unwrap();
    drop(store);

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db =
            turso::Builder::new_local(state_dir.join(AGENT_DB_FILE).to_string_lossy().as_ref())
                .build()
                .await
                .unwrap();
        db.connect()
            .unwrap()
            .execute("DELETE FROM schema_migrations WHERE version = 9", ())
            .await
            .unwrap();
    });

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let models = store.list_model_targets_blocking(Some("openai")).unwrap();
    let sol = models
        .iter()
        .find(|model| model.model_id == "gpt-5.6-sol")
        .unwrap();
    assert!(sol.enabled && sol.favorite);
    assert!(!models.iter().any(|model| model.model_id == "gpt-5.6"));
    assert_eq!(
        store.list_projects_blocking().unwrap()[0]
            .codex_model
            .as_deref(),
        Some("gpt-5.6-sol")
    );
    assert_eq!(
        store.model_defaults_blocking().unwrap().model_id.as_deref(),
        Some("gpt-5.6-sol")
    );

    fs::remove_dir_all(root).unwrap();
}
#[test]
fn ensure_agent_state_dir_creates_directory() {
    let root = temp_root("agent-state-dir");
    let state_dir = root.join("state/clt");

    ensure_agent_state_dir_at(&state_dir).unwrap();

    assert!(state_dir.is_dir());

    fs::remove_dir_all(root).unwrap();
}
