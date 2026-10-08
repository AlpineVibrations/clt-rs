use crate::test_support::prelude::*;
use crate::test_support::temp_root;

use super::{prepare_completed_task_context, restore_completed_task_for_guardian};

fn completed_session(
    root: &Path,
    folders: bool,
    shared: bool,
) -> (
    agent::TursoAgentStore,
    i64,
    String,
    InteractiveGuardianDisposition,
) {
    completed_session_with_guardian(root, folders, shared, None)
}

fn completed_session_with_guardian(
    root: &Path,
    folders: bool,
    shared: bool,
    guardian_override: Option<&str>,
) -> (
    agent::TursoAgentStore,
    i64,
    String,
    InteractiveGuardianDisposition,
) {
    let project = root.join("project");
    init_tasks(&project, folders).unwrap();
    add_task(&project, "Older finished task", None).unwrap();
    move_task(&project, TaskStatus::Todo, TaskStatus::Done, "1").unwrap();
    add_task(
        &project,
        "Finished feature — COMPLETED 2026-10-06: Original work verified codex:session-completed",
        None,
    )
    .unwrap();
    move_task(&project, TaskStatus::Todo, TaskStatus::Done, "1").unwrap();
    let store = agent::TursoAgentStore::open_blocking(&root.join("state")).unwrap();
    store
        .register_project_blocking(&project, "project")
        .unwrap();
    let id = store.list_projects_blocking().unwrap().remove(0).id;
    let (requester, disposition) = if shared {
        assert!(
            store
                .try_acquire_lease_blocking(id, "other-owner", "100", "9999999999")
                .unwrap()
        );
        store
            .set_session_control_state_blocking(
                id,
                "other-session",
                AgentSessionControlState::Running,
            )
            .unwrap();
        let requester = InteractiveAgentLease::holder_for_shared_session(false);
        assert!(
            store
                .reserve_shared_session_interactive_blocking(
                    id,
                    "session-completed",
                    &requester,
                    None,
                    false
                )
                .unwrap()
        );
        (
            requester,
            InteractiveGuardianDisposition::PreserveSharedSession,
        )
    } else {
        let requester = InteractiveAgentLease::holder_for_idle_session();
        assert!(
            store
                .try_acquire_lease_blocking(id, &requester, "100", "9999999999")
                .unwrap()
        );
        assert!(
            store
                .reserve_idle_session_interactive_blocking(
                    id,
                    "session-completed",
                    &requester,
                    None
                )
                .unwrap()
        );
        (
            requester,
            InteractiveGuardianDisposition::PreserveIdleSession,
        )
    };
    let guardian = guardian_override
        .map(str::to_string)
        .unwrap_or_else(|| interactive_guardian_holder(disposition));
    assert!(
        store
            .adopt_interactive_guardian_blocking(
                id,
                Some("session-completed"),
                &requester,
                &guardian,
                60
            )
            .unwrap()
    );
    (store, id, guardian, disposition)
}

