use crate::runner::tests::FakeAgentRunner;
use crate::test_support::prelude::*;
use crate::test_support::*;

struct OrphanWorkingJournalFixture {
    root: PathBuf,
    state_dir: PathBuf,
    project: agent::AgentProject,
    journal: agent::GitFinalizationRecord,
    current_head: String,
}

fn orphan_working_journal_fixture(name: &str, task_bound: bool) -> OrphanWorkingJournalFixture {
    let root = temp_root(name);
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    add_task(&project_root, "Fresh orphan replacement", None).unwrap();
    fs::write(project_root.join("work.txt"), "Initial committed work\n").unwrap();
    let starting_head = initialize_test_git_repository(&project_root);
    fs::write(project_root.join("work.txt"), "Old uncommitted baseline\n").unwrap();
    let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
    assert_eq!(start.starting_head, starting_head);
    let baseline: serde_json::Value = serde_json::from_str(&start.worktree_baseline).unwrap();
    assert!(
        !baseline["tracked_patch_ids"]
            .as_object()
            .unwrap()
            .is_empty()
    );

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    assert!(
        store
            .set_project_git_mode_for_path_blocking(&project_root, AgentGitMode::Commit)
            .unwrap()
    );
    let project = store.list_projects_blocking().unwrap().remove(0);
    let task_identity =
        task_bound.then(|| durable_task_identity("Fresh orphan replacement").unwrap());
    assert!(
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: "session-orphan-working",
                git_mode: AgentGitMode::Commit,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &start.worktree_baseline,
                task_identity: task_identity.as_deref(),
                owner_run_token: None,
                created_at: "100",
            })
            .unwrap()
    );
    store
        .set_session_control_recovery_token_blocking(
            project.id,
            "session-orphan-working",
            if task_bound {
                "dead-worker-token"
            } else {
                "clt-git-finalization:0"
            },
        )
        .unwrap();
    let journal = store
        .git_finalization_blocking(project.id, "session-orphan-working")
        .unwrap()
        .unwrap();
    assert_eq!(journal.state, GitFinalizationState::Working);
    assert_eq!(journal.generation, 0);
    drop(store);

    fs::write(
        project_root.join("work.txt"),
        "New independently committed work\n",
    )
    .unwrap();
    run_test_git(&project_root, &["add", "--all"]);
    run_test_git(&project_root, &["commit", "-m", "Later independent work"]);
    let current_head = run_test_git(&project_root, &["rev-parse", "HEAD"]);
    assert_ne!(current_head, starting_head);
    assert!(run_test_git(&project_root, &["status", "--porcelain"]).is_empty());

    OrphanWorkingJournalFixture {
        root,
        state_dir,
        project,
        journal,
        current_head,
    }
}

