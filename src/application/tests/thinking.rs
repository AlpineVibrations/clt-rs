use crate::application::cycle_task_thinking_in_board;
use crate::test_support::prelude::*;
use crate::test_support::*;

#[test]
fn task_thinking_cycles_preserve_storage_ownership_and_coordination() {
    for folders in [false, true] {
        let root = temp_root("task-thinking-cycle");
        init_tasks(&root, folders).unwrap();
        let board = TaskBoard::for_project(&root);
        let metadata = crate::task::coordination::Metadata {
            id: uuid::Uuid::new_v4().to_string(),
            depends_on: vec![uuid::Uuid::new_v4().to_string()],
            ..Default::default()
        };
        let body = "Fix parser.\n\nKeep details. /think high codex:thinking-session clt:stopped";
        let content = crate::task::coordination::write(body, &metadata).unwrap();
        board
            .insert_content(TaskStatus::Todo, None, &content)
            .unwrap();
        board
            .insert_content(TaskStatus::Todo, None, "Other task /think low")
            .unwrap();
        let original = board.entry(TaskStatus::Todo, 1).unwrap();
        let neighbor = board.entry(TaskStatus::Todo, 2).unwrap();
        assert_eq!(
            cycle_task_thinking_in_board(board.path(), TaskStatus::Todo, &original).unwrap(),
            Some("xhigh")
        );
        let updated = board.entry(TaskStatus::Todo, 1).unwrap();
        assert_eq!(updated.source, original.source);
        assert!(updated.content.contains("Keep details."));
        assert!(
            updated
                .content
                .trim_end()
                .ends_with("codex:thinking-session clt:stopped")
        );
        assert_eq!(
            crate::task::coordination::read(&updated.content).unwrap(),
            metadata
        );
        assert_eq!(
            durable_task_identity(&updated.content),
            durable_task_identity(&original.content)
        );
        assert_eq!(
            board.entry(TaskStatus::Todo, 2).unwrap().content,
            neighbor.content
        );
        assert_eq!(
            board.entry(TaskStatus::Todo, 2).unwrap().source,
            neighbor.source
        );

        // A cached selection cannot overwrite an edit made since the last refresh.
        assert!(
            cycle_task_thinking_in_board(board.path(), TaskStatus::Todo, &original)
                .unwrap_err()
                .to_string()
                .contains("selected task changed")
        );
        for status in [TaskStatus::Doing, TaskStatus::Done, TaskStatus::Backlog] {
            assert!(
                cycle_task_thinking_in_board(board.path(), status, &updated)
                    .unwrap_err()
                    .to_string()
                    .contains("only be cycled on Todo")
            );
        }
        // Ordinary prose edits retain the directive visible in the editor.
        let edited = crate::task::task_content_for_edit(&updated.content)
            .replace("Keep details.", "Keep revised details.");
        update_task_in_board(board.path(), TaskStatus::Todo, 1, &edited).unwrap();
        assert_eq!(
            crate::task::thinking::read(&board.entry(TaskStatus::Todo, 1).unwrap().content),
            Some("xhigh")
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn task_thinking_cycles_on_nested_boards() {
    let root = temp_root("nested-task-thinking");
    init_tasks(&root, true).unwrap();
    let nested = root.join("tasks/todo/0001-parent");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("todo.md"), "# Todo\n- Child /think max\n").unwrap();
    let board = TaskBoard::new(&nested);
    let selected = board.entry(TaskStatus::Todo, 1).unwrap();
    assert_eq!(
        cycle_task_thinking_in_board(&nested, TaskStatus::Todo, &selected).unwrap(),
        Some("ultra")
    );
    let selected = board.entry(TaskStatus::Todo, 1).unwrap();
    assert_eq!(
        cycle_task_thinking_in_board(&nested, TaskStatus::Todo, &selected).unwrap(),
        None
    );
    assert_eq!(board.entry(TaskStatus::Todo, 1).unwrap().content, "Child");
    fs::remove_dir_all(root).unwrap();
}
