use super::*;
use crate::{
    agent::{AgentSessionControlState, NewGitFinalization, recovery},
    task::init_tasks,
    test_support::temp_root,
    worker::tests::reserve_test_worker,
};
use std::fs;

#[test]
fn git_defaults_can_change_without_mutating_active_owners_or_pending_proof() {
    for owner in [
        "dispatching",
        "running",
        "finalizing",
        "interactive",
        "working",
        "tracking",
        "commit_pending",
        "push_pending",
        "launch",
    ] {
        let root = temp_root("git-default-active-owner");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        let state_dir = root.join("state");
        let store = TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .set_project_git_mode_blocking(project.id, AgentGitMode::Commit)
            .unwrap();
        match owner {
            "dispatching" | "running" | "finalizing" | "interactive" | "launch" => {
                assert!(
                    store
                        .try_acquire_lease_blocking(project.id, "owner", "100", "9999999999")
                        .unwrap()
                );
                if owner == "interactive" {
                    assert!(
                        store
                            .reserve_idle_session_interactive_blocking(
                                project.id, "session", "owner", None
                            )
                            .unwrap()
                    );
                    store
                        .set_session_control_state_blocking(
                            project.id,
                            "session",
                            AgentSessionControlState::Interactive,
                        )
                        .unwrap();
                } else {
                    assert!(reserve_test_worker(
                        &store, project.id, "worker", "owner", "101", 1
                    ));
                    store
                        .blocking
                        .block_on_persist(async {
                            let conn = store.repositories.projects_models.connect().await?;
                            conn.execute(
                                "UPDATE agent_workers SET state = ?1 WHERE project_id = ?2",
                                params![
                                    if owner == "launch" { "running" } else { owner },
                                    project.id
                                ],
                            )
                            .await?;
                            Ok(())
                        })
                        .unwrap();
                }
                if owner == "launch" {
                    store
                        .record_git_launch_state_blocking(
                            project.id,
                            "worker",
                            AgentGitMode::Commit,
                            &crate::managed_git::AgentGitStartState {
                                starting_head: "frozen-head".into(),
                                branch_ref: Some("refs/heads/main".into()),
                                upstream_ref: None,
                                worktree_baseline: "{}".into(),
                            },
                            "100",
                        )
                        .unwrap();
                }
            }
            _ => {
                assert!(
                    store
                        .create_git_finalization_blocking(NewGitFinalization {
                            project_id: project.id,
                            codex_session_id: "session",
                            git_mode: AgentGitMode::Commit,
                            starting_head: Some("frozen-head"),
                            branch_ref: Some("refs/heads/main"),
                            upstream_ref: None,
                            worktree_baseline: "{}",
                            task_identity: Some("task"),
                            owner_run_token: None,
                            created_at: "100",
                        })
                        .unwrap()
                );
                store
                    .blocking
                    .block_on_persist(async {
                        let conn = store.repositories.projects_models.connect().await?;
                        conn.execute(
                            "UPDATE git_finalizations SET state = ?1 WHERE project_id = ?2",
                            params![owner, project.id],
                        )
                        .await?;
                        Ok(())
                    })
                    .unwrap();
            }
        }
        let before = recovery::read_snapshot(&state_dir).unwrap().unwrap();
        for by_path in [false, true] {
            for mode in [
                AgentGitMode::Off,
                AgentGitMode::Commit,
                AgentGitMode::CommitAndPush,
            ] {
                let changed = if by_path {
                    store.set_project_git_mode_for_path_blocking(&project_root, mode)
                } else {
                    store.set_project_git_mode_blocking(project.id, mode)
                }
                .unwrap();
                assert!(changed, "{owner}");
                assert_eq!(store.list_projects_blocking().unwrap()[0].git_mode, mode);
                let after = recovery::read_snapshot(&state_dir).unwrap().unwrap();
                for table in [
                    "leases",
                    "agent_workers",
                    "session_controls",
                    "session_git_modes",
                    "agent_git_launch_states",
                    "git_finalizations",
                ] {
                    assert_eq!(
                        after["tables"][table], before["tables"][table],
                        "{owner}: {table}"
                    );
                }
            }
        }
        drop(store);
        let reopened = TursoAgentStore::open_blocking(&state_dir).unwrap();
        assert_eq!(
            reopened.list_projects_blocking().unwrap()[0].git_mode,
            AgentGitMode::CommitAndPush
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}