#[test]
fn scheduler_cancels_orphan_working_journal_and_schedules_fresh_todo_without_git_changes() {
    let fixture = orphan_working_journal_fixture("scheduler-orphan-working-fresh-todo", false);
    let todo_before = fs::read(fixture.project.path.join("tasks/todo.md")).unwrap();
    let work_before = fs::read(fixture.project.path.join("work.txt")).unwrap();
    let start =
        run_agent_scheduler_pass_with_max_global_jobs(&fixture.state_dir, false, &[], 1, None)
            .unwrap();

    assert_eq!(start.jobs.len(), 1);
    assert_eq!(start.pass.skipped_active_lease, 0);
    assert_eq!(start.jobs[0].task_selection, AgentTaskSelection::NextTodo);
    assert_eq!(start.jobs[0].resume_session_id, None);
    let store = agent::TursoAgentStore::open_blocking(&fixture.state_dir).unwrap();
    let cancelled = store
        .git_finalization_blocking(fixture.project.id, &fixture.journal.codex_session_id)
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.state, GitFinalizationState::Cancelled);
    assert_eq!(cancelled.generation, fixture.journal.generation + 1);
    assert_eq!(cancelled.owner_run_token, None);
    assert_eq!(cancelled.git_mode, fixture.journal.git_mode);
    assert_eq!(cancelled.starting_head, fixture.journal.starting_head);
    assert_eq!(cancelled.branch_ref, fixture.journal.branch_ref);
    assert_eq!(cancelled.upstream_ref, fixture.journal.upstream_ref);
    assert_eq!(
        cancelled.worktree_baseline,
        fixture.journal.worktree_baseline
    );
    assert_eq!(cancelled.task_identity, fixture.journal.task_identity);
    assert_eq!(cancelled.commit_oid, fixture.journal.commit_oid);
    assert_eq!(cancelled.created_at, fixture.journal.created_at);
    assert!(
        store
            .session_control_blocking(fixture.project.id, &fixture.journal.codex_session_id)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .list_pending_git_finalizations_blocking(Some(fixture.project.id))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        run_test_git(&fixture.project.path, &["rev-parse", "HEAD"]),
        fixture.current_head
    );
    assert!(run_test_git(&fixture.project.path, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        fs::read(fixture.project.path.join("tasks/todo.md")).unwrap(),
        todo_before
    );
    assert_eq!(
        fs::read(fixture.project.path.join("work.txt")).unwrap(),
        work_before
    );
    assert!(
        store
            .release_lease_blocking(fixture.project.id, &start.jobs[0].holder)
            .unwrap()
    );
    drop(store);
    fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn scheduler_preserves_task_bound_working_journal_after_head_advances() {
    let fixture = orphan_working_journal_fixture("scheduler-bound-working-preserved", true);
    let start =
        run_agent_scheduler_pass_with_max_global_jobs(&fixture.state_dir, false, &[], 1, None)
            .unwrap();
    let store = agent::TursoAgentStore::open_blocking(&fixture.state_dir).unwrap();
    let current = store
        .git_finalization_blocking(fixture.project.id, &fixture.journal.codex_session_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.state, GitFinalizationState::Working);
    assert_eq!(current.generation, fixture.journal.generation);
    assert_eq!(current.task_identity, fixture.journal.task_identity);
    assert_eq!(current.starting_head, fixture.journal.starting_head);
    assert_eq!(current.worktree_baseline, fixture.journal.worktree_baseline);
    assert_eq!(
        store
            .session_control_blocking(fixture.project.id, &fixture.journal.codex_session_id)
            .unwrap()
            .unwrap()
            .run_token
            .as_deref(),
        Some("clt-git-finalization:0")
    );
    assert!(
        start
            .jobs
            .iter()
            .all(|job| job.task_selection != AgentTaskSelection::NextTodo)
    );
    for job in &start.jobs {
        assert!(
            store
                .release_lease_blocking(fixture.project.id, &job.holder)
                .unwrap()
        );
    }
    drop(store);
    fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn scheduler_preserves_unbound_working_journal_with_unrelated_idle_resume_token() {
    let fixture = orphan_working_journal_fixture("scheduler-orphan-unrelated-resume-token", false);
    let store = agent::TursoAgentStore::open_blocking(&fixture.state_dir).unwrap();
    store
        .set_session_control_recovery_token_blocking(
            fixture.project.id,
            &fixture.journal.codex_session_id,
            "unrelated-idle-session-owner",
        )
        .unwrap();
    drop(store);

    let start =
        run_agent_scheduler_pass_with_max_global_jobs(&fixture.state_dir, false, &[], 1, None)
            .unwrap();
    assert!(start.jobs.is_empty());
    let store = agent::TursoAgentStore::open_blocking(&fixture.state_dir).unwrap();
    let current = store
        .git_finalization_blocking(fixture.project.id, &fixture.journal.codex_session_id)
        .unwrap()
        .unwrap();
    assert_eq!(current, fixture.journal);
    let control = store
        .session_control_blocking(fixture.project.id, &fixture.journal.codex_session_id)
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::ResumeRequested);
    assert_eq!(control.child_pid, None);
    assert_eq!(
        control.run_token.as_deref(),
        Some("unrelated-idle-session-owner")
    );
    assert_eq!(
        run_test_git(&fixture.project.path, &["rev-parse", "HEAD"]),
        fixture.current_head
    );
    assert!(run_test_git(&fixture.project.path, &["status", "--porcelain"]).is_empty());
    assert!(
        store
            .lease_for_project_blocking(fixture.project.id)
            .unwrap()
            .is_none()
    );
    drop(store);
    fs::remove_dir_all(fixture.root).unwrap();
}

#[test]
fn scheduler_preserves_orphan_working_journal_while_live_ownership_remains() {
    for ownership in ["lease", "session", "worker"] {
        let fixture = orphan_working_journal_fixture(
            &format!("scheduler-orphan-working-live-{ownership}"),
            false,
        );
        let store = agent::TursoAgentStore::open_blocking(&fixture.state_dir).unwrap();
        let holder = format!("clt-agent-{}", std::process::id());
        let mut inline_generation = None;
        match ownership {
            "lease" => assert!(
                store
                    .try_acquire_lease_blocking(
                        fixture.project.id,
                        &holder,
                        &agent_timestamp(),
                        &agent_timestamp_after(60)
                    )
                    .unwrap()
            ),
            "session" => store
                .mark_session_running_blocking(
                    fixture.project.id,
                    &fixture.journal.codex_session_id,
                    std::process::id(),
                    "live-session-token",
                    &fixture.root.join("session.out"),
                    &fixture.root.join("session.err"),
                )
                .unwrap(),
            "worker" => {
                assert!(
                    store
                        .try_acquire_lease_blocking(
                            fixture.project.id,
                            &holder,
                            &agent_timestamp(),
                            &agent_timestamp_after(60),
                        )
                        .unwrap()
                );
                let worker_token = "live-orphan-protection-worker";
                inline_generation = Some(InlineAgentWorkerGeneration::register(worker_token));
                assert!(crate::worker::tests::reserve_test_inline_worker(
                    &store,
                    fixture.project.id,
                    worker_token,
                    &holder,
                    std::process::id(),
                    &agent_timestamp(),
                ));
            }
            _ => unreachable!(),
        }
        drop(store);

        let start =
            run_agent_scheduler_pass_with_max_global_jobs(&fixture.state_dir, false, &[], 1, None)
                .unwrap();
        assert!(start.jobs.is_empty(), "ownership={ownership}");
        let store = agent::TursoAgentStore::open_blocking(&fixture.state_dir).unwrap();
        let current = store
            .git_finalization_blocking(fixture.project.id, &fixture.journal.codex_session_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            current.state,
            GitFinalizationState::Working,
            "ownership={ownership}"
        );
        assert_eq!(current.generation, fixture.journal.generation);
        assert_eq!(current.starting_head, fixture.journal.starting_head);
        assert_eq!(current.worktree_baseline, fixture.journal.worktree_baseline);
        assert!(
            store
                .session_control_blocking(fixture.project.id, &fixture.journal.codex_session_id,)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            run_test_git(&fixture.project.path, &["rev-parse", "HEAD"]),
            fixture.current_head
        );
        assert!(run_test_git(&fixture.project.path, &["status", "--porcelain"]).is_empty());
        drop(store);
        drop(inline_generation);
        fs::remove_dir_all(fixture.root).unwrap();
    }
}

#[test]
fn agent_daemon_rebuilds_an_active_worker_index_with_quoted_state_names() {
    for state in ["dispatching", "running", "finalizing"] {
        let operation_calls = Cell::new(0);
        let rebuild_calls = Cell::new(0);

        let result = run_agent_daemon_database_operation_with_recovery(
            || {
                operation_calls.set(operation_calls.get() + 1);
                if operation_calls.get() == 1 {
                    anyhow::bail!(
                        "Failed to reserve worker token: Parse error: no such column: {state}"
                    );
                }
                Ok("recovered")
            },
            || {
                rebuild_calls.set(rebuild_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(result, "recovered");
        assert_eq!(operation_calls.get(), 2);
        assert_eq!(rebuild_calls.get(), 1);
    }
}

#[test]
fn agent_daemon_does_not_rebuild_an_unrecoverable_worker_index_twice() {
    let operation_calls = Cell::new(0);
    let rebuild_calls = Cell::new(0);
    let result: Result<()> = run_agent_daemon_database_operation_with_recovery(
        || {
            operation_calls.set(operation_calls.get() + 1);
            anyhow::bail!("Parse error: no such column: dispatching");
        },
        || {
            rebuild_calls.set(rebuild_calls.get() + 1);
            Ok(())
        },
    );

    assert!(result.is_err());
    assert_eq!(operation_calls.get(), 2);
    assert_eq!(rebuild_calls.get(), 1);
}

#[test]
fn agent_daemon_does_not_retry_after_worker_index_rebuild_fails() {
    let operation_calls = Cell::new(0);
    let result: Result<()> = run_agent_daemon_database_operation_with_recovery(
        || {
            operation_calls.set(operation_calls.get() + 1);
            anyhow::bail!("Parse error: no such column: dispatching");
        },
        || anyhow::bail!("Agent database integrity check still fails"),
    );

    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("Parse error: no such column: dispatching"));
    assert!(error.contains("Agent database integrity check still fails"));
    assert_eq!(operation_calls.get(), 1);
}

#[test]
fn agent_daemon_does_not_rebuild_worker_index_for_an_unrelated_missing_column() {
    let rebuild_calls = Cell::new(0);
    let result: Result<()> = run_agent_daemon_database_operation_with_recovery(
        || anyhow::bail!("Failed to reserve worker token: Parse error: no such column: project_id"),
        || {
            rebuild_calls.set(rebuild_calls.get() + 1);
            Ok(())
        },
    );

    assert!(result.is_err());
    assert_eq!(rebuild_calls.get(), 0);
}

#[test]
fn agent_scheduling_decision_stage_is_pure_and_prioritized() {
    let decide = |has_resume_session,
                  resume_interrupted_task,
                  has_blocked_task,
                  blocked_recovery_backoff_active,
                  has_pending_task| {
        decide_agent_scheduling_stage(AgentSchedulingDecisionRequest {
            has_resume_session,
            resume_interrupted_task,
            has_blocked_task,
            blocked_recovery_backoff_active,
            has_pending_task,
        })
        .task_selection
    };

    assert_eq!(
        decide(true, true, true, false, true),
        Some(AgentTaskSelection::ResumeSession)
    );
    assert_eq!(
        decide(false, true, true, false, true),
        Some(AgentTaskSelection::ResumeDoing)
    );
    assert_eq!(
        decide(false, false, true, false, true),
        Some(AgentTaskSelection::RecoverBlocked)
    );
    assert_eq!(
        decide(false, false, true, true, true),
        Some(AgentTaskSelection::NextTodo)
    );
    assert_eq!(decide(false, false, false, false, false), None);
}

#[test]
fn agent_supervision_outcome_stage_classifies_without_side_effects() {
    let success = Command::new("true").status().unwrap();
    let idle = classify_agent_supervision_stage(AgentSupervisionOutcomeRequest {
        wait_result: AgentProcessWait::Exited(success),
        requested_control: None,
        reported_no_tasks: true,
        timeout: Duration::from_secs(9),
    });
    assert_eq!(idle.status, "idle");
    assert_eq!(idle.summary, "Codex reported no available tasks.");
    assert_eq!(idle.stderr_note, None);

    let timeout = classify_agent_supervision_stage(AgentSupervisionOutcomeRequest {
        wait_result: AgentProcessWait::TimedOut(None),
        requested_control: None,
        reported_no_tasks: false,
        timeout: Duration::from_secs(9),
    });
    assert_eq!(timeout.status, "timeout");
    assert_eq!(
        timeout.stderr_note.as_deref(),
        Some("Codex timed out after 9 seconds.")
    );
}

#[test]
fn agent_scan_detects_markdown_backed_pending_tasks() {
    let root = temp_root("agent-scan-markdown");
    add_task(&root, "agent should run", None).unwrap();
    add_task(&root, "agent is running this", None).unwrap();
    move_task(&root, TaskStatus::Todo, TaskStatus::Doing, "2").unwrap();

    let scan = scan_agent_project(&root);

    assert_eq!(scan.status, AgentProjectScanStatus::Pending);
    assert_eq!(scan.todo_count, 1);
    assert_eq!(scan.doing_count, 1);
    assert!(has_pending_agent_task(&root));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_detects_folder_backed_pending_tasks() {
    let root = temp_root("agent-scan-folder");
    init_tasks(&root, true).unwrap();
    fs::write(
        root.join("tasks/todo/0010-write-agent-runner.md"),
        "Write agent runner. Include tests.\n",
    )
    .unwrap();

    let scan = scan_agent_project(&root);

    assert_eq!(scan.status, AgentProjectScanStatus::Pending);
    assert_eq!(scan.todo_count, 1);
    assert_eq!(scan.doing_count, 0);
    assert!(has_pending_agent_task(&root));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_only_reports_blocked_when_every_doing_task_has_a_blocked_note() {
    let root = temp_root("agent-scan-blocked-markdown");
    init_tasks(&root, false).unwrap();
    fs::write(
        root.join("tasks/doing.md"),
        "# Doing Tasks\n- first task — BLOCKED 2026-08-09: waiting on a fixture\n- second task\n",
    )
    .unwrap();

    let partially_blocked = scan_agent_project(&root);

    assert_eq!(partially_blocked.status, AgentProjectScanStatus::Empty);
    assert_eq!(partially_blocked.doing_count, 2);
    assert_eq!(partially_blocked.blocked_doing_count, 1);
    assert!(!partially_blocked.all_actionable_tasks_blocked());

    fs::write(
            root.join("tasks/doing.md"),
            "# Doing Tasks\n- first task — BLOCKED 2026-08-09: waiting on a fixture\n- second task — blocked 2026-08-09: same fixture\n",
        )
        .unwrap();

    let all_blocked = scan_agent_project(&root);

    assert_eq!(all_blocked.status, AgentProjectScanStatus::Blocked);
    assert_eq!(all_blocked.blocked_doing_count, 2);
    assert!(all_blocked.all_actionable_tasks_blocked());
    assert!(all_blocked.has_schedulable_work());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_skips_blocked_todos_until_a_newer_note_unblocks_them() {
    let root = temp_root("agent-scan-blocked-todo");
    init_tasks(&root, false).unwrap();
    fs::write(
            root.join("tasks/todo.md"),
            "# Todo Tasks\n- first — BLOCKED 2026-08-09: dependency unavailable\n- second — BLOCKED 2026-08-09: fixture unavailable\n",
        )
        .unwrap();

    let all_blocked = scan_agent_project(&root);

    assert_eq!(all_blocked.status, AgentProjectScanStatus::Blocked);
    assert_eq!(all_blocked.todo_count, 2);
    assert_eq!(all_blocked.blocked_todo_count, 2);
    assert_eq!(all_blocked.available_todo_count(), 0);
    assert!(!all_blocked.has_pending_task());
    assert!(all_blocked.all_actionable_tasks_blocked());

    fs::write(
            root.join("tasks/todo.md"),
            "# Todo Tasks\n- first — BLOCKED 2026-08-09: dependency unavailable — UNBLOCKED 2026-08-09: dependency restored\n- second — BLOCKED 2026-08-09: fixture unavailable\n",
        )
        .unwrap();

    let one_available = scan_agent_project(&root);

    assert_eq!(one_available.status, AgentProjectScanStatus::Pending);
    assert_eq!(one_available.todo_count, 2);
    assert_eq!(one_available.blocked_todo_count, 1);
    assert_eq!(one_available.available_todo_count(), 1);
    assert!(one_available.has_pending_task());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_detects_folder_task_blocked_note_headings_without_matching_titles() {
    let root = temp_root("agent-scan-blocked-folder");
    init_tasks(&root, true).unwrap();
    fs::write(
        root.join("tasks/doing/0001-waiting.md"),
        "Waiting task.\n\nBlocked note:\n- BLOCKED 2026-08-09: dependency unavailable.\n",
    )
    .unwrap();

    let blocked = scan_agent_project(&root);

    assert_eq!(blocked.status, AgentProjectScanStatus::Blocked);
    assert_eq!(blocked.blocked_doing_count, 1);

    fs::write(
        root.join("tasks/doing/0001-waiting.md"),
        "Add blocked-task monitoring without a blocker note.\n",
    )
    .unwrap();

    let title_only = scan_agent_project(&root);

    assert_eq!(title_only.status, AgentProjectScanStatus::Empty);
    assert_eq!(title_only.blocked_doing_count, 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_does_not_treat_backlog_as_actionable_work() {
    let root = temp_root("agent-scan-backlog");
    add_task(&root, "not ready for an agent", None).unwrap();
    move_task(&root, TaskStatus::Todo, TaskStatus::Backlog, "1").unwrap();

    let scan = scan_agent_project(&root);

    assert_eq!(scan.status, AgentProjectScanStatus::Empty);
    assert_eq!(scan.todo_count, 0);
    assert_eq!(scan.doing_count, 0);
    assert!(!has_pending_agent_task(&root));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scan_reports_empty_missing_uninitialized_and_unavailable_projects() {
    let root = temp_root("agent-scan-states");
    let empty_project = root.join("empty");
    init_tasks(&empty_project, false).unwrap();

    let uninitialized_project = root.join("uninitialized");
    fs::create_dir_all(&uninitialized_project).unwrap();

    let unreadable_project = root.join("unavailable");
    fs::create_dir_all(unreadable_project.join("tasks")).unwrap();
    fs::write(unreadable_project.join("tasks/todo.md"), [0xff, 0xfe]).unwrap();
    fs::write(unreadable_project.join("tasks/doing.md"), "# Doing Tasks\n").unwrap();
    fs::write(unreadable_project.join("tasks/done.md"), "# Done Tasks\n").unwrap();

    assert_eq!(
        scan_agent_project(&empty_project),
        AgentProjectScan::empty()
    );
    assert_eq!(
        scan_agent_project(&root.join("missing")),
        AgentProjectScan::missing()
    );
    assert_eq!(
        scan_agent_project(&uninitialized_project),
        AgentProjectScan::uninitialized()
    );

    let scan = scan_agent_project(&unreadable_project);
    assert_eq!(scan.todo_count, 0);
    assert!(matches!(
        scan.status,
        AgentProjectScanStatus::Unavailable(_)
    ));
    assert!(!has_pending_agent_task(&unreadable_project));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_records_project_scan_timestamp() {
    let root = temp_root("agent-scan-store");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();

    let project = store.list_projects_blocking().unwrap().remove(0);
    assert_eq!(project.last_scan_at, None);

    let scanned_at = store.record_project_scan_blocking(project.id).unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);

    assert_eq!(project.last_scan_at, Some(scanned_at));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_records_and_clears_daemon_scan_errors() {
    let root = temp_root("agent-daemon-scan-store");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap()[0].id;

    store
        .record_project_daemon_scan_blocking(
            project_id,
            "unavailable",
            Some("Operation not permitted (os error 1)"),
        )
        .unwrap();
    let failed = store.list_projects_blocking().unwrap().remove(0);

    assert_eq!(
        failed.last_daemon_scan_status.as_deref(),
        Some("unavailable")
    );
    assert_eq!(
        failed.last_daemon_scan_error.as_deref(),
        Some("Operation not permitted (os error 1)")
    );

    store
        .record_project_daemon_scan_blocking(project_id, "pending", None)
        .unwrap();
    let recovered = store.list_projects_blocking().unwrap().remove(0);

    assert_eq!(
        recovered.last_daemon_scan_status.as_deref(),
        Some("pending")
    );
    assert_eq!(recovered.last_daemon_scan_error, None);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scheduler_persists_unavailable_scan_for_the_ui() {
    let root = temp_root("agent-scheduler-scan-error");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    let tasks_dir = project_root.join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(tasks_dir.join("todo.md"), [0xff, 0xfe]).unwrap();
    fs::write(tasks_dir.join("doing.md"), "# Doing Tasks\n").unwrap();
    fs::write(tasks_dir.join("done.md"), "# Done Tasks\n").unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();

    let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);

    assert_eq!(start.pass.pending_projects, 0);
    assert_eq!(
        project.last_daemon_scan_status.as_deref(),
        Some("unavailable")
    );
    assert!(project.last_daemon_scan_error.is_some());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_records_and_clears_daemon_checkins() {
    let root = temp_root("agent-daemon-checkin-store");
    let state_dir = root.join("state/clt");
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    store
        .record_daemon_checkin_blocking("clt-agent-1", "cli", "100", "110", "155")
        .unwrap();
    let checkins = store.list_daemon_checkins_blocking().unwrap();

    assert_eq!(checkins.len(), 1);
    assert_eq!(checkins[0].holder, "clt-agent-1");
    assert_eq!(checkins[0].mode, "cli");
    assert_eq!(checkins[0].started_at, "100");
    assert_eq!(checkins[0].checked_in_at, "110");
    assert_eq!(checkins[0].expires_at, "155");

    assert!(store.clear_daemon_checkin_blocking("clt-agent-1").unwrap());
    assert!(store.list_daemon_checkins_blocking().unwrap().is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_runtime_status_prefers_fresh_checkins() {
    let fresh_cli = agent::AgentDaemonCheckin {
        holder: "clt-agent-1".to_string(),
        mode: "cli".to_string(),
        started_at: "100".to_string(),
        checked_in_at: "120".to_string(),
        expires_at: "200".to_string(),
    };
    let fresh_service = agent::AgentDaemonCheckin {
        holder: "clt-agent-2".to_string(),
        mode: "service".to_string(),
        started_at: "100".to_string(),
        checked_in_at: "120".to_string(),
        expires_at: "200".to_string(),
    };
    let stale_cli = agent::AgentDaemonCheckin {
        expires_at: "150".to_string(),
        ..fresh_cli.clone()
    };

    assert_eq!(
        format_agent_daemon_runtime_status("installed", std::slice::from_ref(&fresh_cli), 160,),
        "cli active"
    );
    assert_eq!(
        format_agent_daemon_runtime_status("running", &[fresh_cli], 160),
        "cli active; service no-check-in"
    );
    assert_eq!(
        format_agent_daemon_runtime_status("running", &[fresh_service], 160),
        "service active"
    );
    assert_eq!(
        format_agent_daemon_runtime_status("installed", &[stale_cli], 160),
        "cli stale"
    );
    assert_eq!(
        format_agent_daemon_runtime_status("installed", &[], 160),
        "service disabled"
    );
    assert_eq!(
        format_agent_daemon_runtime_status("not-installed", &[], 160),
        "disabled"
    );
}

#[test]
fn agent_service_restart_requires_running_service_with_only_stale_service_checkins() {
    let stale_service = agent::AgentDaemonCheckin {
        holder: "clt-agent-1".to_string(),
        mode: "service".to_string(),
        started_at: "100".to_string(),
        checked_in_at: "120".to_string(),
        expires_at: "150".to_string(),
    };
    let fresh_service = agent::AgentDaemonCheckin {
        holder: "clt-agent-2".to_string(),
        expires_at: "200".to_string(),
        ..stale_service.clone()
    };
    let stale_cli = agent::AgentDaemonCheckin {
        mode: "cli".to_string(),
        ..stale_service.clone()
    };

    assert!(agent_service_needs_restart(
        "running",
        std::slice::from_ref(&stale_service),
        160
    ));
    assert!(agent_service_needs_restart(
        "running",
        &[stale_service.clone(), stale_cli.clone()],
        160
    ));
    assert!(!agent_service_needs_restart(
        "installed",
        std::slice::from_ref(&stale_service),
        160
    ));
    assert!(!agent_service_needs_restart(
        "running",
        &[stale_service, fresh_service],
        160
    ));
    assert!(!agent_service_needs_restart("running", &[stale_cli], 160));
}

#[test]
fn agent_run_once_records_pending_projects_up_to_default_capacity() {
    let root = temp_root("agent-run-once");
    let state_dir = root.join("state/clt");
    let first_project = root.join("alpha");
    let second_project = root.join("beta");
    init_tasks(&first_project, false).unwrap();
    init_tasks(&second_project, false).unwrap();
    add_task(&first_project, "first task", None).unwrap();
    add_task(&second_project, "second task", None).unwrap();
    let first_project = fs::canonicalize(first_project).unwrap();
    let second_project = fs::canonicalize(second_project).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&first_project, "alpha")
        .unwrap();
    store
        .register_project_blocking(&second_project, "beta")
        .unwrap();
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        pass,
        AgentSchedulerPass {
            scanned_projects: 2,
            pending_projects: 2,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 2,
            runs_recorded: 2,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 2);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_loop_repeats_passes_and_respects_success_cooldown() {
    let root = temp_root("agent-daemon-loop");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "daemon task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let runner = Arc::new(FakeAgentRunner::new(&state_dir, "success"));
    drop(store);

    let daemon_runner: Arc<dyn AgentRunner> = runner.clone();
    run_agent_daemon_loop(&state_dir, daemon_runner, Duration::ZERO, Some(2)).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_dispatches_independent_worker_outside_async_runtime() {
    let root = temp_root("agent-daemon-independent-dispatch");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "independent daemon task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project_id = store.list_projects_blocking().unwrap().remove(0).id;
    drop(store);

    run_agent_daemon_loop_with_executor(
        &state_dir,
        AgentDaemonExecutor::Independent {
            executable: PathBuf::from("/tmp/pinned-clt-generation"),
            dispatch: dispatch_independent_agent_worker_without_service,
        },
        Duration::ZERO,
        Some(1),
        new_agent_shutdown_signal(),
    )
    .unwrap();

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let workers = store.list_active_workers_blocking().unwrap();
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].project_id, project_id);
    assert_eq!(workers[0].state, AGENT_WORKER_STATE_DISPATCHING);
    assert_eq!(
        store
            .lease_for_project_blocking(project_id)
            .unwrap()
            .unwrap()
            .holder,
        workers[0].lease_holder
    );
    assert_eq!(store.run_count_blocking().unwrap(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_loop_polls_while_run_is_active_without_reclaiming_own_lease() {
    let root = temp_root("agent-daemon-active-poll");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "long daemon task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let runner = Arc::new(FakeAgentRunner::with_delay(
        &state_dir,
        "success",
        Duration::from_millis(75),
    ));
    drop(store);

    let daemon_runner: Arc<dyn AgentRunner> = runner.clone();
    run_agent_daemon_loop(&state_dir, daemon_runner, Duration::from_millis(5), Some(2)).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_scheduler_returns_acquired_jobs_before_recording_runs() {
    let root = temp_root("agent-daemon-start-job");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "async daemon task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let mut start = run_agent_daemon_scheduler_pass(&state_dir).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        start.pass,
        AgentSchedulerPass {
            scanned_projects: 1,
            pending_projects: 1,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 1,
            runs_recorded: 0,
        }
    );
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(store.lease_count_blocking().unwrap(), 1);
    drop(store);

    let blocked_by_lease = run_agent_daemon_scheduler_pass(&state_dir).unwrap();
    assert_eq!(blocked_by_lease.jobs.len(), 0);
    assert_eq!(blocked_by_lease.pass.skipped_active_lease, 1);

    let runner = FakeAgentRunner::new(&state_dir, "success");
    let shutdown = new_agent_shutdown_signal();
    let completion = run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(completion.status, "success");
    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_scheduler_records_checkin_with_registry_lookup() {
    let root = temp_root("agent-daemon-checkin");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "scheduled task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let checkin = AgentDaemonCheckinSource {
        holder: "clt-scheduler-test".to_string(),
        mode: "cli".to_string(),
        started_at: "100".to_string(),
    };
    let start = run_agent_daemon_scheduler_pass_with_active_and_checkin(
        &state_dir,
        Vec::new(),
        Some(&checkin),
    )
    .unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let checkins = store.list_daemon_checkins_blocking().unwrap();

    assert_eq!(start.pass.scanned_projects, 1);
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(start.jobs[0].holder, checkin.holder);
    assert_eq!(checkins.len(), 1);
    assert_eq!(checkins[0].mode, "cli");
    assert!(daemon_checkin_is_fresh(
        &checkins[0],
        agent_timestamp_seconds()
    ));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_daemon_scheduler_defers_pending_projects_when_active_jobs_fill_capacity() {
    let root = temp_root("agent-daemon-active-capacity");
    let state_dir = root.join("state/clt");
    let first_project = root.join("alpha");
    let second_project = root.join("beta");
    init_tasks(&first_project, false).unwrap();
    init_tasks(&second_project, false).unwrap();
    add_task(&first_project, "active task", None).unwrap();
    add_task(&second_project, "deferred task", None).unwrap();
    let first_project = fs::canonicalize(first_project).unwrap();
    let second_project = fs::canonicalize(second_project).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&first_project, "alpha")
        .unwrap();
    store
        .register_project_blocking(&second_project, "beta")
        .unwrap();
    let projects = store.list_projects_blocking().unwrap();
    let active_project = projects
        .iter()
        .find(|project| project.name == "alpha")
        .unwrap();
    assert!(
        store
            .try_acquire_lease_blocking(active_project.id, "active-daemon-run", "100", "9999999999")
            .unwrap()
    );
    let active_project_id = active_project.id;
    drop(store);

    let start = run_agent_scheduler_pass_with_max_global_jobs(
        &state_dir,
        false,
        &[active_project_id],
        1,
        None,
    )
    .unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(start.jobs.len(), 0);
    assert_eq!(
        start.pass,
        AgentSchedulerPass {
            scanned_projects: 2,
            pending_projects: 1,
            active_agent_jobs: 1,
            skipped_active_lease: 0,
            deferred_projects: 1,
            runs_started: 0,
            runs_recorded: 0,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(store.lease_count_blocking().unwrap(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_run_once_skips_disabled_projects_but_reconciles_abandoned_handoffs() {
    let root = temp_root("agent-run-disabled");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "disabled task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .set_session_control_state_blocking(
            project.id,
            "session-123",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    assert!(
        store
            .begin_stopped_session_interactive_blocking(
                project.id,
                "session-123",
                "clt-interactive-4294967295",
                None,
            )
            .unwrap()
    );
    store
        .set_project_enabled_blocking(project.id, false)
        .unwrap();
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        pass,
        AgentSchedulerPass {
            scanned_projects: 0,
            pending_projects: 0,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 0,
            runs_recorded: 0,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 0);
    assert_eq!(
        store
            .session_control_blocking(project.id, "session-123")
            .unwrap()
            .unwrap()
            .state,
        AgentSessionControlState::ResumeRequested
    );

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn agent_run_once_reclaims_dead_lease_for_disabled_project() {
    let root = temp_root("agent-run-disabled-dead-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "disabled task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "clt-agent-4294967295", "100", "9999999999",)
            .unwrap()
    );
    store
        .set_project_enabled_blocking(project.id, false)
        .unwrap();
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(pass.scanned_projects, 0);
    assert_eq!(pass.runs_started, 0);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn agent_run_once_reclaims_dead_orphaned_interactive_lease_before_expiry() {
    let root = temp_root("agent-run-disabled-dead-interactive-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "disabled task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    let holder = format!("clt-idle-interactive-worker-{}-1-1", u32::MAX);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, &holder, "100", "9999999999",)
            .unwrap()
    );
    store
        .set_project_enabled_blocking(project.id, false)
        .unwrap();
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(pass.scanned_projects, 0);
    assert_eq!(pass.runs_started, 0);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_run_once_skips_projects_with_active_lease() {
    let root = temp_root("agent-run-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "leased task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "test-holder", "100", "9999999999")
            .unwrap()
    );
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        pass,
        AgentSchedulerPass {
            scanned_projects: 1,
            pending_projects: 1,
            active_agent_jobs: 0,
            skipped_active_lease: 1,
            deferred_projects: 0,
            runs_started: 0,
            runs_recorded: 0,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 0);
    assert_eq!(store.lease_count_blocking().unwrap(), 1);
    assert_eq!(runner.ran_project_count(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_run_once_ignores_legacy_local_lock_directory() {
    let root = temp_root("agent-run-local-lock");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "locally locked task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    fs::create_dir(project_root.join(".codex-task-loop.lock")).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        pass,
        AgentSchedulerPass {
            scanned_projects: 1,
            pending_projects: 1,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 1,
            runs_recorded: 1,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn agent_run_once_reclaims_dead_local_process_lease() {
    let root = temp_root("agent-run-dead-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "dead leased task", None).unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "clt-agent-4294967295", "100", "9999999999")
            .unwrap()
    );
    let runner = FakeAgentRunner::new(&state_dir, "success");
    drop(store);

    let pass = run_agent_once_with_runner(&state_dir, &runner).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(
        pass,
        AgentSchedulerPass {
            scanned_projects: 1,
            pending_projects: 1,
            active_agent_jobs: 0,
            skipped_active_lease: 0,
            deferred_projects: 0,
            runs_started: 1,
            runs_recorded: 1,
        }
    );
    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);
    assert_eq!(runner.ran_project_count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn agent_scheduler_resumes_doing_task_after_crashed_process() {
    let root = temp_root("agent-resume-doing-dead-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "interrupted task", None).unwrap();
    move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "clt-agent-4294967295", "100", "9999999999")
            .unwrap()
    );
    drop(store);

    let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    assert_eq!(start.pass.pending_projects, 1);
    assert_eq!(start.pass.runs_started, 1);
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::ResumeDoing
    );

    let runner = FakeAgentRunner::new(&state_dir, "success");
    let shutdown = new_agent_shutdown_signal();
    run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();

    assert_eq!(runner.ran_project_count(), 1);
    assert_eq!(store.run_count_blocking().unwrap(), 1);
    assert_eq!(store.lease_count_blocking().unwrap(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scheduler_resumes_doing_task_after_lease_expiry() {
    let root = temp_root("agent-resume-doing-expired-lease");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    add_task(&project_root, "expired interrupted task", None).unwrap();
    move_task(&project_root, TaskStatus::Todo, TaskStatus::Doing, "1").unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(
        store
            .try_acquire_lease_blocking(project.id, "old-holder", "100", "101")
            .unwrap()
    );
    drop(store);

    let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    assert_eq!(start.pass.pending_projects, 1);
    assert_eq!(start.pass.runs_started, 1);
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::ResumeDoing
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scheduler_monitors_when_all_todo_and_doing_tasks_are_blocked() {
    let root = temp_root("agent-recover-blocked");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- queued — BLOCKED 2026-08-09: credentials unavailable\n",
    )
    .unwrap();
    fs::write(
            project_root.join("tasks/doing.md"),
            "# Doing Tasks\n- first — BLOCKED 2026-08-09: dependency unavailable\n- second — BLOCKED 2026-08-09: tests cannot start\n",
        )
        .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    assert_eq!(start.pass.pending_projects, 1);
    assert_eq!(start.pass.runs_started, 1);
    assert_eq!(start.jobs.len(), 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::RecoverBlocked
    );
    assert_eq!(start.jobs[0].blocked_task_count_before, 3);

    let runner = FakeAgentRunner::new(&state_dir, "success");
    let shutdown = new_agent_shutdown_signal();
    let completion = run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();

    assert_eq!(completion.status, "blocked");
    assert!(
        completion
            .summary
            .contains("left 3 blocked task(s) unresolved across todo and doing")
    );

    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    assert!(project.last_blocked_recovery_at.is_some());
    assert_eq!(project.failure_count, 0);
    drop(store);

    let backed_off = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert!(backed_off.jobs.is_empty());
    assert_eq!(backed_off.pass.runs_started, 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scheduler_rechecks_blocked_work_before_todo_then_uses_backoff() {
    let root = temp_root("agent-blocked-before-todo");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
            project_root.join("tasks/todo.md"),
            "# Todo Tasks\n- queued blocker — BLOCKED 2026-08-09: dependency unavailable\n- ready task\n",
        )
        .unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- waiting task — BLOCKED 2026-08-09: dependency unavailable\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let mut start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    assert_eq!(start.jobs.len(), 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::RecoverBlocked
    );
    assert_eq!(start.jobs[0].blocked_task_count_before, 2);

    let runner = FakeAgentRunner::new(&state_dir, "success");
    let shutdown = new_agent_shutdown_signal();
    let completion = run_agent_job(start.jobs.pop().unwrap(), &runner, &shutdown).unwrap();
    assert_eq!(completion.status, "blocked");
    assert!(
        completion
            .summary
            .contains("left 2 blocked task(s) unresolved")
    );

    let during_backoff = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();
    assert_eq!(during_backoff.jobs.len(), 1);
    assert_eq!(
        during_backoff.jobs[0].task_selection,
        AgentTaskSelection::NextTodo
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scheduler_recovers_blocked_work_without_taking_unblocked_doing() {
    let root = temp_root("agent-unblocked-doing");
    let state_dir = root.join("state/clt");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- queued blocker — BLOCKED 2026-08-09: dependency unavailable\n",
    )
    .unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        "# Doing Tasks\n- manually active task\n",
    )
    .unwrap();
    let project_root = fs::canonicalize(project_root).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    drop(store);

    let start = run_agent_scheduler_pass(&state_dir, false, &[]).unwrap();

    assert_eq!(start.jobs.len(), 1);
    assert_eq!(start.pass.pending_projects, 1);
    assert_eq!(start.pass.runs_started, 1);
    assert_eq!(
        start.jobs[0].task_selection,
        AgentTaskSelection::RecoverBlocked
    );

    fs::remove_dir_all(root).unwrap();
}
#[test]
fn agent_lease_holder_liveness_detects_current_process_holder() {
    let holder = agent_lease_holder();

    assert_eq!(agent_lease_holder_pid(&holder), Some(std::process::id()));
    assert_eq!(
        agent_lease_holder_liveness(&holder),
        AgentLeaseHolderLiveness::CurrentProcess
    );
    let interactive_holder = format!("clt-interactive-{}", std::process::id());
    assert_eq!(
        agent_lease_holder_pid(&interactive_holder),
        Some(std::process::id())
    );
    assert_eq!(
        agent_lease_holder_liveness(&agent_scheduler_lease_holder()),
        AgentLeaseHolderLiveness::CurrentProcess
    );
    assert_eq!(agent_lease_holder_pid("external-agent"), None);
}
#[test]
fn generated_interactive_holders_are_reclaimed_by_full_token_ttl_not_pid() {
    for holder in [
        InteractiveAgentLease::holder_for_current_process(),
        InteractiveAgentLease::holder_for_idle_session(),
        InteractiveAgentLease::holder_for_stopped_session(),
        interactive_guardian_holder(InteractiveGuardianDisposition::ResumeExec),
        interactive_guardian_holder(InteractiveGuardianDisposition::PreserveIdleSession),
        interactive_guardian_holder(InteractiveGuardianDisposition::RestoreStopped),
    ] {
        assert_eq!(agent_lease_holder_pid(&holder), None);
        assert_eq!(
            interactive_lease_holder_pid(&holder),
            Some(std::process::id())
        );
        assert_eq!(
            interactive_lease_holder_liveness(&holder),
            Some(AgentLeaseHolderLiveness::CurrentProcess)
        );
        let lease = agent::AgentLeaseRecord {
            project_id: 1,
            project_name: "project".to_string(),
            project_path: PathBuf::from("/tmp/project"),
            holder,
            acquired_at: "100".to_string(),
            expires_at: "200".to_string(),
        };

        assert!(!agent_lease_is_reclaimable(&lease, true, 199));
        assert!(agent_lease_is_reclaimable(&lease, false, 200));
    }
}

#[test]
fn agent_backoff_duration_requires_positive_integer() {
    assert_eq!(
        parse_agent_timeout_duration(AGENT_FAILURE_BACKOFF_SECONDS_ENV, "300").unwrap(),
        300
    );
    assert_eq!(
        parse_agent_timeout_duration(AGENT_SUCCESS_COOLDOWN_SECONDS_ENV, "5").unwrap(),
        5
    );
    assert!(parse_agent_timeout_duration(AGENT_FAILURE_BACKOFF_SECONDS_ENV, "0").is_err());
    assert!(parse_agent_timeout_duration(AGENT_SUCCESS_COOLDOWN_SECONDS_ENV, "soon").is_err());
}

#[test]
fn agent_project_cooldown_reason_reports_success_and_failure_delays() {
    let mut project = agent::AgentProject {
        id: 1,
        path: PathBuf::from("/tmp/project"),
        name: "project".to_string(),
        enabled: true,
        git_mode: AgentGitMode::Off,
        codex_provider: None,
        codex_model: None,
        codex_reasoning_effort: None,
        codex_fast_enabled: false,
        last_scan_at: None,
        last_daemon_scan_status: None,
        last_daemon_scan_error: None,
        last_run_at: None,
        last_success_at: Some("100".to_string()),
        last_failure_at: None,
        last_blocked_recovery_at: None,
        failure_count: 0,
    };

    assert_eq!(
        agent_project_cooldown_reason(
            &project,
            102,
            Duration::from_secs(5),
            Duration::from_secs(300)
        ),
        Some("success cooldown active for 3s".to_string())
    );

    project.last_failure_at = Some("100".to_string());
    project.failure_count = 1;

    assert_eq!(
        agent_project_cooldown_reason(
            &project,
            250,
            Duration::from_secs(5),
            Duration::from_secs(300)
        ),
        Some("failure backoff active for 150s".to_string())
    );
}

#[test]
fn blocked_task_recovery_uses_failure_backoff_without_delaying_todo_work() {
    let project = agent::AgentProject {
        id: 1,
        path: PathBuf::from("/tmp/project"),
        name: "project".to_string(),
        enabled: true,
        git_mode: AgentGitMode::Off,
        codex_provider: None,
        codex_model: None,
        codex_reasoning_effort: None,
        codex_fast_enabled: false,
        last_scan_at: None,
        last_daemon_scan_status: None,
        last_daemon_scan_error: None,
        last_run_at: Some("100".to_string()),
        last_success_at: None,
        last_failure_at: None,
        last_blocked_recovery_at: Some("100".to_string()),
        failure_count: 0,
    };

    assert_eq!(
        agent_task_cooldown_reason(
            &project,
            AgentTaskSelection::RecoverBlocked,
            250,
            Duration::from_secs(5),
            Duration::from_secs(300),
        ),
        Some("blocked-task recovery backoff active for 150s".to_string())
    );
    assert_eq!(
        agent_task_cooldown_reason(
            &project,
            AgentTaskSelection::NextTodo,
            250,
            Duration::from_secs(5),
            Duration::from_secs(300),
        ),
        None
    );
    assert_eq!(
        agent_task_cooldown_reason(
            &project,
            AgentTaskSelection::ResumeDoing,
            101,
            Duration::from_secs(5),
            Duration::from_secs(300),
        ),
        None
    );
}

#[test]
fn daemon_stops_without_database_retries_when_registry_recovery_is_required() {
    let root = temp_root("daemon-recovery-required");
    let state_dir = root.join("state/clt");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("recovery-required"),
        "shared WAL ownership failure",
    )
    .unwrap();
    let runner = Arc::new(FakeAgentRunner::new(&state_dir, "success"));
    let daemon_runner: Arc<dyn AgentRunner> = runner.clone();
    let error = run_agent_daemon_loop(&state_dir, daemon_runner, Duration::ZERO, None).unwrap_err();
    assert!(error.to_string().contains("recovery required"));
    assert_eq!(runner.ran_project_count(), 0);
    assert!(!state_dir.join(AGENT_DB_FILE).exists());
    fs::remove_dir_all(root).unwrap();
}
