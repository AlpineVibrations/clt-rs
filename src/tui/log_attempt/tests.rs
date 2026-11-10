use super::*;
use crate::task::{TaskSource, task_entry_from_text};
use crate::tui::tests::tui_agent_project_for_test;
use crate::tui::*;

fn outcome(
    store: &agent::TursoAgentStore,
    project_id: i64,
    root: &Path,
    at: &str,
    status: &'static str,
    files: bool,
) {
    let out = root.join(format!("{at}-000-p1-s1.out"));
    let err = root.join(format!("{at}-000-p1-s1.err"));
    store
        .record_run_outcome_blocking(agent::AgentRunOutcome {
            project_id,
            status,
            started_at: at,
            finished_at: Some(at),
            exit_code: Some(1),
            log_dir: None,
            stdout_path: files.then(|| out.to_str().unwrap()),
            stderr_path: files.then(|| err.to_str().unwrap()),
            summary: Some("Saved failure details"),
            codex_session_id: Some("same-conversation"),
        })
        .unwrap();
}

#[test]
fn unrecorded_resume_log_wins_over_older_failure_and_acknowledgement() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
    store
        .register_project_blocking(root.path(), "demo")
        .unwrap();
    let mut item = tui_agent_project_for_test(1, "demo");
    item.project = store.list_projects_blocking().unwrap().remove(0);
    let id = item.project.id;
    let old_out = root.path().join("100-000-p1-s1.out");
    let old_err = root.path().join("100-000-p1-s1.err");
    fs::write(&old_out, "").unwrap();
    fs::write(&old_err, "Old active-writer conflict").unwrap();
    outcome(&store, id, root.path(), "100", "failure", true);
    outcome(&store, id, root.path(), "200", "success", false);
    let new_out = root.path().join("300-000-p1-s1.out");
    let new_err = root.path().join("300-000-p1-s1.err");
    fs::write(&new_out, "").unwrap();
    fs::write(
        &new_err,
        "session id: same-conversation\nNewer resumed progress",
    )
    .unwrap();
    store
        .mark_session_running_blocking(
            id,
            "same-conversation",
            4242,
            "300-000-p1-s1",
            &new_out,
            &new_err,
        )
        .unwrap();
    store
        .finalize_reaped_automated_session_blocking(id, 4242, "300-000-p1-s1", "worker", 60)
        .unwrap();
    let mut panel = TuiAgentPanel::new(root.path());
    panel.projects = vec![item];
    panel.current_project_registration = None;
    panel.state.select(Some(0));
    let task = task_entry_from_text(
        TaskSource::MarkdownLine { line_index: 1 },
        "Resume",
        "Resume codex:same-conversation",
        false,
    );
    for runtime in [
        TuiAgentRuntimeState::Idle,
        TuiAgentRuntimeState::RetryScheduled,
        TuiAgentRuntimeState::Running,
    ] {
        panel.projects[0].runtime_state = runtime;
        let project_view = selected_tui_agent_log_view_at(&panel, &state)
            .unwrap()
            .unwrap();
        let task_view = selected_tui_task_log_view_at(&panel, TaskStatus::Todo, &task, &state)
            .unwrap()
            .unwrap();
        for view in [project_view, task_view] {
            assert_eq!(view.path.as_deref(), Some(new_err.as_path()));
            assert!(!view.is_live);
            assert!(view.content.contains("Newer resumed progress"));
            let title = tui_agent_log_title(&view);
            assert!(title.contains("SAVED ·"));
            assert!(title.contains("Outcome unrecorded"));
            assert!(!title.contains("Failed"));
            assert_eq!(view.attempt.as_ref().unwrap().started_at, Some(300));
        }
    }
    // Opening a fresh attempt updates the same log view automatically.
    let mut view = selected_tui_task_log_view_at(&panel, TaskStatus::Todo, &task, &state).unwrap();
    let live_out = root.path().join("400-000-p1-s1.out");
    let live_err = root.path().join("400-000-p1-s1.err");
    fs::write(&live_out, "").unwrap();
    fs::write(&live_err, "session id: same-conversation\nRunning again").unwrap();
    store
        .mark_session_running_blocking(
            id,
            "same-conversation",
            4243,
            "400-000-p1-s1",
            &live_out,
            &live_err,
        )
        .unwrap();
    sync_open_tui_task_log_view_at(
        &mut panel,
        root.path(),
        TaskStatus::Doing,
        Some(&task),
        &mut view,
        &state,
    );
    assert!(view.as_ref().unwrap().is_live);
    assert!(view.as_ref().unwrap().content.contains("Running again"));
    outcome(&store, id, root.path(), "400", "success", true);
    store
        .finalize_reaped_automated_session_blocking(id, 4243, "400-000-p1-s1", "worker", 60)
        .unwrap();
    sync_open_tui_task_log_view_at(
        &mut panel,
        root.path(),
        TaskStatus::Done,
        Some(&task),
        &mut view,
        &state,
    );
    let saved = view.unwrap();
    assert!(!saved.is_live);
    assert!(tui_agent_log_title(&saved).contains("Succeeded"));
    assert!(tui_agent_log_title(&saved).contains("run "));
    outcome(&store, id, root.path(), "500", "failure", false);
    let failure = selected_tui_agent_log_view_at(&panel, &state)
        .unwrap()
        .unwrap();
    assert!(failure.content.contains("Saved failure details"));
    assert!(tui_agent_log_title(&failure).contains("Failed"));
    assert_eq!(failure.attempt.unwrap().started_at, Some(500));
}

#[test]
fn saved_time_comes_from_attempt_not_file_modification() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("1794161902-549831000-p29-s14.err");
    fs::write(&path, "Old attempt copied today").unwrap();
    assert_eq!(Attempt::from_path(&path).started_at, Some(1794161902));
    assert!(
        Attempt::from_path(Path::new("unknown.err"))
            .label(false)
            .contains("Time unrecorded")
    );
}
