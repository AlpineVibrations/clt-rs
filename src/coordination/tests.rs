use super::*;
use crate::test_support::{prelude::*, run_test_git};

fn task(id: u128, status: TaskStatus, description: &str, scope: Scope) -> Task {
    let meta = Metadata {
        id: uuid::Uuid::from_u128(id).to_string(),
        scope,
        ..Default::default()
    };
    let content = metadata::write(description, &meta).unwrap();
    Task {
        board: PathBuf::from("tasks"),
        status,
        index: id as usize,
        entry: task_entry_from_text(
            TaskSource::MarkdownLine {
                line_index: id as usize,
            },
            &content,
            &content,
            false,
        ),
        meta,
        error: None,
        completion_wait: None,
        owner: "CLI claimed; activity unconfirmed".into(),
    }
}

fn snapshot(tasks: Vec<Task>) -> Snapshot {
    Snapshot {
        tasks,
        ..Default::default()
    }
}

#[test]
fn stable_references_survive_edits_reordering_and_moves_in_both_layouts() {
    for folders in [false, true] {
        let root = tempfile::tempdir().unwrap();
        init_tasks(root.path(), folders).unwrap();
        add_task(root.path(), "Publish API", None).unwrap();
        add_task(root.path(), "Use API", None).unwrap();
        let api = update(root.path(), TaskStatus::Todo, 1, Update::default()).unwrap();
        let client = update(
            root.path(),
            TaskStatus::Todo,
            2,
            Update {
                depends_on: vec![api.clone()],
                ..Default::default()
            },
        )
        .unwrap();
        let board = get_tasks_dir(root.path());
        assert_eq!(ready_tasks(root.path()).unwrap().len(), 1);
        update_task_in_board(&board, TaskStatus::Todo, 1, "Publish the revised API").unwrap();
        reorder_task_in_board(&board, TaskStatus::Todo, 0, 1).unwrap();
        let tasks = TaskBoard::new(&board);
        assert_eq!(
            metadata::read(&tasks.entry(TaskStatus::Todo, 2).unwrap().content)
                .unwrap()
                .id,
            api
        );
        assert!(
            ensure_admitted(&board, TaskStatus::Todo, 1)
                .unwrap_err()
                .to_string()
                .contains(&api)
        );
        move_task(root.path(), TaskStatus::Todo, TaskStatus::Doing, "2").unwrap();
        move_task(root.path(), TaskStatus::Doing, TaskStatus::Done, "1").unwrap();
        let ready = ready_tasks(root.path()).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(metadata::read(&ready[0].content).unwrap().id, client);
        let stopped = format!("{} clt:stopped", ready[0].content);
        tasks
            .write_entry_content(TaskStatus::Todo, &ready[0], &stopped)
            .unwrap();
        assert!(ready_tasks(root.path()).unwrap().is_empty());
    }
}

