use super::*;

const ORPHAN_SESSION_ID: &str = "01a06983-a806-77c3-8d39-4c49d6ecac05";

struct OrphanFixture {
    root: PathBuf,
    state_dir: PathBuf,
    project_root: PathBuf,
    project: agent::AgentProject,
    store: agent::TursoAgentStore,
    journal: agent::GitFinalizationRecord,
}

impl OrphanFixture {
    fn new(label: &str, change_baseline: impl FnOnce(&mut serde_json::Value)) -> Self {
        let root = temp_root(label);
        let state_dir = root.join("state/clt");
        let project_root = root.join("project");
        init_tasks(&project_root, false).unwrap();
        initialize_test_git_repository(&project_root);
        let start = capture_agent_git_start_state(&project_root, AgentGitMode::Commit).unwrap();
        let mut baseline: serde_json::Value =
            serde_json::from_str(&start.worktree_baseline).unwrap();
        change_baseline(&mut baseline);
        let baseline = serde_json::to_string(&baseline).unwrap();
        let store = agent::TursoAgentStore::open_blocking(&state_dir).unwrap();
        store
            .register_project_blocking(&project_root, "project")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        assert!(
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: ORPHAN_SESSION_ID,
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some(&start.starting_head),
                    branch_ref: start.branch_ref.as_deref(),
                    upstream_ref: start.upstream_ref.as_deref(),
                    worktree_baseline: &baseline,
                    task_identity: None,
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap()
        );
        assert!(
            store
                .ensure_pending_git_finalization_resume_requested_blocking(
                    project.id,
                    ORPHAN_SESSION_ID,
                )
                .unwrap()
        );
        let journal = store
            .git_finalization_blocking(project.id, ORPHAN_SESSION_ID)
            .unwrap()
            .unwrap();
        Self {
            root,
            state_dir,
            project_root,
            project,
            store,
            journal,
        }
    }

    fn lease(&self) -> AgentGitFinalizationLease {
        try_acquire_agent_git_finalization_lease(&self.state_dir, &self.project, true)
            .unwrap()
            .unwrap()
    }

    fn current_journal(&self) -> agent::GitFinalizationRecord {
        self.store
            .git_finalization_blocking(self.project.id, ORPHAN_SESSION_ID)
            .unwrap()
            .unwrap()
    }

    fn assert_unchanged(&self) {
        assert_eq!(self.current_journal(), self.journal);
        let control = self
            .store
            .session_control_blocking(self.project.id, ORPHAN_SESSION_ID)
            .unwrap()
            .unwrap();
        assert_eq!(
            control.state,
            agent::AgentSessionControlState::ResumeRequested
        );
        assert_eq!(control.run_token.as_deref(), Some("clt-git-finalization:0"));
    }

    fn cleanup(self, lease: AgentGitFinalizationLease) {
        drop(lease);
        drop(self.store);
        fs::remove_dir_all(self.root).unwrap();
    }
}

#[test]
fn orphan_cancellation_preserves_every_partial_or_malformed_sealed_baseline() {
    for (field, value) in [
        ("staged_index_tree", serde_json::json!("saved-tree")),
        ("manifest_parent_head", serde_json::json!("saved-parent")),
        ("staged_non_task_patch_ids", serde_json::json!({})),
        ("staged_index_tree", serde_json::json!(false)),
        ("manifest_parent_head", serde_json::json!([])),
        ("staged_non_task_patch_ids", serde_json::json!("malformed")),
    ] {
        let fixture = OrphanFixture::new("orphan-sealed-baseline", |baseline| {
            baseline[field] = value;
        });
        let lease = fixture.lease();
        assert!(
            !cancel_orphaned_working_git_finalization(
                &fixture.store,
                &fixture.project_root,
                &fixture.journal,
                &lease,
            )
            .unwrap(),
            "preserve non-null sealed field {field}"
        );
        fixture.assert_unchanged();
        fixture.cleanup(lease);
    }
}

