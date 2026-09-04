use crate::test_support::prelude::*;
use crate::test_support::*;
use crate::tui::tests::tui_agent_project_for_test;

fn assert_folder_move_preserves_managed_destination(automated_completion: bool) {
    let root = temp_root("managed-destination-conversion");
    init_tasks(&root, false).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let board_dir = root.join("tasks");
    let todo_dir = convert_status_to_directory(&board_dir, TaskStatus::Todo).unwrap();
    let source = todo_dir.join("0001-unrelated-task.md");
    let source_content = "Unrelated task. COMPLETED 2026-09-04: checked codex:session-unrelated\n";
    fs::write(&source, source_content).unwrap();
    let destination = if automated_completion {
        TaskStatus::Done
    } else {
        TaskStatus::Doing
    };
    let destination_file = board_dir.join(destination.filename());
    let destination_content = format!(
        "{}- Protected task codex:session-protected\n",
        destination.header()
    );
    fs::write(&destination_file, &destination_content).unwrap();

    let store = open_agent_store().unwrap();
    store.register_project_blocking(&root, "project").unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.git_mode, AgentGitMode::Off);
    assert!(
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: "session-protected",
                git_mode: AgentGitMode::Commit,
                starting_head: Some("1111111111111111111111111111111111111111"),
                branch_ref: Some("refs/heads/master"),
                upstream_ref: None,
                worktree_baseline: "{}",
                task_identity: None,
                owner_run_token: None,
                created_at: "100",
            })
            .unwrap()
    );

    let result = if automated_completion {
        move_task_to_done_with_agent_store(
            &root,
            TaskStatus::Todo,
            "1",
            &AutomatedAgentChildContext {
                project_id: project.id,
                run_token: "run-unrelated".to_string(),
            },
            &store,
        )
        .map(|_| ())
    } else {
        ManagedTaskWorkflow::new(&root).move_task(TaskStatus::Todo, destination, "1")
    };
    let error = result.expect_err("a task move must not convert protected destination evidence");
    assert!(format!("{error:#}").contains("managed Git journal"));
    assert_eq!(fs::read_to_string(&source).unwrap(), source_content);
    assert_eq!(
        fs::read_to_string(&destination_file).unwrap(),
        destination_content
    );
    assert!(!board_dir.join(destination.as_str()).exists());
    assert!(
        !board_dir
            .join(format!("{}.bak", destination.filename()))
            .exists()
    );

    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn folder_move_preserves_managed_destination() {
    assert_folder_move_preserves_managed_destination(false);
}

#[test]
fn automated_folder_completion_preserves_managed_destination() {
    assert_folder_move_preserves_managed_destination(true);
}

#[test]
fn agent_codex_session_id_parser_reads_the_exec_header() {
    assert_eq!(
        parse_agent_codex_session_id("session id: 019fe7ab-f267-76e3-b82c-d7c5705be8d1").as_deref(),
        Some("019fe7ab-f267-76e3-b82c-d7c5705be8d1")
    );
    assert_eq!(parse_agent_codex_session_id("session id:"), None);
    assert_eq!(parse_agent_codex_session_id("other output"), None);
}

#[test]
fn terminal_codex_session_marker_is_parsed_and_hidden_from_task_text() {
    let content = "Fix keyboard navigation — COMPLETED 2026-08-25: done codex:019fe7ab-f267-76e3-b82c-d7c5705be8d1\n";

    assert_eq!(
        codex_session_id_from_task_content(content),
        Some("019fe7ab-f267-76e3-b82c-d7c5705be8d1")
    );
    assert_eq!(
        task_content_without_codex_session(content),
        "Fix keyboard navigation — COMPLETED 2026-08-25: done"
    );
    assert_eq!(
        normalize_task_text(content),
        "Fix keyboard navigation — COMPLETED 2026-08-25: done"
    );
    assert_eq!(
        codex_session_id_from_task_content("codex:session-123 is mentioned in the task"),
        None
    );
    assert_eq!(
        normalize_task_text("codex:session-123 is mentioned in the task"),
        "codex:session-123 is mentioned in the task"
    );
    assert_eq!(codex_session_id_from_task_content("task codex:"), None);
}

