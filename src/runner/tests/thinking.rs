use crate::runner::selected_task_thinking;
use crate::test_support::prelude::*;
use crate::test_support::*;

#[test]
fn task_thinking_follows_ready_selection_and_exact_resumed_sessions() {
    for folders in [false, true] {
        let root = temp_root("selected-task-thinking");
        init_tasks(&root, folders).unwrap();
        let board = TaskBoard::for_project(&root);
        board
            .insert_content(TaskStatus::Todo, None, "Stopped /think ultra clt:stopped")
            .unwrap();
        board
            .insert_content(
                TaskStatus::Todo,
                None,
                "Blocked. /think low BLOCKED 2026-10-25: waiting",
            )
            .unwrap();
        board
            .insert_content(TaskStatus::Todo, None, "First ready. Details /think max")
            .unwrap();
        board
            .insert_content(TaskStatus::Todo, None, "Other ready /think low")
            .unwrap();
        assert_eq!(
            selected_task_thinking(&root, AgentTaskSelection::NextTodo, None).unwrap(),
            Some("max")
        );
        board
            .insert_content(
                TaskStatus::Doing,
                None,
                "Owned /think high task codex:owned",
            )
            .unwrap();
        for selection in [
            AgentTaskSelection::ResumeSession,
            AgentTaskSelection::SupervisorRetry,
            AgentTaskSelection::ResumeDoing,
        ] {
            assert_eq!(
                selected_task_thinking(&root, selection, Some("owned")).unwrap(),
                Some("high")
            );
        }
        assert_eq!(
            selected_task_thinking(&root, AgentTaskSelection::ResumeDoing, None).unwrap(),
            Some("high")
        );
        assert_eq!(
            selected_task_thinking(&root, AgentTaskSelection::SupervisorReview, Some("owned"))
                .unwrap(),
            None
        );
        assert_eq!(
            selected_task_thinking(&root, AgentTaskSelection::ResumeSession, Some("missing"))
                .unwrap(),
            None
        );

        // A planning conversation is still selected from Todo and keeps its setting.
        let first = board.entry(TaskStatus::Todo, 3).unwrap();
        board
            .write_entry_content(
                TaskStatus::Todo,
                &first,
                "First ready /think medium codex:planned",
            )
            .unwrap();
        assert_eq!(
            selected_task_thinking(&root, AgentTaskSelection::NextTodo, None).unwrap(),
            Some("medium")
        );
        board
            .insert_content(
                TaskStatus::Done,
                None,
                "Ambiguous /think ultra codex:planned",
            )
            .unwrap();
        assert!(
            selected_task_thinking(&root, AgentTaskSelection::ResumeSession, Some("planned"))
                .unwrap_err()
                .to_string()
                .contains("unambiguous")
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn task_thinking_keeps_goal_directives_at_the_start_of_launch_prose() {
    let root = temp_root("thinking-goal-prompt");
    init_tasks(&root, false).unwrap();
    add_task(&root, "/think high /goal Ship the fix.", None).unwrap();
    let mut project = crate::tui::tests::tui_agent_project_for_test(1, "project").project;
    project.path = root.clone();
    let mut command = Command::new("codex");
    configure_automated_codex_subcommand(
        &mut command,
        &project,
        AgentTaskSelection::NextTodo,
        None,
    )
    .unwrap();
    let prompt = command.get_args().last().unwrap().to_string_lossy();
    assert!(prompt.contains("Selected task for this run:\n/goal Ship the fix."));
    assert!(prompt.contains("preserve it in the stored task"));
    assert!(
        TaskBoard::for_project(&root)
            .entry(TaskStatus::Todo, 1)
            .unwrap()
            .content
            .starts_with("/think high")
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn task_thinking_overrides_launch_settings_without_changing_project_defaults() {
    use std::os::unix::fs::PermissionsExt;
    for task_level in ["high", "max", "default"] {
        let root = temp_root("task-thinking-launch");
        let state = root.join("state");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(
            &project_root,
            &format!("Fix parser. Details /think {task_level} after title."),
            None,
        )
        .unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project_id = store.list_projects_blocking().unwrap()[0].id;
        store
            .set_project_codex_settings_blocking(
                project_id,
                Some("openai"),
                Some("gpt-5.6-terra"),
                Some("low"),
                false,
            )
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let fake = root.join("fake-codex");
        fs::write(
            &fake,
            "#!/bin/sh\nprintf 'arg=%s\\n' \"$@\" >&2\nprintf 'NO_TASKS_LEFT\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let runner = CodexAgentRunner::with_command(state, Duration::from_secs(5), fake);
        let result = runner
            .run_project(
                &project,
                AgentTaskSelection::NextTodo,
                None,
                "thinking-test",
                None,
                &new_agent_shutdown_signal(),
            )
            .unwrap();
        assert_eq!(result.status, "idle");
        let args = fs::read_to_string(result.stderr_path).unwrap();
        let expected = if task_level == "default" {
            "low"
        } else {
            task_level
        };
        assert!(
            args.contains(&format!("arg=model_reasoning_effort=\"{expected}\"\n")),
            "{args}"
        );
        if task_level != "default" {
            assert!(!args.contains("arg=model_reasoning_effort=\"low\"\n"));
        }
        assert_eq!(
            store.list_projects_blocking().unwrap()[0]
                .codex_reasoning_effort
                .as_deref(),
            Some("low")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
