use super::*;
use crate::task::init_tasks;
use crate::test_support::temp_root;
use crate::worker::tests::reserve_test_worker;
use std::fs;

const OBSERVER: &str = "clt-reattached-123-claim";

fn supervision_fixture(
    label: &str,
) -> (PathBuf, PathBuf, TursoAgentStore, AgentSessionControlRecord) {
    let root = temp_root(label);
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let store = TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&fs::canonicalize(&project_root).unwrap(), label)
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    store
        .mark_session_running_blocking(
            project_id,
            "orphan-session",
            456,
            "original-run",
            &root.join("original.out"),
            &root.join("original.err"),
        )
        .unwrap();
    let control = store
        .session_control_blocking(project_id, "orphan-session")
        .unwrap()
        .unwrap();
    (root, state_dir, store, control)
}

fn current_control(
    store: &TursoAgentStore,
    expected: &AgentSessionControlRecord,
) -> AgentSessionControlRecord {
    store
        .session_control_blocking(expected.project_id, &expected.codex_session_id)
        .unwrap()
        .unwrap()
}

#[test]
fn planning_reservations_reject_live_owners_and_unfinished_workers() {
    for blocker in [
        "running",
        "stop_requested",
        "interrupt_requested",
        "ready_interactive",
        "interactive",
        "queued-pid",
        "queued-holder",
        "queued-launch",
        "worker",
        "missing-lease",
        "expired-lease",
    ] {
        let (root, _, store, expected) = supervision_fixture("planning-reservation-blocker");
        let project = expected.project_id;
        store
            .set_session_control_recovery_token_blocking(
                project,
                &expected.codex_session_id,
                "original-run",
            )
            .unwrap();
        store
            .set_session_control_state_blocking(
                project,
                "planning",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        let holder = "clt-stopped-interactive-planning";
        if blocker != "missing-lease" {
            let expiry = if blocker == "expired-lease" {
                "101"
            } else {
                "9999999999"
            };
            assert!(
                store
                    .try_acquire_lease_blocking(project, holder, "100", expiry)
                    .unwrap()
            );
        }
        match blocker {
            "queued-pid" | "queued-holder" | "queued-launch" => {
                store
                    .blocking
                    .block_on_persist(async {
                        let conn = store.repositories.sessions_runs.connect().await?;
                        conn.execute(
                            "UPDATE session_controls SET child_pid = ?1, interactive_holder = ?2,
                            interactive_launch_token = ?3
                         WHERE project_id = ?4 AND codex_session_id = ?5",
                            params![
                                (blocker == "queued-pid").then_some(123_i64),
                                (blocker == "queued-holder").then_some("other-owner"),
                                (blocker == "queued-launch").then_some("other-launch"),
                                project,
                                expected.codex_session_id.as_str(),
                            ],
                        )
                        .await?;
                        Ok(())
                    })
                    .unwrap();
            }
            "worker" => {
                assert!(reserve_test_worker(
                    &store,
                    project,
                    "unfinished",
                    holder,
                    "100",
                    1
                ));
                assert!(
                    store
                        .release_lease_blocking(project, "clt-worker-unfinished")
                        .unwrap()
                );
                assert!(
                    store
                        .try_acquire_lease_blocking(project, holder, "100", "9999999999")
                        .unwrap()
                );
            }
            "missing-lease" | "expired-lease" => {}
            state => store
                .set_session_control_state_blocking(
                    project,
                    &expected.codex_session_id,
                    AgentSessionControlState::from_database(state).unwrap(),
                )
                .unwrap(),
        }
        let before = store
            .session_controls_for_project_blocking(project)
            .unwrap();
        assert!(
            !store
                .register_planning_session_blocking(project, "new-planning", holder)
                .unwrap(),
            "{blocker}"
        );
        assert!(
            !store
                .reserve_idle_session_interactive_blocking(project, "planning", holder, None)
                .unwrap(),
            "{blocker}"
        );
        assert_eq!(
            store
                .session_controls_for_project_blocking(project)
                .unwrap(),
            before,
            "{blocker}"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn queued_interactive_reservation_refuses_an_unfinished_worker() {
    let (root, _state_dir, store, expected) = supervision_fixture("queued-interactive-worker");
    store
        .set_session_control_recovery_token_blocking(
            expected.project_id,
            &expected.codex_session_id,
            "original-run",
        )
        .unwrap();
    assert!(
        store
            .try_acquire_lease_blocking(expected.project_id, "scheduler", "100", "9999999999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        expected.project_id,
        "worker",
        "scheduler",
        "100",
        1
    ));
    // A released lease must not allow takeover before its worker finishes.
    assert!(
        store
            .release_lease_blocking(expected.project_id, "clt-worker-worker")
            .unwrap()
    );
    let holder = "clt-stopped-interactive-test";
    assert!(
        store
            .try_acquire_lease_blocking(expected.project_id, holder, "100", "9999999999")
            .unwrap()
    );
    assert!(
        !store
            .reserve_idle_session_interactive_blocking(
                expected.project_id,
                &expected.codex_session_id,
                holder,
                Some("original-run"),
            )
            .unwrap()
    );
    assert_eq!(
        current_control(&store, &expected).state,
        AgentSessionControlState::ResumeRequested
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn queued_shared_resume_distinguishes_its_own_worker_from_another_task() {
    for own_worker in [true, false] {
        let (root, _state_dir, store, expected) = supervision_fixture("queued-shared-worker");
        store
            .set_session_control_recovery_token_blocking(
                expected.project_id,
                &expected.codex_session_id,
                "original-run",
            )
            .unwrap();
        assert!(
            store
                .try_acquire_lease_blocking(expected.project_id, "scheduler", "100", "9999999999",)
                .unwrap()
        );
        let worker_token = if own_worker {
            "original-run"
        } else {
            "another-task"
        };
        assert!(reserve_test_worker(
            &store,
            expected.project_id,
            worker_token,
            "scheduler",
            "100",
            1
        ));
        let holder = "clt-stopped-shared-interactive-test";
        assert_eq!(
            store
                .reserve_shared_session_interactive_blocking(
                    expected.project_id,
                    &expected.codex_session_id,
                    holder,
                    Some("original-run"),
                )
                .unwrap(),
            !own_worker
        );
        assert_eq!(
            store
                .lease_for_project_blocking(expected.project_id)
                .unwrap()
                .unwrap()
                .holder,
            format!("clt-worker-{worker_token}")
        );
        if !own_worker {
            assert!(
                store
                    .cancel_idle_session_interactive_blocking(
                        expected.project_id,
                        &expected.codex_session_id,
                        holder,
                    )
                    .unwrap()
            );
            assert_eq!(
                current_control(&store, &expected).state,
                AgentSessionControlState::Stopped
            );
        } else {
            assert_eq!(
                current_control(&store, &expected).state,
                AgentSessionControlState::ResumeRequested
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn orphan_supervision_claim_is_durable_and_excludes_stale_competing_claimants() {
    let (root, state_dir, store, expected) = supervision_fixture("supervision-single-claim");
    store
        .set_session_control_state_blocking(
            expected.project_id,
            "older-stopped",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    assert!(
        store
            .claim_orphaned_session_supervision_blocking(&expected, OBSERVER, 60)
            .unwrap()
    );
    assert_eq!(current_control(&store, &expected), expected);
    drop(store);
    let reopened = TursoAgentStore::open_blocking(&state_dir).unwrap();
    assert!(
        !reopened
            .claim_orphaned_session_supervision_blocking(&expected, "competing-observer", 60)
            .unwrap()
    );
    assert!(
        !reopened
            .claim_orphaned_session_supervision_blocking(&expected, OBSERVER, 60)
            .unwrap()
    );
    assert_eq!(
        reopened
            .lease_for_project_blocking(expected.project_id)
            .unwrap()
            .unwrap()
            .holder,
        OBSERVER
    );
    assert_eq!(reopened.run_count_blocking().unwrap(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn orphan_supervision_claim_preserves_controls_requested_during_identification() {
    for interrupt in [false, true] {
        let (root, _, store, expected) = supervision_fixture("supervision-control-race");
        if interrupt {
            assert!(
                store
                    .request_session_interrupt_blocking(
                        expected.project_id,
                        &expected.codex_session_id,
                        456,
                        "original-run",
                        "interactive-requester"
                    )
                    .unwrap()
            );
        } else {
            assert!(
                store
                    .request_session_stop_blocking(
                        expected.project_id,
                        &expected.codex_session_id,
                        456,
                        "original-run"
                    )
                    .unwrap()
            );
        }
        let requested = current_control(&store, &expected);
        assert!(
            store
                .claim_orphaned_session_supervision_blocking(&expected, OBSERVER, 60)
                .unwrap()
        );
        assert_eq!(current_control(&store, &expected), requested);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn orphan_supervision_claim_rejects_lease_worker_and_ambiguous_session_ownership() {
    for blocker in ["lease", "worker", "other-session", "guardian"] {
        let (root, _, store, expected) = supervision_fixture("supervision-ownership-blocker");
        match blocker {
            "lease" | "worker" => {
                assert!(store.try_acquire_lease_blocking(expected.project_id, "existing-owner", "100", "101").unwrap());
                if blocker == "worker" {
                    assert!(reserve_test_worker(&store, expected.project_id, "other-worker", "existing-owner", "100", 2));
                    assert!(store.release_lease_blocking(expected.project_id, "clt-worker-other-worker").unwrap());
                }
            }
            "other-session" => store.set_session_control_state_blocking(expected.project_id, "different-session", AgentSessionControlState::ResumeRequested).unwrap(),
            "guardian" => store.blocking.block_on_persist(async {
                let conn = store.repositories.sessions_runs.connect().await?;
                conn.execute("UPDATE session_controls SET interactive_launch_token = 'guardian-token' WHERE project_id = ?1", [expected.project_id]).await?;
                Ok(())
            }).unwrap(),
            _ => unreachable!(),
        }
        let before = current_control(&store, &expected);
        assert!(
            !store
                .claim_orphaned_session_supervision_blocking(&expected, OBSERVER, 60)
                .unwrap(),
            "blocker: {blocker}"
        );
        assert_eq!(current_control(&store, &expected), before);
        assert!(
            store
                .lease_for_project_blocking(expected.project_id)
                .unwrap()
                .is_none_or(|lease| lease.holder != OBSERVER)
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn orphan_supervision_claim_rejects_changed_or_missing_process_generation() {
    let (root, _, store, expected) = supervision_fixture("supervision-generation-claim");
    for change in [
        "session",
        "pid",
        "token",
        "missing-pid",
        "missing-token",
        "terminal",
    ] {
        let mut stale = expected.clone();
        match change {
            "session" => stale.codex_session_id = "different-session".into(),
            "pid" => stale.child_pid = Some(999),
            "token" => stale.run_token = Some("different-run".into()),
            "missing-pid" => stale.child_pid = None,
            "missing-token" => stale.run_token = None,
            "terminal" => stale.state = AgentSessionControlState::Stopped,
            _ => unreachable!(),
        }
        assert!(
            !store
                .claim_orphaned_session_supervision_blocking(&stale, OBSERVER, 60)
                .unwrap(),
            "change: {change}"
        );
        assert!(
            store
                .lease_for_project_blocking(expected.project_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(current_control(&store, &expected), expected);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reattached_supervision_finalization_preserves_resume_stop_and_interactive_intent() {
    for state in [
        AgentSessionControlState::Running,
        AgentSessionControlState::StopRequested,
        AgentSessionControlState::InterruptRequested,
    ] {
        let (root, _, store, expected) = supervision_fixture("supervision-finalization-intent");
        assert!(
            store
                .claim_orphaned_session_supervision_blocking(&expected, OBSERVER, 60)
                .unwrap()
        );
        let terminal = match state {
            AgentSessionControlState::Running => AgentSessionControlState::ResumeRequested,
            AgentSessionControlState::StopRequested => {
                assert!(
                    store
                        .request_session_stop_blocking(
                            expected.project_id,
                            &expected.codex_session_id,
                            456,
                            "original-run"
                        )
                        .unwrap()
                );
                AgentSessionControlState::Stopped
            }
            AgentSessionControlState::InterruptRequested => {
                assert!(
                    store
                        .request_session_interrupt_blocking(
                            expected.project_id,
                            &expected.codex_session_id,
                            456,
                            "original-run",
                            "interactive-requester"
                        )
                        .unwrap()
                );
                AgentSessionControlState::ReadyInteractive
            }
            _ => unreachable!(),
        };
        assert!(
            store
                .finalize_reattached_automated_session_blocking(&expected, OBSERVER, 60)
                .unwrap()
        );
        let finalized = current_control(&store, &expected);
        assert_eq!(finalized.state, terminal);
        assert_eq!(finalized.child_pid, None);
        assert_eq!(finalized.run_token, expected.run_token);
        assert_eq!(finalized.stdout_path, expected.stdout_path);
        assert_eq!(finalized.stderr_path, expected.stderr_path);
        let lease = store
            .lease_for_project_blocking(expected.project_id)
            .unwrap();
        if terminal == AgentSessionControlState::ReadyInteractive {
            assert_eq!(lease.unwrap().holder, "interactive-requester");
            assert_eq!(
                finalized.interactive_holder.as_deref(),
                Some("interactive-requester")
            );
        } else {
            assert!(lease.is_none());
        }
        assert!(
            !store
                .finalize_reattached_automated_session_blocking(&expected, OBSERVER, 60)
                .unwrap()
        );
        assert_eq!(store.run_count_blocking().unwrap(), 0);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn reattached_supervision_finalization_cannot_change_a_successor_lease_or_generation() {
    for change in [
        "missing-lease",
        "successor-lease",
        "session",
        "pid",
        "token",
        "guardian",
    ] {
        let (root, _, store, expected) = supervision_fixture("supervision-stale-finalizer");
        assert!(
            store
                .claim_orphaned_session_supervision_blocking(&expected, OBSERVER, 60)
                .unwrap()
        );
        let mut stale = expected.clone();
        match change {
            "missing-lease" | "successor-lease" => {
                assert!(store.release_lease_blocking(expected.project_id, OBSERVER).unwrap());
                if change == "successor-lease" {
                    assert!(store.try_acquire_lease_blocking(expected.project_id, "successor", "100", "9999999999").unwrap());
                }
            }
            "session" => stale.codex_session_id = "different-session".into(),
            "pid" => stale.child_pid = Some(999),
            "token" => stale.run_token = Some("different-run".into()),
            "guardian" => store.blocking.block_on_persist(async {
                let conn = store.repositories.sessions_runs.connect().await?;
                conn.execute("UPDATE session_controls SET interactive_launch_token = 'guardian-token' WHERE project_id = ?1", [expected.project_id]).await?;
                Ok(())
            }).unwrap(),
            _ => unreachable!(),
        }
        let before = current_control(&store, &expected);
        assert!(
            !store
                .finalize_reattached_automated_session_blocking(&stale, OBSERVER, 60)
                .unwrap(),
            "change: {change}"
        );
        assert_eq!(current_control(&store, &expected), before);
        if change == "successor-lease" {
            assert_eq!(
                store
                    .lease_for_project_blocking(expected.project_id)
                    .unwrap()
                    .unwrap()
                    .holder,
                "successor"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
