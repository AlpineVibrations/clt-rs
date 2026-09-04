use super::*;
use crate::agent::{
    AGENT_DB_FILE, AGENT_EXTERNAL_COMPLETION_REASON, AgentGitMode, AgentModelDefaults,
    AgentModelProvider, AgentModelTarget, AgentProject, AgentSessionControlState,
    GitFinalizationState, NewGitFinalization, runtime::AgentStoreBlockingAdapter,
};
use crate::managed_git::capture_agent_git_start_state;
use crate::task::init_tasks;
use crate::test_support::{initialize_test_git_repository, temp_root};
use crate::worker::tests::reserve_test_worker;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "wal_tail_tests.rs"]
mod wal_tail_tests;

const CHECKPOINT_CHILD_STATE: &str = "CLT_REGISTRY_RECOVERY_CHECKPOINT_TEST_STATE";
const REOPEN_CHILD_STATE: &str = "CLT_REGISTRY_RECOVERY_REOPEN_TEST_STATE";

#[test]
fn registry_open_after_partial_checkpoint_preserves_pin_ownership() {
    let (root, state_dir, store, project) = registered_store("registry-partial-checkpoint");
    store
        .write_checkpoint_pressure_blocking(project.id, 1_100)
        .unwrap();
    let (wal_frames, checkpointed_frames) = store
        .blocking
        .block_on(async {
            let conn = store.recovery_db.connect()?;
            let mut rows = conn.query("PRAGMA wal_checkpoint(PASSIVE)", ()).await?;
            let row = rows
                .next()
                .await?
                .context("checkpoint returned no result")?;
            Ok((row.get::<i64>(1)?, row.get::<i64>(2)?))
        })
        .unwrap();
    assert!(checkpointed_frames > 0);
    assert!(checkpointed_frames < wal_frames);
    assert_overlapping_store_preserves_pin(&state_dir, &store, project.id);
    drop(store);
    assert!(!state_dir.join(REQUIRED_FILE).exists());
    // Upgrading an older registry also exports its first external snapshot.
    fs::remove_file(state_dir.join(SNAPSHOT_FILE)).unwrap();
    for _ in 0..3 {
        let reopened = TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(reopened.list_projects_blocking().unwrap()[0].id, project.id);
        assert!(state_dir.join(SNAPSHOT_FILE).exists());
        assert_overlapping_store_preserves_pin(&state_dir, &reopened, project.id);
        drop(reopened);
        assert!(!state_dir.join(REQUIRED_FILE).exists());
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent::recovery::tests::registry_reopened_reader_child_preserves_pin_ownership",
                "--nocapture",
            ])
            .env(REOPEN_CHILD_STATE, &state_dir)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "reopened child failed: {}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(!state_dir.join(REQUIRED_FILE).exists());
    }
    fs::remove_dir_all(root).unwrap();
}

fn assert_overlapping_store_preserves_pin(
    state_dir: &Path,
    primary: &TursoAgentStore,
    project_id: i64,
) {
    let peer = TursoAgentStore::open_blocking(state_dir).unwrap();
    assert_eq!(peer.list_projects_blocking().unwrap()[0].id, project_id);
    let recorded_scan = peer.record_project_scan_blocking(project_id).unwrap();
    drop(peer);
    assert!(!state_dir.join(REQUIRED_FILE).exists());
    let project = primary.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.id, project_id);
    assert_eq!(
        project.last_scan_at.as_deref(),
        Some(recorded_scan.as_str())
    );
}

#[test]
fn registry_reopened_reader_child_preserves_pin_ownership() {
    let Some(state_dir) = std::env::var_os(REOPEN_CHILD_STATE) else {
        return;
    };
    let state_dir = Path::new(&state_dir);
    let store = TursoAgentStore::open_blocking(state_dir).unwrap();
    assert!(!store.list_projects_blocking().unwrap().is_empty());
    drop(store);
    assert!(!state_dir.join(REQUIRED_FILE).exists());
}

fn registered_store(label: &str) -> (PathBuf, PathBuf, TursoAgentStore, AgentProject) {
    let root = temp_root(label);
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, label)
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    (root, state_dir, store, project)
}

fn bundle_contents(state_dir: &Path) -> Vec<(&'static str, Option<Vec<u8>>)> {
    BUNDLE_FILES
        .into_iter()
        .map(|name| (name, fs::read(state_dir.join(name)).ok()))
        .collect()
}

fn corrupt_database(state_dir: &Path) {
    fs::write(state_dir.join(AGENT_DB_FILE), b"unreadable registry header").unwrap();
}

