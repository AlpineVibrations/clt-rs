//! The gated worker owns one private Codex app-server and its only input pipe.
//! The TUI submits to this worker's mailbox; it never attaches another writer.
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    agent::open_agent_store_at,
    session_control::steering::{self, SteeringReply, SteeringRequest, SteeringTarget},
    task::{acquire_board_mutation_lock, get_tasks_dir},
};

const RPC_TIMEOUT: Duration = Duration::from_secs(20);

mod log;

#[derive(Debug, Serialize, Deserialize)]
struct LaunchSpec {
    program: PathBuf,
    arguments: Vec<String>,
    params: Value,
    prompt: String,
    resume: Option<String>,
    state_dir: PathBuf,
    project_id: i64,
    run_token: String,
    mailbox: PathBuf,
}

pub(crate) fn adapter_command(
    original: &Command,
    state_dir: &Path,
    project_id: i64,
    run_token: &str,
    stderr_path: &Path,
) -> Result<Command> {
    let spec = launch_spec(original, state_dir, project_id, run_token, stderr_path)?;
    let mut command = Command::new(std::env::current_exe()?);
    #[cfg(not(test))]
    command
        .args(["--local", "agent", "app-server-session", "--spec"])
        .arg(serde_json::to_string(&spec)?);
    #[cfg(test)]
    command
        .args([
            "--exact",
            "runner::app_server::tests::adapter_process_entry",
            "--nocapture",
        ])
        .env("CLT_TEST_APP_SERVER_SPEC", serde_json::to_string(&spec)?);
    if let Some(cwd) = original.get_current_dir() {
        command.current_dir(cwd);
    }
    for (key, value) in original.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    Ok(command)
}

fn launch_spec(
    original: &Command,
    state_dir: &Path,
    project_id: i64,
    run_token: &str,
    stderr_path: &Path,
) -> Result<LaunchSpec> {
    let args = original
        .get_args()
        .map(|arg| arg.to_str().context("Non-UTF-8 Codex argument"))
        .collect::<Result<Vec<_>>>()?;
    let mut params = json!({"cwd": original.get_current_dir(), "ephemeral": false, "config": {}});
    let mut arguments = vec![
        "app-server".to_string(),
        "--listen".into(),
        "stdio://".into(),
    ];
    let mut index = 0;
    while args.get(index).copied() != Some("exec") {
        let flag = args
            .get(index)
            .context("Missing automated Codex subcommand")?;
        let value = *args.get(index + 1).context("Missing Codex option value")?;
        match *flag {
            "--sandbox" => params["sandbox"] = json!(value),
            "--ask-for-approval" => params["approvalPolicy"] = json!(value),
            "--model" => params["model"] = json!(value),
            "--config" => {
                let (key, _) = value
                    .split_once('=')
                    .context("Invalid Codex configuration")?;
                let document = value.parse::<toml_edit::DocumentMut>()?;
                let scalar = document
                    .get(key)
                    .context("Unsupported Codex configuration")?;
                let value_json = if let Some(value) = scalar.as_str() {
                    json!(value)
                } else if let Some(value) = scalar.as_bool() {
                    json!(value)
                } else {
                    anyhow::bail!("Unsupported automated Codex configuration: {key}")
                };
                if key == "model_provider" {
                    params["modelProvider"] = value_json.clone();
                }
                params["config"][key] = value_json;
                arguments.extend([flag.to_string(), value.to_string()]);
            }
            "--enable" | "--disable" => {
                params["config"][format!("features.{value}")] = json!(*flag == "--enable");
                arguments.extend([flag.to_string(), value.to_string()]);
            }
            _ => anyhow::bail!("Unsupported automated Codex flag: {flag}"),
        }
        index += 2;
    }
    let tail = &args[index + 1..];
    let (resume, prompt) = match tail {
        ["resume", "--skip-git-repo-check", session, prompt] => {
            (Some(session.to_string()), *prompt)
        }
        ["--skip-git-repo-check", "-C", _, prompt] => (None, *prompt),
        _ => anyhow::bail!("Unsupported automated Codex command shape"),
    };
    Ok(LaunchSpec {
        program: original.get_program().into(),
        arguments,
        params,
        prompt: prompt.to_string(),
        resume,
        state_dir: state_dir.to_path_buf(),
        project_id,
        run_token: run_token.to_string(),
        mailbox: steering::mailbox_for_log(stderr_path),
    })
}

