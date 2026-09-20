//! Create a durable planning conversation without starting an automated turn.
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::{
    agent::{AgentProject, AgentSessionControlState, TursoAgentStore, open_agent_store_at},
    platform::{configure_agent_child_command, stop_agent_child_process},
    runner::{agent_codex_command, configure_agent_provider_credential},
    session_control::{InteractiveAgentLease, codex_session_for_task},
    task::{
        TaskBoard, TaskEntry, TaskStatus, acquire_board_mutation_lock, read_task_entries,
        task_content_with_codex_session,
    },
};

pub(crate) struct PreparedPlanningSession {
    pub(crate) session_id: String,
    pub(crate) lease: InteractiveAgentLease,
}

pub(crate) fn prepare_todo_planning_session(
    state_dir: &Path,
    project: &AgentProject,
    board_dir: &Path,
    selected: &TaskEntry,
) -> Result<PreparedPlanningSession> {
    prepare_todo_planning_session_with(state_dir, project, board_dir, selected, |store| {
        let target = store.resolve_model_target_blocking(project)?;
        let reasoning = match (
            project.codex_reasoning_effort.clone(),
            target.provider_id.as_deref(),
            target.model_id.as_deref(),
        ) {
            (Some(effort), _, _) => Some(effort),
            (None, Some(provider), Some(model)) => {
                store.model_target_reasoning_blocking(provider, model)?
            }
            _ => None,
        };
        let mut config = json!({"features.fast_mode": project.codex_fast_enabled});
        if let Some(reasoning) = reasoning {
            config["model_reasoning_effort"] = json!(reasoning);
        }
        if project.codex_fast_enabled {
            config["service_tier"] = json!("fast");
        }
        let params = json!({
            "cwd": project.path,
            "model": target.model_id,
            "modelProvider": target.provider_id,
            "config": config,
            "ephemeral": false,
            "approvalPolicy": "on-request",
            "sandbox": "workspace-write",
        });
        let prompt = format!(
            "I opened this CLT Todo for planning and discussion. Help me refine it and update its task content if I ask. Keep it in Todo and do not begin implementation or an automated task run unless I explicitly request that.\n\nTask board: {}\nSelected task:\n{}",
            board_dir.display(),
            selected.content
        );
        let mut command = Command::new(agent_codex_command());
        command
            .arg("app-server")
            .arg("--listen")
            .arg("stdio://")
            .current_dir(&project.path);
        if let Some(provider) = store.resolve_credential_provider_blocking(project)? {
            configure_agent_provider_credential(&mut command, store, &provider)?;
        }
        create_planning_thread(&mut command, params, prompt, Duration::from_secs(20))
    })
}

fn prepare_todo_planning_session_with(
    state_dir: &Path,
    project: &AgentProject,
    board_dir: &Path,
    selected: &TaskEntry,
    create: impl FnOnce(&TursoAgentStore) -> Result<String>,
) -> Result<PreparedPlanningSession> {
    let holder = InteractiveAgentLease::holder_for_stopped_session();
    let lease = InteractiveAgentLease::try_acquire_with_holder_at(state_dir, project.id, &holder, 60)?
        .context("This project is busy; wait for its current run to finish before creating a planning session")?;
    let store = open_agent_store_at(state_dir)?;
    anyhow::ensure!(
        store
            .session_controls_for_project_blocking(project.id)?
            .iter()
            .all(|control| control.state == AgentSessionControlState::Stopped),
        "This project already has an active Codex session"
    );
    {
        let _lock = acquire_board_mutation_lock(board_dir)?;
        revalidate_todo(board_dir, selected)?;
    }
    let session_id = create(&store)?;
    anyhow::ensure!(
        valid_session_id(&session_id),
        "Codex returned an invalid session ID"
    );
    let _lock = acquire_board_mutation_lock(board_dir)?;
    let current = revalidate_todo(board_dir, selected)?;
    anyhow::ensure!(
        store.register_planning_session_blocking(project.id, &session_id, &holder)?,
        "The project reservation changed before the planning session could be linked"
    );
    TaskBoard::new(board_dir).write_entry_content(
        TaskStatus::Todo,
        &current,
        &task_content_with_codex_session(&current.content, &session_id),
    )?;
    Ok(PreparedPlanningSession { session_id, lease })
}