#[test]
fn registry_recovery_restores_preferences_and_exact_git_journals_from_snapshot() {
    let (root, state_dir, store, project) = registered_store("registry-recovery-snapshot");
    fs::write(
        project.path.join("tasks/doing.md"),
        "# Doing Tasks\n- Externally accepted task codex:session-external\n",
    )
    .unwrap();
    initialize_test_git_repository(&project.path);
    let launch = capture_agent_git_start_state(&project.path, AgentGitMode::Commit).unwrap();
    let provider = AgentModelProvider {
        id: "recovery-provider".into(),
        name: "Recovery provider".into(),
        base_url: Some("https://example.invalid/v1".into()),
        env_key: Some("RECOVERY_TEST_KEY".into()),
        built_in: false,
        enabled: true,
    };
    let target = AgentModelTarget {
        provider_id: provider.id.clone(),
        model_id: "recovery-model".into(),
        label: "Recovery model".into(),
        enabled: true,
        favorite: true,
        reasoning_effort: Some("high".into()),
    };
    store.upsert_model_provider_blocking(&provider).unwrap();
    store.upsert_model_target_blocking(&target).unwrap();
    store
        .set_model_default_blocking(&provider.id, &target.model_id)
        .unwrap();
    store
        .set_project_codex_settings_blocking(
            project.id,
            Some(&provider.id),
            Some(&target.model_id),
            Some("high"),
            true,
        )
        .unwrap();
    store
        .set_project_git_mode_blocking(project.id, AgentGitMode::CommitAndPush)
        .unwrap();
    store
        .set_project_enabled_blocking(project.id, false)
        .unwrap();
    store
        .record_git_launch_state_blocking(
            project.id,
            "unconsumed-launch",
            AgentGitMode::Commit,
            &launch,
            "100",
        )
        .unwrap();

    for (session, push_pending) in [
        ("session-working", false),
        ("session-push", true),
        ("session-external", false),
    ] {
        store
            .mark_session_running_blocking(
                project.id,
                session,
                123,
                "dead-worker-token",
                &root.join(format!("{session}.out")),
                &root.join(format!("{session}.err")),
            )
            .unwrap();
        assert!(store.create_git_finalization_blocking(NewGitFinalization {
            project_id: project.id,
            codex_session_id: session,
            git_mode: AgentGitMode::CommitAndPush,
            starting_head: Some("1111111111111111111111111111111111111111"),
            branch_ref: Some("refs/heads/frozen-branch"),
            upstream_ref: Some("refs/remotes/origin/frozen-branch"),
            worktree_baseline: r#"{"version":1,"tracked_patch_ids":{"source.rs":"frozen-patch"},"untracked_blob_ids":{"new.rs":"frozen-blob"},"require_clean":false}"#,
            task_identity: Some(session),
            owner_run_token: Some("dead-worker-token"),
            created_at: "101",
        }).unwrap());
        if push_pending {
            for (generation, state, commit) in [
                (0, GitFinalizationState::Tracking, None),
                (1, GitFinalizationState::CommitPending, None),
                (
                    2,
                    GitFinalizationState::PushPending,
                    Some("2222222222222222222222222222222222222222"),
                ),
            ] {
                assert!(
                    store
                        .compare_and_set_git_finalization_blocking(
                            project.id,
                            session,
                            generation,
                            state,
                            Some("dead-worker-token"),
                            commit,
                            Some("publication interrupted"),
                            "102",
                        )
                        .unwrap()
                );
            }
        }
        if session == "session-external" {
            assert!(
                store
                    .compare_and_set_git_finalization_blocking(
                        project.id,
                        session,
                        0,
                        GitFinalizationState::Cancelled,
                        None,
                        None,
                        Some(AGENT_EXTERNAL_COMPLETION_REASON),
                        "103",
                    )
                    .unwrap()
            );
        }
    }
    let mut expected_journals = store
        .list_pending_git_finalizations_blocking(Some(project.id))
        .unwrap();
    for journal in &mut expected_journals {
        journal.owner_run_token = None;
    }
    let expected_cancelled = store
        .git_finalization_blocking(project.id, "session-external")
        .unwrap()
        .unwrap();
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "fixture-scheduler", "100", "9999999999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        project.id,
        "unconsumed-launch",
        "fixture-scheduler",
        "100",
        1,
    ));
    let original_worker = store.list_active_workers_blocking().unwrap().remove(0);
    assert!(read_snapshot(&state_dir).unwrap().is_some());
    assert!(!state_dir.join(DIRTY_FILE).exists());
    drop(store);
    corrupt_database(&state_dir);
    let damaged_bundle = bundle_contents(&state_dir);
    assert!(
        damaged_bundle
            .iter()
            .any(|(name, bytes)| *name == "agent.db-wal"
                && bytes.as_ref().is_some_and(|bytes| bytes.len() > 32))
    );

    let report = recover_registry(&state_dir).unwrap();

    assert!(report.rebuilt_registry);
    assert_eq!(bundle_contents(&report.quarantine), damaged_bundle);
    let reopened = TursoAgentStore::open_blocking(&state_dir).unwrap();
    let restored = reopened.list_projects_blocking().unwrap().remove(0);
    assert_eq!(restored.id, project.id);
    assert_eq!(restored.path, project.path);
    assert_eq!(restored.name, project.name);
    assert!(!restored.enabled);
    assert_eq!(restored.git_mode, AgentGitMode::CommitAndPush);
    assert_eq!(
        restored.codex_provider.as_deref(),
        Some(provider.id.as_str())
    );
    assert_eq!(
        restored.codex_model.as_deref(),
        Some(target.model_id.as_str())
    );
    assert_eq!(restored.codex_reasoning_effort.as_deref(), Some("high"));
    assert!(restored.codex_fast_enabled);
    assert!(
        reopened
            .list_model_providers_blocking()
            .unwrap()
            .contains(&provider)
    );
    assert!(
        reopened
            .list_model_targets_blocking(Some(&provider.id))
            .unwrap()
            .contains(&target)
    );
    assert_eq!(
        reopened.model_defaults_blocking().unwrap(),
        AgentModelDefaults {
            provider_id: Some(provider.id),
            model_id: Some(target.model_id),
        }
    );
    assert_eq!(
        reopened
            .git_launch_state_blocking(project.id, "unconsumed-launch")
            .unwrap(),
        Some((AgentGitMode::Commit, launch))
    );
    assert_eq!(
        reopened
            .list_pending_git_finalizations_blocking(Some(project.id))
            .unwrap(),
        expected_journals
    );
    assert_eq!(
        reopened
            .git_finalization_blocking(project.id, "session-external")
            .unwrap(),
        Some(expected_cancelled)
    );
    assert!(!crate::scheduler::project_has_resumable_doing_task(&state_dir, &restored).unwrap());
    let control = reopened
        .session_control_blocking(project.id, "session-working")
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::ResumeRequested);
    assert_eq!(control.run_token.as_deref(), Some("clt-git-finalization:0"));
    assert_eq!(control.child_pid, None);
    assert!(
        reopened
            .session_control_blocking(project.id, "session-push")
            .unwrap()
            .is_none()
    );
    assert!(reopened.list_active_workers_blocking().unwrap().is_empty());
    let restored_worker = reopened.list_terminal_workers_blocking().unwrap().remove(0);
    assert_eq!(restored_worker.state, "abandoned");
    assert_eq!(restored_worker.worker_token, original_worker.worker_token);
    assert_eq!(restored_worker.project_id, original_worker.project_id);
    assert_eq!(restored_worker.service_label, original_worker.service_label);
    assert_eq!(restored_worker.binary_path, original_worker.binary_path);
    assert_eq!(
        restored_worker.command_arguments,
        original_worker.command_arguments
    );
    assert_eq!(restored_worker.lease_holder, original_worker.lease_holder);
    assert_eq!(restored_worker.worker_pid, original_worker.worker_pid);
    let durable = read_snapshot(&state_dir).unwrap().unwrap();
    assert!(
        durable["tables"]["agent_workers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|worker| worker["worker_token"] == "unconsumed-launch"),
        "A second rebuild must retain the worker identity needed to reclaim its launch boundary"
    );
    assert!(!state_dir.join(REQUIRED_FILE).exists());
    assert!(!state_dir.join("recovery-in-progress.json").exists());
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_recovery_rebuilds_coordination_after_checkpoint_pressure_with_live_peers() {
    let (root, state_dir, store, project) = registered_store("registry-recovery-coordination");
    let tui = TursoAgentStore::open_blocking(&state_dir).unwrap();
    let worker = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "agent::recovery::tests::registry_recovery_checkpoint_writer_exits_without_dropping_store",
            "--nocapture",
        ])
        .env(CHECKPOINT_CHILD_STATE, &state_dir)
        .output()
        .unwrap();
    assert_eq!(
        worker.status.code(),
        Some(29),
        "worker failed before its abrupt exit: stdout={}; stderr={}",
        String::from_utf8_lossy(&worker.stdout),
        String::from_utf8_lossy(&worker.stderr)
    );
    assert_eq!(tui.list_projects_blocking().unwrap()[0].id, project.id);
    assert_eq!(store.list_projects_blocking().unwrap()[0].id, project.id);
    let wal = fs::read(state_dir.join("agent.db-wal")).unwrap();
    let page_size = u32::from_be_bytes(wal[8..12].try_into().unwrap()) as usize;
    assert!((wal.len() - 32) / (24 + page_size) > 1_000);
    drop(tui);
    drop(store);
    fs::write(
        state_dir.join("agent.db-tshm"),
        b"stale coordination metadata",
    )
    .unwrap();
    fs::write(
        state_dir.join("agent.db-shm"),
        b"stale sqlite index metadata",
    )
    .unwrap();
    let original = bundle_contents(&state_dir);

    let report = recover_registry(&state_dir).unwrap();

    assert!(!report.rebuilt_registry);
    assert_eq!(bundle_contents(&report.quarantine), original);
    let reopened = TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(reopened.list_projects_blocking().unwrap()[0].id, project.id);
    reopened
        .blocking
        .block_on(integrity_check(&reopened.recovery_db))
        .unwrap();
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_recovery_checkpoint_writer_exits_without_dropping_store() {
    let Some(state_dir) = std::env::var_os(CHECKPOINT_CHILD_STATE) else {
        return;
    };
    let store = TursoAgentStore::open_blocking(Path::new(&state_dir)).unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .write_checkpoint_pressure_blocking(project.id, 1_100)
        .unwrap();
    // Exit without dropping the checkpoint pin, database handles or access lock.
    std::process::exit(29);
}