pub(crate) fn run_serialized(spec: &str) -> Result<()> {
    let spec: LaunchSpec = serde_json::from_str(spec)?;
    let mut child = Command::new(&spec.program)
        .args(&spec.arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        // Keep the supervisor's process group, environment, cwd and Git identity.
        .spawn()
        .context("Unable to start the worker's Codex app-server")?;
    let input = child.stdin.take().context("Missing app-server input")?;
    let output = child.stdout.take().context("Missing app-server output")?;
    let (sender, receiver) = mpsc::sync_channel(256);
    thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let event = line
                .context("Codex app-server read failed")
                .and_then(|line| {
                    serde_json::from_str::<Value>(&line).context("Invalid Codex app-server frame")
                });
            let failed = event.is_err();
            if sender.send(event).is_err() || failed {
                break;
            }
        }
    });
    let mut protocol = Protocol::new(&spec, input, receiver);
    let result = protocol.run();
    protocol.close_mailbox();
    drop(protocol); // EOF asks the private server to shut down.
    let deadline = Instant::now() + Duration::from_secs(2);
    while child.try_wait()?.is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    if child.try_wait()?.is_none() {
        child.kill()?;
    }
    child.wait()?;
    result
}

struct Protocol<'a, W> {
    spec: &'a LaunchSpec,
    input: W,
    events: Receiver<Result<Value>>,
    next_id: u64,
    target: Option<SteeringTarget>,
    pending: HashMap<u64, (SteeringRequest, Instant)>,
    thread_id: String,
    session_id: String,
    active_turn: Option<String>,
    completed_turn: Option<String>,
    turn_count: usize,
    used_tools: bool,
    final_text: String,
    log: log::EventLog,
}

impl<'a, W: Write> Protocol<'a, W> {
    fn new(spec: &'a LaunchSpec, input: W, events: Receiver<Result<Value>>) -> Self {
        Self {
            spec,
            input,
            events,
            next_id: 1,
            target: None,
            pending: HashMap::new(),
            thread_id: String::new(),
            session_id: String::new(),
            active_turn: None,
            completed_turn: None,
            turn_count: 0,
            used_tools: false,
            final_text: String::new(),
            log: log::EventLog::default(),
        }
    }

    fn write(&mut self, message: Value) -> Result<()> {
        serde_json::to_writer(&mut self.input, &message)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        Ok(())
    }