#[test]
fn completed_interactive_session_reuses_task_and_preserves_terminal_git_proof() {
    for folders in [false, true] {
        for shared in [false, true] {
            for mode in [
                AgentGitMode::Off,
                AgentGitMode::Commit,
                AgentGitMode::CommitAndPush,
            ] {
                let root = temp_root("completed-interactive-lifecycle");
                let (store, id, guardian, disposition) = completed_session(&root, folders, shared);
                let board = get_tasks_dir(&root.join("project"));
                let original = read_task_entries(&board, TaskStatus::Done).unwrap();
                if mode != AgentGitMode::Off {
                    assert!(
                        store
                            .create_git_finalization_blocking(agent::NewGitFinalization {
                                project_id: id,
                                codex_session_id: "session-completed",
                                git_mode: mode,
                                starting_head: Some("1111111111111111111111111111111111111111"),
                                branch_ref: Some("refs/heads/main"),
                                upstream_ref: Some("refs/remotes/origin/main"),
                                worktree_baseline: "{}",
                                task_identity: Some("Finished feature"),
                                owner_run_token: None,
                                created_at: "100",
                            })
                            .unwrap()
                    );
                    let mut states = vec![
                        GitFinalizationState::Tracking,
                        GitFinalizationState::CommitPending,
                    ];
                    if mode == AgentGitMode::CommitAndPush {
                        states.push(GitFinalizationState::PushPending);
                    }
                    states.push(GitFinalizationState::Completed);
                    for state in states {
                        let journal = store
                            .git_finalization_blocking(id, "session-completed")
                            .unwrap()
                            .unwrap();
                        assert!(
                            store
                                .compare_and_set_git_finalization_blocking(
                                    id,
                                    "session-completed",
                                    journal.generation,
                                    state,
                                    None,
                                    Some("2222222222222222222222222222222222222222"),
                                    None,
                                    "101",
                                )
                                .unwrap()
                        );
                    }
                }
                let journal_before = store
                    .git_finalization_blocking(id, "session-completed")
                    .unwrap();
                let other_control = store.session_control_blocking(id, "other-session").unwrap();
                assert!(
                    prepare_completed_task_context(&store, id, "session-completed", &guardian)
                        .unwrap()
                );
                assert!(
                    read_task_entries(&board, TaskStatus::Doing)
                        .unwrap()
                        .is_empty()
                );
                let unchanged = read_task_entries(&board, TaskStatus::Done).unwrap();
                assert_eq!(unchanged.len(), original.len());
                for (after, before) in unchanged.iter().zip(&original) {
                    assert_eq!(after.content, before.content);
                    assert_eq!(after.source, before.source);
                    assert!(!task_content_is_manual(&after.content));
                    assert!(!task_content_is_interactive_done(&after.content));
                }
                assert!(
                    store
                        .register_interactive_guardian_child_blocking(
                            id,
                            "session-completed",
                            &guardian,
                            std::process::id(),
                            60
                        )
                        .unwrap()
                );
                assert!(
                    !finish_interactive_guardian_after_reap(
                        &store,
                        id,
                        "session-completed",
                        &guardian,
                        Duration::from_secs(60),
                        disposition
                    )
                    .unwrap()
                );
                assert!(
                    read_task_entries(&board, TaskStatus::Doing)
                        .unwrap()
                        .is_empty()
                );
                let done = read_task_entries(&board, TaskStatus::Done).unwrap();
                assert_eq!(done.len(), 2);
                assert_eq!(done[0].content.trim_end(), original[0].content.trim_end());
                assert_eq!(done[1].source, original[1].source);
                assert_eq!(
                    store
                        .git_finalization_blocking(id, "session-completed")
                        .unwrap(),
                    journal_before
                );
                assert_eq!(
                    store.session_control_blocking(id, "other-session").unwrap(),
                    other_control
                );
                let lease = store.lease_for_project_blocking(id).unwrap();
                assert_eq!(
                    lease.as_ref().map(|lease| lease.holder.as_str()),
                    shared.then_some("other-owner")
                );
                assert_eq!(
                    store
                        .session_control_blocking(id, "session-completed")
                        .unwrap()
                        .unwrap()
                        .state,
                    AgentSessionControlState::Stopped
                );
                drop(store);
                fs::remove_dir_all(root).unwrap();
            }
        }
    }
}

