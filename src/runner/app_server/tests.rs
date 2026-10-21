use super::*;
use crate::session_control::steering::tests::Fixture;

fn spec(f: &Fixture) -> LaunchSpec {
    LaunchSpec {
        program: "codex".into(),
        arguments: vec![],
        params: json!({"cwd": f.project.path, "approvalPolicy": "never", "sandbox": "danger-full-access"}),
        prompt: "Resume this exact task and preserve its frozen Git boundary".into(),
        resume: Some(f.target.session_id.clone()),
        state_dir: f.state.clone(),
        project_id: f.project.id,
        run_token: f.target.run_token.clone(),
        mailbox: f.target.mailbox.clone(),
    }
}

fn active<'a>(
    spec: &'a LaunchSpec,
    f: &Fixture,
) -> (Protocol<'a, Vec<u8>>, mpsc::Sender<Result<Value>>) {
    let (sender, receiver) = mpsc::channel();
    let mut protocol = Protocol::new(spec, Vec::new(), receiver);
    protocol.thread_id = f.target.thread_id.clone();
    protocol.session_id = f.target.session_id.clone();
    protocol.start_turn(&f.target.turn_id).unwrap();
    (protocol, sender)
}

fn messages(protocol: &Protocol<'_, Vec<u8>>) -> Vec<Value> {
    String::from_utf8(protocol.input.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn adapter_process_entry() {
    let Ok(spec) = std::env::var("CLT_TEST_APP_SERVER_SPEC") else {
        return;
    };
    let result = run_serialized(&spec);
    if let Err(error) = &result {
        eprintln!("{error:#}");
    }
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}

#[test]
fn owning_worker_steers_active_turn_and_only_acknowledges_success() {
    let f = Fixture::new(false, false);
    let original = fs::read(&f.path).unwrap();
    let spec = spec(&f);
    let (mut protocol, _sender) = active(&spec, &f);
    let request =
        steering::request_message(&f.state, &f.target, "Use the existing API\nThen test it")
            .unwrap();
    protocol.mailbox().unwrap();
    let sent = messages(&protocol);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["method"], "turn/steer");
    assert_eq!(sent[0]["params"]["threadId"], f.target.thread_id);
    assert_eq!(sent[0]["params"]["expectedTurnId"], "turn-one");
    assert_eq!(sent[0]["params"]["input"][0]["text"], request.text);
    assert!(!steering::reply_path(&spec.mailbox, &request.id).exists());
    protocol.mailbox().unwrap(); // Pending file is not sent twice.
    assert_eq!(messages(&protocol).len(), 1);
    protocol
        .event(json!({"id": sent[0]["id"], "result": {"turnId": "turn-one"}}))
        .unwrap();
    assert!(steering::wait_for_reply(&request, Duration::ZERO).is_ok());
    protocol.mailbox().unwrap();
    assert_eq!(messages(&protocol).len(), 1);
    assert_eq!(fs::read(&f.path).unwrap(), original);
}

#[test]
fn worker_rechecks_ownership_and_turn_after_request_is_queued() {
    for change in ["turn", "stop", "session", "run"] {
        let f = Fixture::new(false, false);
        let spec = spec(&f);
        let (mut protocol, _sender) = active(&spec, &f);
        let request = steering::request_message(&f.state, &f.target, "Do this").unwrap();
        match change {
            "turn" => protocol.start_turn("next-turn").unwrap(),
            "stop" => {
                f.store
                    .request_session_stop_blocking(
                        f.project.id,
                        &f.target.session_id,
                        f.target.child_pid,
                        &f.target.run_token,
                    )
                    .unwrap();
            }
            "session" => {
                protocol.target.as_mut().unwrap().session_id = "different".into();
            }
            "run" => {
                protocol.target.as_mut().unwrap().run_token = "different".into();
            }
            _ => unreachable!(),
        }
        protocol.mailbox().unwrap();
        assert!(messages(&protocol).is_empty());
        assert!(steering::wait_for_reply(&request, Duration::ZERO).is_err());
    }
}

#[test]
fn stale_turn_errors_wrong_receipts_and_disconnect_never_acknowledge() {
    for response in [
        json!({"error": {"code": -32600, "message": "expectedTurnId mismatch"}}),
        json!({"result": {"turnId": "another-turn"}}),
        Value::Null,
    ] {
        let f = Fixture::new(false, false);
        let spec = spec(&f);
        let (mut protocol, _sender) = active(&spec, &f);
        let request = steering::request_message(&f.state, &f.target, "Do this").unwrap();
        protocol.mailbox().unwrap();
        if response.is_null() {
            protocol.close_mailbox();
        } else {
            let mut response = response;
            response["id"] = messages(&protocol)[0]["id"].clone();
            protocol.event(response).unwrap();
        }
        assert!(steering::wait_for_reply(&request, Duration::ZERO).is_err());
    }
}

#[test]
fn malformed_requests_are_rejected_without_stopping_the_worker() {
    let f = Fixture::new(false, false);
    let spec = spec(&f);
    let (mut protocol, _sender) = active(&spec, &f);
    let id = uuid::Uuid::now_v7().to_string();
    fs::write(steering::request_path(&spec.mailbox, &id), "not json").unwrap();
    protocol.mailbox().unwrap();
    assert!(messages(&protocol).is_empty());
    let receipt: SteeringReply =
        steering::read_json(&steering::reply_path(&spec.mailbox, &id)).unwrap();
    assert!(!receipt.accepted);
    assert!(protocol.active_turn.is_some());
}

#[test]
fn resume_must_return_the_same_session_and_other_thread_events_are_ignored() {
    let f = Fixture::new(false, false);
    let spec = spec(&f);
    let (mut protocol, sender) = active(&spec, &f);
    protocol
        .event(json!({"method": "turn/started", "params": {
            "threadId": "subagent", "turn": {"id": "subagent-turn"}
        }}))
        .unwrap();
    assert_eq!(protocol.active_turn.as_deref(), Some("turn-one"));
    sender.send(Ok(json!({"id": 1, "result": {}}))).unwrap();
    sender
        .send(Ok(
            json!({"id": 2, "result": {"thread": {"id": uuid::Uuid::now_v7().to_string()}}}),
        ))
        .unwrap();
    assert!(
        protocol
            .run()
            .unwrap_err()
            .to_string()
            .contains("different session")
    );
    assert!(
        !messages(&protocol)
            .iter()
            .any(|v| v["method"] == "turn/start")
    );
}

#[test]
fn protocol_preserves_resume_prompt_config_and_goal_continuations() {
    let f = Fixture::new(false, false);
    let spec = spec(&f);
    let (sender, receiver) = mpsc::channel();
    let id = &f.target.session_id;
    for event in [
        json!({"id": 1, "result": {}}),
        json!({"id": 2, "result": {"thread": {"id": id}, "model": "test-model", "reasoningEffort": "high"}}),
        json!({"method": "turn/started", "params": {"threadId": id, "turn": {"id": "one"}}}),
        json!({"id": 3, "result": {"turn": {"id": "one"}}}),
        json!({"method": "item/completed", "params": {"threadId": id, "item": {"type": "mcpToolCall"}}}),
        json!({"method": "turn/completed", "params": {"threadId": id, "turn": {"id": "one", "status": "completed"}}}),
        json!({"id": 4, "result": {"goal": {"status": "active"}}}),
        json!({"method": "turn/started", "params": {"threadId": id, "turn": {"id": "two"}}}),
        json!({"method": "item/completed", "params": {"threadId": id, "item": {"type": "agentMessage", "text": "Done"}}}),
        json!({"method": "turn/completed", "params": {"threadId": id, "turn": {"id": "two", "status": "completed"}}}),
        json!({"id": 5, "result": {"goal": {"status": "complete"}}}),
    ] {
        sender.send(Ok(event)).unwrap();
    }
    let mut protocol = Protocol::new(&spec, Vec::new(), receiver);
    protocol.run().unwrap();
    assert_eq!(protocol.turn_count, 2);
    assert_eq!(protocol.final_text, "Done");
    let sent = messages(&protocol);
    assert_eq!(sent[2]["method"], "thread/resume");
    assert_eq!(sent[2]["params"]["threadId"], *id);
    assert_eq!(sent[2]["params"]["approvalPolicy"], "never");
    assert_eq!(sent[3]["params"]["input"][0]["text"], spec.prompt);
    assert_eq!(
        sent.iter().filter(|v| v["method"] == "turn/start").count(),
        1
    );
}

#[test]
fn failed_interrupted_and_disconnected_turns_fail_the_run() {
    for status in ["failed", "interrupted"] {
        let f = Fixture::new(false, false);
        let spec = spec(&f);
        let (mut protocol, _sender) = active(&spec, &f);
        assert!(
            protocol
                .event(json!({"method": "turn/completed", "params": {
                    "threadId": f.target.thread_id, "turn": {"id": "turn-one", "status": status}
                }}))
                .is_err()
        );
        assert!(!spec.mailbox.join("active.json").exists());
    }
    let f = Fixture::new(false, false);
    let spec = spec(&f);
    let (mut protocol, sender) = active(&spec, &f);
    drop(sender);
    assert!(
        protocol
            .run()
            .unwrap_err()
            .to_string()
            .contains("disconnected")
    );
}

#[test]
fn adapter_preserves_launch_configuration_and_environment() {
    let f = Fixture::new(false, false);
    let mut original = Command::new("/path with spaces/codex");
    original
        .current_dir(&f.project.path)
        .env("GIT_AUTHOR_NAME", "CLT Agent")
        .env("CLT_TEST_PROVIDER_KEY", "credential-value")
        .args([
            "--sandbox",
            "danger-full-access",
            "--ask-for-approval",
            "never",
            "--enable",
            "goals",
            "--config",
            "model_provider=\"provider\"",
            "--model",
            "model-id",
            "--config",
            "model_reasoning_effort=\"high\"",
            "--enable",
            "fast_mode",
            "--config",
            "service_tier=\"fast\"",
            "exec",
            "resume",
            "--skip-git-repo-check",
        ])
        .arg(&f.target.session_id)
        .arg("Frozen prompt\n/goal embedded text");
    let spec = launch_spec(
        &original,
        &f.state,
        f.project.id,
        "run-token",
        &f.temp.path().join("run.err"),
    )
    .unwrap();
    assert_eq!(spec.resume.as_deref(), Some(f.target.session_id.as_str()));
    assert_eq!(spec.params["model"], "model-id");
    assert_eq!(spec.params["modelProvider"], "provider");
    assert_eq!(spec.params["config"]["features.goals"], true);
    assert_eq!(spec.params["config"]["model_reasoning_effort"], "high");
    assert_eq!(spec.params["config"]["service_tier"], "fast");
    assert_eq!(spec.prompt, "Frozen prompt\n/goal embedded text");
    let command = adapter_command(
        &original,
        &f.state,
        f.project.id,
        "run-token",
        &f.temp.path().join("run.err"),
    )
    .unwrap();
    assert_eq!(command.get_current_dir(), original.get_current_dir());
    assert!(
        command
            .get_envs()
            .any(|(k, v)| k == "GIT_AUTHOR_NAME" && v.unwrap() == "CLT Agent")
    );
    assert!(
        command
            .get_envs()
            .any(|(k, v)| k == "CLT_TEST_PROVIDER_KEY" && v.unwrap() == "credential-value")
    );
    assert!(
        !serde_json::to_string(&spec)
            .unwrap()
            .contains("credential-value")
    );
}

#[cfg(unix)]
#[test]
fn gated_runner_delivers_during_work_and_cancels_its_private_server() {
    use crate::{
        application::{AgentTaskSelection, new_agent_shutdown_signal},
        runner::{CodexAgentRunner, agent_timestamp, agent_timestamp_after},
        task::{TaskStatus, read_task_entries},
    };
    use std::os::unix::fs::PermissionsExt;
    for cancel in [false, true] {
        let f = Fixture::new(false, false);
        f.store
            .mark_session_running_with_git_mode_blocking(
                f.project.id,
                &f.target.session_id,
                f.target.child_pid,
                &f.target.run_token,
                &f.temp.path().join("run.out"),
                &f.temp.path().join("run.err"),
                crate::agent::AgentGitMode::Off,
            )
            .unwrap();
        f.store
            .request_session_stop_blocking(
                f.project.id,
                &f.target.session_id,
                f.target.child_pid,
                &f.target.run_token,
            )
            .unwrap();
        f.store
            .complete_session_stop_blocking(f.project.id, &f.target.session_id, &f.target.run_token)
            .unwrap();
        f.store
            .request_stopped_session_resume_blocking(
                f.project.id,
                &f.target.session_id,
                Some(&f.target.run_token),
            )
            .unwrap();
        f.store
            .try_acquire_lease_blocking(
                f.project.id,
                "adapter-test",
                &agent_timestamp(),
                &agent_timestamp_after(60),
            )
            .unwrap();
        let original = fs::read(&f.path).unwrap();
        let fake = f.temp.path().join("fake-codex");
        // A private stdio peer: it stays in the active turn until steering or
        // cancellation, with no network, model call, or shared daemon.
        let script = r#"#!/bin/sh
printf '%s\n' "$$" > server.pid
while IFS= read -r line; do
  printf '%s\n' "$line" >> protocol.jsonl
  id=${line#*\"id\":}
  id=${id%%,*}
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
    *'"method":"thread/resume"'*) printf '{"id":%s,"result":{"thread":{"id":"SESSION"},"model":"mock-model","reasoningEffort":"high"}}\n' "$id" ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"active"}}}\n' "$id"
      printf '%s\n' '{"method":"turn/started","params":{"threadId":"SESSION","turn":{"id":"active"}}}'
      printf '%s\n' '{"method":"item/completed","params":{"threadId":"other-thread","item":{"type":"agentMessage","text":"foreign thread output"}}}'
      printf '%s\n' '{"method":"item/started","params":{"threadId":"SESSION","item":{"type":"commandExecution","id":"cmd-one","command":"cargo test","cwd":"/project","status":"inProgress"}}}'
      printf '%s\n' '{"method":"item/commandExecution/outputDelta","params":{"threadId":"SESSION","itemId":"cmd-one","delta":"first test passed\nsecond test passed"}}'
      printf '%s\n' '{"method":"item/completed","params":{"threadId":"SESSION","item":{"type":"commandExecution","id":"cmd-one","command":"cargo test","cwd":"/project","status":"completed","exitCode":0,"aggregatedOutput":"first test passed\nsecond test passed","commandActions":[{"command":"cargo test"}]}}}'
      printf '%s\n' '{"method":"item/completed","params":{"threadId":"SESSION","item":{"type":"reasoning","summary":[],"content":[]}}}' ;;
    *'"method":"turn/steer"'*)
      printf '{"id":%s,"result":{"turnId":"active"}}\n' "$id"
      printf '%s\n' '{"method":"item/completed","params":{"threadId":"SESSION","item":{"type":"agentMessage","text":"Steering received"}}}'
      printf '%s\n' '{"method":"turn/completed","params":{"threadId":"SESSION","turn":{"id":"active","status":"completed"}}}' ;;
    *'"method":"thread/goal/get"'*) printf '{"id":%s,"result":{"goal":null}}\n' "$id" ;;
  esac