    fn send(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({"id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.send(method, params)?;
        let deadline = Instant::now() + RPC_TIMEOUT;
        loop {
            let event = self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .with_context(|| format!("Codex {method} timed out or disconnected"))??;
            if event["id"].as_u64() == Some(id) {
                anyhow::ensure!(
                    event.get("error").is_none(),
                    "Codex {method}: {}",
                    event["error"]
                );
                return event
                    .get("result")
                    .cloned()
                    .context("Missing Codex RPC result");
            }
            self.event(event)?;
        }
    }

    fn run(&mut self) -> Result<()> {
        self.rpc(
            "initialize",
            json!({"clientInfo": {"name": "clt", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": true}}),
        )?;
        self.write(json!({"method": "initialized"}))?;
        let mut params = self.spec.params.clone();
        let method = if let Some(session) = &self.spec.resume {
            params["threadId"] = json!(session);
            "thread/resume"
        } else {
            "thread/start"
        };
        let response = self.rpc(method, params)?;
        self.thread_id = response["thread"]["id"]
            .as_str()
            .context("Missing Codex thread ID")?
            .to_string();
        self.session_id = response["thread"]["sessionId"]
            .as_str()
            .unwrap_or(&self.thread_id)
            .to_string();
        anyhow::ensure!(
            uuid::Uuid::parse_str(&self.session_id).is_ok(),
            "Invalid Codex session ID"
        );
        if let Some(expected) = &self.spec.resume {
            anyhow::ensure!(
                &self.session_id == expected,
                "Codex resumed a different session"
            );
        }
        eprintln!(
            "CLT Codex app-server\n--------\nworkdir: {}\nmodel: {}\nreasoning effort: {}\nsession id: {}\n--------\nuser\n{}\ncodex",
            self.spec.params["cwd"].as_str().unwrap_or_default(),
            response["model"].as_str().unwrap_or("default"),
            response["reasoningEffort"].as_str().unwrap_or("default"),
            self.session_id,
            self.spec.prompt
        );
        let result = self.rpc(
            "turn/start",
            json!({"threadId": self.thread_id,
            "input": [{"type": "text", "text": self.spec.prompt}]}),
        )?;
        if self.active_turn.is_none() && self.completed_turn.is_none() {
            self.start_turn(
                result["turn"]["id"]
                    .as_str()
                    .context("Missing Codex turn ID")?,
            )?;
        }
        let mut goal_wait: Option<Instant> = None;
        loop {
            self.mailbox()?;
            if let Some(completed) = self.completed_turn.take() {
                let used_tools = self.used_tools;
                let turn_count = self.turn_count;
                let goal = self.rpc("thread/goal/get", json!({"threadId": self.thread_id}))?;
                // Codex schedules goal continuations itself. A continuation with
                // no tool calls suppresses further automatic work, as in exec.
                if self.active_turn.is_none() && self.completed_turn.is_none() {
                    if goal["goal"]["status"] != "active" || (turn_count > 1 && !used_tools) {
                        // Responses and completion notifications can cross. Give
                        // already-sent requests their bounded acknowledgement
                        // window before closing this connection.
                        while !self.pending.is_empty() {
                            self.mailbox()?;
                            match self.events.recv_timeout(Duration::from_millis(50)) {
                                Ok(event) => self.event(event?)?,
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                        eprintln!("CLT turn completed: {completed}");
                        println!("{}", self.final_text);
                        return Ok(());
                    }
                    goal_wait = Some(Instant::now());
                }
            }
            if self.active_turn.is_some() {
                goal_wait = None;
            }
            anyhow::ensure!(
                !goal_wait.is_some_and(|start| start.elapsed() >= RPC_TIMEOUT),
                "Codex has an active goal but did not start its continuation; preserving the session for recovery"
            );
            match self.events.recv_timeout(Duration::from_millis(50)) {
                Ok(event) => self.event(event?)?,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("Codex app-server disconnected before completion")
                }
            }
        }
    }

    fn start_turn(&mut self, id: &str) -> Result<()> {
        if self.active_turn.as_deref() == Some(id) {
            return Ok(());
        }
        self.active_turn = Some(id.to_string());
        self.turn_count += 1;
        self.used_tools = false;
        self.final_text.clear();
        self.log.start_turn();
        let target = SteeringTarget {
            project_id: self.spec.project_id,
            session_id: self.session_id.clone(),
            thread_id: self.thread_id.clone(),
            run_token: self.spec.run_token.clone(),
            child_pid: std::process::id(),
            turn_id: id.to_string(),
            mailbox: self.spec.mailbox.clone(),
        };
        steering::write_json(&target.mailbox.join("active.json"), &target)?;
        self.target = Some(target);
        Ok(())
    }

    fn event(&mut self, event: Value) -> Result<()> {
        if let Some(id) = event["id"].as_u64()
            && let Some((request, _)) = self.pending.remove(&id)
        {
            let accepted = event.get("error").is_none()
                && event["result"]["turnId"].as_str() == Some(&request.target.turn_id);
            let message = if accepted {
                "Sent to the running agent".to_string()
            } else {
                format!(
                    "Codex did not confirm steering acceptance: {}",
                    event.get("error").unwrap_or(&event)
                )
            };
            self.receipt(&request, accepted, message)?;
            return Ok(());
        }
        if event.get("method").is_some() && event.get("id").is_some() {
            // Automated policy is never; do not invent answers to interactive requests.
            self.write(json!({"id": event["id"], "error": {"code": -32601,
                "message": "CLT automated runs cannot answer interactive requests"}}))?;
            anyhow::bail!(
                "Codex requested unsupported interaction: {}",
                event["method"]
            );
        }
        let params = &event["params"];
        if params["threadId"].as_str() != Some(&self.thread_id) {
            return Ok(());
        }
        match event["method"].as_str().unwrap_or_default() {
            "turn/started" => {
                self.start_turn(params["turn"]["id"].as_str().context("Missing turn ID")?)?
            }
            "turn/completed" => {
                let id = params["turn"]["id"]
                    .as_str()
                    .context("Missing completed turn ID")?;
                anyhow::ensure!(
                    self.active_turn.as_deref() == Some(id),
                    "Unexpected completed Codex turn"
                );
                self.active_turn = None;
                self.target = None;
                let _ = fs::remove_file(self.spec.mailbox.join("active.json"));
                anyhow::ensure!(
                    params["turn"]["status"] == "completed",
                    "Codex turn {}: {}",
                    params["turn"]["status"],
                    params["turn"]["error"]
                );
                self.completed_turn = Some(id.to_string());
            }
            "item/started" | "item/completed" => {
                let item = &params["item"];
                let kind = item["type"].as_str().unwrap_or_default();
                if !matches!(
                    kind,
                    "agentMessage" | "userMessage" | "reasoning" | "plan" | "contextCompaction"
                ) {
                    self.used_tools = true;
                }
                if event["method"] == "item/completed"
                    && kind == "agentMessage"
                    && let Some(text) = item["text"].as_str()
                {
                    self.final_text = text.to_string();
                }
            }
            _ => {}
        }
        eprint!(
            "{}",
            self.log
                .event(event["method"].as_str().unwrap_or_default(), params)
        );
        Ok(())
    }

    fn receipt(&self, request: &SteeringRequest, accepted: bool, message: String) -> Result<()> {
        steering::write_json(
            &steering::reply_path(&self.spec.mailbox, &request.id),
            &SteeringReply { accepted, message },
        )
    }

    fn mailbox(&mut self) -> Result<()> {
        if !self.spec.mailbox.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(&self.spec.mailbox)? {
            let path = entry?.path();
            let Some(id) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".request.json"))
            else {
                continue;
            };
            if uuid::Uuid::parse_str(id).is_err()
                || steering::reply_path(&self.spec.mailbox, id).exists()
                || self.pending.values().any(|(request, _)| request.id == id)
            {
                continue;
            }
            let request = match steering::read_json::<SteeringRequest>(&path) {
                Ok(request) if request.id == id => request,
                _ => {
                    steering::write_json(
                        &steering::reply_path(&self.spec.mailbox, id),
                        &SteeringReply {
                            accepted: false,
                            message: "Invalid steering request; nothing was sent".into(),
                        },
                    )?;
                    continue;
                }
            };
            let result = self.deliver(&request);
            match result {
                Ok(rpc_id) => {
                    self.pending.insert(rpc_id, (request, Instant::now()));
                }
                Err(error) => self.receipt(&request, false, format!("{error:#}"))?,
            }
        }
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, (_, start))| start.elapsed() >= RPC_TIMEOUT)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some((request, _)) = self.pending.remove(&id) {
                self.receipt(&request, false, "Delivery is unconfirmed: Codex did not reply. Check the agent output before retrying".into())?;
            }
        }
        Ok(())
    }

    fn deliver(&mut self, request: &SteeringRequest) -> Result<u64> {
        anyhow::ensure!(
            self.target.as_ref() == Some(&request.target),
            "The active run or turn changed; the message was not sent"
        );
        anyhow::ensure!(
            !request.text.trim().is_empty() && request.text.len() <= steering::MAX_MESSAGE_BYTES,
            "Steering text must contain 1–65536 bytes"
        );
        let store = open_agent_store_at(&self.spec.state_dir)?;
        let root = steering::project_root(&store, self.spec.project_id)?;
        let _lock = acquire_board_mutation_lock(&get_tasks_dir(&root))?;
        steering::validate_target(&store, &request.target)?;
        self.send(
            "turn/steer",
            json!({"threadId": self.thread_id,
            "expectedTurnId": request.target.turn_id, "clientUserMessageId": request.id,
            "input": [{"type": "text", "text": request.text}]}),
        )
        .context("Delivery is unconfirmed: the Codex connection failed while sending")
    }

    fn close_mailbox(&mut self) {
        self.target = None;
        let _ = fs::remove_file(self.spec.mailbox.join("active.json"));
        for (_, (request, _)) in self.pending.drain().collect::<Vec<_>>() {
            let _ = self.receipt(&request, false,
                "Delivery is unconfirmed: the Codex connection ended before acknowledging it. Check the agent output before retrying".into());
        }
        let _ = self.mailbox(); // Reject queued messages; never send after shutdown.
    }
}

#[cfg(test)]
mod tests;
