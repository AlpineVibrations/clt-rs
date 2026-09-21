use std::{fs, io::Cursor, process::Command, time::Duration};

use anyhow::Result;
use serde_json::{Value, json};

use super::{create_planning_thread, planning_protocol, prepare_todo_planning_session_with};
use crate::{
    agent::{
        AgentGitMode, AgentProject, AgentSessionControlState, NewGitFinalization, TursoAgentStore,
    },
    session_control::{
        InteractiveAgentLease, InteractiveGuardianDisposition, codex_session_for_task,
        interactive_guardian_holder,
    },
    task::{TaskStatus, get_tasks_dir, init_tasks, read_task_entries},
};

fn replies() -> String {
    [
        json!({"id": 1, "result": {}}),
        json!({"method": "thread/started", "params": {}}),
        json!({"id": 2, "result": {"thread": {"id": "thread-123", "sessionId": "session-456"}}}),
        json!({"id": 3, "result": {}}),
    ]
    .iter()
    .map(|value| format!("{value}\n"))
    .collect()
}

#[test]
fn planning_protocol_persists_full_context_without_starting_a_turn() {
    let mut written = Vec::new();
    let prompt = "Plan this task.\nFull details with \"quotes\" and Unicode: café.";
    let id = planning_protocol(
        &mut written,
        Cursor::new(replies()),
        json!({"cwd": "/a b"}),
        prompt,
    )
    .unwrap();
    assert_eq!(id, "session-456");
    let messages: Vec<Value> = String::from_utf8(written)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        messages
            .iter()
            .map(|v| v["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "initialize",
            "initialized",
            "thread/start",
            "thread/inject_items"
        ]
    );
    assert_eq!(messages[2]["params"]["cwd"], "/a b");
    assert_eq!(messages[3]["params"]["threadId"], "thread-123");
    assert_eq!(
        messages[3]["params"]["items"][0]["content"][0]["text"],
        prompt
    );
}

#[test]
fn planning_protocol_supports_legacy_ids_and_rejects_failed_persistence() {
    let legacy = replies().replace(",\"sessionId\":\"session-456\"", "");
    assert_eq!(
        planning_protocol(Vec::new(), Cursor::new(legacy), json!({}), "task").unwrap(),
        "thread-123"
    );
    let failure = replies().replace(
        "{\"id\":3,\"result\":{}}",
        "{\"id\":3,\"error\":{\"message\":\"unsupported\"}}",
    );
    let error = planning_protocol(Vec::new(), Cursor::new(failure), json!({}), "task").unwrap_err();
    assert!(error.to_string().contains("thread/inject_items"));
    for invalid in [
        "",
        "not json\n",
        "{\"id\":1,\"error\":{\"message\":\"startup failed\"}}\n",
    ] {
        assert!(planning_protocol(Vec::new(), Cursor::new(invalid), json!({}), "task").is_err());
    }
    let invalid_id = replies().replace("session-456", "bad/session");
    assert!(planning_protocol(Vec::new(), Cursor::new(invalid_id), json!({}), "task").is_err());
}

struct Fixture {
    _root: tempfile::TempDir,
    state: std::path::PathBuf,
    project: AgentProject,
    store: TursoAgentStore,
}

impl Fixture {
    fn new(folders: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let project_path = root.path().join("project");
        fs::create_dir(&project_path).unwrap();
        let project_path = fs::canonicalize(project_path).unwrap();
        init_tasks(&project_path, folders).unwrap();
        let store = TursoAgentStore::open_blocking(&state).unwrap();
        store
            .register_project_blocking(&project_path, "planning-test")
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        Self {
            _root: root,
            state,
            project,
            store,
        }
    }
}