#[test]
fn linked_unfinished_tasks_only_display_stopped_flags() {
    let running = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 0 },
        "running task codex:session-running",
        "running task codex:session-running",
        false,
    );
    let stopped = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 1 },
        "stopped task codex:session-stopped",
        "stopped task codex:session-stopped",
        false,
    );
    let session_states = TaskAgentSessionStates::from([
        (
            "session-running".to_string(),
            AgentSessionControlState::Interactive,
        ),
        (
            "session-stopped".to_string(),
            AgentSessionControlState::Stopped,
        ),
    ]);

    assert_eq!(
        task_display_text_with_agent_flag(&running, TaskStatus::Doing, &session_states),
        "running task"
    );
    assert_eq!(
        task_tui_display_text_with_agent_flag(&stopped, TaskStatus::Doing, true, &session_states),
        "[STOPPED] stopped task"
    );
    assert_eq!(
        task_display_text_with_agent_flag(&stopped, TaskStatus::Done, &session_states),
        "stopped task"
    );
}

#[test]
fn displaced_codex_session_marker_is_recovered_and_repositioned() {
    let content = "Fix keyboard navigation. codex:session-123\n\nCompletion note.\n";

    assert_eq!(codex_session_id_from_task_content(content), None);
    assert_eq!(
        recoverable_codex_session_id_from_task_content(content),
        Some("session-123")
    );
    assert_eq!(
        task_content_without_recoverable_codex_session(content),
        "Fix keyboard navigation.\n\nCompletion note."
    );
    assert_eq!(
        task_content_with_codex_session(content, "session-123"),
        "Fix keyboard navigation.\n\nCompletion note. codex:session-123"
    );

    let session_id = "019fe7ab-f267-76e3-b82c-d7c5705be8d1";
    let inline = format!("Fix keyboard navigation codex:{session_id} — COMPLETED: done");
    assert_eq!(
        recoverable_codex_session_id_from_task_content(&inline),
        Some(session_id)
    );
    assert_eq!(
        task_content_with_codex_session(&inline, session_id),
        format!("Fix keyboard navigation — COMPLETED: done codex:{session_id}")
    );
}

