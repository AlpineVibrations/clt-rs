//! Per-run mailbox between the TUI and the process owning Codex's stdio.
//! Receipts mean turn/steer succeeded, never merely that a file was queued.
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    agent::{AgentSessionControlState, TursoAgentStore, open_agent_store_at},
    session_control::{codex_session_for_task, collect_codex_session_tasks_in_board},
    task::{
        TaskEntry, TaskStatus, acquire_board_mutation_lock, get_tasks_dir, task_content_is_manual,
        task_entry_is_stopped,
    },
};

pub(crate) const MAX_MESSAGE_BYTES: usize = 64 * 1024;
// A JSON-escaped control byte can occupy six bytes, plus the target metadata.
const MAX_ENVELOPE_BYTES: u64 = (MAX_MESSAGE_BYTES * 6 + 16 * 1024) as u64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SteeringTarget {
    pub project_id: i64,
    pub session_id: String,
    pub thread_id: String,
    pub run_token: String,
    pub child_pid: u32,
    pub turn_id: String,
    pub mailbox: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SteeringRequest {
    pub id: String,
    pub target: SteeringTarget,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SteeringReply {
    pub accepted: bool,
    pub message: String,
}

pub(crate) fn mailbox_for_log(stderr_path: &Path) -> PathBuf {
    stderr_path.with_extension("steering")
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let directory = path
        .parent()
        .context("Steering file has no parent directory")?;
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let file = fs::File::open(path)?;
    anyhow::ensure!(
        file.metadata()?.len() <= MAX_ENVELOPE_BYTES,
        "Steering message is too large"
    );
    Ok(serde_json::from_reader(file.take(MAX_ENVELOPE_BYTES))?)
}

pub(crate) fn target_for_task(
    state_dir: &Path,
    root: &Path,
    status: TaskStatus,
    task: &TaskEntry,
) -> Result<Option<SteeringTarget>> {
    if status != TaskStatus::Doing
        || task_entry_is_stopped(task)
        || task_content_is_manual(&task.content)
    {
        return Ok(None);
    }
    let Some(session_id) = codex_session_for_task(task) else {
        return Ok(None);
    };
    let store = open_agent_store_at(state_dir)?;
    let root = fs::canonicalize(root)?;
    let _lock = acquire_board_mutation_lock(&get_tasks_dir(&root))?;
    let Some(project) = store
        .list_projects_blocking()?
        .into_iter()
        .find(|p| p.path == root)
    else {
        return Ok(None);
    };
    let Some(control) = store.session_control_blocking(project.id, &session_id)? else {
        return Ok(None);
    };
    if control.state != AgentSessionControlState::Running || control.child_pid.is_none() {
        return Ok(None);
    }
    let log = control
        .stderr_path
        .as_deref()
        .context("This run has no steering endpoint")?;
    let mailbox = mailbox_for_log(Path::new(log));
    let target: SteeringTarget = read_json(&mailbox.join("active.json")).context(
        "This run is not accepting steering yet. Runs launched before the upgrade use i/c for interaction",
    )?;
    anyhow::ensure!(
        target.project_id == project.id
            && target.session_id == session_id
            && target.mailbox == mailbox,
        "The selected session's steering endpoint changed"
    );
    validate_target(&store, &target)?;
    Ok(Some(target))
}

/// Recheck both the saved process generation and the unique Doing task. The
/// caller holds the board lock through publishing or sending its request.
pub(crate) fn validate_target(store: &TursoAgentStore, target: &SteeringTarget) -> Result<()> {
    let control = store
        .session_control_blocking(target.project_id, &target.session_id)?
        .context("The selected Codex session has ended")?;
    anyhow::ensure!(
        control.state == AgentSessionControlState::Running
            && control.child_pid == Some(target.child_pid)
            && control.run_token.as_deref() == Some(&target.run_token)
            && control.interactive_holder.is_none()
            && control.interactive_launch_token.is_none()
            && control
                .stderr_path
                .as_deref()
                .map(Path::new)
                .map(mailbox_for_log)
                .as_ref()
                == Some(&target.mailbox),
        "The selected Codex run stopped or changed; the message was not sent"
    );
    let root = project_root(store, target.project_id)?;
    let mut tasks = Vec::new();
    collect_codex_session_tasks_in_board(&get_tasks_dir(&root), &target.session_id, &mut tasks)?;
    anyhow::ensure!(
        tasks.len() == 1
            && tasks[0].0 == TaskStatus::Doing
            && !task_entry_is_stopped(&tasks[0].1)
            && !task_content_is_manual(&tasks[0].1.content),
        "The selected session no longer belongs to one running Doing task"
    );
    Ok(())
}

pub(crate) fn project_root(store: &TursoAgentStore, project_id: i64) -> Result<PathBuf> {
    store
        .list_projects_blocking()?
        .into_iter()
        .find(|p| p.id == project_id)
        .map(|p| p.path)
        .context("The selected project is no longer registered")
}

pub(crate) fn request_message(
    state_dir: &Path,
    target: &SteeringTarget,
    text: &str,
) -> Result<SteeringRequest> {
    anyhow::ensure!(!text.trim().is_empty(), "Steering text cannot be empty");
    anyhow::ensure!(
        text.len() <= MAX_MESSAGE_BYTES,
        "Steering text exceeds 64 KiB"
    );
    let store = open_agent_store_at(state_dir)?;
    let root = project_root(&store, target.project_id)?;
    let _lock = acquire_board_mutation_lock(&get_tasks_dir(&root))?;
    validate_target(&store, target)?;
    let current: SteeringTarget = read_json(&target.mailbox.join("active.json"))
        .context("The running agent is no longer accepting steering")?;
    anyhow::ensure!(
        &current == target,
        "The active turn changed; reopen the steering prompt"
    );
    let request = SteeringRequest {
        id: uuid::Uuid::now_v7().to_string(),
        target: target.clone(),
        text: text.to_string(),
    };
    write_json(
        &request_path(&request.target.mailbox, &request.id),
        &request,
    )?;
    Ok(request)
}

pub(crate) fn request_path(mailbox: &Path, id: &str) -> PathBuf {
    mailbox.join(format!("{id}.request.json"))
}

pub(crate) fn reply_path(mailbox: &Path, id: &str) -> PathBuf {
    mailbox.join(format!("{id}.reply.json"))
}

pub(crate) fn wait_for_reply(request: &SteeringRequest, timeout: Duration) -> Result<String> {
    let path = reply_path(&request.target.mailbox, &request.id);
    let started = Instant::now();
    loop {
        match read_json::<SteeringReply>(&path) {
            Ok(reply) => {
                anyhow::ensure!(reply.accepted, "{}", reply.message);
                return Ok(reply.message);
            }
            Err(error) if !path.exists() => {
                if started.elapsed() >= timeout {
                    return Err(error).context("Delivery is unconfirmed. Check the agent output before retrying; the request may have been accepted");
                }
            }
            Err(error) => return Err(error).context("Unable to read the steering receipt"),
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
pub(crate) mod tests;
