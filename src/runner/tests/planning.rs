use crate::test_support::prelude::*;
use crate::test_support::*;

#[test]
fn returned_todo_keeps_session_fences_and_refuses_missing_git_boundary() {
    for scenario in [
        "missing-journal",
        "stopped",
        "running",
        "interactive",
        "duplicate",
        "blocked",
        "terminal",
    ] {
        let root = temp_root("returned-todo-fences");
        let state = root.join("state");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        add_task(&project_root, "Existing work. codex:prior-session", None).unwrap();
        initialize_test_git_repository(&project_root);
        let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let mut project = store.list_projects_blocking().unwrap().remove(0);
        project.git_mode = AgentGitMode::Commit;
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
                project_id: project.id,
                status: "blocked",
                started_at: "100",
                finished_at: Some("101"),
                exit_code: Some(0),
                log_dir: None,
                stdout_path: None,
                stderr_path: None,
                summary: None,
                codex_session_id: Some("prior-session"),
            })
            .unwrap();
        let control_state = match scenario {
            "stopped" => Some(AgentSessionControlState::Stopped),
            "running" => Some(AgentSessionControlState::Running),
            "interactive" => Some(AgentSessionControlState::Interactive),
            _ => None,
        };
        if let Some(control_state) = control_state {
            store
                .set_session_control_state_blocking(project.id, "prior-session", control_state)
                .unwrap();
        }
        if scenario == "duplicate" {
            add_task(&project_root, "Other task. codex:prior-session", None).unwrap();
        }
        if scenario == "blocked" {
            let board = TaskBoard::new(get_tasks_dir(&project_root));
            let task = board.entry(TaskStatus::Todo, 1).unwrap();
            board
                .write_entry_content(
                    TaskStatus::Todo,
                    &task,
                    "Existing work. BLOCKED 2026-10-06: Waiting. codex:prior-session",
                )
                .unwrap();
        }
        if scenario == "terminal" {
            let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: "prior-session",
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some(&start.starting_head),
                    branch_ref: start.branch_ref.as_deref(),
                    upstream_ref: start.upstream_ref.as_deref(),
                    worktree_baseline: &start.worktree_baseline,
                    task_identity: None,
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap();
            assert!(
                store
                    .compare_and_set_git_finalization_blocking(
                        project.id,
                        "prior-session",
                        0,
                        GitFinalizationState::Cancelled,
                        None,
                        None,
                        None,
                        "101"
                    )
                    .unwrap()
            );
        }
        let before = store
            .git_finalization_blocking(project.id, "prior-session")
            .unwrap();
        let control_before = store
            .session_control_blocking(project.id, "prior-session")
            .unwrap();
        let runner = CodexAgentRunner::with_command(
            state,
            Duration::from_secs(1),
            root.join("must-not-launch"),
        );
        let error = runner
            .run_project(
                &project,
                AgentTaskSelection::NextTodo,
                Some("prior-session"),
                "holder",
                None,
                &new_agent_shutdown_signal(),
            )
            .unwrap_err();
        let expected = match scenario {
            "missing-journal" => "no frozen Git start journal",
            "duplicate" => "exactly one task",
            "blocked" => "no longer belongs to the next ready Todo",
            "terminal" => "Git finalization history",
            _ => "busy or stopped",
        };
        assert!(
            format!("{error:#}").contains(expected),
            "{scenario}: {error:#}"
        );
        assert_eq!(
            store
                .git_finalization_blocking(project.id, "prior-session")
                .unwrap(),
            before
        );
        assert_eq!(
            store
                .session_control_blocking(project.id, "prior-session")
                .unwrap(),
            control_before
        );
        assert!(
            store
                .git_launch_state_for_project_blocking(project.id)
                .unwrap()
                .is_none()
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

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
fn todo_runner_starts_plans_and_resumes_prior_work_with_each_git_mode() {
    use std::os::unix::fs::PermissionsExt;

    for (mode, registered, folders, resumed) in [
        (AgentGitMode::Off, true, false, false),
        (AgentGitMode::Off, false, true, false),
        (AgentGitMode::Commit, true, true, false),
        (AgentGitMode::CommitAndPush, true, false, false),
        (AgentGitMode::Off, false, false, true),
        (AgentGitMode::Off, false, true, true),
        (AgentGitMode::Commit, false, true, true),
        (AgentGitMode::CommitAndPush, false, false, true),
        (AgentGitMode::CommitAndPush, false, true, true),
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
        let original_journal = if resumed {
            store
                .record_run_outcome_blocking(agent::AgentRunOutcome {
                    project_id: project.id,
                    status: "blocked",
                    started_at: "100",
                    finished_at: Some("101"),
                    exit_code: Some(0),
                    log_dir: None,
                    stdout_path: None,
                    stderr_path: None,
                    summary: Some("Waiting for prerequisite"),
                    codex_session_id: Some("planning-session"),
                })
                .unwrap();
            if mode != AgentGitMode::Off {
                let start = capture_agent_git_start_state(&project_root, mode).unwrap();
                let identity = durable_task_identity("Implement the agreed plan.").unwrap();
                assert!(
                    store
                        .create_git_finalization_blocking(agent::NewGitFinalization {
                            project_id: project.id,
                            codex_session_id: "planning-session",
                            git_mode: mode,
                            starting_head: Some(&start.starting_head),
                            branch_ref: start.branch_ref.as_deref(),
                            upstream_ref: start.upstream_ref.as_deref(),
                            worktree_baseline: &start.worktree_baseline,
                            task_identity: Some(&identity),
                            owner_run_token: None,
                            created_at: "100",
                        })
                        .unwrap()
                );
                // The checkout can advance while blocked. Resume must preserve
                // the older boundary instead of checkpointing a fresh start.
                fs::write(project_root.join("independent.txt"), "Other work\n").unwrap();
                run_test_git(&project_root, &["add", "independent.txt"]);
                run_test_git(&project_root, &["commit", "-m", "Independent work"]);
            }
            let board = TaskBoard::new(get_tasks_dir(&project_root));
            let task = board.entry(TaskStatus::Todo, 1).unwrap();
            board.write_entry_content(TaskStatus::Todo, &task,
                "Implement the agreed plan. BLOCKED 2026-10-05: Waiting. UNBLOCKED 2026-10-06: Prerequisite restored. codex:planning-session").unwrap();
            store
                .git_finalization_blocking(project.id, "planning-session")
                .unwrap()
        } else {
            None
        };
        let exact_resume = resumed && mode == AgentGitMode::CommitAndPush && !folders;
        if exact_resume {
            assert!(
                store
                    .ensure_pending_git_finalization_resume_requested_blocking(
                        project.id,
                        "planning-session"
                    )
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
            if resumed {
                let doing =
                    read_task_entries(&get_tasks_dir(&thread_project.path), TaskStatus::Doing)
                        .unwrap();
                assert_eq!(doing.len(), 1);
                assert_eq!(
                    codex_session_for_task(&doing[0]).as_deref(),
                    Some("planning-session")
                );
                fs::write(activated, "done").unwrap();
                return;
            }
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
                if exact_resume {
                    AgentTaskSelection::ResumeSession
                } else {
                    AgentTaskSelection::NextTodo
                },
                exact_resume.then_some("planning-session"),
                "holder",
                None,
                &new_agent_shutdown_signal(),
            )
            .unwrap();
        activation.join().unwrap();
        assert_eq!(result.status, "success", "{}", result.summary);
        assert_eq!(result.codex_session_id.as_deref(), Some("planning-session"));
        assert!(
            fs::read_to_string(&result.stderr_path).unwrap().contains(
                "arg=exec\narg=resume\narg=--skip-git-repo-check\narg=planning-session\n"
            )
        );
        let log = fs::read_to_string(&result.stderr_path).unwrap();
        if resumed {
            assert!(log.contains("Existing task recovery:"));
            assert!(!log.contains("Start the planned task:"));
        } else {
            assert!(log.contains("Start the planned task:"));
        }
        if let Some(original) = original_journal {
            let current = store
                .git_finalization_blocking(project.id, "planning-session")
                .unwrap()
                .unwrap();
            assert_eq!(current.starting_head, original.starting_head);
            assert_eq!(current.branch_ref, original.branch_ref);
            assert_eq!(current.upstream_ref, original.upstream_ref);
            assert_eq!(current.worktree_baseline, original.worktree_baseline);
            assert_eq!(current.task_identity, original.task_identity);
            assert_eq!(current.created_at, original.created_at);
            assert!(
                store
                    .git_launch_state_for_project_blocking(project.id)
                    .unwrap()
                    .is_none()
            );
        }
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
