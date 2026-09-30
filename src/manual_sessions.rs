//! Directly opened Codex sessions reserve tasks in the board, independently of
//! daemon process lifetimes. Claim publication and scheduler lease acquisition
//! share the project board lock, so exactly one side can take ownership.
use std::{fs, io::ErrorKind, path::Path};

use anyhow::{Context, Result};

use crate::{
    agent::{AGENT_DB_FILE, AgentSessionControlState, agent_state_dir, open_agent_store_at},
    application::move_task_in_board_after_lock,
    runner::{automated_agent_child_context, canonicalize_existing_path},
    session_control::collect_codex_session_tasks_in_board,
    task::{
        TaskBoard, TaskStatus, acquire_board_mutation_lock, board_has_manual_task,
        board_mutation_lock_path, codex_session_id_is_uuid, codex_session_markers_in_task_content,
        ensure_existing_board, get_tasks_dir, insert_task_content, parse_one_based_task_index,
        read_task_entries, recoverable_codex_session_id_from_task_content, task_content_is_manual,
        task_content_with_manual_session, task_content_without_manual_marker,
        task_content_without_stop_marker,
    },
};

fn current_session_id(session: Option<&str>) -> Result<String> {
    let session = session
        .map(str::to_string)
        .or_else(|| std::env::var("CODEX_THREAD_ID").ok())
        .context("A current Codex session ID is required; pass --session or set CODEX_THREAD_ID")?;
    anyhow::ensure!(
        codex_session_id_is_uuid(&session),
        "Use the exact current Codex session UUID"
    );
    Ok(session)
}

fn with_manual_board<T>(
    root: &Path,
    session: &str,
    operation: impl FnOnce(&Path, &Path) -> Result<T>,
) -> Result<T> {
    anyhow::ensure!(
        automated_agent_child_context()?.is_none(),
        "Manual claims are for directly opened Codex sessions, outside a CLT agent run"
    );
    let root = canonicalize_existing_path(root)?;
    ensure_existing_board(&root)?;
    let state_dir = agent_state_dir()?;
    let store = match fs::metadata(state_dir.join(AGENT_DB_FILE)) {
        Ok(_) => Some(open_agent_store_at(&state_dir)?),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("Unable to verify existing CLT ownership"),
    };
    let project = store
        .as_ref()
        .map(|store| store.list_projects_blocking())
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .filter(|project| root.starts_with(&project.path))
        .max_by_key(|project| project.path.as_os_str().len());
    let board_dir = get_tasks_dir(&root);
    let project_board = project
        .as_ref()
        .map(|project| get_tasks_dir(&project.path))
        .unwrap_or_else(|| board_dir.clone());
    let _project_lock = acquire_board_mutation_lock(&project_board)?;
    let _nested_lock = (board_mutation_lock_path(&board_dir)?
        != board_mutation_lock_path(&project_board)?)
    .then(|| acquire_board_mutation_lock(&board_dir))
    .transpose()?;
    if let (Some(store), Some(project)) = (&store, &project) {
        anyhow::ensure!(
            store.lease_for_project_blocking(project.id)?.is_none()
                && !store
                    .list_active_workers_blocking()?
                    .iter()
                    .any(|worker| worker.project_id == project.id)
                && !store
                    .session_controls_for_project_blocking(project.id)?
                    .iter()
                    .any(|control| control.state != AgentSessionControlState::Stopped
                        || control.child_pid.is_some()
                        || control.interactive_holder.is_some()
                        || control.interactive_launch_token.is_some())
                && store
                    .git_launch_state_for_project_blocking(project.id)?
                    .is_none()
                && store
                    .list_pending_git_finalizations_blocking(Some(project.id))?
                    .is_empty(),
            "The project still has a CLT owner or unfinished Git launch/finalization; stop or finish that run before claiming manual work"
        );
        anyhow::ensure!(
            store.planning_session_can_start_blocking(project.id, session)?,
            "This conversation already has an automated run; use its existing CLT session controls"
        );
    }
    operation(&board_dir, &project_board)
}

