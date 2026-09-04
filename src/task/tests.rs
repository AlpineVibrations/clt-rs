use crate::test_support::prelude::*;
use crate::test_support::*;

#[test]
fn add_task_creates_missing_task_store() {
    let root = temp_root("auto-init");

    let result = add_task(&root, "write from a fresh directory", None);

    assert!(result.is_ok());
    let todo = fs::read_to_string(root.join("tasks/todo.md")).unwrap();
    let doing = fs::read_to_string(root.join("tasks/doing.md")).unwrap();
    let done = fs::read_to_string(root.join("tasks/done.md")).unwrap();
    let backlog = fs::read_to_string(root.join("tasks/backlog.md")).unwrap();

    assert!(todo.contains("# To Do Tasks"));
    assert!(todo.contains("- write from a fresh directory"));
    assert_eq!(doing, "# Doing Tasks\n");
    assert_eq!(done, "# Done Tasks\n");
    assert_eq!(backlog, "# Backlog Tasks\n");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn init_tasks_can_create_folder_backed_statuses() {
    let root = temp_root("init-folders");

    init_tasks(&root, true).unwrap();

    assert!(root.join("tasks/todo").is_dir());
    assert!(root.join("tasks/doing").is_dir());
    assert!(root.join("tasks/done").is_dir());
    assert!(root.join("tasks/backlog").is_dir());
    assert!(!root.join("tasks/todo.md").exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn existing_folder_board_repairs_status_directories_missing_after_clone() {
    let root = temp_root("repair-folder-board");
    let done_dir = root.join("tasks/done");
    fs::create_dir_all(&done_dir).unwrap();
    fs::write(done_dir.join("0001-shipped.md"), "Shipped already.\n").unwrap();

    assert!(ensure_existing_board(&root).unwrap());
    assert!(root.join("tasks/todo").is_dir());
    assert!(root.join("tasks/doing").is_dir());
    assert!(root.join("tasks/done").is_dir());
    assert!(root.join("tasks/backlog").is_dir());
    assert!(done_dir.join("0001-shipped.md").is_file());
    assert!(!root.join("tasks/todo.md").exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn existing_markdown_board_repairs_missing_status_files() {
    let root = temp_root("repair-markdown-board");
    let tasks_dir = root.join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(
        tasks_dir.join("done.md"),
        "# Done Tasks\n- shipped already\n",
    )
    .unwrap();

    assert!(ensure_existing_board(&root).unwrap());
    assert_eq!(
        fs::read_to_string(tasks_dir.join("todo.md")).unwrap(),
        "# To Do Tasks\n"
    );
    assert_eq!(
        fs::read_to_string(tasks_dir.join("doing.md")).unwrap(),
        "# Doing Tasks\n"
    );
    assert_eq!(
        fs::read_to_string(tasks_dir.join("done.md")).unwrap(),
        "# Done Tasks\n- shipped already\n"
    );
    assert_eq!(
        fs::read_to_string(tasks_dir.join("backlog.md")).unwrap(),
        "# Backlog Tasks\n"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tasks_directory_without_status_store_stays_uninitialized() {
    let root = temp_root("unrecognized-tasks-directory");
    let tasks_dir = root.join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(tasks_dir.join("notes.md"), "Not a task board.\n").unwrap();

    assert!(!ensure_existing_board(&root).unwrap());
    assert!(!tasks_dir.join("todo.md").exists());
    assert!(!tasks_dir.join("todo").exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ensure_task_store_preserves_existing_files() {
    let root = temp_root("preserve");
    let tasks_dir = root.join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(tasks_dir.join("todo.md"), "# Custom Todo\n- keep me\n").unwrap();

    ensure_task_store(&root).unwrap();

    let todo = fs::read_to_string(tasks_dir.join("todo.md")).unwrap();
    let doing = fs::read_to_string(tasks_dir.join("doing.md")).unwrap();
    let done = fs::read_to_string(tasks_dir.join("done.md")).unwrap();
    let backlog = fs::read_to_string(tasks_dir.join("backlog.md")).unwrap();

    assert_eq!(todo, "# Custom Todo\n- keep me\n");
    assert_eq!(doing, "# Doing Tasks\n");
    assert_eq!(done, "# Done Tasks\n");
    assert_eq!(backlog, "# Backlog Tasks\n");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn expand_tasks_can_expand_one_markdown_status_to_folder() {
    let root = temp_root("expand-one");
    let tasks_dir = root.join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(
        tasks_dir.join("todo.md"),
        "# To Do Tasks\n- first task\n- second task\n",
    )
    .unwrap();
    fs::write(tasks_dir.join("doing.md"), "# Doing Tasks\n").unwrap();
    fs::write(tasks_dir.join("done.md"), "# Done Tasks\n").unwrap();

    expand_tasks(&root, Some("todo".to_string())).unwrap();

    assert!(tasks_dir.join("todo").is_dir());
    assert!(tasks_dir.join("todo.md.bak").exists());
    assert!(tasks_dir.join("doing.md").exists());
    let entries = read_task_entries(&tasks_dir, TaskStatus::Todo).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].summary, "first task");
    assert_eq!(entries[1].summary, "second task");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn expand_tasks_without_status_expands_all_statuses() {
    let root = temp_root("expand-all");
    add_task(&root, "todo task", None).unwrap();
    move_task(&root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();

    expand_tasks(&root, None).unwrap();

    assert!(root.join("tasks/todo").is_dir());
    assert!(root.join("tasks/doing").is_dir());
    assert!(root.join("tasks/done").is_dir());
    assert!(root.join("tasks/backlog").is_dir());
    assert!(root.join("tasks/todo.md.bak").exists());
    assert!(root.join("tasks/doing.md.bak").exists());
    assert!(root.join("tasks/done.md.bak").exists());
    assert!(root.join("tasks/backlog.md.bak").exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn insert_subtask_expands_markdown_parent_and_reuses_nested_board() {
    let root = temp_root("insert-subtask-markdown");
    add_task(&root, "Ship dashboard", Some("FEATURE".to_string())).unwrap();
    add_task(&root, "Keep sibling", None).unwrap();
    let tasks_dir = root.join("tasks");
    let expected_parent = task_entry_at(&tasks_dir, TaskStatus::Todo, 1).unwrap();

    let subtask_board = insert_subtask_in_board(
        &tasks_dir,
        TaskStatus::Todo,
        1,
        &expected_parent,
        "Draft dashboard spec",
        Some("DOCS".to_string()),
    )
    .unwrap();

    assert!(tasks_dir.join("todo").is_dir());
    assert!(tasks_dir.join("todo.md.bak").is_file());
    let parent_entries = read_task_entries(&tasks_dir, TaskStatus::Todo).unwrap();
    assert_eq!(parent_entries.len(), 2);
    assert_eq!(parent_entries[0].summary, "Ship dashboard");
    assert_eq!(parent_entries[0].metadata.as_deref(), Some("FEATURE"));
    assert!(parent_entries[0].has_subtasks);
    assert_eq!(parent_entries[1].summary, "Keep sibling");
    assert_eq!(
        fs::read_to_string(subtask_board.join("task.md")).unwrap(),
        "Ship dashboard (FEATURE)\n"
    );
    assert_eq!(
        read_tasks_in_board(&subtask_board, TaskStatus::Todo).unwrap(),
        vec!["- Draft dashboard spec (DOCS)"]
    );

    let expected_parent = task_entry_at(&tasks_dir, TaskStatus::Todo, 1).unwrap();
    let reused_board = insert_subtask_in_board(
        &tasks_dir,
        TaskStatus::Todo,
        1,
        &expected_parent,
        "Build dashboard",
        None,
    )
    .unwrap();

    assert_eq!(reused_board, subtask_board);
    assert_eq!(
        read_tasks_in_board(&subtask_board, TaskStatus::Todo).unwrap(),
        vec!["- Draft dashboard spec (DOCS)", "- Build dashboard"]
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn insert_subtask_preserves_folder_backed_parent_detail() {
    let root = temp_root("insert-subtask-folder");
    init_tasks(&root, true).unwrap();
    let tasks_dir = root.join("tasks");
    let parent_path = tasks_dir.join("doing/0001-research-api.md");
    let parent_content =
        "Research the API. Keep detailed notes.\n\n- Audit callers\n- Draft rollout\n";
    fs::write(&parent_path, parent_content).unwrap();
    let expected_parent = task_entry_at(&tasks_dir, TaskStatus::Doing, 1).unwrap();

    let subtask_board = insert_subtask_in_board(
        &tasks_dir,
        TaskStatus::Doing,
        1,
        &expected_parent,
        "Audit callers",
        None,
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(subtask_board.join("task.md")).unwrap(),
        parent_content
    );
    assert_eq!(
        read_tasks_in_board(&subtask_board, TaskStatus::Todo).unwrap(),
        vec!["- Audit callers"]
    );
    assert!(!tasks_dir.join("doing.md.bak").exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn insert_subtask_rejects_parent_changed_while_prompt_was_open() {
    let root = temp_root("insert-subtask-stale-parent");
    add_task(&root, "Original parent", None).unwrap();
    let tasks_dir = root.join("tasks");
    let expected_parent = task_entry_at(&tasks_dir, TaskStatus::Todo, 1).unwrap();
    update_task_in_board(&tasks_dir, TaskStatus::Todo, 1, "Changed parent").unwrap();

    let error = insert_subtask_in_board(
        &tasks_dir,
        TaskStatus::Todo,
        1,
        &expected_parent,
        "Must not attach",
        None,
    )
    .unwrap_err();

    assert!(error.to_string().contains("Parent task changed"));
    assert!(tasks_dir.join("todo.md").is_file());
    assert!(!tasks_dir.join("todo").exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn move_task_writes_destination_and_removes_source() {
    let root = temp_root("move");

    add_task(&root, "ship the fix", None).unwrap();
    ManagedTaskWorkflow::new(&root)
        .move_task(TaskStatus::Todo, TaskStatus::Doing, "1")
        .unwrap();

    let todo = fs::read_to_string(root.join("tasks/todo.md")).unwrap();
    let doing = fs::read_to_string(root.join("tasks/doing.md")).unwrap();

    assert_eq!(todo, "# To Do Tasks\n");
    assert_eq!(doing, "# Doing Tasks\n- ship the fix\n");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_status_keeps_the_existing_serialized_names_and_order() {
    assert_eq!(
        TaskStatus::ALL.map(TaskStatus::as_str),
        ["todo", "doing", "done", "backlog"]
    );
    assert_eq!(TaskStatus::parse_arg("0").unwrap(), TaskStatus::Backlog);
    assert_eq!(TaskStatus::parse_arg("1").unwrap(), TaskStatus::Todo);
    assert_eq!(TaskStatus::parse_arg("2").unwrap(), TaskStatus::Doing);
    assert_eq!(TaskStatus::parse_arg("3").unwrap(), TaskStatus::Done);
    assert_eq!(TaskStatus::Todo.filename(), "todo.md");
    assert_eq!(TaskStatus::Doing.header(), "# Doing Tasks\n");
}

#[test]
fn task_board_exposes_typed_storage_operations() {
    let root = temp_root("typed-task-board");
    init_tasks(&root, false).unwrap();
    let board = TaskBoard::for_project(&root);

    board
        .insert_content(TaskStatus::Todo, None, "typed task")
        .unwrap();
    let entry = board.entry(TaskStatus::Todo, 1).unwrap();

    assert_eq!(entry.summary, "typed task");
    assert!(board.entries(TaskStatus::Doing).unwrap().is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_module_has_no_agent_or_tui_dependencies() {
    let source = include_str!("../task.rs");
    for forbidden in [
        "use super::*",
        "crate::agent",
        "super::agent",
        "agent::",
        "crate::tui",
        "super::tui",
        "tui::",
    ] {
        assert!(
            !source.contains(forbidden),
            "task.rs must not depend on {forbidden}"
        );
    }
}

#[test]
fn move_task_supports_backlog_as_a_status() {
    let root = temp_root("move-backlog");

    add_task(&root, "consider this later", None).unwrap();
    move_task(&root, TaskStatus::Todo, TaskStatus::Backlog, "1").unwrap();

    assert!(read_tasks(&root, "todo").unwrap().is_empty());
    assert_eq!(
        read_tasks(&root, "backlog").unwrap(),
        vec!["- consider this later"]
    );
    assert_eq!(normalize_status_arg("0").unwrap(), TaskStatus::Backlog);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn move_task_to_done_adds_to_top() {
    let root = temp_root("move-done-top");

    add_task(&root, "older done task", None).unwrap();
    add_task(&root, "newer done task", None).unwrap();
    move_task(&root, TaskStatus::Todo, TaskStatus::Done, "1").unwrap();
    move_task(&root, TaskStatus::Todo, TaskStatus::Done, "1").unwrap();

    let done = fs::read_to_string(root.join("tasks/done.md")).unwrap();

    assert_eq!(done, "# Done Tasks\n- newer done task\n- older done task\n");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn folder_backed_status_reads_task_files_as_first_sentence() {
    let root = temp_root("folder-read");
    let todo_dir = root.join("tasks/todo");
    fs::create_dir_all(&todo_dir).unwrap();
    fs::write(
        todo_dir.join("write-launch-plan.md"),
        "Write launch plan. Include rollout details and owners.\n\nAdd links later.\n",
    )
    .unwrap();

    let tasks = read_tasks(&root, "todo").unwrap();

    assert_eq!(tasks, vec!["- Write launch plan."]);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn archive_reader_uses_archived_directory_without_creating_a_store() {
    let root = temp_root("archive-dir-read");
    init_tasks(&root, false).unwrap();
    let archived_dir = root.join("tasks/archived");
    fs::create_dir_all(&archived_dir).unwrap();
    fs::write(
        archived_dir.join("old-task.md"),
        "Review the old launch plan. Keep historical notes here.\n",
    )
    .unwrap();

    let entries = read_archived_task_entries(&root.join("tasks")).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].summary, "Review the old launch plan.");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn archive_reader_returns_empty_when_archive_store_is_absent() {
    let root = temp_root("archive-missing-read");
    init_tasks(&root, false).unwrap();

    let entries = read_archived_task_entries(&root.join("tasks")).unwrap();

    assert!(entries.is_empty());
    assert!(!root.join("tasks/archived").exists());
    assert!(!root.join("tasks/archived.md").exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn archiving_folder_task_preserves_content_and_legacy_archive() {
    let root = temp_root("archive-folder-task");
    let tasks_dir = root.join("tasks");
    fs::create_dir_all(tasks_dir.join("todo")).unwrap();
    fs::write(
        tasks_dir.join("todo/long-task.md"),
        "Archive this task. Preserve the full details.\n\n- First detail\n- Second detail\n",
    )
    .unwrap();
    fs::write(
        tasks_dir.join("archived.md"),
        "# Archived Tasks\n- older archived task\n",
    )
    .unwrap();

    move_task_to_archive_in_board(&tasks_dir, TaskStatus::Todo, "1").unwrap();

    assert!(
        directory_task_paths(&tasks_dir.join("todo"))
            .unwrap()
            .is_empty()
    );
    assert!(tasks_dir.join("archived.md.bak").exists());
    let archived = read_archived_task_entries(&tasks_dir).unwrap();
    assert_eq!(archived.len(), 2);
    assert_eq!(archived[0].summary, "older archived task");
    assert_eq!(archived[1].summary, "Archive this task.");
    assert!(archived[1].content.contains("Second detail"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn moving_folder_backed_task_preserves_long_file_content() {
    let root = temp_root("folder-move");
    let todo_dir = root.join("tasks/todo");
    fs::create_dir_all(&todo_dir).unwrap();
    fs::write(
            todo_dir.join("research-api.md"),
            "Research the API migration. This file keeps the longer task notes.\n\n- Audit callers\n- Draft rollout\n",
        )
        .unwrap();

    move_task(&root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();

    assert!(directory_task_paths(&todo_dir).unwrap().is_empty());
    let doing_entries = read_task_entries(&root.join("tasks"), TaskStatus::Doing).unwrap();
    assert_eq!(doing_entries.len(), 1);
    assert_eq!(doing_entries[0].summary, "Research the API migration.");
    assert!(doing_entries[0].content.contains("Audit callers"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn moving_folder_task_converts_markdown_destination_to_directory() {
    let root = temp_root("folder-convert-dest");
    let tasks_dir = root.join("tasks");
    fs::create_dir_all(tasks_dir.join("todo")).unwrap();
    fs::write(
        tasks_dir.join("todo/long-task.md"),
        "Move this rich task. Preserve all follow-up detail.\n\nSecond paragraph.\n",
    )
    .unwrap();
    fs::write(
        tasks_dir.join("doing.md"),
        "# Doing Tasks\n- existing task\n",
    )
    .unwrap();

    move_task(&root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();

    assert!(tasks_dir.join("doing").is_dir());
    assert!(tasks_dir.join("doing.md.bak").exists());
    let doing_entries = read_task_entries(&tasks_dir, TaskStatus::Doing).unwrap();
    assert_eq!(doing_entries.len(), 2);
    assert!(
        doing_entries
            .iter()
            .any(|entry| entry.summary == "existing task")
    );
    assert!(
        doing_entries
            .iter()
            .any(|entry| entry.content.contains("Second paragraph."))
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn folder_task_with_status_stores_is_detected_as_subtask_board() {
    let root = temp_root("folder-subtasks");
    let epic_dir = root.join("tasks/doing/ship-epic");
    fs::create_dir_all(&epic_dir).unwrap();
    fs::write(epic_dir.join("task.md"), "Ship epic. Parent task detail.\n").unwrap();
    fs::write(epic_dir.join("todo.md"), "# To Do Tasks\n- draft spec\n").unwrap();
    fs::write(epic_dir.join("doing.md"), "# Doing Tasks\n").unwrap();
    fs::write(epic_dir.join("done.md"), "# Done Tasks\n").unwrap();

    let entries = read_task_entries(&root.join("tasks"), TaskStatus::Doing).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].summary, "Ship epic.");
    assert!(entries[0].has_subtasks);
    assert_eq!(
        read_tasks_in_board(&epic_dir, TaskStatus::Todo).unwrap(),
        vec!["- draft spec"]
    );

    fs::remove_dir_all(root).unwrap();
}