#[test]
fn registry_recovery_refuses_a_live_store_without_changing_the_bundle() {
    let (root, state_dir, store, project) = registered_store("registry-recovery-live-store");
    let original = bundle_contents(&state_dir);
    let snapshot = fs::read(state_dir.join(SNAPSHOT_FILE)).unwrap();

    let error = recover_registry(&state_dir)
        .err()
        .expect("live store must fence recovery");

    assert!(format!("{error:#}").contains("still in use"));
    assert_eq!(bundle_contents(&state_dir), original);
    assert_eq!(fs::read(state_dir.join(SNAPSHOT_FILE)).unwrap(), snapshot);
    assert!(!state_dir.join("quarantine").exists());
    assert!(!state_dir.join(REQUIRED_FILE).exists());
    assert_eq!(store.list_projects_blocking().unwrap()[0].id, project.id);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_missing_database_with_snapshot_refuses_empty_reinitialization() {
    let (root, state_dir, store, _) = registered_store("registry-recovery-missing-database");
    drop(store);
    let snapshot = fs::read(state_dir.join(SNAPSHOT_FILE)).unwrap();
    let wal = fs::read(state_dir.join("agent.db-wal")).unwrap();
    fs::remove_file(state_dir.join(AGENT_DB_FILE)).unwrap();

    let error = TursoAgentStore::open_blocking(&state_dir)
        .err()
        .expect("missing database with a durable snapshot requires recovery");

    assert!(format!("{error:#}").contains("recovery required"));
    assert!(!state_dir.join(AGENT_DB_FILE).exists());
    assert_eq!(fs::read(state_dir.join(SNAPSHOT_FILE)).unwrap(), snapshot);
    assert_eq!(fs::read(state_dir.join("agent.db-wal")).unwrap(), wal);
    assert!(
        fs::read_to_string(state_dir.join(REQUIRED_FILE))
            .unwrap()
            .contains("database is missing")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_recovery_progress_marker_alone_blocks_open_before_required_marker_is_written() {
    let (root, state_dir, store, _) = registered_store("registry-recovery-progress-gap");
    drop(store);
    let original = bundle_contents(&state_dir);
    let snapshot = fs::read(state_dir.join(SNAPSHOT_FILE)).unwrap();
    let archive = quarantine_bundle(&state_dir).unwrap();
    atomic_write(
        &state_dir.join("recovery-in-progress.json"),
        &serde_json::to_vec(&archive).unwrap(),
    )
    .unwrap();
    assert!(!state_dir.join(REQUIRED_FILE).exists());

    let error = TursoAgentStore::open_blocking(&state_dir)
        .err()
        .expect("the recovery progress file must fence new clients by itself");

    assert!(format!("{error:#}").contains("recovery required"));
    assert_eq!(bundle_contents(&state_dir), original);
    assert_eq!(fs::read(state_dir.join(SNAPSHOT_FILE)).unwrap(), snapshot);
    assert!(state_dir.join("recovery-in-progress.json").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_recovery_refuses_stale_snapshot_after_interrupted_update() {
    let (root, state_dir, store, _) = registered_store("registry-recovery-dirty-snapshot");
    drop(store);
    begin_update(&state_dir).unwrap();
    corrupt_database(&state_dir);
    let original = bundle_contents(&state_dir);

    let error = recover_registry(&state_dir)
        .err()
        .expect("dirty fallback must fail closed");

    let message = format!("{error:#}");
    assert!(
        message.contains("snapshot may predate an interrupted Git transition"),
        "{message}"
    );
    assert_eq!(bundle_contents(&state_dir), original);
    let archive: PathBuf =
        serde_json::from_slice(&fs::read(state_dir.join("recovery-in-progress.json")).unwrap())
            .unwrap();
    assert_eq!(bundle_contents(&archive), original);
    assert!(state_dir.join(DIRTY_FILE).exists());
    assert!(state_dir.join(REQUIRED_FILE).exists());
    assert!(TursoAgentStore::open_blocking(&state_dir).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_recovery_restarts_from_the_quarantine_after_an_interrupted_attempt() {
    let (root, state_dir, store, project) = registered_store("registry-recovery-interrupted");
    drop(store);
    let original = bundle_contents(&state_dir);
    let archive = quarantine_bundle(&state_dir).unwrap();
    atomic_write(
        &state_dir.join("recovery-in-progress.json"),
        &serde_json::to_vec(&archive).unwrap(),
    )
    .unwrap();
    mark_required(&state_dir, "Interrupted registry reconstruction").unwrap();
    corrupt_database(&state_dir);
    fs::write(state_dir.join("agent.db-wal"), b"partial replacement WAL").unwrap();

    let report = recover_registry(&state_dir).unwrap();

    assert!(!report.rebuilt_registry);
    assert_ne!(report.quarantine, archive);
    assert_eq!(bundle_contents(&archive), original);
    assert_eq!(bundle_contents(&report.quarantine), original);
    let reopened = TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert_eq!(reopened.list_projects_blocking().unwrap()[0].id, project.id);
    assert!(!state_dir.join("recovery-in-progress.json").exists());
    assert!(!state_dir.join(REQUIRED_FILE).exists());
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_shared_wal_panic_marks_recovery_required_and_stops_further_database_calls() {
    let root = temp_root("registry-recovery-panic");
    let adapter = AgentStoreBlockingAdapter::new(&root, false).unwrap();
    let calls = AtomicUsize::new(0);

    let error = adapter
        .block_on(async {
            calls.fetch_add(1, Ordering::SeqCst);
            panic!("shared WAL frame index length changed while publishing an entry");
            #[allow(unreachable_code)]
            Ok(())
        })
        .unwrap_err();

    assert!(format!("{error:#}").contains("recovery required"));
    assert!(
        fs::read_to_string(root.join(REQUIRED_FILE))
            .unwrap()
            .contains("frame index length changed")
    );
    let retry = adapter.block_on(async {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert!(retry.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(TursoAgentStore::open_blocking(&root).is_err());
    drop(adapter);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_shared_wal_error_marks_recovery_required_but_unrelated_errors_do_not() {
    let root = temp_root("registry-recovery-error");
    let adapter = AgentStoreBlockingAdapter::new(&root, false).unwrap();
    let unrelated: Result<()> =
        adapter.block_on(async { anyhow::bail!("ordinary database error") });
    assert!(unrelated.is_err());
    assert!(!root.join(REQUIRED_FILE).exists());
    let failure: Result<()> =
        adapter.block_on(async { anyhow::bail!("shared WAL ownership assertion failed") });
    assert!(format!("{:#}", failure.unwrap_err()).contains("recovery required"));
    assert!(root.join(REQUIRED_FILE).exists());
    drop(adapter);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_recovery_uses_snapshot_when_database_is_missing_or_empty() {
    for empty in [false, true] {
        let (root, state_dir, store, project) = registered_store("registry-lost-database");
        drop(store);
        if empty {
            fs::write(state_dir.join(AGENT_DB_FILE), b"").unwrap();
        } else {
            fs::remove_file(state_dir.join(AGENT_DB_FILE)).unwrap();
        }
        let original = bundle_contents(&state_dir);
        let report = recover_registry(&state_dir).unwrap();
        assert!(report.rebuilt_registry);
        assert_eq!(bundle_contents(&report.quarantine), original);
        let reopened = TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            reopened.list_projects_blocking().unwrap()[0].path,
            project.path
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}