#[test]
fn dependency_cycles_self_edges_missing_and_duplicate_ids_are_actionable() {
    let mut a = task(1, TaskStatus::Todo, "API", Scope::default());
    let mut b = task(2, TaskStatus::Todo, "Client", Scope::default());
    b.meta.depends_on.push(a.meta.id.clone());
    a.meta.depends_on.push(b.meta.id.clone());
    let view = snapshot(vec![a.clone(), b.clone()]);
    assert!(view.waits(&b)[0].contains("cycle"));
    a.meta.depends_on = vec![a.meta.id.clone()];
    assert!(snapshot(vec![a.clone()]).waits(&a)[0].contains("cycle"));
    assert!(snapshot(vec![b.clone()]).waits(&b)[0].contains("Missing prerequisite"));
    a.meta.depends_on.clear();
    let mut duplicate = a.clone();
    duplicate.index = 9;
    assert!(snapshot(vec![a, duplicate, b.clone()]).waits(&b)[0].contains("Duplicate task ID"));

    let root = tempfile::tempdir().unwrap();
    init_tasks(root.path(), false).unwrap();
    add_task(root.path(), "API", None).unwrap();
    add_task(root.path(), "Client", None).unwrap();
    let a = update(root.path(), TaskStatus::Todo, 1, Update::default()).unwrap();
    let b = update(
        root.path(),
        TaskStatus::Todo,
        2,
        Update {
            depends_on: vec![a.clone()],
            ..Default::default()
        },
    )
    .unwrap();
    for dependency in [a, b] {
        assert!(
            update(
                root.path(),
                TaskStatus::Todo,
                1,
                Update {
                    depends_on: vec![dependency],
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    assert!(
        metadata::read(
            &TaskBoard::for_project(root.path())
                .entry(TaskStatus::Todo, 1)
                .unwrap()
                .content
        )
        .unwrap()
        .depends_on
        .is_empty()
    );
}

#[test]
fn coordination_waits_leave_other_ready_tasks_eligible_without_blocker_notes() {
    let root = tempfile::tempdir().unwrap();
    init_tasks(root.path(), false).unwrap();
    add_task(root.path(), "Publish API", None).unwrap();
    add_task(root.path(), "Consume API", None).unwrap();
    add_task(root.path(), "Independent work", None).unwrap();
    let id = update(root.path(), TaskStatus::Todo, 1, Update::default()).unwrap();
    update(
        root.path(),
        TaskStatus::Todo,
        2,
        Update {
            depends_on: vec![id],
            ..Default::default()
        },
    )
    .unwrap();
    move_task(root.path(), TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
    let scan = scan_agent_project(root.path());
    assert_eq!(scan.coordination_wait_count, 1);
    assert_eq!(scan.blocked_todo_count, 0);
    assert_eq!(scan.available_todo_count(), 1);
    assert_eq!(
        ready_tasks(root.path()).unwrap()[0].summary,
        "Independent work"
    );
    move_task(root.path(), TaskStatus::Doing, TaskStatus::Done, "1").unwrap();
    assert_eq!(scan_agent_project(root.path()).available_todo_count(), 2);
}

#[test]
fn a_waiting_active_consumer_does_not_deadlock_its_prerequisite() {
    let a = task(
        1,
        TaskStatus::Doing,
        "Publish API",
        Scope {
            interfaces: vec!["Response".into()],
            ..Default::default()
        },
    );
    let mut b = task(2, TaskStatus::Doing, "Consume API", a.meta.scope.clone());
    b.meta.depends_on.push(a.meta.id.clone());
    let view = snapshot(vec![a.clone(), b.clone()]);
    assert!(!view.waits(&b).is_empty());
    assert!(view.waits(&a).is_empty());
}

#[test]
fn unfinished_git_proof_keeps_prerequisite_waiting_until_publication() {
    let root = tempfile::tempdir().unwrap();
    let project_root = root.path().join("project");
    init_tasks(&project_root, false).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&root.path().join("state")).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    store
        .register_project_blocking(&project_root, "test")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let mut api = task(
        1,
        TaskStatus::Done,
        "Publish API codex:api-session",
        Scope::default(),
    );
    api.board = get_tasks_dir(&project_root);
    let mut client = task(2, TaskStatus::Todo, "Use API", Scope::default());
    client.meta.depends_on = vec![api.meta.id.clone()];
    store
        .create_git_finalization_blocking(agent::NewGitFinalization {
            project_id: project.id,
            codex_session_id: "api-session",
            git_mode: AgentGitMode::CommitAndPush,
            starting_head: Some("head"),
            branch_ref: Some("refs/heads/main"),
            upstream_ref: None,
            worktree_baseline: "{}",
            task_identity: Some("api"),
            owner_run_token: None,
            created_at: "1",
        })
        .unwrap();
    for state in [
        GitFinalizationState::Working,
        GitFinalizationState::Tracking,
        GitFinalizationState::CommitPending,
        GitFinalizationState::PushPending,
        GitFinalizationState::Completed,
    ] {
        let old = store
            .git_finalization_blocking(project.id, "api-session")
            .unwrap()
            .unwrap();
        if old.state != state {
            assert!(
                store
                    .compare_and_set_git_finalization_blocking(
                        project.id,
                        "api-session",
                        old.generation,
                        state,
                        None,
                        Some("commit"),
                        None,
                        "2"
                    )
                    .unwrap()
            );
        }
        let mut view = snapshot(vec![api.clone(), client.clone()]);
        view.load_runtime_from_store(&get_tasks_dir(&project_root), &store)
            .unwrap();
        assert_eq!(
            view.waits(&client).is_empty(),
            state == GitFinalizationState::Completed
        );
        if state == GitFinalizationState::PushPending {
            assert!(view.waits(&client)[0].contains("PUSH-PENDING"));
        }
    }
}

#[test]
fn independent_manual_and_automated_work_share_context_without_waiting() {
    let a = task(
        1,
        TaskStatus::Doing,
        "Fix UI clt:manual codex:manual",
        Scope {
            intent: "Align buttons".into(),
            files: vec!["ui.rs".into()],
            ..Default::default()
        },
    );
    let b = task(
        2,
        TaskStatus::Todo,
        "Optimize parser",
        Scope {
            files: vec!["parser.rs".into()],
            ..Default::default()
        },
    );
    let mut view = snapshot(vec![a.clone(), b.clone()]);
    view.changed_files
        .extend(["ui.rs".into(), "unrelated.txt".into()]);
    assert!(view.waits(&b).is_empty());
    assert!(view.ready(&b.entry));
    let context = view.context(None);
    assert!(context.contains("Align buttons"));
    assert!(context.contains("CLI claimed; activity unconfirmed"));
    assert!(context.contains("codex:manual"));
    assert!(context.contains("unrelated.txt"));
}

#[test]
fn incompatible_behavior_across_different_files_is_a_specific_conflict() {
    let a = task(
        1,
        TaskStatus::Doing,
        "Order scheduler",
        Scope {
            files: vec!["scheduler.rs".into()],
            behavior: [("queue.order".into(), "fifo".into())].into(),
            ..Default::default()
        },
    );
    let b = task(
        2,
        TaskStatus::Todo,
        "Change admission",
        Scope {
            files: vec!["admission.rs".into()],
            behavior: [("queue.order".into(), "lifo".into())].into(),
            ..Default::default()
        },
    );
    let view = snapshot(vec![a.clone(), b.clone()]);
    let waits = view.waits(&b);
    assert!(waits[0].contains("queue.order"));
    assert!(waits[0].contains("fifo"));
    assert!(waits[0].contains("lifo"));
    let c = task(
        3,
        TaskStatus::Todo,
        "Update documentation",
        Scope::default(),
    );
    let mut view = view;
    view.tasks.push(c.clone());
    assert!(view.waits(&c).is_empty());
}

#[test]
fn compatible_shared_file_review_survives_edits_but_scope_changes_invalidate_it() {
    let a = task(
        1,
        TaskStatus::Doing,
        "Fix parser",
        Scope {
            files: vec!["src/common.rs".into()],
            ..Default::default()
        },
    );
    let mut b = task(2, TaskStatus::Todo, "Fix renderer", a.meta.scope.clone());
    let mut view = snapshot(vec![a.clone(), b.clone()]);
    view.changed_files.insert("src/common.rs".into());
    assert!(view.waits(&b)[0].contains("Potential overlap"));
    b.meta.reviews.push(Review {
        other: a.meta.id.clone(),
        evidence: evidence(&a, &b),
        compatible: true,
        reason: "Parser and renderer touch distinct functions; neither changes the shared type"
            .into(),
    });
    view.tasks[1] = b.clone();
    assert!(view.waits(&b).is_empty());
    let mut a = a;
    a.meta.scope.interfaces.push("SharedType".into());
    view.tasks[0] = a;
    assert!(!view.waits(&b).is_empty());
    view.tasks[0].status = TaskStatus::Done;
    assert!(view.waits(&b).is_empty());
}

#[test]
fn requirements_and_semantic_scope_detect_cross_file_overlap() {
    let a = task(
        1,
        TaskStatus::Doing,
        "Change `api::Response` in producer",
        Scope {
            files: vec!["producer.rs".into()],
            ..Default::default()
        },
    );
    let b = task(
        2,
        TaskStatus::Todo,
        "Consume `api::Response` in client",
        Scope {
            files: vec!["client.rs".into()],
            ..Default::default()
        },
    );
    assert!(snapshot(vec![a, b.clone()]).waits(&b)[0].contains("requirements name api::Response"));
    let a = task(
        1,
        TaskStatus::Doing,
        "Modify the parser",
        Scope {
            files: vec!["src/parser.rs".into()],
            ..Default::default()
        },
    );
    let b = task(2, TaskStatus::Todo, "Use `src/parser.rs`", Scope::default());
    assert!(
        snapshot(vec![a, b.clone()]).waits(&b)[0]
            .contains("declared scope meets requirement src/parser.rs")
    );
    for kind in ["module", "interface", "resource"] {
        let mut scope = Scope::default();
        match kind {
            "module" => scope.modules.push("scheduler".into()),
            "interface" => scope.interfaces.push("Queue".into()),
            _ => scope.resources.push("migration-19".into()),
        };
        let a = task(1, TaskStatus::Doing, "First", scope.clone());
        let b = task(2, TaskStatus::Todo, "Second", scope);
        assert!(snapshot(vec![a, b.clone()]).waits(&b)[0].contains(kind));
    }
}

#[test]
fn metadata_keeps_session_stop_markers_and_existing_git_identity() {
    let original = "Implement feature.\n\nRequirements here. codex:owned-session clt:stopped";
    let meta = Metadata {
        id: uuid::Uuid::new_v4().to_string(),
        scope: Scope {
            intent: "Use <!-- --> safely".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let updated = metadata::write(original, &meta).unwrap();
    assert_eq!(metadata::read(&updated).unwrap(), meta);
    assert!(updated.ends_with("codex:owned-session clt:stopped"));
    assert_eq!(
        durable_task_identity(original),
        durable_task_identity(&updated)
    );
    let edited = metadata::preserve(&updated, "Reworded description").unwrap();
    assert_eq!(metadata::read(&edited).unwrap().id, meta.id);
    assert!(!task_content_for_edit(&updated).contains("clt-coordination"));
}

#[test]
fn finalization_lock_excludes_only_staging_and_recovers_after_owner_exit() {
    let root = tempfile::tempdir().unwrap();
    run_test_git(root.path(), &["init"]);
    fs::write(root.path().join("shared.txt"), "user work").unwrap();
    run_test_git(root.path(), &["add", "shared.txt"]);
    let staged = run_test_git(root.path(), &["write-tree"]);
    let owner = finalization::acquire(root.path(), "first-session").unwrap();
    let error = finalization::acquire(root.path(), "second-session")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("first-session"));
    fs::write(root.path().join("other.txt"), "parallel edits").unwrap();
    assert_eq!(staged, run_test_git(root.path(), &["write-tree"]));
    drop(owner);
    assert!(finalization::acquire(root.path(), "second-session").is_ok());
    assert_eq!(staged, run_test_git(root.path(), &["write-tree"]));
}
