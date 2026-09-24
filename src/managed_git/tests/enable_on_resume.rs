use crate::test_support::prelude::*;
use crate::test_support::*;

const SESSION: &str = "paused-without-git";
const HOLDER: &str = "resuming-holder";
const RUN: &str = "managed-resume";

fn fixture(mode: AgentGitMode) -> (PathBuf, TursoAgentStore, AgentProject) {
    let root = temp_root("enable-git-on-resume");
    let project_root = root.join("project");
    init_tasks(&project_root, false).unwrap();
    fs::write(
        project_root.join("tasks/todo.md"),
        "# Todo Tasks\n- Finish feature\n",
    )
    .unwrap();
    fs::write(project_root.join("feature.txt"), "before\n").unwrap();
    initialize_test_git_repository(&project_root);
    let remote = root.join("remote.git");
    fs::create_dir_all(&remote).unwrap();
    run_test_git(&remote, &["init", "--bare"]);
    run_test_git(
        &project_root,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    run_test_git(&project_root, &["push", "-u", "origin", "HEAD"]);
    let store = TursoAgentStore::open_blocking(&root.join("state/clt")).unwrap();
    store
        .register_project_blocking(&project_root, "project")
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .mark_session_running_with_git_mode_blocking(
            project.id,
            SESSION,
            123,
            "unmanaged-run",
            &root.join("old.out"),
            &root.join("old.err"),
            AgentGitMode::Off,
        )
        .unwrap();
    fs::write(project_root.join("tasks/todo.md"), "# Todo Tasks\n").unwrap();
    fs::write(
        project_root.join("tasks/doing.md"),
        format!("# Doing Tasks\n- Finish feature codex:{SESSION}\n"),
    )
    .unwrap();
    fs::write(
        project_root.join("feature.txt"),
        "work before Git was enabled\n",
    )
    .unwrap();
    fs::write(project_root.join("staged.txt"), "staged before restart\n").unwrap();
    run_test_git(&project_root, &["add", "staged.txt"]);
    fs::write(project_root.join("unrelated.txt"), "user work\n").unwrap();
    store
        .set_session_control_recovery_token_blocking(project.id, SESSION, "unmanaged-run")
        .unwrap();
    store
        .set_project_git_mode_blocking(project.id, mode)
        .unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    store
        .try_acquire_lease_blocking(
            project.id,
            HOLDER,
            &agent_timestamp(),
            &agent_timestamp_after(300),
        )
        .unwrap();
    (root, store, project)
}

#[test]
fn paused_task_can_enable_git_and_complete_with_commit_or_push() {
    for mode in [AgentGitMode::Commit, AgentGitMode::CommitAndPush] {
        let (root, store, project) = fixture(mode);
        let before = run_test_git(&project.path, &["rev-parse", "HEAD"]);
        enable_agent_git_for_resumed_session(&store, &project, SESSION, RUN, HOLDER).unwrap();
        let journal = store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap()
            .unwrap();
        assert_eq!(journal.git_mode, mode);
        assert_eq!(
            store
                .session_git_mode_blocking(project.id, SESSION)
                .unwrap(),
            Some(mode)
        );
        assert_eq!(journal.state, GitFinalizationState::Working);
        assert_eq!(journal.owner_run_token.as_deref(), Some(RUN));
        assert_eq!(
            run_test_git(&project.path, &["show", "HEAD:feature.txt"]),
            "before"
        );
        assert_eq!(
            run_test_git(&project.path, &["diff", "--cached", "--name-only"]),
            "staged.txt"
        );
        assert_eq!(
            fs::read_to_string(project.path.join("feature.txt")).unwrap(),
            "work before Git was enabled\n"
        );
        let checkpoint = run_test_git(&project.path, &["rev-parse", "HEAD"]);
        assert_ne!(checkpoint, before);
        assert_eq!(journal.starting_head.as_deref(), Some(checkpoint.as_str()));
        // Retrying preparation keeps the first captured boundary.
        enable_agent_git_for_resumed_session(&store, &project, SESSION, RUN, HOLDER).unwrap();
        assert_eq!(
            store
                .git_finalization_blocking(project.id, SESSION)
                .unwrap()
                .unwrap(),
            journal
        );
        assert!(
            store
                .register_known_session_with_child_blocking(AgentKnownSessionRegistration {
                    project_id: project.id,
                    codex_session_id: SESSION,
                    child_pid: 124,
                    run_token: RUN,
                    stdout_path: &root.join("new.out"),
                    stderr_path: &root.join("new.err"),
                    lease_holder: HOLDER,
                    lease_timeout_seconds: 300,
                    claim_requested_resume: true,
                })
                .unwrap()
        );
        fs::write(
            project.path.join("tasks/doing.md"),
            format!(
                "# Doing Tasks\n- Finish feature — COMPLETED 2026-09-24: verified codex:{SESSION}\n"
            ),
        )
        .unwrap();
        run_test_git(
            &project.path,
            &["add", "feature.txt", "staged.txt", "tasks/doing.md"],
        );
        assert!(
            move_task_to_done_with_agent_store(
                &project.path,
                TaskStatus::Doing,
                "1",
                &AutomatedAgentChildContext {
                    project_id: project.id,
                    run_token: RUN.to_string()
                },
                &store
            )
            .unwrap()
        );
        run_test_git(&project.path, &["add", "tasks"]);
        run_test_agent_git(
            &project.path,
            &[
                "commit",
                "-m",
                "Finish resumed feature",
                "-m",
                &format!("CLT-Task: codex:{SESSION}"),
            ],
        );
        let commit = run_test_git(&project.path, &["rev-parse", "HEAD"]);
        let pending = store
            .git_finalization_blocking(project.id, SESSION)
            .unwrap()
            .unwrap();
        let done =
            reconcile_agent_git_finalization(&store, &project.path, pending, Some(RUN), None)
                .unwrap();
        assert_eq!(done.state, GitFinalizationState::Completed);
        assert_eq!(done.commit_oid.as_deref(), Some(commit.as_str()));
        assert_eq!(
            run_test_git(
                &project.path,
                &["rev-list", "--count", &format!("{checkpoint}..HEAD")]
            ),
            "1"
        );
        assert_eq!(
            run_test_git(&project.path, &["status", "--porcelain"]),
            "?? unrelated.txt"
        );
        let branch = run_test_git(&project.path, &["symbolic-ref", "HEAD"]);
        let remote_commit = run_test_git(&root.join("remote.git"), &["rev-parse", &branch]);
        assert_eq!(
            remote_commit,
            if mode == AgentGitMode::CommitAndPush {
                commit
            } else {
                before
            }
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn enabling_git_on_paused_work_rejects_completed_work_and_lost_ownership() {
    for scenario in ["completed", "committed", "wrong-holder", "live-session"] {
        let (root, store, project) = fixture(AgentGitMode::Commit);
        match scenario {
            "completed" => fs::write(project.path.join("tasks/doing.md"), format!("# Doing Tasks\n- Finish feature — COMPLETED 2026-09-24: already done codex:{SESSION}\n")).unwrap(),
            "committed" => { run_test_git(&project.path, &["commit", "--allow-empty", "-m", &format!("CLT-Task: codex:{SESSION}")]); },
            "live-session" => store.mark_session_running_with_git_mode_blocking(project.id, SESSION, 125, "unmanaged-run", &root.join("old.out"), &root.join("old.err"), AgentGitMode::Off).unwrap(),
            _ => {},
        }
        let before = run_test_git(&project.path, &["rev-parse", "HEAD"]);
        let holder = if scenario == "wrong-holder" {
            "other"
        } else {
            HOLDER
        };
        assert!(
            enable_agent_git_for_resumed_session(&store, &project, SESSION, RUN, holder).is_err(),
            "{scenario}"
        );
        assert!(
            store
                .git_finalization_blocking(project.id, SESSION)
                .unwrap()
                .is_none()
        );
        assert_eq!(run_test_git(&project.path, &["rev-parse", "HEAD"]), before);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
