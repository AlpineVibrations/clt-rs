use super::*;

#[test]
fn activation_preserves_concurrent_task_edits_in_both_board_layouts() {
    for folders in [false, true] {
        let root = temp_root(&format!("activation-task-edits-{folders}"));
        let project_root = root.join("project");
        init_tasks(&project_root, folders).unwrap();
        if folders {
            for (name, content) in [
                ("0001-selected.md", "Start first"),
                ("0002-other.md", "Other original"),
                ("0003-remove.md", "Remove task"),
            ] {
                fs::write(project_root.join("tasks/todo").join(name), content).unwrap();
            }
        } else {
            fs::write(
                project_root.join("tasks/todo.md"),
                "# Todo Tasks\n- Start first\n- Other original\n- Remove task\n",
            )
            .unwrap();
        }
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&root.join("state")).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        store
            .record_git_launch_state_blocking(
                project.id,
                "edit-run",
                AgentGitMode::Commit,
                &start,
                "100",
            )
            .unwrap();
        store
            .mark_session_running_with_git_mode_blocking(
                project.id,
                "edit-session",
                123,
                "edit-run",
                &root.join("out"),
                &root.join("err"),
                AgentGitMode::Commit,
            )
            .unwrap();
        let context = AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "edit-run".into(),
        };

        // A person adds, edits, and removes other tasks before the child activates.
        // Adding at the front also changes the selected task's list index.
        if folders {
            fs::write(project_root.join("tasks/todo/0000-added.md"), "Added task").unwrap();
            fs::write(
                project_root.join("tasks/todo/0002-other.md"),
                "Other edited",
            )
            .unwrap();
            fs::remove_file(project_root.join("tasks/todo/0003-remove.md")).unwrap();
        } else {
            fs::write(
                project_root.join("tasks/todo.md"),
                "# Todo Tasks\n- Added task\n- Start first\n- Other edited\n",
            )
            .unwrap();
        }
        fs::write(project_root.join("user-notes.txt"), "Untracked notes\n").unwrap();
        let index_before = run_test_git(&project_root, &["write-tree"]);

        // The new task cannot be substituted for the committed selected task.
        let error = move_task_to_doing_with_agent_git_journal(
            &project_root,
            "1",
            &context,
            &project,
            &store,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("committed exactly once"));
        assert!(read_tasks(&project_root, "doing").unwrap().is_empty());

        move_task_to_doing_with_agent_git_journal(&project_root, "2", &context, &project, &store)
            .unwrap();
        assert_eq!(
            read_tasks(&project_root, "todo").unwrap(),
            vec!["- Added task", "- Other edited"]
        );
        assert_eq!(
            read_tasks(&project_root, "doing").unwrap(),
            vec!["- Start first"]
        );
        let active = TaskBoard::new(get_tasks_dir(&project_root))
            .entry(TaskStatus::Doing, 1)
            .unwrap();
        assert_eq!(active.content.trim(), "Start first codex:edit-session");
        if folders {
            assert!(project_root.join("tasks/doing/0001-selected.md").exists());
            assert!(project_root.join("tasks/todo/0000-added.md").exists());
            assert!(project_root.join("tasks/todo/0002-other.md").exists());
            assert!(!project_root.join("tasks/todo/0003-remove.md").exists());
        }
        assert_eq!(
            fs::read_to_string(project_root.join("user-notes.txt")).unwrap(),
            "Untracked notes\n"
        );
        assert_eq!(run_test_git(&project_root, &["write-tree"]), index_before);
        assert_eq!(
            run_test_git(&project_root, &["rev-parse", "HEAD"]),
            start.starting_head
        );
        let journal = store
            .git_finalization_blocking(project.id, "edit-session")
            .unwrap()
            .unwrap();
        assert_eq!(journal.worktree_baseline, start.worktree_baseline);
        assert_eq!(journal.task_identity, durable_task_identity("Start first"));
        assert_eq!(journal.state, GitFinalizationState::Working);
        fs::remove_dir_all(root).unwrap();
    }
}