#[test]
fn orphan_cancellation_rejects_mismatched_frozen_and_runtime_snapshots() {
    let fixture = OrphanFixture::new("orphan-snapshot-mismatch", |_| {});
    let lease = fixture.lease();
    let mut snapshots = Vec::new();
    let mut changed = fixture.journal.clone();
    changed.starting_head = Some("different-head".to_owned());
    snapshots.push(changed);
    let mut changed = fixture.journal.clone();
    changed.branch_ref = Some("refs/heads/different".to_owned());
    snapshots.push(changed);
    let mut changed = fixture.journal.clone();
    changed.upstream_ref = Some("refs/remotes/different/master".to_owned());
    snapshots.push(changed);
    let mut changed = fixture.journal.clone();
    changed.git_mode = AgentGitMode::CommitAndPush;
    snapshots.push(changed);
    let mut changed = fixture.journal.clone();
    let mut baseline: serde_json::Value = serde_json::from_str(&changed.worktree_baseline).unwrap();
    baseline["require_clean"] = serde_json::json!(true);
    changed.worktree_baseline = serde_json::to_string(&baseline).unwrap();
    snapshots.push(changed);
    let mut changed = fixture.journal.clone();
    changed.created_at = "101".to_owned();
    snapshots.push(changed);
    let mut changed = fixture.journal.clone();
    changed.updated_at = "101".to_owned();
    snapshots.push(changed);
    let mut changed = fixture.journal.clone();
    changed.last_error = Some("newer runtime observation".to_owned());
    snapshots.push(changed);
    for snapshot in snapshots {
        assert!(
            !cancel_orphaned_working_git_finalization(
                &fixture.store,
                &fixture.project_root,
                &snapshot,
                &lease,
            )
            .unwrap(),
            "stale snapshot must not retire the journal: {snapshot:?}"
        );
        fixture.assert_unchanged();
    }
    fixture.cleanup(lease);
}

#[test]
fn orphan_cancellation_rechecks_generation_after_waiting_for_board_lock() {
    let fixture = OrphanFixture::new("orphan-generation-race", |_| {});
    let lease = fixture.lease();
    assert!(
        !cancel_orphaned_working_git_finalization_with_before_lock(
            &fixture.store,
            &fixture.project_root,
            &fixture.journal,
            &lease,
            || {
                assert!(
                    fixture
                        .store
                        .compare_and_set_git_finalization_blocking(
                            fixture.project.id,
                            ORPHAN_SESSION_ID,
                            fixture.journal.generation,
                            GitFinalizationState::Working,
                            None,
                            None,
                            Some("concurrent reconciliation"),
                            "200",
                        )
                        .unwrap()
                );
            },
        )
        .unwrap()
    );
    let current = fixture.current_journal();
    assert_eq!(current.state, GitFinalizationState::Working);
    assert_eq!(current.generation, fixture.journal.generation + 1);
    assert_eq!(
        current.last_error.as_deref(),
        Some("concurrent reconciliation")
    );
    assert_eq!(current.starting_head, fixture.journal.starting_head);
    assert_eq!(current.worktree_baseline, fixture.journal.worktree_baseline);
    let control = fixture
        .store
        .session_control_blocking(fixture.project.id, ORPHAN_SESSION_ID)
        .unwrap()
        .unwrap();
    assert_eq!(control.run_token.as_deref(), Some("clt-git-finalization:1"));
    fixture.cleanup(lease);
}

#[test]
fn orphan_cancellation_preserves_live_sessions_started_after_lease_acquisition() {
    for session_id in [ORPHAN_SESSION_ID, "01a06983-a806-77c3-8d39-4c49d6ecac06"] {
        let fixture = OrphanFixture::new("orphan-live-session-race", |_| {});
        let lease = fixture.lease();
        let result = cancel_orphaned_working_git_finalization_with_before_lock(
            &fixture.store,
            &fixture.project_root,
            &fixture.journal,
            &lease,
            || {
                fixture
                    .store
                    .mark_session_running_blocking(
                        fixture.project.id,
                        session_id,
                        123,
                        "new-live-run",
                        &fixture.root.join("live.out"),
                        &fixture.root.join("live.err"),
                    )
                    .unwrap();
            },
        );
        assert!(!matches!(result, Ok(true)), "live session was retired");
        assert_eq!(fixture.current_journal(), fixture.journal);
        let control = fixture
            .store
            .session_control_blocking(fixture.project.id, session_id)
            .unwrap()
            .unwrap();
        assert_eq!(control.state, agent::AgentSessionControlState::Running);
        assert_eq!(control.run_token.as_deref(), Some("new-live-run"));
        assert_eq!(control.child_pid, Some(123));
        fixture.cleanup(lease);
    }
}