#[test]
fn planning_links_exact_todo_in_markdown_folder_and_nested_boards() {
    for folders in [false, true] {
        for nested in [false, true] {
            let f = Fixture::new(folders);
            let board = if nested {
                let nested_root = get_tasks_dir(&f.project.path).join("todo/parent");
                // Nested boards use the task directory itself as their board root.
                fs::create_dir_all(&nested_root).unwrap();
                for status in ["todo", "doing", "done"] {
                    fs::write(nested_root.join(format!("{status}.md")), "").unwrap();
                }
                nested_root
            } else {
                get_tasks_dir(&f.project.path)
            };
            let path = if folders && !nested {
                board.join("todo/0001-plan.md")
            } else {
                board.join("todo.md")
            };
            let original = if folders && !nested {
                "Plan the feature.\nPreserve all details.\n"
            } else {
                "# Todo\n- Plan the feature. Preserve all details.\n- Leave this task untouched.\n"
            };
            fs::write(&path, original).unwrap();
            let selected = read_task_entries(&board, TaskStatus::Todo)
                .unwrap()
                .remove(0);
            let prepared =
                prepare_todo_planning_session_with(&f.state, &f.project, &board, &selected, |_| {
                    assert!(f.store.lease_for_project_blocking(f.project.id)?.is_some());
                    Ok("planning-123".into())
                })
                .unwrap();
            assert_eq!(prepared.session_id, "planning-123");
            let tasks = read_task_entries(&board, TaskStatus::Todo).unwrap();
            assert_eq!(
                codex_session_for_task(&tasks[0]).as_deref(),
                Some("planning-123")
            );
            assert!(tasks[0].content.contains("Preserve all details."));
            assert!(
                read_task_entries(&board, TaskStatus::Doing)
                    .unwrap()
                    .is_empty()
            );
            if !(folders && !nested) {
                assert_eq!(tasks[1].content, "Leave this task untouched.");
            }
            let second =
                prepare_todo_planning_session_with(&f.state, &f.project, &board, &selected, |_| {
                    panic!("must not create twice")
                });
            assert!(second.is_err());
            prepared.lease.release().unwrap();
            let second =
                prepare_todo_planning_session_with(&f.state, &f.project, &board, &tasks[0], |_| {
                    panic!("must reuse linked session")
                });
            assert!(second.is_err());
            assert!(
                f.store
                    .lease_for_project_blocking(f.project.id)
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[test]
fn planning_preserves_edits_and_releases_lease_on_failure_or_stale_selection() {
    for change in ["edit", "move", "link", "failure"] {
        let f = Fixture::new(false);
        let board = get_tasks_dir(&f.project.path);
        fs::write(board.join("todo.md"), "- Plan this\n").unwrap();
        let selected = read_task_entries(&board, TaskStatus::Todo)
            .unwrap()
            .remove(0);
        let result =
            prepare_todo_planning_session_with(&f.state, &f.project, &board, &selected, |_| {
                match change {
                    "edit" => fs::write(board.join("todo.md"), "- Human edit\n")?,
                    "move" => {
                        fs::write(board.join("todo.md"), "")?;
                        fs::write(board.join("doing.md"), "- Plan this\n")?;
                    }
                    "link" => {
                        fs::write(board.join("todo.md"), "- Plan this codex:other-session\n")?
                    }
                    _ => anyhow::bail!("app-server unavailable"),
                }
                Ok("planning-123".into())
            });
        assert!(result.is_err());
        assert!(
            !fs::read_to_string(board.join("todo.md"))
                .unwrap()
                .contains("planning-123")
        );
        assert!(
            f.store
                .lease_for_project_blocking(f.project.id)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn planning_does_not_start_or_change_tasks_in_a_busy_project() {
    let f = Fixture::new(false);
    let board = get_tasks_dir(&f.project.path);
    fs::write(board.join("todo.md"), "- Plan this\n").unwrap();
    let selected = read_task_entries(&board, TaskStatus::Todo)
        .unwrap()
        .remove(0);
    let busy = InteractiveAgentLease::try_acquire_with_holder_at(
        &f.state,
        f.project.id,
        "other-owner",
        60,
    )
    .unwrap()
    .unwrap();
    let result =
        prepare_todo_planning_session_with(&f.state, &f.project, &board, &selected, |_| {
            panic!("must not launch")
        });
    assert!(result.is_err());
    assert_eq!(
        f.store
            .lease_for_project_blocking(f.project.id)
            .unwrap()
            .unwrap()
            .holder,
        "other-owner"
    );
    assert_eq!(
        fs::read_to_string(board.join("todo.md")).unwrap(),
        "- Plan this\n"
    );
    busy.release().unwrap();
}

#[test]
fn planning_rechecks_session_ownership_before_publishing_the_task_link() {
    for during_creation in [false, true] {
        let f = Fixture::new(false);
        let board = get_tasks_dir(&f.project.path);
        fs::write(board.join("todo.md"), "- Plan this\n").unwrap();
        let selected = read_task_entries(&board, TaskStatus::Todo)
            .unwrap()
            .remove(0);
        let claim = || {
            f.store.set_session_control_state_blocking(
                f.project.id,
                "other-session",
                AgentSessionControlState::Running,
            )
        };
        if !during_creation {
            claim().unwrap();
        }
        let result =
            prepare_todo_planning_session_with(&f.state, &f.project, &board, &selected, |_| {
                assert!(during_creation, "busy preflight must not create a session");
                claim()?;
                Ok("planning-123".into())
            });
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(board.join("todo.md")).unwrap(),
            "- Plan this\n"
        );
        assert!(
            f.store
                .session_control_blocking(f.project.id, "planning-123")
                .unwrap()
                .is_none()
        );
        assert!(
            f.store
                .lease_for_project_blocking(f.project.id)
                .unwrap()
                .is_none()
        );
    }
}

#[cfg(unix)]
#[test]
fn planning_process_handles_success_failure_and_timeout() -> Result<()> {
    let mut success = Command::new("/bin/sh");
    success.args([
        "-c",
        "printf '%s' \"$1\"; cat >/dev/null",
        "fake-codex",
        &replies(),
    ]);
    assert_eq!(
        create_planning_thread(
            &mut success,
            json!({}),
            "task".into(),
            Duration::from_secs(3)
        )?,
        "session-456"
    );
    let mut failed = Command::new("/bin/sh");
    failed.args(["-c", "exit 1"]);
    assert!(
        create_planning_thread(
            &mut failed,
            json!({}),
            "task".into(),
            Duration::from_secs(3)
        )
        .is_err()
    );
    let mut hanging = Command::new("/bin/sh");
    hanging.args(["-c", "cat >/dev/null"]);
    assert!(
        create_planning_thread(
            &mut hanging,
            json!({}),
            "task".into(),
            Duration::from_millis(20)
        )
        .unwrap_err()
        .to_string()
        .contains("timed out")
    );
    Ok(())
}

#[test]
fn planning_reservation_can_reopen_after_cancel_and_cannot_resume_automation() {
    let f = Fixture::new(false);
    let board = get_tasks_dir(&f.project.path);
    fs::write(board.join("todo.md"), "- Plan this\n").unwrap();
    let selected = read_task_entries(&board, TaskStatus::Todo)
        .unwrap()
        .remove(0);
    let prepared =
        prepare_todo_planning_session_with(&f.state, &f.project, &board, &selected, |_| {
            Ok("planning-123".into())
        })
        .unwrap();
    for _ in 0..2 {
        assert!(
            f.store
                .reserve_idle_session_interactive_blocking(
                    f.project.id,
                    &prepared.session_id,
                    &prepared.lease.holder,
                    None
                )
                .unwrap()
        );
        assert!(
            f.store
                .cancel_idle_session_interactive_blocking(
                    f.project.id,
                    &prepared.session_id,
                    &prepared.lease.holder
                )
                .unwrap()
        );
    }
    prepared.lease.release().unwrap();
    let message = crate::session_control::toggle_tui_codex_session_stop_at(
        &f.state,
        f.project.id,
        &prepared.session_id,
    )
    .unwrap();
    assert!(message.contains("press c"));
    assert_eq!(
        f.store
            .session_control_blocking(f.project.id, &prepared.session_id)
            .unwrap()
            .unwrap()
            .state,
        crate::agent::AgentSessionControlState::Stopped
    );
}

#[test]
fn planning_preserves_queued_recovery_through_interactive_exit_and_reopen() {
    let f = Fixture::new(false);
    f.store
        .set_project_enabled_blocking(f.project.id, false)
        .unwrap();
    f.store
        .mark_session_running_blocking(
            f.project.id,
            "queued-recovery",
            1234,
            "original-run",
            &f.project.path.join("out"),
            &f.project.path.join("err"),
        )
        .unwrap();
    assert!(
        f.store
            .create_git_finalization_blocking(NewGitFinalization {
                project_id: f.project.id,
                codex_session_id: "queued-recovery",
                git_mode: AgentGitMode::CommitAndPush,
                starting_head: Some("1111111111111111111111111111111111111111"),
                branch_ref: Some("refs/heads/main"),
                upstream_ref: Some("refs/remotes/origin/main"),
                worktree_baseline: "preserved baseline",
                task_identity: Some("recovery-task"),
                owner_run_token: Some("original-run"),
                created_at: "100",
            })
            .unwrap()
    );
    f.store
        .set_session_control_recovery_token_blocking(
            f.project.id,
            "queued-recovery",
            "original-run",
        )
        .unwrap();
    let recovery = f
        .store
        .session_control_blocking(f.project.id, "queued-recovery")
        .unwrap();
    let journal = f
        .store
        .git_finalization_blocking(f.project.id, "queued-recovery")
        .unwrap();
    let board = get_tasks_dir(&f.project.path);
    fs::write(board.join("todo.md"), "- Plan this\n").unwrap();
    let doing = "- Recover this codex:queued-recovery\n";
    fs::write(board.join("doing.md"), doing).unwrap();
    let selected = read_task_entries(&board, TaskStatus::Todo)
        .unwrap()
        .remove(0);
    let prepared =
        prepare_todo_planning_session_with(&f.state, &f.project, &board, &selected, |_| {
            Ok("planning-123".into())
        })
        .unwrap();
    let mut lease = prepared.lease;
    for visit in 0..2 {
        if visit > 0 {
            lease = InteractiveAgentLease::try_acquire_with_holder_at(
                &f.state,
                f.project.id,
                &InteractiveAgentLease::holder_for_stopped_session(),
                60,
            )
            .unwrap()
            .unwrap();
        }
        assert!(
            f.store
                .reserve_idle_session_interactive_blocking(
                    f.project.id,
                    &prepared.session_id,
                    &lease.holder,
                    None,
                )
                .unwrap()
        );
        let disposition = InteractiveGuardianDisposition::RestoreStopped;
        let guardian = interactive_guardian_holder(disposition);
        assert!(
            f.store
                .adopt_interactive_guardian_blocking(
                    f.project.id,
                    Some(&prepared.session_id),
                    &lease.holder,
                    &guardian,
                    60,
                )
                .unwrap()
        );
        assert!(
            !f.store
                .try_acquire_lease_blocking(f.project.id, "scheduler", "100", "9999999999",)
                .unwrap()
        );
        assert!(
            f.store
                .register_interactive_guardian_child_blocking(
                    f.project.id,
                    &prepared.session_id,
                    &guardian,
                    std::process::id(),
                    60,
                )
                .unwrap()
        );
        assert_eq!(
            f.store
                .session_control_blocking(f.project.id, "queued-recovery")
                .unwrap(),
            recovery
        );
        assert!(
            f.store
                .finish_interactive_guardian_blocking(
                    f.project.id,
                    &prepared.session_id,
                    &guardian,
                    disposition,
                )
                .unwrap()
        );
        assert_eq!(
            f.store
                .session_control_blocking(f.project.id, "queued-recovery")
                .unwrap(),
            recovery
        );
        assert_eq!(
            f.store
                .git_finalization_blocking(f.project.id, "queued-recovery")
                .unwrap(),
            journal
        );
        assert_eq!(
            f.store
                .session_control_blocking(f.project.id, &prepared.session_id)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Stopped
        );
    }
    lease.release().unwrap();
    assert_eq!(fs::read_to_string(board.join("doing.md")).unwrap(), doing);
    let todo = read_task_entries(&board, TaskStatus::Todo).unwrap();
    assert_eq!(todo.len(), 1);
    assert_eq!(
        codex_session_for_task(&todo[0]).as_deref(),
        Some("planning-123")
    );
}

#[test]
fn planned_todo_can_be_claimed_by_a_fresh_git_off_worker() {
    let f = Fixture::new(false);
    let board = get_tasks_dir(&f.project.path);
    fs::write(board.join("todo.md"), "- Plan this\n").unwrap();
    let selected = read_task_entries(&board, TaskStatus::Todo)
        .unwrap()
        .remove(0);
    let prepared =
        prepare_todo_planning_session_with(&f.state, &f.project, &board, &selected, |_| {
            Ok("planning-123".into())
        })
        .unwrap();
    prepared.lease.release().unwrap();
    f.store
        .mark_session_running_blocking(
            f.project.id,
            "implementation-456",
            1234,
            "run-456",
            &f.project.path.join("out"),
            &f.project.path.join("err"),
        )
        .unwrap();
    crate::application::move_task_to_doing_with_agent_session(
        &f.project.path,
        "1",
        &crate::runner::AutomatedAgentChildContext {
            project_id: f.project.id,
            run_token: "run-456".into(),
        },
        &f.project,
        &f.store,
    )
    .unwrap();
    let doing = read_task_entries(&board, TaskStatus::Doing).unwrap();
    assert_eq!(doing.len(), 1);
    assert_eq!(
        codex_session_for_task(&doing[0]).as_deref(),
        Some("implementation-456")
    );
    assert!(
        read_task_entries(&board, TaskStatus::Todo)
            .unwrap()
            .is_empty()
    );
}

#[test]
#[ignore = "requires an installed Codex CLI; creates only local conversation data, with no model turn"]
fn installed_codex_planning_session_is_durable() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut command = Command::new("codex");
    command
        .args(["app-server", "--listen", "stdio://"])
        .env("CODEX_HOME", home.path())
        .current_dir(home.path());
    let id = create_planning_thread(
        &mut command,
        json!({
            "cwd": home.path(), "ephemeral": false, "approvalPolicy": "on-request", "sandbox": "workspace-write"
        }),
        "CLT planning durability smoke test. Wait for my next message.".into(),
        Duration::from_secs(20),
    )?;
    fn find_rollout(dir: &std::path::Path, id: &str) -> Result<Option<String>> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                if let Some(content) = find_rollout(&path, id)? {
                    return Ok(Some(content));
                }
            } else if path.file_name().unwrap().to_string_lossy().contains(id) {
                return Ok(Some(fs::read_to_string(path)?));
            }
        }
        Ok(None)
    }
    let rollout = find_rollout(home.path(), &id)?
        .expect("a durable rollout must exist after app-server exits");
    assert!(rollout.contains("CLT planning durability smoke test"));
    assert!(!rollout.contains("\"type\":\"turn_started\""));
    Ok(())
}

#[test]
fn stopped_todo_cannot_launch_a_planning_session() {
    for folders in [false, true] {
        let f = Fixture::new(folders);
        crate::task::add_task(&f.project.path, "Keep this task stopped. clt:stopped", None)
            .unwrap();
        let board = get_tasks_dir(&f.project.path);
        let selected = read_task_entries(&board, TaskStatus::Todo)
            .unwrap()
            .remove(0);
        let result =
            prepare_todo_planning_session_with(&f.state, &f.project, &board, &selected, |_| {
                panic!("Stopped tasks must not start Codex")
            });
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("task is stopped")
        );
        assert!(
            f.store
                .lease_for_project_blocking(f.project.id)
                .unwrap()
                .is_none()
        );
        assert!(
            f.store
                .session_controls_for_project_blocking(f.project.id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            read_task_entries(&board, TaskStatus::Todo).unwrap()[0].content,
            selected.content
        );
    }
}
