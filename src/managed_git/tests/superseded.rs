use super::*;
use crate::managed_git::retire_superseded_working_git_journals;

const OLD: &str = "old-feather-session";
const COMPLETED: &str = "completed-feather-session";
const RUN: &str = "replacement-run";
const TASK: &str = "Fix feather mask";

struct Fixture {
    root: PathBuf,
    state: PathBuf,
    project: agent::AgentProject,
    store: agent::TursoAgentStore,
    old: agent::GitFinalizationRecord,
    completed: agent::GitFinalizationRecord,
}

impl Fixture {
    fn new(folders: bool, sealed_field: Option<(&str, serde_json::Value)>, finish: bool) -> Self {
        let root = temp_root("superseded-working");
        let state = root.join("state");
        let path = root.join("project");
        init_tasks(&path, folders).unwrap();
        TaskBoard::for_project(&path)
            .insert_content(
                TaskStatus::Doing,
                None,
                &format!("{TASK} COMPLETED 2026-10-22: verified codex:{COMPLETED}"),
            )
            .unwrap();
        initialize_test_git_repository(&path);
        let path = fs::canonicalize(path).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
        store.register_project_blocking(&path, "project").unwrap();
        store
            .set_project_git_mode_for_path_blocking(&path, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .mark_session_running_blocking(
                project.id,
                COMPLETED,
                123,
                RUN,
                &root.join("run.out"),
                &root.join("run.err"),
            )
            .unwrap();
        let start = capture_agent_git_start_state(&path, AgentGitMode::Commit).unwrap();
        ensure_agent_git_working_record(&store, &project, COMPLETED, RUN, Some(&start)).unwrap();
        bind_agent_git_working_task_identity(&store, &project, COMPLETED, RUN).unwrap();
        fs::write(path.join("feature.txt"), "verified implementation\n").unwrap();
        run_test_git(&path, &["add", "feature.txt"]);
        let context = AutomatedAgentChildContext {
            project_id: project.id,
            run_token: RUN.into(),
        };
        move_task_to_done_with_agent_store(&path, TaskStatus::Doing, "1", &context, &store)
            .unwrap();
        run_test_git(&path, &["add", "--all"]);
        run_test_agent_git(
            &path,
            &[
                "commit",
                "-m",
                "Fix feather mask",
                "-m",
                &format!("CLT-Task: codex:{COMPLETED}"),
            ],
        );
        let pending = store
            .git_finalization_blocking(project.id, COMPLETED)
            .unwrap()
            .unwrap();
        let completed = if finish {
            let completed =
                reconcile_agent_git_finalization(&store, &path, pending, Some(RUN), None).unwrap();
            assert_eq!(completed.state, GitFinalizationState::Completed);
            completed
        } else {
            pending
        };
        store
            .clear_running_session_control_blocking(project.id, COMPLETED, Some(RUN))
            .unwrap();
        let mut baseline: serde_json::Value =
            serde_json::from_str(&start.worktree_baseline).unwrap();
        if let Some((field, value)) = sealed_field {
            baseline[field] = value;
        }
        let baseline = serde_json::to_string(&baseline).unwrap();
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: OLD,
                git_mode: AgentGitMode::Commit,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &baseline,
                task_identity: completed.task_identity.as_deref(),
                owner_run_token: None,
                created_at: "100",
            })
            .unwrap();
        store
            .ensure_pending_git_finalization_resume_requested_blocking(project.id, OLD)
            .unwrap();
        let old = store
            .git_finalization_blocking(project.id, OLD)
            .unwrap()
            .unwrap();
        Self {
            root,
            state,
            project,
            store,
            old,
            completed,
        }
    }

    fn retire(&self) -> usize {
        retire_superseded_working_git_journals(&self.state, &self.project).unwrap()
    }

    fn assert_unchanged(&self) {
        assert_eq!(
            self.store
                .git_finalization_blocking(self.project.id, OLD)
                .unwrap()
                .unwrap(),
            self.old
        );
        assert_eq!(
            self.store
                .git_finalization_blocking(self.project.id, COMPLETED)
                .unwrap()
                .unwrap(),
            self.completed
        );
        assert!(
            self.store
                .session_control_blocking(self.project.id, OLD)
                .unwrap()
                .is_some()
        );
    }

    fn cleanup(self) {
        drop(self.store);
        fs::remove_dir_all(self.root).unwrap();
    }
}

