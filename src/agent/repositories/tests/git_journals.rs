use super::*;
use crate::agent::{AgentSessionControlState, recovery};
use crate::runner::{agent_timestamp, agent_timestamp_after};
use crate::task::init_tasks;
use crate::test_support::temp_root;
use crate::worker::tests::reserve_test_worker;
use std::{
    fs,
    path::{Path, PathBuf},
};

const ORPHAN_REASON: &str =
    "Abandoned unbound Git journal: no task identity or board marker remains";
const FINALIZER: &str = "orphan-journal-test-finalizer";

fn assert_registry_is_clean(state_dir: &Path) {
    assert!(!state_dir.join("registry-dirty").exists());
    assert!(!state_dir.join("recovery-required").exists());
}

fn orphan_fixture(label: &str) -> (PathBuf, PathBuf, TursoAgentStore, GitFinalizationRecord) {
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
    assert!(store.create_git_finalization_blocking(NewGitFinalization {
        project_id: project.id,
        codex_session_id: "orphan-session",
        git_mode: AgentGitMode::CommitAndPush,
        starting_head: Some("1111111111111111111111111111111111111111"),
        branch_ref: Some("refs/heads/frozen"),
        upstream_ref: Some("refs/remotes/origin/frozen"),
        worktree_baseline: r#"{"version":1,"tracked_patch_ids":{"source.rs":"frozen-patch"},"untracked_blob_ids":{"new.rs":"frozen-blob"},"require_clean":false}"#,
        task_identity: None,
        owner_run_token: None,
        created_at: "100",
    }).unwrap());
    store
        .set_session_control_recovery_token_blocking(
            project.id,
            "orphan-session",
            "clt-git-finalization:0",
        )
        .unwrap();
    assert!(
        store
            .try_acquire_git_finalization_lease_blocking(
                project.id,
                FINALIZER,
                &agent_timestamp(),
                &agent_timestamp_after(600),
                None,
            )
            .unwrap()
    );
    let journal = store
        .git_finalization_blocking(project.id, "orphan-session")
        .unwrap()
        .unwrap();
    (root, state_dir, store, journal)
}