#[test]
fn task_edit_hides_and_preserves_terminal_codex_session_marker() {
    let root = temp_root("edit-codex-session-marker");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/done.md"),
        "# Done Tasks\n- original task codex:session-123\n",
    )
    .unwrap();
    let board_dir = root.join("tasks");
    let entry = read_task_entries(&board_dir, TaskStatus::Done)
        .unwrap()
        .remove(0);

    assert_eq!(task_display_text(&entry), "original task");
    assert_eq!(task_full_display_text(&entry), "original task");
    assert_eq!(
        task_content_without_codex_session(&entry.content),
        "original task"
    );

    update_task_in_board(&board_dir, TaskStatus::Done, 1, "edited task").unwrap();

    assert_eq!(
        fs::read_to_string(root.join("tasks/done.md")).unwrap(),
        "# Done Tasks\n- edited task codex:session-123\n"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn working_git_task_updates_preserve_the_stable_payload() {
    let identity = durable_task_identity("Implement durable finalization").unwrap();
    ensure_working_task_content_preserves_identity(
            "session-working",
            &identity,
            "Implement durable finalization\n\nCompletion note:\n- COMPLETED 2026-09-02: cargo test passed codex:session-working",
        )
        .unwrap();

    let error = ensure_working_task_content_preserves_identity(
        "session-working",
        &identity,
        "Implement a different feature codex:session-working",
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("cannot change its durable task payload"));
}

#[test]
fn folder_task_stores_codex_session_marker_without_displaying_it() {
    let root = temp_root("folder-codex-session-marker");
    init_tasks(&root, true).unwrap();
    let done_path = root.join("tasks/done/0001-finished-task.md");
    fs::write(&done_path, "Finished task.\n\nCompletion details.\n").unwrap();
    let board_dir = root.join("tasks");
    let entry = read_task_entries(&board_dir, TaskStatus::Done)
        .unwrap()
        .remove(0);

    let content =
        attach_codex_session_to_task(&root, TaskStatus::Done, &entry, "session-456").unwrap();

    assert_eq!(
        content,
        "Finished task.\n\nCompletion details. codex:session-456"
    );
    assert_eq!(
        fs::read_to_string(&done_path).unwrap(),
        "Finished task.\n\nCompletion details. codex:session-456\n"
    );
    let entry = read_task_entries(&board_dir, TaskStatus::Done)
        .unwrap()
        .remove(0);
    assert_eq!(task_display_text(&entry), "Finished task.");
    assert_eq!(
        task_full_display_text(&entry),
        "Finished task. Completion details."
    );

    update_task_in_board(
        &board_dir,
        TaskStatus::Done,
        1,
        "Edited task.\n\nNew details.",
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&done_path).unwrap(),
        "Edited task.\n\nNew details. codex:session-456\n"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_edits_move_displaced_codex_session_markers_to_the_end() {
    let root = temp_root("edit-displaced-codex-session-marker");
    init_tasks(&root, true).unwrap();
    let done_path = root.join("tasks/done/0001-finished-task.md");
    fs::write(
        &done_path,
        "Finished task. codex:session-456\n\nCompletion note.\n",
    )
    .unwrap();
    let board_dir = root.join("tasks");
    let entry = read_task_entries(&board_dir, TaskStatus::Done)
        .unwrap()
        .remove(0);

    assert_eq!(
        codex_session_for_task(&entry).as_deref(),
        Some("session-456")
    );
    assert_eq!(task_display_text(&entry), "Finished task.");
    update_task_in_board(
        &board_dir,
        TaskStatus::Done,
        1,
        "Edited task.\n\nUpdated completion note.",
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(&done_path).unwrap(),
        "Edited task.\n\nUpdated completion note. codex:session-456\n"
    );
    let updated = read_task_entries(&board_dir, TaskStatus::Done)
        .unwrap()
        .remove(0);
    assert_eq!(
        codex_session_id_from_task_content(&updated.content),
        Some("session-456")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn moving_a_markdown_task_preserves_its_codex_session_marker() {
    let root = temp_root("move-codex-session-marker");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/doing.md"),
        "# Doing Tasks\n- resumable task codex:session-123\n",
    )
    .unwrap();

    move_task(&root, TaskStatus::Doing, TaskStatus::Done, "1").unwrap();

    let done = read_task_entries(&get_tasks_dir(&root), TaskStatus::Done)
        .unwrap()
        .remove(0);
    assert_eq!(
        codex_session_for_task(&done).as_deref(),
        Some("session-123")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn codex_session_attachment_does_not_overwrite_a_concurrent_markdown_edit() {
    let root = temp_root("markdown-session-attachment-cas");
    init_tasks(&root, false).unwrap();
    let doing_path = root.join("tasks/doing.md");
    fs::write(&doing_path, "# Doing Tasks\n- original task\n").unwrap();
    let stale = read_task_entries(&root.join("tasks"), TaskStatus::Doing)
        .unwrap()
        .remove(0);
    fs::write(&doing_path, "# Doing Tasks\n- concurrently edited task\n").unwrap();

    assert!(attach_codex_session_to_task(&root, TaskStatus::Doing, &stale, "session-123").is_err());
    assert_eq!(
        fs::read_to_string(&doing_path).unwrap(),
        "# Doing Tasks\n- concurrently edited task\n"
    );

    let fresh = read_task_entries(&root.join("tasks"), TaskStatus::Doing)
        .unwrap()
        .remove(0);
    attach_codex_session_to_task(&root, TaskStatus::Doing, &fresh, "session-123").unwrap();
    assert_eq!(
        fs::read_to_string(&doing_path).unwrap(),
        "# Doing Tasks\n- concurrently edited task codex:session-123\n"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn codex_session_attachment_does_not_overwrite_a_concurrent_folder_task_edit() {
    let root = temp_root("folder-session-attachment-cas");
    init_tasks(&root, true).unwrap();
    let doing_path = root.join("tasks/doing/0001-active-task.md");
    fs::write(&doing_path, "Original task.\n").unwrap();
    let stale = read_task_entries(&root.join("tasks"), TaskStatus::Doing)
        .unwrap()
        .remove(0);
    fs::write(&doing_path, "Concurrently edited task.\n").unwrap();

    assert!(attach_codex_session_to_task(&root, TaskStatus::Doing, &stale, "session-123").is_err());
    assert_eq!(
        fs::read_to_string(&doing_path).unwrap(),
        "Concurrently edited task.\n"
    );

    let fresh = read_task_entries(&root.join("tasks"), TaskStatus::Doing)
        .unwrap()
        .remove(0);
    attach_codex_session_to_task(&root, TaskStatus::Doing, &fresh, "session-123").unwrap();
    assert_eq!(
        fs::read_to_string(&doing_path).unwrap(),
        "Concurrently edited task. codex:session-123\n"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn codex_session_attachment_serializes_with_a_concurrent_task_edit() {
    let root = temp_root("session-attachment-concurrent-edit");
    init_tasks(&root, false).unwrap();
    let board_dir = root.join("tasks");
    fs::write(
        board_dir.join("doing.md"),
        "# Doing Tasks\n- original task\n",
    )
    .unwrap();
    let entry = read_task_entries(&board_dir, TaskStatus::Doing)
        .unwrap()
        .remove(0);

    let (marker_ready_tx, marker_ready_rx) = mpsc::channel();
    let (release_marker_tx, release_marker_rx) = mpsc::channel();
    let marker_root = root.clone();
    let marker_thread = thread::spawn(move || {
        attach_codex_session_to_task_with_before_replace(
            &marker_root,
            TaskStatus::Doing,
            &entry,
            "session-123",
            move || {
                marker_ready_tx.send(()).unwrap();
                release_marker_rx.recv().unwrap();
            },
        )
    });
    marker_ready_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("marker attachment did not reach its final check");

    let (edit_contended_tx, edit_contended_rx) = mpsc::channel();
    let edit_board_dir = board_dir.clone();
    let edit_thread = thread::spawn(move || {
        update_task_in_board_with_contention_callback(
            &edit_board_dir,
            TaskStatus::Doing,
            1,
            "edited task",
            move || edit_contended_tx.send(()).unwrap(),
        )
    });
    let edit_contended = edit_contended_rx.recv_timeout(Duration::from_secs(2));
    release_marker_tx.send(()).unwrap();

    marker_thread.join().unwrap().unwrap();
    edit_thread.join().unwrap().unwrap();
    assert!(
        edit_contended.is_ok(),
        "task edit did not wait for the in-flight marker attachment"
    );
    assert_eq!(
        fs::read_to_string(board_dir.join("doing.md")).unwrap(),
        "# Doing Tasks\n- edited task codex:session-123\n"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn codex_session_attachment_serializes_with_a_concurrent_task_move() {
    let root = temp_root("session-attachment-concurrent-move");
    init_tasks(&root, false).unwrap();
    let board_dir = root.join("tasks");
    fs::write(
        board_dir.join("doing.md"),
        "# Doing Tasks\n- original task\n",
    )
    .unwrap();
    let entry = read_task_entries(&board_dir, TaskStatus::Doing)
        .unwrap()
        .remove(0);

    let (marker_ready_tx, marker_ready_rx) = mpsc::channel();
    let (release_marker_tx, release_marker_rx) = mpsc::channel();
    let marker_root = root.clone();
    let marker_thread = thread::spawn(move || {
        attach_codex_session_to_task_with_before_replace(
            &marker_root,
            TaskStatus::Doing,
            &entry,
            "session-123",
            move || {
                marker_ready_tx.send(()).unwrap();
                release_marker_rx.recv().unwrap();
            },
        )
    });
    marker_ready_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("marker attachment did not reach its final check");

    let (move_contended_tx, move_contended_rx) = mpsc::channel();
    let move_board_dir = board_dir.clone();
    let move_thread = thread::spawn(move || {
        move_task_in_board_with_contention_callback(
            &move_board_dir,
            TaskStatus::Doing,
            TaskStatus::Done,
            "1",
            move || move_contended_tx.send(()).unwrap(),
        )
    });
    let move_contended = move_contended_rx.recv_timeout(Duration::from_secs(2));
    release_marker_tx.send(()).unwrap();

    marker_thread.join().unwrap().unwrap();
    move_thread.join().unwrap().unwrap();
    assert!(
        move_contended.is_ok(),
        "task move did not wait for the in-flight marker attachment"
    );
    assert!(
        read_task_entries(&board_dir, TaskStatus::Doing)
            .unwrap()
            .is_empty()
    );
    let done = read_task_entries(&board_dir, TaskStatus::Done).unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(
        codex_session_for_task(&done[0]).as_deref(),
        Some("session-123")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_run_does_not_copy_an_existing_live_session_marker_to_another_task() {
    let root = temp_root("completion-session-marker-dedup");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/done.md"),
        "# Done Tasks\n- unrelated concurrent completion\n- actual task codex:session-123\n",
    )
    .unwrap();
    let mut project = tui_agent_project_for_test(1, "project").project;
    project.path = root.clone();
    let job = AgentRunJob {
        state_dir: root.join("state/clt"),
        project,
        holder: "holder".to_string(),
        worker_token: None,
        max_global_jobs: 12,
        task_selection: AgentTaskSelection::NextTodo,
        resume_session_id: None,
        blocked_task_count_before: 0,
        done_task_contents_before: Vec::new(),
        blocked_task_snapshots_before: Vec::new(),
    };

    attach_codex_session_after_run(&job, "session-123", "success").unwrap();

    assert_eq!(
        fs::read_to_string(root.join("tasks/done.md")).unwrap(),
        "# Done Tasks\n- unrelated concurrent completion\n- actual task codex:session-123\n"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_run_reports_when_its_session_marker_target_is_ambiguous() {
    let root = temp_root("completion-session-marker-ambiguous");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/done.md"),
        "# Done Tasks\n- concurrent completion one\n- concurrent completion two\n",
    )
    .unwrap();
    let mut project = tui_agent_project_for_test(1, "project").project;
    project.path = root.clone();
    let job = AgentRunJob {
        state_dir: root.join("state/clt"),
        project,
        holder: "holder".to_string(),
        worker_token: None,
        max_global_jobs: 12,
        task_selection: AgentTaskSelection::NextTodo,
        resume_session_id: None,
        blocked_task_count_before: 0,
        done_task_contents_before: Vec::new(),
        blocked_task_snapshots_before: Vec::new(),
    };

    let error = attach_codex_session_after_run(&job, "session-123", "success")
        .expect_err("ambiguous completion must be reported");
    assert!(
        error
            .to_string()
            .contains("exactly one completed or blocked task")
    );
    assert!(
        !fs::read_to_string(root.join("tasks/done.md"))
            .unwrap()
            .contains("codex:session-123")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_and_blocked_outcomes_are_jointly_ambiguous_for_session_attachment() {
    let root = temp_root("completion-and-blocked-session-ambiguous");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/done.md"),
        "# Done Tasks\n- unrelated concurrent completion\n",
    )
    .unwrap();
    fs::write(
        root.join("tasks/todo.md"),
        "# Todo Tasks\n- agent target — BLOCKED 2026-08-25: waiting\n",
    )
    .unwrap();
    let mut project = tui_agent_project_for_test(1, "project").project;
    project.path = root.clone();
    let job = AgentRunJob {
        state_dir: root.join("state/clt"),
        project,
        holder: "holder".to_string(),
        worker_token: None,
        max_global_jobs: 12,
        task_selection: AgentTaskSelection::NextTodo,
        resume_session_id: None,
        blocked_task_count_before: 0,
        done_task_contents_before: Vec::new(),
        blocked_task_snapshots_before: Vec::new(),
    };

    assert!(attach_codex_session_after_run(&job, "session-123", "blocked").is_err());
    assert!(
        !fs::read_to_string(root.join("tasks/done.md"))
            .unwrap()
            .contains("codex:session-123")
    );
    assert!(
        !fs::read_to_string(root.join("tasks/todo.md"))
            .unwrap()
            .contains("codex:session-123")
    );

    fs::remove_dir_all(root).unwrap();
}