#[test]
fn superseded_working_attempt_retires_only_after_proven_completion() {
    for folders in [false, true] {
        let fixture = Fixture::new(folders, None, true);
        let now = agent_timestamp();
        fixture
            .store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
                project_id: fixture.project.id,
                status: "failure",
                started_at: &now,
                finished_at: Some(&now),
                exit_code: None,
                log_dir: None,
                stdout_path: None,
                stderr_path: None,
                summary: Some("The task's Git journal remains WORKING"),
                codex_session_id: Some(OLD),
            })
            .unwrap();
        fs::write(
            fixture.project.path.join("shared.txt"),
            "shared staged work\n",
        )
        .unwrap();
        run_test_git(&fixture.project.path, &["add", "shared.txt"]);
        fs::write(
            fixture.project.path.join("feature.txt"),
            "concurrent edit\n",
        )
        .unwrap();
        let head = run_test_git(&fixture.project.path, &["rev-parse", "HEAD"]);
        let index = run_test_git(&fixture.project.path, &["write-tree"]);
        let diff = run_test_git(&fixture.project.path, &["diff", "HEAD"]);
        assert_eq!(
            reconcile_orphaned_agent_git_journals(&fixture.state, &fixture.project).unwrap(),
            1
        );
        assert_eq!(fixture.retire(), 0);
        let cancelled = fixture
            .store
            .git_finalization_blocking(fixture.project.id, OLD)
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.state, GitFinalizationState::Cancelled);
        assert_eq!(cancelled.generation, fixture.old.generation + 1);
        assert_eq!(cancelled.task_identity, fixture.old.task_identity);
        assert_eq!(cancelled.starting_head, fixture.old.starting_head);
        assert_eq!(cancelled.worktree_baseline, fixture.old.worktree_baseline);
        assert!(cancelled.last_error.as_deref().unwrap().contains(COMPLETED));
        assert!(
            fixture
                .store
                .session_control_blocking(fixture.project.id, OLD)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            fixture
                .store
                .git_finalization_blocking(fixture.project.id, COMPLETED)
                .unwrap()
                .unwrap(),
            fixture.completed
        );
        assert_eq!(
            fixture.store.list_projects_blocking().unwrap()[0].failure_count,
            0
        );
        assert_eq!(
            run_test_git(&fixture.project.path, &["rev-parse", "HEAD"]),
            head
        );
        assert_eq!(run_test_git(&fixture.project.path, &["write-tree"]), index);
        assert_eq!(run_test_git(&fixture.project.path, &["diff", "HEAD"]), diff);
        fixture.cleanup();
    }
}

#[test]
fn superseded_working_attempt_preserves_uncertain_or_live_ownership() {
    for case in [
        "unfinished-proof",
        "duplicate-task",
        "old-marker",
        "live-session",
        "lease",
        "worker",
        "launch",
        "missing-completion",
    ] {
        let fixture = Fixture::new(false, None, case != "unfinished-proof");
        let board = TaskBoard::for_project(&fixture.project.path);
        match case {
            "duplicate-task" => board.insert_content(TaskStatus::Todo, None, TASK).unwrap(),
            "old-marker" => board
                .insert_content(
                    TaskStatus::Backlog,
                    None,
                    &format!("Different text codex:{OLD}"),
                )
                .unwrap(),
            "live-session" => {
                fixture
                    .store
                    .mark_session_running_blocking(
                        fixture.project.id,
                        OLD,
                        123,
                        "live",
                        &fixture.root.join("out"),
                        &fixture.root.join("err"),
                    )
                    .unwrap();
            }
            "lease" | "worker" => {
                fixture
                    .store
                    .try_acquire_lease_blocking(
                        fixture.project.id,
                        "live",
                        &agent_timestamp(),
                        &agent_timestamp_after(600),
                    )
                    .unwrap();
                if case == "worker" {
                    assert!(reserve_test_worker(
                        &fixture.store,
                        fixture.project.id,
                        "live-worker",
                        "live",
                        &agent_timestamp(),
                        12
                    ));
                }
            }
            "launch" => {
                let start =
                    capture_agent_git_start_state(&fixture.project.path, AgentGitMode::Commit)
                        .unwrap();
                assert!(
                    fixture
                        .store
                        .record_git_launch_state_blocking(
                            fixture.project.id,
                            "pending-launch",
                            AgentGitMode::Commit,
                            &start,
                            &agent_timestamp(),
                        )
                        .unwrap()
                );
            }
            "missing-completion" => {
                let entry = board.entries(TaskStatus::Done).unwrap().remove(0);
                board
                    .write_entry_content(
                        TaskStatus::Done,
                        &entry,
                        &format!("Different task codex:{COMPLETED}"),
                    )
                    .unwrap();
            }
            _ => {}
        }
        assert_eq!(fixture.retire(), 0, "{case}");
        fixture.assert_unchanged();
        fixture.cleanup();
    }
}

#[test]
fn superseded_working_attempt_preserves_partial_and_malformed_seals() {
    for (field, value) in [
        ("staged_index_tree", serde_json::json!("tree")),
        ("manifest_parent_head", serde_json::json!("parent")),
        ("staged_non_task_patch_ids", serde_json::json!({})),
        ("staged_index_tree", serde_json::json!(false)),
    ] {
        let fixture = Fixture::new(false, Some((field, value)), true);
        assert_eq!(fixture.retire(), 0);
        fixture.assert_unchanged();
        fixture.cleanup();
    }
}

#[test]
fn superseded_working_retirement_rechecks_both_journal_snapshots() {
    let fixture = Fixture::new(false, None, true);
    for change_old in [false, true] {
        let mut old = fixture.old.clone();
        let mut completed = fixture.completed.clone();
        if change_old {
            old.generation += 1;
        } else {
            completed.generation += 1;
        }
        assert!(
            !fixture
                .store
                .retire_superseded_working_git_finalization_blocking(
                    &old,
                    &completed,
                    &agent_timestamp()
                )
                .unwrap()
        );
        fixture.assert_unchanged();
    }
    fixture.cleanup();
}
