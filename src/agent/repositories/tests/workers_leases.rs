use super::*;
use crate::{
    agent::{AgentGitMode, NewGitFinalization},
    runner::{agent_timestamp, agent_timestamp_after},
    task::init_tasks,
    test_support::temp_root,
};

#[derive(Clone, Copy, Debug)]
struct ControlCase<'a> {
    state: &'a str,
    holder: Option<&'a str>,
    token: Option<&'a str>,
    child: Option<i64>,
    journal_state: &'a str,
    allowed: bool,
}

#[test]
fn unstarted_scheduler_lease_cleanup_is_snapshot_and_owner_fenced() {
    let root = temp_root("unstarted-scheduler-lease-fences");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let store = TursoAgentStore::open_blocking(&root.join("state")).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let holder = "clt-scheduler-test";

    for fence in [
        "renewed",
        "reacquired",
        "transferred",
        "dispatching",
        "running",
        "finalizing",
        "launch",
        "running-control",
        "interactive-control",
        "ready-control",
        "stop-control",
        "child",
        "interactive-holder",
        "interactive-launch",
        "queued",
        "stopped",
        "none",
    ] {
        assert!(
            store
                .try_acquire_lease_blocking(project.id, holder, "100", "9999999999")
                .unwrap()
        );
        let observed = store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap();
        store.blocking.block_on_persist(async {
            let conn = store.repositories.workers_leases.connect().await?;
            match fence {
                "renewed" | "reacquired" | "transferred" => {
                    let column = match fence {
                        "renewed" => "expires_at",
                        "reacquired" => "acquired_at",
                        _ => "holder",
                    };
                    conn.execute(&format!("UPDATE leases SET {column} = 'changed' WHERE project_id = ?1"), [project.id]).await?;
                }
                "dispatching" | "running" | "finalizing" => {
                    conn.execute(
                        "INSERT INTO agent_workers (worker_token, project_id, state,
                         protocol_version, lease_holder, service_label, binary_path,
                         command_arguments, path_env, task_selection, created_at)
                         VALUES ('worker', ?1, ?2, 1, 'other-worker', 'test', '/clt', '[]', '', 'next_todo', '100')",
                        params![project.id, fence],
                    ).await?;
                }
                "launch" => {
                    conn.execute(
                        "INSERT INTO agent_git_launch_states (project_id, run_token, git_mode,
                         starting_head, worktree_baseline, created_at)
                         VALUES (?1, 'launch', 'commit', 'head', '{}', '100')",
                        [project.id],
                    ).await?;
                }
                "none" => {}
                _ => {
                    let state = match fence {
                        "running-control" => "running",
                        "interactive-control" => "interactive",
                        "ready-control" => "ready_interactive",
                        "stop-control" => "stop_requested",
                        "stopped" => "stopped",
                        _ => "resume_requested",
                    };
                    conn.execute(
                        "INSERT INTO session_controls (project_id, codex_session_id, state,
                         run_token, child_pid, interactive_holder, interactive_launch_token, updated_at)
                         VALUES (?1, 'session', ?2, 'resume-token', ?3, ?4, ?5, '100')",
                        params![project.id, state, (fence == "child").then_some(123),
                            (fence == "interactive-holder").then_some("guardian"),
                            (fence == "interactive-launch").then_some("launch")],
                    ).await?;
                }
            }
            Ok(())
        }).unwrap();
        let allowed = matches!(fence, "queued" | "stopped" | "none");
        assert_eq!(
            store
                .release_unstarted_scheduler_lease_blocking(&observed)
                .unwrap(),
            allowed,
            "{fence}"
        );
        assert_eq!(
            store
                .lease_for_project_blocking(project.id)
                .unwrap()
                .is_none(),
            allowed,
            "{fence}"
        );
        store
            .blocking
            .block_on_persist(async {
                let conn = store.repositories.workers_leases.connect().await?;
                for table in [
                    "agent_workers",
                    "agent_git_launch_states",
                    "session_controls",
                    "leases",
                ] {
                    conn.execute(
                        &format!("DELETE FROM {table} WHERE project_id = ?1"),
                        [project.id],
                    )
                    .await?;
                }
                Ok(())
            })
            .unwrap();
    }
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unstarted_scheduler_cleanup_preserves_pending_git_and_queued_session() {
    let root = temp_root("unstarted-scheduler-pending-git");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let store = TursoAgentStore::open_blocking(&root.join("state")).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .create_git_finalization_blocking(NewGitFinalization {
                project_id: project.id,
                codex_session_id: "session",
                git_mode: AgentGitMode::CommitAndPush,
                starting_head: Some("1111111111111111111111111111111111111111"),
                branch_ref: Some("refs/heads/main"),
                upstream_ref: None,
                worktree_baseline: "{}",
                task_identity: Some("finished task"),
                owner_run_token: None,
                created_at: "100",
            })
            .unwrap()
    );
    for state in ["working", "tracking", "commit_pending", "push_pending"] {
        store.blocking.block_on_persist(async {
            let conn = store.repositories.workers_leases.connect().await?;
            conn.execute("UPDATE git_finalizations SET state = ?1, generation = 3 WHERE project_id = ?2", params![state, project.id]).await?;
            conn.execute(
                "INSERT OR REPLACE INTO session_controls (project_id, codex_session_id,
                 state, run_token, stdout_path, stderr_path, updated_at)
                 VALUES (?1, 'session', 'resume_requested', 'clt-git-finalization:3', 'saved.out', 'saved.err', '100')",
                [project.id],
            ).await?;
            Ok(())
        }).unwrap();
        let journal = store
            .git_finalization_blocking(project.id, "session")
            .unwrap()
            .unwrap();
        let control = store
            .session_control_blocking(project.id, "session")
            .unwrap()
            .unwrap();
        assert!(
            store
                .try_acquire_lease_blocking(project.id, "scheduler", "100", "9999999999")
                .unwrap()
        );
        let lease = store
            .lease_for_project_blocking(project.id)
            .unwrap()
            .unwrap();
        assert!(
            store
                .release_unstarted_scheduler_lease_blocking(&lease)
                .unwrap()
        );
        assert_eq!(
            store
                .git_finalization_blocking(project.id, "session")
                .unwrap()
                .unwrap(),
            journal
        );
        assert_eq!(
            store
                .session_control_blocking(project.id, "session")
                .unwrap()
                .unwrap(),
            control
        );
        // Existing Git reconciliation can acquire its guarded lease immediately.
        assert!(
            store
                .try_acquire_git_finalization_lease_blocking(
                    project.id,
                    "finalizer",
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                    None,
                )
                .unwrap()
        );
        assert!(
            store
                .release_lease_blocking(project.id, "finalizer")
                .unwrap()
        );
    }
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn git_finalizer_shared_session_policy_is_consistent_across_lease_checks() {
    let root = temp_root("finalizer-shared-session-fences");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let store = TursoAgentStore::open_blocking(&root.join("state")).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    for session in ["active", "interactive"] {
        assert!(
            store
                .create_git_finalization_blocking(NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: session,
                    git_mode: AgentGitMode::CommitAndPush,
                    starting_head: Some("1111111111111111111111111111111111111111"),
                    branch_ref: Some("refs/heads/main"),
                    upstream_ref: None,
                    worktree_baseline: "{}",
                    task_identity: Some(session),
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
    }
    let shared = ControlCase {
        state: "interactive",
        holder: Some("clt-shared-interactive-worker-123-456"),
        token: Some("clt-shared-interactive-worker-123-456"),
        child: Some(123),
        journal_state: "completed",
        allowed: true,
    };
    let mut cases = Vec::new();
    for holder in [
        "clt-shared-interactive-worker-123-456",
        "clt-stopped-shared-interactive-worker-123-456",
        "clt-readonly-interactive-worker-123-456",
        "clt-stopped-readonly-interactive-worker-123-456",
    ] {
        for (state, token, child) in [
            ("ready_interactive", None, None),
            ("interactive", Some(holder), None),
            ("interactive", Some(holder), Some(123)),
            ("stop_requested", Some(holder), Some(123)),
        ] {
            cases.push(ControlCase {
                state,
                holder: Some(holder),
                token,
                child,
                ..shared
            });
        }
    }
    for journal_state in [
        "cancelled",
        "working",
        "tracking",
        "commit_pending",
        "push_pending",
    ] {
        cases.push(ControlCase {
            journal_state,
            allowed: journal_state == "cancelled",
            ..shared
        });
    }
    for holder in [
        None,
        Some("clt-idle-interactive-worker-123-456"),
        Some("clt-stopped-interactive-worker-123-456"),
        Some("clt-interactive-worker-123-456"),
    ] {
        cases.push(ControlCase {
            holder,
            token: holder,
            allowed: false,
            ..shared
        });
    }
    for token in [None, Some("different-guardian")] {
        cases.push(ControlCase {
            token,
            allowed: false,
            ..shared
        });
    }
    for state in ["running", "resume_requested"] {
        cases.push(ControlCase {
            state,
            allowed: false,
            ..shared
        });
    }
    for case in cases {
        store
            .blocking
            .block_on_persist(async {
                let conn = store.repositories.workers_leases.connect().await?;
                conn.execute(
                    "UPDATE git_finalizations SET state = ?1
                          WHERE project_id = ?2 AND codex_session_id = 'interactive'",
                    params![case.journal_state, project.id],
                )
                .await?;
                conn.execute(
                    "INSERT OR REPLACE INTO session_controls
                            (project_id, codex_session_id, state, child_pid, interactive_holder,
                             interactive_launch_token, updated_at)
                         VALUES (?1, 'interactive', ?2, ?3, ?4, ?5, ?6)",
                    params![
                        project.id,
                        case.state,
                        case.child,
                        case.holder,
                        case.token,
                        agent_timestamp()
                    ],
                )
                .await?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store
                .try_acquire_git_finalization_lease_blocking(
                    project.id,
                    "finalizer",
                    &agent_timestamp(),
                    &agent_timestamp_after(60),
                    None,
                )
                .unwrap(),
            case.allowed,
            "acquisition: {case:?}"
        );
        if !case.allowed {
            // Model ownership changing after the finalizer acquired its
            // lease: renewal and each side-effect fence must also fail.
            assert!(
                store
                    .try_acquire_lease_blocking(
                        project.id,
                        "finalizer",
                        &agent_timestamp(),
                        &agent_timestamp_after(60),
                    )
                    .unwrap()
            );
        }
        assert_eq!(
            store
                .git_finalization_lease_is_owned_blocking(
                    project.id,
                    "finalizer",
                    &agent_timestamp(),
                )
                .unwrap(),
            case.allowed,
            "ownership: {case:?}"
        );
        assert_eq!(
            store
                .renew_git_finalization_lease_blocking(
                    project.id,
                    "finalizer",
                    &agent_timestamp_after(120),
                )
                .unwrap(),
            case.allowed,
            "renewal: {case:?}"
        );
        assert!(
            store
                .release_lease_blocking(project.id, "finalizer")
                .unwrap()
        );
    }
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
