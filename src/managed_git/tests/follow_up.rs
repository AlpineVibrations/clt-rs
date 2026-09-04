use crate::task::blocked_follow_up_session;
use crate::test_support::prelude::*;
use crate::test_support::*;

#[test]
fn blocked_follow_up_is_sealed_resealable_and_recoverable_in_both_board_formats() {
    for folders in [false, true] {
        let root = temp_root("git-blocked-follow-up");
        let project_root = root.join("project");
        let state_dir = root.join("state");
        init_tasks(&project_root, folders).unwrap();
        let board = TaskBoard::for_project(&project_root);
        board
            .insert_content(
                TaskStatus::Doing,
                None,
                "Implement feature — BLOCKED 2026-09-03: GPU harness fails codex:session-follow-up",
            )
            .unwrap();
        board
            .insert_content(TaskStatus::Doing, None, "Keep another task unchanged")
            .unwrap();
        initialize_test_git_repository(&project_root);
        let project_root = fs::canonicalize(project_root).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                "session-follow-up",
                123,
                "run-follow-up",
                &root.join("out"),
                &root.join("err"),
            )
            .unwrap();
        let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        ensure_agent_git_working_record(
            &store,
            &project,
            "session-follow-up",
            "run-follow-up",
            Some(&start),
        )
        .unwrap();
        bind_agent_git_working_task_identity(
            &store,
            &project,
            "session-follow-up",
            "run-follow-up",
        )
        .unwrap();
        let original = board.entry(TaskStatus::Doing, 1).unwrap();
        let other = board.entry(TaskStatus::Doing, 2).unwrap();
        let mut recovery_job = AgentRunJob {
            state_dir: state_dir.clone(),
            project: project.clone(),
            holder: "recovery".to_string(),
            worker_token: None,
            max_global_jobs: 1,
            task_selection: AgentTaskSelection::RecoverBlocked,
            resume_session_id: Some("session-follow-up".to_string()),
            blocked_task_count_before: 1,
            done_task_contents_before: completed_task_contents(&project_root).unwrap(),
            blocked_task_snapshots_before: blocked_task_snapshots(&project_root).unwrap(),
        };
        assert!(blocked_recovery_made_no_progress(&recovery_job));
        ManagedTaskWorkflow::new(&project_root)
            .add_blocked_follow_up(
                TaskStatus::Doing,
                "1",
                "Repair GPU harness.",
                "GPU harness fails identically at starting revision; requires working GPU runtime",
            )
            .unwrap();
        let preserved = board.entry(TaskStatus::Doing, 2).unwrap();
        assert_eq!(preserved.source, other.source);
        assert_eq!(preserved.content, other.content);
        board.write_entry_content(TaskStatus::Doing, &original, "Implement feature — COMPLETED 2026-09-04: feature tests passed; GPU baseline failure recorded in linked follow-up codex:session-follow-up").unwrap();
        fs::write(project_root.join("feature.txt"), "implemented\n").unwrap();
        run_test_git(&project_root, &["add", "feature.txt", "tasks"]);
        // A separate concurrent Todo must remain outside the task commit.
        board
            .insert_content(TaskStatus::Todo, None, "Concurrent human task")
            .unwrap();
        let context = AutomatedAgentChildContext {
            project_id: project.id,
            run_token: "run-follow-up".to_string(),
        };
        move_task_to_done_with_agent_store(&project_root, TaskStatus::Doing, "1", &context, &store)
            .unwrap();
        let doing_path = if folders {
            "tasks/doing"
        } else {
            "tasks/doing.md"
        };
        let done_path = if folders {
            "tasks/done"
        } else {
            "tasks/done.md"
        };
        run_test_git(&project_root, &["add", "--", doing_path, done_path]);
        let pending = store
            .git_finalization_blocking(project.id, "session-follow-up")
            .unwrap()
            .unwrap();
        // Corrected implementation after a hook may be resealed with the same follow-up.
        fs::write(
            project_root.join("feature.txt"),
            "formatted implementation\n",
        )
        .unwrap();
        run_test_git(&project_root, &["add", "feature.txt"]);
        let manifest = capture_agent_git_resealed_manifest(
            AgentGitProofContext {
                store: &store,
                project_id: project.id,
            },
            &project_root,
            &pending.worktree_baseline,
            "session-follow-up",
            pending.task_identity.as_deref().unwrap(),
            pending.starting_head.as_deref().unwrap(),
            pending.branch_ref.as_deref(),
        )
        .unwrap();
        assert!(
            store
                .reseal_git_finalization_manifest_blocking(
                    project.id,
                    "session-follow-up",
                    pending.generation,
                    pending.task_identity.as_deref().unwrap(),
                    &manifest,
                    "run-follow-up",
                    "200"
                )
                .unwrap()
        );
        run_test_agent_git(
            &project_root,
            &[
                "commit",
                "-m",
                "Implement feature and track GPU failure",
                "-m",
                "CLT-Task: codex:session-follow-up",
            ],
        );
        let sealed = store
            .git_finalization_blocking(project.id, "session-follow-up")
            .unwrap()
            .unwrap();
        let completed = reconcile_agent_git_finalization(
            &store,
            &project_root,
            sealed,
            Some("run-follow-up"),
            None,
        )
        .unwrap();
        assert_eq!(completed.state, GitFinalizationState::Completed);
        assert_eq!(
            run_test_git(
                &project_root,
                &[
                    "rev-list",
                    "--count",
                    &format!("{}..HEAD", start.starting_head)
                ]
            ),
            "1"
        );
        let entries = git_ref_task_entries(&project_root, "HEAD").unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.status == "doing"
                    && blocked_follow_up_session(&entry.content) == Some("session-follow-up"))
                .count(),
            1
        );
        assert!(
            !entries
                .iter()
                .any(|entry| entry.content.contains("Concurrent human task"))
        );
        let scan = scan_agent_project(&project_root);
        assert_eq!(scan.blocked_doing_count, 1);
        assert!(!blocked_recovery_made_no_progress(&recovery_job));
        recovery_job.blocked_task_snapshots_before[0].content =
            "Different blocked task".to_string();
        assert!(blocked_recovery_made_no_progress(&recovery_job));
        let follow_up = board
            .entries(TaskStatus::Doing)
            .unwrap()
            .into_iter()
            .find(task_entry_is_blocked)
            .unwrap();
        assert_eq!(
            recoverable_codex_session_id_from_task_content(&follow_up.content),
            None
        );
        let doing_before = task_contents_for_status(&project_root, TaskStatus::Doing).unwrap();
        let blocked_before = blocked_task_snapshots(&project_root).unwrap();
        assert_eq!(
            automated_codex_session_to_resume(&project_root, AgentTaskSelection::RecoverBlocked)
                .unwrap(),
            None
        );
        assert!(
            attach_codex_session_to_active_task(
                &project_root,
                AgentTaskSelection::RecoverBlocked,
                &doing_before,
                &blocked_before,
                "follow-up-own-session"
            )
            .unwrap()
        );
        assert_eq!(
            task_status_for_codex_session(&project_root, "session-follow-up").unwrap(),
            Some(TaskStatus::Done)
        );
        assert_eq!(
            task_status_for_codex_session(&project_root, "follow-up-own-session").unwrap(),
            Some(TaskStatus::Doing)
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn follow_up_allowance_preserves_exact_board_scope() {
    for folders in [false, true] {
        for mutation in [
            "unrelated",
            "unblocked",
            "wrong-parent",
            "duplicate-session",
            "header",
            "existing",
            "attachment",
            "symlink",
        ] {
            let root = temp_root("follow-up-scope");
            init_tasks(&root, folders).unwrap();
            let board = TaskBoard::for_project(&root);
            board
                .insert_content(
                    TaskStatus::Doing,
                    None,
                    "Finish feature codex:session-scope",
                )
                .unwrap();
            board
                .insert_content(TaskStatus::Doing, None, "Existing task")
                .unwrap();
            initialize_test_git_repository(&root);
            let parent = run_test_git(&root, &["rev-parse", "HEAD"]);
            let identity = durable_task_identity("Finish feature").unwrap();
            let scope = agent_git_task_scope_without_selected(&root, &parent, &identity).unwrap();
            let selected = board.entry(TaskStatus::Doing, 1).unwrap();
            board
                .write_entry_content(
                    TaskStatus::Doing,
                    &selected,
                    "Finish feature — COMPLETED 2026-09-04: verified codex:session-scope",
                )
                .unwrap();
            ManagedTaskWorkflow::new(&root)
                .add_blocked_follow_up(
                    TaskStatus::Doing,
                    "1",
                    "Repair harness.",
                    "baseline failure; requires runtime",
                )
                .unwrap();
            match mutation {
                "unrelated" => board
                    .insert_content(TaskStatus::Todo, None, "Unrelated addition")
                    .unwrap(),
                "header" => {
                    fs::write(root.join("tasks/unrelated.txt"), "extra board payload\n").unwrap()
                }
                "attachment" if folders => {
                    let follow_up = board.entry(TaskStatus::Doing, 3).unwrap();
                    let TaskSource::Path { path, .. } = follow_up.source else {
                        unreachable!()
                    };
                    fs::remove_file(&path).unwrap();
                    fs::create_dir(&path).unwrap();
                    fs::write(path.join("task.md"), follow_up.content).unwrap();
                    fs::write(path.join("attachment.txt"), "unsealed attachment").unwrap();
                }
                "attachment" => fs::write(
                    root.join("tasks/doing.md"),
                    format!(
                        "{}\nExtra footer\n",
                        fs::read_to_string(root.join("tasks/doing.md")).unwrap()
                    ),
                )
                .unwrap(),
                "symlink" => {
                    #[cfg(unix)]
                    if folders {
                        let follow_up = board.entry(TaskStatus::Doing, 3).unwrap();
                        let TaskSource::Path { path, .. } = follow_up.source else {
                            unreachable!()
                        };
                        // A link to otherwise valid text must not widen the file allowance.
                        let target = root.join("linked.txt");
                        fs::write(&target, follow_up.content).unwrap();
                        fs::remove_file(&path).unwrap();
                        std::os::unix::fs::symlink(target, &path).unwrap();
                    } else {
                        board
                            .insert_content(TaskStatus::Todo, None, "Unrelated task")
                            .unwrap();
                    }
                    #[cfg(not(unix))]
                    board
                        .insert_content(TaskStatus::Todo, None, "Unrelated task")
                        .unwrap();
                }
                _ => {
                    let index = if mutation == "existing" { 2 } else { 3 };
                    let entry = board.entry(TaskStatus::Doing, index).unwrap();
                    let replacement = match mutation {
                        "unblocked" => {
                            "Repair harness — UNBLOCKED 2026-09-04: ready clt-follow-up:session-scope"
                        }
                        "wrong-parent" => {
                            "Repair harness — BLOCKED 2026-09-04: failure clt-follow-up:other-session"
                        }
                        "duplicate-session" => {
                            "Repair harness — BLOCKED 2026-09-04: failure clt-follow-up:session-scope codex:session-scope"
                        }
                        "existing" => "Rewritten existing task",
                        _ => unreachable!(),
                    };
                    board
                        .write_entry_content(TaskStatus::Doing, &entry, replacement)
                        .unwrap();
                }
            }
            run_test_git(&root, &["add", "tasks"]);
            let staged = run_test_git(&root, &["write-tree"]);
            assert!(
                project_agent_git_completed_tree(
                    &root,
                    &staged,
                    "session-scope",
                    &identity,
                    &parent,
                    &scope
                )
                .is_err(),
                "accepted {mutation} with folders={folders}"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn completed_note_cannot_hide_a_later_blocker() {
    assert!(!task_content_has_completed_note(
        "Feature — COMPLETED 2026-09-03: implemented — BLOCKED 2026-09-04: acceptance fails"
    ));
    assert!(task_content_has_completed_note(
        "Feature — BLOCKED 2026-09-03: acceptance fails — COMPLETED 2026-09-04: fixed and verified"
    ));
}