done
"#.replace("SESSION", &f.target.session_id);
        fs::write(&fake, script).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let mut runner =
            CodexAgentRunner::with_command(f.state.clone(), Duration::from_secs(12), fake);
        runner.app_server = true;
        runner.heartbeat_interval = Duration::from_millis(20);
        let project = f.project.clone();
        let resume_session_id = f.target.session_id.clone();
        let handle = thread::spawn(move || {
            runner.run_project(
                &project,
                AgentTaskSelection::ResumeSession,
                Some(&resume_session_id),
                "adapter-test",
                None,
                &new_agent_shutdown_signal(),
            )
        });
        let task = read_task_entries(&get_tasks_dir(&f.project.path), TaskStatus::Doing)
            .unwrap()
            .remove(0);
        let deadline = Instant::now() + Duration::from_secs(8);
        let target = loop {
            if let Ok(Some(target)) =
                steering::target_for_task(&f.state, &f.project.path, TaskStatus::Doing, &task)
                && target.run_token != "run-one"
            {
                break target;
            }
            if handle.is_finished() {
                match handle.join().unwrap() {
                    Ok(result) => panic!(
                        "Adapter exited before accepting input: {}\n{}",
                        result.summary,
                        fs::read_to_string(result.stderr_path).unwrap_or_default()
                    ),
                    Err(error) => panic!("Adapter failed before accepting input: {error:#}"),
                }
            }
            assert!(
                Instant::now() < deadline,
                "Adapter did not become steerable"
            );
            thread::sleep(Duration::from_millis(20));
        };
        let server_pid: u32 = fs::read_to_string(f.project.path.join("server.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_ne!(target.child_pid, server_pid); // The recorded child owns the pipe.
        if cancel {
            assert!(
                f.store
                    .request_session_stop_blocking(
                        f.project.id,
                        &target.session_id,
                        target.child_pid,
                        &target.run_token
                    )
                    .unwrap()
            );
            assert!(steering::request_message(&f.state, &target, "Too late").is_err());
        } else {
            let request =
                steering::request_message(&f.state, &target, "Keep task text unchanged\nUse café")
                    .unwrap();
            assert!(steering::wait_for_reply(&request, Duration::from_secs(5)).is_ok());
        }
        let result = handle.join().unwrap().unwrap();
        assert_eq!(
            result.codex_session_id.as_deref(),
            Some(f.target.session_id.as_str())
        );
        assert_eq!(fs::read(&f.path).unwrap(), original);
        let captured = fs::read_to_string(f.project.path.join("protocol.jsonl")).unwrap();
        let messages: Vec<Value> = captured
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let start = messages
            .iter()
            .find(|v| v["method"] == "turn/start")
            .unwrap();
        assert!(
            start["params"]["input"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Existing task recovery")
        );
        assert_eq!(
            messages
                .iter()
                .filter(|v| v["method"] == "turn/steer")
                .count(),
            usize::from(!cancel)
        );
        assert_eq!(unsafe { libc::kill(server_pid as libc::pid_t, 0) }, -1);
        if !cancel {
            assert_eq!(result.exit_code, Some(0));
            let transcript = fs::read_to_string(&result.stderr_path).unwrap();
            assert!(transcript.contains("exec\nin /project\ncargo test\nfirst test passed\nsecond test passed\nexec: completed (exit code 0)\n"));
            assert_eq!(transcript.matches("first test passed").count(), 1);
            assert!(transcript.contains("codex\nSteering received\n"));
            for unwanted in [
                "aggregatedOutput",
                "commandActions",
                "reasoning: {",
                "foreign thread output",
            ] {
                assert!(
                    !transcript.contains(unwanted),
                    "Unexpected {unwanted} in log"
                );
            }
            assert_eq!(
                fs::read_to_string(&result.stdout_path).unwrap().trim(),
                // The test-only adapter entry point adds the harness banner.
                "running 1 test\nSteering received"
            );
            let settings =
                crate::runner::agent_run_settings_from_log(Path::new(&result.stderr_path)).unwrap();
            assert_eq!(settings.model.as_deref(), Some("mock-model"));
        }
    }
}
