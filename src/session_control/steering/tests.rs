use super::*;
use crate::{
    agent::{AgentProject, AgentSessionControlState},
    task::{init_tasks, read_task_entries},
};

pub(crate) struct Fixture {
    pub(crate) temp: tempfile::TempDir,
    pub(crate) state: PathBuf,
    pub(crate) store: TursoAgentStore,
    pub(crate) project: AgentProject,
    pub(crate) path: PathBuf,
    pub(crate) target: SteeringTarget,
}

impl Fixture {
    pub(crate) fn new(folders: bool, nested: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("project");
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        init_tasks(&root, folders).unwrap();
        let board = if nested {
            let board = get_tasks_dir(&root).join("doing/parent");
            fs::create_dir_all(&board).unwrap();
            for status in ["todo", "doing", "done"] {
                fs::write(board.join(format!("{status}.md")), "").unwrap();
            }
            board
        } else {
            get_tasks_dir(&root)
        };
        let session = uuid::Uuid::now_v7().to_string();
        let path = if folders && !nested {
            board.join("doing/0001-task.md")
        } else {
            board.join("doing.md")
        };
        fs::write(&path, format!("- Preserve this task. codex:{session}\n")).unwrap();
        let store = TursoAgentStore::open_blocking(&state).unwrap();
        store.register_project_blocking(&root, "steering").unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let log = temp.path().join("run.err");
        store
            .mark_session_running_blocking(
                project.id,
                &session,
                std::process::id(),
                "run-one",
                &temp.path().join("run.out"),
                &log,
            )
            .unwrap();
        let target = SteeringTarget {
            project_id: project.id,
            session_id: session.clone(),
            thread_id: session,
            child_pid: std::process::id(),
            run_token: "run-one".into(),
            turn_id: "turn-one".into(),
            mailbox: mailbox_for_log(&log),
        };
        write_json(&target.mailbox.join("active.json"), &target).unwrap();
        Self {
            temp,
            state,
            store,
            project,
            path,
            target,
        }
    }
}

#[test]
fn steering_preserves_markdown_folder_and_nested_task_content() {
    for folders in [false, true] {
        for nested in [false, true] {
            let f = Fixture::new(folders, nested);
            let original = fs::read(&f.path).unwrap();
            let board = if folders && !nested {
                f.path.parent().unwrap().parent().unwrap()
            } else {
                f.path.parent().unwrap()
            };
            let task = read_task_entries(board, TaskStatus::Doing)
                .unwrap()
                .remove(0);
            assert_eq!(
                target_for_task(&f.state, &f.project.path, TaskStatus::Doing, &task).unwrap(),
                Some(f.target.clone())
            );
            assert!(
                target_for_task(&f.state, &f.project.path, TaskStatus::Todo, &task)
                    .unwrap()
                    .is_none()
            );
            let text = "Keep the API.\nUse café and \"quotes\".";
            let request = request_message(&f.state, &f.target, text).unwrap();
            assert_eq!(
                read_json::<SteeringRequest>(&request_path(&f.target.mailbox, &request.id))
                    .unwrap()
                    .text,
                text
            );
            assert_eq!(fs::read(&f.path).unwrap(), original);
            write_json(
                &reply_path(&f.target.mailbox, &request.id),
                &SteeringReply {
                    accepted: true,
                    message: "Sent to the running agent".into(),
                },
            )
            .unwrap();
            assert!(
                wait_for_reply(&request, Duration::ZERO)
                    .unwrap()
                    .starts_with("Sent")
            );
        }
    }
}

#[test]
fn stale_turn_stopped_replaced_and_ambiguous_tasks_cannot_queue_input() {
    for change in ["turn", "stop", "pid", "run", "task", "duplicate"] {
        let f = Fixture::new(false, false);
        match change {
            "turn" => {
                let mut new = f.target.clone();
                new.turn_id = "next-turn".into();
                write_json(&new.mailbox.join("active.json"), &new).unwrap();
            }
            "stop" => {
                f.store
                    .set_session_control_state_blocking(
                        f.project.id,
                        &f.target.session_id,
                        AgentSessionControlState::StopRequested,
                    )
                    .unwrap();
            }
            "pid" | "run" => {
                f.store
                    .mark_session_running_blocking(
                        f.project.id,
                        &f.target.session_id,
                        if change == "pid" {
                            42
                        } else {
                            f.target.child_pid
                        },
                        if change == "run" {
                            "new-run"
                        } else {
                            "run-one"
                        },
                        &f.temp.path().join("run.out"),
                        &f.temp.path().join("run.err"),
                    )
                    .unwrap();
            }
            "task" => fs::write(&f.path, "- Different task\n").unwrap(),
            "duplicate" => fs::write(
                &f.path,
                format!(
                    "- First codex:{}\n- Second codex:{}\n",
                    f.target.session_id, f.target.session_id
                ),
            )
            .unwrap(),
            _ => unreachable!(),
        }
        assert!(
            request_message(&f.state, &f.target, "Do this").is_err(),
            "{change}"
        );
        assert_eq!(fs::read_dir(&f.target.mailbox).unwrap().count(), 1);
    }
}

#[test]
fn queued_is_not_accepted_and_errors_preserve_uncertainty() {
    let f = Fixture::new(false, false);
    assert!(request_message(&f.state, &f.target, " \n").is_err());
    assert!(request_message(&f.state, &f.target, &"a".repeat(MAX_MESSAGE_BYTES + 1)).is_err());
    let request = request_message(&f.state, &f.target, "Valid").unwrap();
    assert!(
        format!(
            "{:#}",
            wait_for_reply(&request, Duration::ZERO).unwrap_err()
        )
        .contains("unconfirmed")
    );
    write_json(
        &reply_path(&f.target.mailbox, &request.id),
        &SteeringReply {
            accepted: false,
            message: "Stale turn".into(),
        },
    )
    .unwrap();
    assert!(
        wait_for_reply(&request, Duration::ZERO)
            .unwrap_err()
            .to_string()
            .contains("Stale turn")
    );
}

#[test]
fn escaped_text_at_the_size_limit_survives_the_mailbox() {
    let f = Fixture::new(false, false);
    let text = format!("x{}", "\u{1}".repeat(MAX_MESSAGE_BYTES - 1));
    let request = request_message(&f.state, &f.target, &text).unwrap();
    let stored: SteeringRequest = read_json(&request_path(&f.target.mailbox, &request.id)).unwrap();
    assert_eq!(stored.text, text);
}