fn revalidate_todo(board_dir: &Path, selected: &TaskEntry) -> Result<TaskEntry> {
    read_task_entries(board_dir, TaskStatus::Todo)?
        .into_iter()
        .find(|current| {
            current.source == selected.source
                && current.content == selected.content
                && codex_session_for_task(current).is_none()
        })
        .context("The selected Todo changed or already has a Codex session; select it again")
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn create_planning_thread(
    command: &mut Command,
    params: Value,
    prompt: String,
    timeout: Duration,
) -> Result<String> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    configure_agent_child_command(command);
    let mut child = command
        .spawn()
        .context("Unable to start Codex app-server for planning")?;
    let input = child
        .stdin
        .take()
        .context("Codex app-server stdin is unavailable")?;
    let output = child
        .stdout
        .take()
        .context("Codex app-server stdout is unavailable")?;
    let (sender, receiver) = mpsc::channel();
    let reader = thread::Builder::new()
        .name("clt-planning-session".into())
        .spawn(move || {
            let result = planning_protocol(input, BufReader::new(output), params, &prompt);
            let _ = sender.send(result);
        });
    let result = match reader {
        Ok(_) => receiver
            .recv_timeout(timeout)
            .context("Codex planning-session creation timed out or disconnected")
            .and_then(|result| result),
        Err(error) => Err(error).context("Unable to start the Codex planning protocol"),
    };
    // No model turn is launched. Reap the local server and any startup children on
    // success, protocol failure, or timeout before releasing the project fence.
    stop_agent_child_process(&mut child).context("Unable to stop the planning app-server")?;
    result
}

fn planning_protocol(
    mut input: impl Write,
    mut output: impl BufRead,
    params: Value,
    prompt: &str,
) -> Result<String> {
    rpc(
        &mut input,
        &mut output,
        1,
        "initialize",
        json!({
            "clientInfo": {"name": "clt", "version": env!("CARGO_PKG_VERSION")}
        }),
    )?;
    writeln!(input, "{{\"method\":\"initialized\"}}")?;
    input.flush()?;
    let result = rpc(&mut input, &mut output, 2, "thread/start", params)?;
    let thread_id = result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .context("Codex did not return a thread ID")?;
    // Older Codex releases expose only id; newer releases distinguish sessionId.
    let session_id = result
        .pointer("/thread/sessionId")
        .and_then(Value::as_str)
        .unwrap_or(thread_id);
    anyhow::ensure!(
        valid_session_id(session_id),
        "Codex returned an invalid session ID"
    );
    // An empty thread/start is not durable. Injecting context persists the rollout
    // without running a model turn, so interactive resume can open this exact ID.
    rpc(
        &mut input,
        &mut output,
        3,
        "thread/inject_items",
        json!({
            "threadId": thread_id,
            "items": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": prompt}]}]
        }),
    )?;
    Ok(session_id.to_string())
}

fn rpc(
    input: &mut impl Write,
    output: &mut impl BufRead,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value> {
    serde_json::to_writer(
        &mut *input,
        &json!({"id": id, "method": method, "params": params}),
    )?;
    writeln!(input)?;
    input.flush()?;
    loop {
        let mut line = String::new();
        anyhow::ensure!(
            output.read_line(&mut line)? > 0,
            "Codex app-server exited during {method}"
        );
        let message: Value =
            serde_json::from_str(&line).context("Invalid Codex app-server response")?;
        if message.get("id") != Some(&json!(id)) {
            continue;
        }
        if let Some(error) = message.get("error") {
            anyhow::bail!("Codex {method} failed: {error}");
        }
        return message
            .get("result")
            .cloned()
            .with_context(|| format!("Codex {method} returned no result"));
    }
}

#[cfg(test)]
mod tests;
