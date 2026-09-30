use crate::test_support::prelude::*;
use crate::test_support::*;

#[test]
fn planned_todo_selection_preserves_queue_order_and_skips_unavailable_tasks() {
    for folders in [false, true] {
        let root = temp_root("planned-todo-selection");
        init_tasks(&root, folders).unwrap();
        for content in [
            "Stopped plan. codex:stopped clt:stopped",
            "Blocked plan. BLOCKED 2026-09-30: Waiting. codex:blocked",
            "First ready task.",
            "Second ready task. codex:plan-two",
        ] {
            add_task(&root, content, None).unwrap();
        }
        assert_eq!(
            automated_codex_session_to_resume(&root, AgentTaskSelection::NextTodo).unwrap(),
            None,
        );
        let board = TaskBoard::new(get_tasks_dir(&root));
        let task = board.entry(TaskStatus::Todo, 3).unwrap();
        board
            .write_entry_content(TaskStatus::Todo, &task, "First ready task. codex:plan-one")
            .unwrap();
        assert_eq!(
            automated_codex_session_to_resume(&root, AgentTaskSelection::NextTodo)
                .unwrap()
                .as_deref(),
            Some("plan-one"),
        );
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
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &args[..4],
            ["exec", "resume", "--skip-git-repo-check", "plan-one"]
        );
        let prompt = args.last().unwrap();
        assert!(prompt.contains("supersedes the earlier planning-only instruction"));
        assert!(prompt.contains("Preserve its codex:plan-one marker"));
        assert!(prompt.contains("Do not select another task"));
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn planned_todo_runner_resumes_the_same_session_and_activates_with_each_git_mode() {
    use std::os::unix::fs::PermissionsExt;

    for (mode, registered, folders) in [
        (AgentGitMode::Off, true, false),
        (AgentGitMode::Off, false, true),
        (AgentGitMode::Commit, true, true),
        (AgentGitMode::CommitAndPush, true, false),
    ] {
        let root = temp_root("planned-todo-runner");
        let state = root.join("state");
        let project_root = root.join("project");
        init_tasks(&project_root, folders).unwrap();
        add_task(
            &project_root,
            "Implement the agreed plan. codex:planning-session",
            None,
        )
        .unwrap();
        add_task(&project_root, "Leave queued. codex:other-plan", None).unwrap();
        let project_root = fs::canonicalize(project_root).unwrap();
        if mode != AgentGitMode::Off {
            initialize_test_git_repository(&project_root);
            if mode == AgentGitMode::CommitAndPush {
                let remote = root.join("remote.git");
                fs::create_dir_all(&remote).unwrap();
                run_test_git(&remote, &["init", "--bare"]);
                run_test_git(
                    &project_root,
                    &["remote", "add", "origin", remote.to_str().unwrap()],
                );
                run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
            }
        }
        let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let mut project = store.list_projects_blocking().unwrap().remove(0);
        project.git_mode = mode;
        if registered {
            assert!(
                store
                    .register_shared_planning_session_blocking(project.id, "planning-session")
                    .unwrap()
            );
        }
        assert!(
            store
                .try_acquire_lease_blocking(
                    project.id,
                    "holder",
                    &agent_timestamp(),
                    &agent_timestamp_after(60)
                )
                .unwrap()
        );
        let started = root.join("started");
        let activated = root.join("activated");
        let fake = root.join("codex");
        fs::write(&fake, format!(
            "#!/bin/sh\nprintf 'arg=%s\\n' \"$@\" >&2\ntouch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf 'activated\\n'\n",
            started.display(), activated.display(),
        )).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let thread_state = state.clone();
        let thread_project = project.clone();
        let activation = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !started.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(started.exists(), "Codex was never released");
            let store = agent::TursoAgentStore::open_blocking(&thread_state).unwrap();
            let control = store
                .session_control_blocking(thread_project.id, "planning-session")
                .unwrap()
                .unwrap();
            assert_eq!(control.state, AgentSessionControlState::Running);
            let context = AutomatedAgentChildContext {
                project_id: thread_project.id,
                run_token: control.run_token.unwrap(),
            };
            let wrong_task = if mode == AgentGitMode::Off {
                move_task_to_doing_with_agent_session(
                    &thread_project.path,
                    "2",
                    &context,
                    &thread_project,
                    &store,
                )
            } else {
                move_task_to_doing_with_agent_git_journal(
                    &thread_project.path,
                    "2",
                    &context,
                    &thread_project,
                    &store,
                )
            };
            assert!(
                wrong_task
                    .unwrap_err()
                    .to_string()
                    .contains("different Codex session")
            );
            if mode == AgentGitMode::Off {
                move_task_to_doing_with_agent_session(
                    &thread_project.path,
                    "1",
                    &context,
                    &thread_project,
                    &store,
                )
                .unwrap();
            } else {
                let journal = store
                    .git_finalization_blocking(thread_project.id, "planning-session")
                    .unwrap()
                    .unwrap();
                assert_eq!(journal.state, GitFinalizationState::Working);
                assert_eq!(
                    journal.owner_run_token.as_deref(),
                    Some(context.run_token.as_str())
                );
                move_task_to_doing_with_agent_git_journal(
                    &thread_project.path,
                    "1",
                    &context,
                    &thread_project,
                    &store,
                )
                .unwrap();
            }
            fs::write(activated, "done").unwrap();
        });
        let runner = CodexAgentRunner::with_command(state, Duration::from_secs(15), fake);
        let result = runner
            .run_project(
                &project,
                AgentTaskSelection::NextTodo,
                None,
                "holder",
                None,
                &new_agent_shutdown_signal(),
            )
            .unwrap();
        activation.join().unwrap();
        assert_eq!(result.status, "success", "{}", result.summary);
        assert_eq!(result.codex_session_id.as_deref(), Some("planning-session"));
        assert!(
            fs::read_to_string(result.stderr_path).unwrap().contains(
                "arg=exec\narg=resume\narg=--skip-git-repo-check\narg=planning-session\n"
            )
        );
        let doing = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Doing).unwrap();
        assert_eq!(doing.len(), 1);
        assert_eq!(
            codex_session_for_task(&doing[0]).as_deref(),
            Some("planning-session")
        );
        let todo = read_task_entries(&get_tasks_dir(&project_root), TaskStatus::Todo).unwrap();
        assert_eq!(todo.len(), 1);
        assert_eq!(
            codex_session_for_task(&todo[0]).as_deref(),
            Some("other-plan")
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
