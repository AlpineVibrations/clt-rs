use super::*;
use crate::{
    agent::TursoAgentStore,
    task::{TaskBoard, init_tasks},
    test_support::temp_root,
    worker::tests::reserve_test_worker,
};
use std::{fs, path::PathBuf};

fn fixture(folders: bool) -> (PathBuf, TursoAgentStore, AgentWorkerRecord) {
    let root = temp_root("worker-activity");
    let project = root.join("project");
    init_tasks(&project, folders).unwrap();
    let store = TursoAgentStore::open_blocking(&root.join("state")).unwrap();
    store
        .register_project_blocking(&project, "project")
        .unwrap();
    let id = store.list_projects_blocking().unwrap()[0].id;
    assert!(
        store
            .try_acquire_lease_blocking(id, "scheduler", "100", "9999999999")
            .unwrap()
    );
    assert!(reserve_test_worker(
        &store,
        id,
        "worker",
        "scheduler",
        "101",
        1
    ));
    assert!(
        store
            .claim_worker_blocking("worker", std::process::id(), "102")
            .unwrap()
    );
    let worker = store.list_active_workers_blocking().unwrap().remove(0);
    (root, store, worker)
}

#[test]
fn activity_follows_the_exact_worker_session_across_board_moves() {
    for folders in [false, true] {
        let (root, store, mut worker) = fixture(folders);
        let board = TaskBoard::new(get_tasks_dir(&worker.project_path));
        board
            .insert_content(TaskStatus::Todo, None, "Other task codex:other")
            .unwrap();
        board
            .insert_content(TaskStatus::Todo, None, "Feather mask codex:owned")
            .unwrap();
        for (session, token) in [("other", "other-worker"), ("owned", "worker")] {
            store
                .mark_session_running_blocking(
                    worker.project_id,
                    session,
                    std::process::id(),
                    token,
                    &root.join("out"),
                    &root.join("err"),
                )
                .unwrap();
        }
        let controls = store
            .session_controls_for_project_blocking(worker.project_id)
            .unwrap();
        let activity = describe_worker_activity(&worker, &controls);
        assert_eq!(activity.session_id.as_deref(), Some("owned"));
        assert_eq!(activity.summary, "Working on: TODO #2 — Feather mask");
        board
            .move_task_after_lock(TaskStatus::Todo, TaskStatus::Doing, 2)
            .unwrap();
        worker.task_selection = "resume_session".into();
        worker.resume_session_id = Some("owned".into());
        let activity = describe_worker_activity(&worker, &controls);
        assert_eq!(activity.phase, WorkerActivityPhase::Resuming);
        assert_eq!(activity.summary, "Resuming: DOING #1 — Feather mask");
        board
            .move_task_after_lock(TaskStatus::Doing, TaskStatus::Done, 1)
            .unwrap();
        worker.state = "finalizing".into();
        let activity = describe_worker_activity(&worker, &controls);
        assert_eq!(activity.phase, WorkerActivityPhase::Settling);
        assert!(activity.summary.contains("DONE #1 — Feather mask"));
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn activity_does_not_guess_a_task_from_queue_order_or_reviewer_session() {
    let (root, store, mut worker) = fixture(false);
    TaskBoard::new(get_tasks_dir(&worker.project_path))
        .insert_content(TaskStatus::Doing, None, "Unrelated task")
        .unwrap();
    let activity = describe_worker_activity(&worker, &[]);
    assert_eq!(activity.phase, WorkerActivityPhase::Starting);
    assert!(activity.summary.contains("waiting for task ownership"));
    assert!(!activity.summary.contains("Unrelated"));
    worker.task_selection = "supervisor_review".into();
    let activity = describe_worker_activity(&worker, &[]);
    assert_eq!(activity.phase, WorkerActivityPhase::Reviewing);
    assert_eq!(
        activity.summary,
        "Running: supervisor review of blocked tasks"
    );
    worker.task_selection = "resume_session".into();
    worker.resume_session_id = Some("missing-session".into());
    assert!(
        describe_worker_activity(&worker, &[])
            .summary
            .contains("waiting for its task link")
    );
    store
        .mark_session_running_blocking(
            worker.project_id,
            "conflicting-session",
            std::process::id(),
            "worker",
            &root.join("out"),
            &root.join("err"),
        )
        .unwrap();
    let controls = store
        .session_controls_for_project_blocking(worker.project_id)
        .unwrap();
    let ambiguous = describe_worker_activity(&worker, &controls);
    assert!(ambiguous.summary.contains("ambiguous"));
    assert!(ambiguous.session_id.is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn activity_locates_nested_tasks_and_reports_duplicate_links() {
    let (root, _store, mut worker) = fixture(true);
    let nested = get_tasks_dir(&worker.project_path).join("doing/0001-epic");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("task.md"), "Parent epic").unwrap();
    fs::write(nested.join("todo.md"), "- Nested task codex:owned\n").unwrap();
    worker.resume_session_id = Some("owned".into());
    let activity = describe_worker_activity(&worker, &[]);
    assert!(
        activity
            .summary
            .contains("doing/0001-epic / TODO #1 — Nested task")
    );
    fs::write(nested.join("doing.md"), "- Duplicate task codex:owned\n").unwrap();
    assert!(
        describe_worker_activity(&worker, &[])
            .summary
            .contains("multiple task links")
    );
    fs::remove_dir_all(root).unwrap();
}