#[test]
fn orphan_journal_cancellation_preserves_frozen_proof_and_persists_exact_control_removal() {
    let (root, state_dir, store, expected) = orphan_fixture("orphan-journal-atomic-cancel");
    store
        .set_session_control_state_blocking(
            expected.project_id,
            "unrelated-stopped",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    let unrelated = store
        .session_control_blocking(expected.project_id, "unrelated-stopped")
        .unwrap();
    let now = agent_timestamp();

    assert!(
        store
            .cancel_orphaned_working_git_finalization_blocking(
                &expected,
                FINALIZER,
                ORPHAN_REASON,
                &now
            )
            .unwrap()
    );

    let mut cancelled = expected.clone();
    cancelled.state = GitFinalizationState::Cancelled;
    cancelled.generation += 1;
    cancelled.last_error = Some(ORPHAN_REASON.to_string());
    cancelled.updated_at = now.clone();
    cancelled.completed_at = Some(now);
    assert_eq!(
        store
            .git_finalization_blocking(expected.project_id, &expected.codex_session_id)
            .unwrap(),
        Some(cancelled.clone())
    );
    assert!(
        store
            .session_control_blocking(expected.project_id, &expected.codex_session_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .session_control_blocking(expected.project_id, "unrelated-stopped")
            .unwrap(),
        unrelated
    );
    assert_eq!(
        store
            .lease_for_project_blocking(expected.project_id)
            .unwrap()
            .unwrap()
            .holder,
        FINALIZER
    );
    let snapshot = recovery::read_snapshot(&state_dir).unwrap().unwrap();
    let durable = snapshot["tables"]["git_finalizations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|journal| journal["codex_session_id"] == expected.codex_session_id)
        .unwrap();
    assert_eq!(durable["state"], "cancelled");
    assert_eq!(durable["generation"], 1);
    assert_eq!(durable["last_error"], ORPHAN_REASON);
    assert_eq!(
        durable["starting_head"],
        expected.starting_head.as_deref().unwrap()
    );
    assert_eq!(durable["worktree_baseline"], expected.worktree_baseline);
    assert!(
        !snapshot["tables"]["session_controls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|control| control["codex_session_id"] == expected.codex_session_id)
    );
    assert!(
        !store
            .cancel_orphaned_working_git_finalization_blocking(
                &expected,
                FINALIZER,
                ORPHAN_REASON,
                &agent_timestamp()
            )
            .unwrap()
    );
    assert_eq!(
        store
            .git_finalization_blocking(expected.project_id, &expected.codex_session_id)
            .unwrap(),
        Some(cancelled)
    );
    assert_registry_is_clean(&state_dir);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn orphan_journal_cancellation_requires_its_live_fence_and_exact_idle_control() {
    let (root, state_dir, store, expected) = orphan_fixture("orphan-journal-exact-idle");
    assert!(
        !store
            .cancel_orphaned_working_git_finalization_blocking(
                &expected,
                "wrong-finalizer",
                ORPHAN_REASON,
                &agent_timestamp()
            )
            .unwrap()
    );
    assert_registry_is_clean(&state_dir);
    for invalid_token in [
        Some("dead-worker-token"),
        Some("clt-git-finalization:1"),
        None,
    ] {
        store.blocking.block_on_persist(async {
            let conn = store.repositories.git_journals.connect().await?;
            conn.execute(
                "UPDATE session_controls SET run_token = ?1 WHERE project_id = ?2 AND codex_session_id = ?3",
                params![invalid_token, expected.project_id, expected.codex_session_id.as_str()],
            ).await?;
            Ok(())
        }).unwrap();
        assert!(
            !store
                .cancel_orphaned_working_git_finalization_blocking(
                    &expected,
                    FINALIZER,
                    ORPHAN_REASON,
                    &agent_timestamp()
                )
                .unwrap()
        );
        assert_registry_is_clean(&state_dir);
        assert_eq!(
            store
                .session_control_blocking(expected.project_id, &expected.codex_session_id)
                .unwrap()
                .unwrap()
                .run_token
                .as_deref(),
            invalid_token
        );
        assert_eq!(
            store
                .git_finalization_blocking(expected.project_id, &expected.codex_session_id)
                .unwrap(),
            Some(expected.clone())
        );
    }
    store
        .set_session_control_recovery_token_blocking(
            expected.project_id,
            &expected.codex_session_id,
            "clt-git-finalization:0",
        )
        .unwrap();
    assert!(
        store
            .release_lease_blocking(expected.project_id, FINALIZER)
            .unwrap()
    );
    assert!(
        store
            .try_acquire_lease_blocking(expected.project_id, FINALIZER, "1", "2")
            .unwrap()
    );
    assert!(
        !store
            .cancel_orphaned_working_git_finalization_blocking(
                &expected,
                FINALIZER,
                ORPHAN_REASON,
                &agent_timestamp()
            )
            .unwrap()
    );
    assert_eq!(
        store
            .git_finalization_blocking(expected.project_id, &expected.codex_session_id)
            .unwrap(),
        Some(expected)
    );
    assert_registry_is_clean(&state_dir);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn orphan_journal_cancellation_rechecks_active_owners_after_the_fence_was_acquired() {
    for late_worker in [false, true] {
        let (root, state_dir, store, expected) = orphan_fixture("orphan-journal-late-owner");
        if late_worker {
            assert!(reserve_test_worker(
                &store,
                expected.project_id,
                "late-worker",
                FINALIZER,
                &agent_timestamp(),
                1,
            ));
        } else {
            store
                .mark_session_running_blocking(
                    expected.project_id,
                    "different-live-session",
                    std::process::id(),
                    "different-worker-token",
                    &root.join("other.out"),
                    &root.join("other.err"),
                )
                .unwrap();
        }
        let current_holder = store
            .lease_for_project_blocking(expected.project_id)
            .unwrap()
            .unwrap()
            .holder;
        let control_before = store
            .session_control_blocking(expected.project_id, &expected.codex_session_id)
            .unwrap();

        assert!(
            !store
                .cancel_orphaned_working_git_finalization_blocking(
                    &expected,
                    &current_holder,
                    ORPHAN_REASON,
                    &agent_timestamp(),
                )
                .unwrap()
        );
        assert_registry_is_clean(&state_dir);

        assert_eq!(
            store
                .git_finalization_blocking(expected.project_id, &expected.codex_session_id)
                .unwrap(),
            Some(expected.clone())
        );
        assert_eq!(
            store
                .session_control_blocking(expected.project_id, &expected.codex_session_id)
                .unwrap(),
            control_before
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn orphan_journal_cancellation_accepts_absent_or_stopped_idle_control_without_an_owner() {
    for state in [None, Some(AgentSessionControlState::Stopped)] {
        let (root, _state_dir, store, expected) = orphan_fixture("orphan-journal-idle-control");
        store
            .blocking
            .block_on_persist(async {
                let conn = store.repositories.git_journals.connect().await?;
                conn.execute(
                    "DELETE FROM session_controls WHERE project_id = ?1 AND codex_session_id = ?2",
                    params![expected.project_id, expected.codex_session_id.as_str()],
                )
                .await?;
                Ok(())
            })
            .unwrap();
        if let Some(state) = state {
            store
                .set_session_control_state_blocking(
                    expected.project_id,
                    &expected.codex_session_id,
                    state,
                )
                .unwrap();
        }
        assert!(
            store
                .cancel_orphaned_working_git_finalization_blocking(
                    &expected,
                    FINALIZER,
                    ORPHAN_REASON,
                    &agent_timestamp()
                )
                .unwrap()
        );
        assert!(
            store
                .session_control_blocking(expected.project_id, &expected.codex_session_id)
                .unwrap()
                .is_none()
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