#[test]
fn completed_task_can_finish_before_interactive_exit() {
    let root = temp_root("completed-interactive-explicit-done");
    let (store, id, guardian, disposition) = completed_session(&root, true, false);
    let board = get_tasks_dir(&root.join("project"));
    assert!(seed_legacy_reopened_task(&store, id, "session-completed", &guardian).unwrap());
    move_task_to_done_in_board_with_store(&board, TaskStatus::Doing, "1", &store).unwrap();
    let done_before_exit = task_entry_at(&board, TaskStatus::Done, 1).unwrap();
    assert!(!task_content_is_interactive_done(&done_before_exit.content));
    assert!(
        !finish_interactive_guardian_after_reap(
            &store,
            id,
            "session-completed",
            &guardian,
            Duration::from_secs(60),
            disposition
        )
        .unwrap()
    );
    assert_eq!(
        task_entry_at(&board, TaskStatus::Done, 1).unwrap().source,
        done_before_exit.source
    );
    assert_eq!(
        read_task_entries(&board, TaskStatus::Done).unwrap().len(),
        2
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_interactive_launch_restores_completed_task() {
    let root = temp_root("completed-interactive-failed-launch");
    let (store, id, guardian, disposition) = completed_session(&root, false, false);
    let board = get_tasks_dir(&root.join("project"));
    let original = task_entry_at(&board, TaskStatus::Done, 1).unwrap().content;
    assert!(seed_legacy_reopened_task(&store, id, "session-completed", &guardian).unwrap());
    // The guardian owns the reservation, but its child was never registered.
    assert!(
        !finish_interactive_guardian_after_reap(
            &store,
            id,
            "session-completed",
            &guardian,
            Duration::from_secs(60),
            disposition
        )
        .unwrap()
    );
    assert!(
        read_task_entries(&board, TaskStatus::Doing)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        task_entry_at(&board, TaskStatus::Done, 1).unwrap().content,
        original
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_task_reopen_and_restoration_require_the_exact_guardian() {
    let root = temp_root("completed-interactive-owner-fence");
    let (store, id, guardian, _) = completed_session(&root, false, false);
    let board = get_tasks_dir(&root.join("project"));
    assert!(
        prepare_completed_task_context(&store, id, "session-completed", "stale-holder").is_err()
    );
    assert!(
        read_task_entries(&board, TaskStatus::Doing)
            .unwrap()
            .is_empty()
    );
    assert!(seed_legacy_reopened_task(&store, id, "session-completed", &guardian).unwrap());
    restore_completed_task_for_guardian(&store, id, "session-completed", "stale-holder").unwrap();
    assert_eq!(
        read_task_entries(&board, TaskStatus::Doing).unwrap().len(),
        1
    );
    restore_completed_task_for_guardian(&store, id, "session-completed", &guardian).unwrap();
    assert!(
        read_task_entries(&board, TaskStatus::Doing)
            .unwrap()
            .is_empty()
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_task_recovers_exact_duplicate_left_by_an_interrupted_move() {
    let root = temp_root("completed-interactive-duplicate-recovery");
    let (store, id, guardian, _) = completed_session(&root, false, false);
    let board = get_tasks_dir(&root.join("project"));
    assert!(seed_legacy_reopened_task(&store, id, "session-completed", &guardian).unwrap());
    let doing = task_entry_at(&board, TaskStatus::Doing, 1).unwrap();
    insert_task_content(&board, TaskStatus::Done, Some(0), &doing.content).unwrap();
    restore_completed_task_for_guardian(&store, id, "session-completed", &guardian).unwrap();
    assert!(
        read_task_entries(&board, TaskStatus::Doing)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        read_task_entries(&board, TaskStatus::Done).unwrap().len(),
        2
    );
    assert!(!task_content_is_interactive_done(
        &task_entry_at(&board, TaskStatus::Done, 1).unwrap().content
    ));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn provisional_done_context_preserves_unfinished_git_proof() {
    let root = temp_root("completed-interactive-pending-proof");
    let (store, id, guardian, _) = completed_session(&root, false, false);
    let board = get_tasks_dir(&root.join("project"));
    let original = task_entry_at(&board, TaskStatus::Done, 1).unwrap().content;
    assert!(
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: id,
                codex_session_id: "session-completed",
                git_mode: AgentGitMode::Commit,
                starting_head: Some("1111111111111111111111111111111111111111"),
                branch_ref: Some("refs/heads/main"),
                upstream_ref: None,
                worktree_baseline: "{}",
                task_identity: Some("Finished feature"),
                owner_run_token: None,
                created_at: "100",
            })
            .unwrap()
    );
    let journal = store
        .git_finalization_blocking(id, "session-completed")
        .unwrap();
    assert!(prepare_completed_task_context(&store, id, "session-completed", &guardian).unwrap());
    assert_eq!(
        task_entry_at(&board, TaskStatus::Done, 1).unwrap().content,
        original
    );
    assert!(
        read_task_entries(&board, TaskStatus::Doing)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .git_finalization_blocking(id, "session-completed")
            .unwrap(),
        journal
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_task_can_reopen_into_a_mixed_storage_layout() {
    let root = temp_root("completed-interactive-mixed-layout");
    let (store, id, guardian, disposition) = completed_session(&root, false, false);
    let board = get_tasks_dir(&root.join("project"));
    convert_status_to_directory(&board, TaskStatus::Done).unwrap();
    let older = task_entry_at(&board, TaskStatus::Done, 2).unwrap();
    assert!(seed_legacy_reopened_task(&store, id, "session-completed", &guardian).unwrap());
    assert!(board.join("doing").is_dir());
    assert!(
        !finish_interactive_guardian_after_reap(
            &store,
            id,
            "session-completed",
            &guardian,
            Duration::from_secs(60),
            disposition
        )
        .unwrap()
    );
    assert!(
        read_task_entries(&board, TaskStatus::Doing)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        task_entry_at(&board, TaskStatus::Done, 2).unwrap().source,
        older.source
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn crashed_completed_interactive_guardian_restores_done_without_exec_resume() {
    let root = temp_root("completed-interactive-stale-guardian");
    let dead_guardian = "clt-idle-interactive-worker-4000000-crashed";
    let (store, id, _, disposition) =
        completed_session_with_guardian(&root, false, false, Some(dead_guardian));
    assert_eq!(
        disposition,
        InteractiveGuardianDisposition::PreserveIdleSession
    );
    assert!(seed_legacy_reopened_task(&store, id, "session-completed", dead_guardian).unwrap());
    reconcile_stale_agent_session_controls(
        &root.join("state"),
        id,
        store.lease_for_project_blocking(id).unwrap().as_ref(),
        false,
        agent_timestamp_seconds(),
    )
    .unwrap();
    let board = get_tasks_dir(&root.join("project"));
    assert!(
        read_task_entries(&board, TaskStatus::Doing)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .session_control_blocking(id, "session-completed")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::Stopped
    );
    assert!(store.lease_for_project_blocking(id).unwrap().is_none());
    assert_eq!(
        read_task_entries(&board, TaskStatus::Done).unwrap().len(),
        2
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_task_cleanup_rejects_a_child_registered_after_the_stale_snapshot() {
    let root = temp_root("completed-interactive-registration-race");
    let (store, id, guardian, disposition) = completed_session(&root, false, false);
    assert!(seed_legacy_reopened_task(&store, id, "session-completed", &guardian).unwrap());
    assert!(
        store
            .register_interactive_guardian_child_blocking(
                id,
                "session-completed",
                &guardian,
                std::process::id(),
                60,
            )
            .unwrap()
    );
    assert!(
        !recover_stale_interactive_guardian_with_task(
            &store,
            id,
            "session-completed",
            &guardian,
            None,
            disposition,
        )
        .unwrap()
    );
    let board = get_tasks_dir(&root.join("project"));
    assert_eq!(
        read_task_entries(&board, TaskStatus::Doing).unwrap().len(),
        1
    );
    assert_eq!(
        read_task_entries(&board, TaskStatus::Done).unwrap().len(),
        1
    );
    assert_eq!(
        store
            .session_control_blocking(id, "session-completed")
            .unwrap()
            .unwrap()
            .child_pid,
        Some(std::process::id())
    );
    assert_eq!(
        store
            .lease_for_project_blocking(id)
            .unwrap()
            .unwrap()
            .holder,
        guardian
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interrupted_cleanup_restores_the_task_after_session_ownership_was_released() {
    let root = temp_root("completed-interactive-interrupted-cleanup");
    let (store, id, guardian, disposition) = completed_session(&root, false, false);
    assert!(seed_legacy_reopened_task(&store, id, "session-completed", &guardian).unwrap());
    assert!(
        store
            .recover_stale_interactive_guardian_blocking(
                id,
                "session-completed",
                &guardian,
                None,
                disposition,
            )
            .unwrap()
    );
    reconcile_stale_agent_session_controls(
        &root.join("state"),
        id,
        None,
        false,
        agent_timestamp_seconds(),
    )
    .unwrap();
    let board = get_tasks_dir(&root.join("project"));
    assert!(
        read_task_entries(&board, TaskStatus::Doing)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        read_task_entries(&board, TaskStatus::Done).unwrap().len(),
        2
    );
    assert!(!task_content_is_interactive_done(
        &task_entry_at(&board, TaskStatus::Done, 1).unwrap().content
    ));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

fn seed_legacy_reopened_task(
    store: &agent::TursoAgentStore,
    id: i64,
    session: &str,
    guardian: &str,
) -> anyhow::Result<bool> {
    assert!(prepare_completed_task_context(
        store, id, session, guardian
    )?);
    let project = store
        .list_projects_blocking()?
        .into_iter()
        .find(|p| p.id == id)
        .unwrap();
    let board = get_tasks_dir(&project.path);
    let entry = task_entry_at(&board, TaskStatus::Done, 1)?;
    super::prepare_destination(&board, &entry, TaskStatus::Doing)?;
    TaskBoard::new(&board).write_entry_content(
        TaskStatus::Done,
        &entry,
        &task_content_with_interactive_done_marker(&entry.content),
    )?;
    TaskBoard::new(&board).move_task_without_reordering_after_lock(
        TaskStatus::Done,
        TaskStatus::Doing,
        1,
    )?;
    Ok(true)
}

#[test]
fn completed_context_does_not_convert_mixed_storage() {
    let root = temp_root("completed-context-mixed-storage");
    let (store, id, guardian, disposition) = completed_session(&root, false, false);
    let board = get_tasks_dir(&root.join("project"));
    convert_status_to_directory(&board, TaskStatus::Done).unwrap();
    let before = read_task_entries(&board, TaskStatus::Done).unwrap();
    assert!(prepare_completed_task_context(&store, id, "session-completed", &guardian).unwrap());
    assert!(board.join("doing.md").is_file());
    assert!(!board.join("doing").exists());
    assert!(
        !finish_interactive_guardian_after_reap(
            &store,
            id,
            "session-completed",
            &guardian,
            Duration::from_secs(60),
            disposition
        )
        .unwrap()
    );
    let after = read_task_entries(&board, TaskStatus::Done).unwrap();
    for (before, after) in before.iter().zip(after.iter()) {
        assert_eq!(before.source, after.source);
        assert_eq!(before.content, after.content);
    }
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_cleanup_does_not_create_a_missing_board() {
    let root = temp_root("completed-context-missing-board");
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&root.join("state")).unwrap();
    store
        .register_project_blocking(&project, "project")
        .unwrap();
    let id = store.list_projects_blocking().unwrap()[0].id;
    super::restore_idle_completed_tasks(&store, id).unwrap();
    assert!(!get_tasks_dir(&project).exists());
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