#[test]
fn orphan_cancellation_finds_displaced_nested_archived_and_ambiguous_markers() {
    for (relative_path, contents) in [
        (
            "backlog.md",
            format!("1. Retained task codex:{ORPHAN_SESSION_ID}\n   Later note\n"),
        ),
        (
            "todo/0001-parent/doing.md",
            format!("1. Nested task codex:{ORPHAN_SESSION_ID}\n"),
        ),
        (
            "archive.md",
            format!("1. Archived task codex:{ORPHAN_SESSION_ID}\n"),
        ),
        (
            "archive/0001-archived/task.md",
            format!("Archived detail\ncodex:{ORPHAN_SESSION_ID}\n"),
        ),
        (
            "done.md",
            format!(
                "1. Ambiguous marker codex:{ORPHAN_SESSION_ID}\n   Follow-up codex:01a06983-a806-77c3-8d39-4c49d6ecac06\n"
            ),
        ),
        (
            "todo.md.backup",
            format!("1. Backup retains codex:{ORPHAN_SESSION_ID}\n"),
        ),
    ] {
        let fixture = OrphanFixture::new("orphan-recoverable-marker", |_| {});
        let lease = fixture.lease();
        let path = get_tasks_dir(&fixture.project_root).join(relative_path);
        let original_head = run_test_git(&fixture.project_root, &["rev-parse", "HEAD"]);
        assert!(
            !cancel_orphaned_working_git_finalization_with_before_lock(
                &fixture.store,
                &fixture.project_root,
                &fixture.journal,
                &lease,
                || {
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(&path, &contents).unwrap();
                },
            )
            .unwrap(),
            "marker in {relative_path} must prevent cancellation"
        );
        fixture.assert_unchanged();
        assert_eq!(fs::read_to_string(path).unwrap(), contents);
        assert_eq!(
            run_test_git(&fixture.project_root, &["rev-parse", "HEAD"]),
            original_head
        );
        fixture.cleanup(lease);
    }
}

#[cfg(unix)]
#[test]
fn orphan_cancellation_refuses_task_tree_symlinks_without_following_them() {
    let fixture = OrphanFixture::new("orphan-symlink-marker", |_| {});
    let lease = fixture.lease();
    let outside = fixture.root.join("outside.md");
    let contents = format!("1. External task codex:{ORPHAN_SESSION_ID}\n");
    fs::write(&outside, &contents).unwrap();
    std::os::unix::fs::symlink(
        &outside,
        get_tasks_dir(&fixture.project_root).join("linked.md"),
    )
    .unwrap();
    let result = cancel_orphaned_working_git_finalization(
        &fixture.store,
        &fixture.project_root,
        &fixture.journal,
        &lease,
    );
    assert!(!matches!(result, Ok(true)));
    fixture.assert_unchanged();
    assert_eq!(fs::read_to_string(outside).unwrap(), contents);
    fixture.cleanup(lease);
}

#[test]
fn orphan_cancellation_refuses_an_unrelated_project_directory() {
    let fixture = OrphanFixture::new("orphan-wrong-project-root", |_| {});
    let lease = fixture.lease();
    let unrelated_root = fixture.root.join("unrelated-project");
    init_tasks(&unrelated_root, false).unwrap();
    let result = cancel_orphaned_working_git_finalization(
        &fixture.store,
        &unrelated_root,
        &fixture.journal,
        &lease,
    );
    assert!(result.is_err());
    fixture.assert_unchanged();
    fixture.cleanup(lease);
}

#[test]
fn orphan_reconcile_preserves_an_unrelated_idle_resume_token() {
    let fixture = OrphanFixture::new("orphan-unrelated-resume-token", |_| {});
    fixture
        .store
        .mark_session_running_blocking(
            fixture.project.id,
            ORPHAN_SESSION_ID,
            123,
            "unrelated-run-token",
            &fixture.root.join("session.out"),
            &fixture.root.join("session.err"),
        )
        .unwrap();
    assert!(
        fixture
            .store
            .transition_session_control_state_blocking(
                fixture.project.id,
                ORPHAN_SESSION_ID,
                AgentSessionControlState::Running,
                AgentSessionControlState::ResumeRequested,
            )
            .unwrap()
    );
    let result = reconcile_orphaned_agent_git_journals(&fixture.state_dir, &fixture.project);
    assert!(!matches!(result, Ok(retired) if retired > 0));
    assert_eq!(fixture.current_journal(), fixture.journal);
    let control = fixture
        .store
        .session_control_blocking(fixture.project.id, ORPHAN_SESSION_ID)
        .unwrap()
        .unwrap();
    assert_eq!(control.state, AgentSessionControlState::ResumeRequested);
    assert_eq!(control.run_token.as_deref(), Some("unrelated-run-token"));
    drop(fixture.store);
    fs::remove_dir_all(fixture.root).unwrap();
}
