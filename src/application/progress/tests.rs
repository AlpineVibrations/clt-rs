use super::*;
use crate::{
    session_control::steering::tests::Fixture,
    task::{TaskStatus, read_task_entries},
};

fn selected_task(f: &Fixture, folders: bool, nested: bool) -> TaskEntry {
    let board = if folders && !nested {
        f.path.parent().unwrap().parent().unwrap()
    } else {
        f.path.parent().unwrap()
    };
    read_task_entries(board, TaskStatus::Doing)
        .unwrap()
        .remove(0)
}

fn snapshot(f: &Fixture) -> ProgressSnapshot {
    ProgressSnapshot {
        target: f.target.clone(),
        recorded_at: "2026-10-23T12:00:00Z".into(),
        update: Some("Implementation is ready; running the regression tests.".into()),
        activity: "Command running: cargo test".into(),
    }
}

#[test]
fn progress_reads_only_the_selected_run_without_changing_tasks_or_controls() {
    for folders in [false, true] {
        for nested in [false, true] {
            let f = Fixture::new(folders, nested);
            let task = selected_task(&f, folders, nested);
            let before_task = fs::read(&f.path).unwrap();
            let before_control = f
                .store
                .session_control_blocking(f.project.id, &f.target.session_id)
                .unwrap();
            let path = f.target.mailbox.join("progress.json");
            steering::write_json(&path, &snapshot(&f)).unwrap();
            f.store
                .mark_session_running_blocking(
                    f.project.id,
                    "other-session",
                    std::process::id(),
                    "other-run",
                    &f.temp.path().join("other.out"),
                    &f.temp.path().join("other.err"),
                )
                .unwrap();
            let summary = task_progress_at(&f.state, &f.project.path, &task).unwrap();
            assert!(summary.contains("Agent: running"));
            assert!(summary.contains("Implementation is ready"));
            assert!(summary.contains("cargo test"));
            assert!(summary.contains("2026-10-23T12:00:00Z"));
            assert!(!summary.contains("other-session"));
            assert_eq!(fs::read(&f.path).unwrap(), before_task);
            assert_eq!(
                f.store
                    .session_control_blocking(f.project.id, &f.target.session_id)
                    .unwrap(),
                before_control
            );
            assert_eq!(fs::read_dir(&f.target.mailbox).unwrap().count(), 2);
        }
    }
}

#[test]
fn progress_rejects_stale_runs_and_handles_missing_or_corrupt_snapshots() {
    let f = Fixture::new(false, false);
    let task = selected_task(&f, false, false);
    let path = f.target.mailbox.join("progress.json");
    for wrong in 0..6 {
        let mut progress = snapshot(&f);
        match wrong {
            0 => progress.target.run_token = "old-run".into(),
            1 => progress.target.session_id = "other-session".into(),
            2 => progress.target.project_id += 1,
            3 => progress.target.child_pid += 1,
            4 => progress.target.mailbox = f.temp.path().join("other-mailbox"),
            _ => {}
        }
        if wrong == 5 {
            fs::write(&path, "partial json").unwrap();
        } else {
            steering::write_json(&path, &progress).unwrap();
        }
        let summary = task_progress_at(&f.state, &f.project.path, &task).unwrap();
        assert!(summary.contains("No progress details"));
        assert!(!summary.contains("Implementation is ready"));
    }
    fs::remove_file(path).unwrap();
    assert!(
        task_progress_at(&f.state, &f.project.path, &task)
            .unwrap()
            .contains("No progress details")
    );
}

#[test]
fn progress_for_unlinked_or_stopped_tasks_never_starts_a_session() {
    let f = Fixture::new(false, false);
    let mut task = selected_task(&f, false, false);
    task.content = "Waiting task".into();
    let absent_state = f.temp.path().join("no-state");
    assert!(
        task_progress_at(&absent_state, &f.project.path, &task)
            .unwrap()
            .contains("No agent session")
    );
    task.content.push_str(" clt:stopped");
    assert!(
        task_progress_at(&absent_state, &f.project.path, &task)
            .unwrap()
            .contains("Stopped before starting")
    );
    assert!(!absent_state.exists());
}

#[test]
fn progress_text_is_bounded_and_unicode_safe() {
    let text = format!("\x1b\t{}\nsecond paragraph", "好".repeat(500));
    let shortened = short_text(&text, 400);
    assert!(shortened.ends_with('…'));
    assert!(shortened.chars().count() <= 401);
    assert!(!shortened.chars().any(char::is_control));
    assert_eq!(short_text("one\n\ntwo", 20), "one two");
}