pub(super) fn start_manual_task(
    root: &Path,
    description: &str,
    session: Option<&str>,
) -> Result<()> {
    let session = current_session_id(session)?;
    anyhow::ensure!(
        !description.trim().is_empty(),
        "Task description cannot be empty"
    );
    anyhow::ensure!(
        codex_session_markers_in_task_content(description).is_empty(),
        "Pass the conversation ID with --session; do not embed a codex: marker in the description"
    );
    with_manual_board(root, &session, |board_dir, project_board| {
        anyhow::ensure!(
            !board_has_manual_task(project_board)?,
            "The project already has a manual task; finish or hand it off first"
        );
        let mut linked = Vec::new();
        collect_codex_session_tasks_in_board(project_board, &session, &mut linked)?;
        anyhow::ensure!(
            linked.is_empty(),
            "This Codex session already belongs to a task; use clt claim for that task"
        );
        let content = task_content_with_manual_session(description, &session);
        insert_task_content(board_dir, TaskStatus::Doing, None, &content)
    })
}

pub(super) fn claim_manual_task(
    root: &Path,
    status: TaskStatus,
    index: &str,
    session: Option<&str>,
) -> Result<()> {
    let session = current_session_id(session)?;
    let index = parse_one_based_task_index(index)?;
    anyhow::ensure!(
        status != TaskStatus::Done,
        "Completed tasks cannot be claimed"
    );
    with_manual_board(root, &session, |board_dir, project_board| {
        let board = TaskBoard::new(board_dir);
        let entry = board.entry(status, index)?;
        anyhow::ensure!(
            !board_has_manual_task(project_board)? || task_content_is_manual(&entry.content),
            "The project already has a manual task; finish or hand it off first"
        );
        anyhow::ensure!(
            codex_session_markers_in_task_content(&entry.content)
                .iter()
                .all(|(_, _, existing)| *existing == session),
            "Task already belongs to a different Codex session; reopen that conversation first"
        );
        let mut linked = Vec::new();
        collect_codex_session_tasks_in_board(project_board, &session, &mut linked)?;
        anyhow::ensure!(
            linked.is_empty()
                || (linked.len() == 1
                    && linked[0].0 == status
                    && linked[0].1.source == entry.source
                    && linked[0].1.content == entry.content),
            "This Codex session already belongs to another task"
        );
        let content = task_content_with_manual_session(&entry.content, &session);
        // Claim first: a failed or interrupted move remains reserved in place.
        board.write_entry_content(status, &entry, &content)?;
        if status != TaskStatus::Doing {
            move_task_in_board_after_lock(board_dir, status, TaskStatus::Doing, index)?;
        }
        Ok(())
    })
}

pub(super) fn handoff_manual_task(root: &Path, status: TaskStatus, index: &str) -> Result<()> {
    anyhow::ensure!(
        status != TaskStatus::Done,
        "Completed tasks cannot be handed off"
    );
    let index = parse_one_based_task_index(index)?;
    // This read supplies identity only. Re-read and compare after taking the lock.
    ensure_existing_board(root)?;
    let before = TaskBoard::new(get_tasks_dir(root)).entry(status, index)?;
    let session = recoverable_codex_session_id_from_task_content(&before.content)
        .context("Manual handoff requires an attached Codex conversation")?
        .to_string();
    with_manual_board(root, &session, |board_dir, project_board| {
        let board = TaskBoard::new(board_dir);
        let entry = board.entry(status, index)?;
        anyhow::ensure!(
            entry.source == before.source && entry.content == before.content,
            "Task changed before handoff; list tasks and retry"
        );
        anyhow::ensure!(
            task_content_is_manual(&entry.content),
            "Task is not manually claimed"
        );
        let mut linked = Vec::new();
        collect_codex_session_tasks_in_board(project_board, &session, &mut linked)?;
        anyhow::ensure!(
            linked.len() == 1,
            "Conversation belongs to multiple tasks; resolve duplicate links before handoff"
        );
        // Publish Todo while it is still fenced. Only the final content write
        // releases it; failures retain the claim and are safe to retry in Todo.
        if status != TaskStatus::Todo {
            move_task_in_board_after_lock(board_dir, status, TaskStatus::Todo, index)?;
        }
        let todo = read_task_entries(board_dir, TaskStatus::Todo)?
            .into_iter()
            .find(|task| {
                recoverable_codex_session_id_from_task_content(&task.content)
                    == Some(session.as_str())
            })
            .context("Handed-off task disappeared before releasing its claim")?;
        let content =
            task_content_without_manual_marker(task_content_without_stop_marker(&todo.content));
        board.write_entry_content(TaskStatus::Todo, &todo, &content)
    })
}
