//! Read the selected task's saved progress without controlling its session.
use std::{fs, path::Path};

use anyhow::Result;

use crate::{
    agent::{AGENT_DB_FILE, open_agent_store_at},
    runner::app_server::progress::{ProgressSnapshot, short_text},
    session_control::{codex_session_for_task, steering},
    task::{TaskEntry, task_display_text, task_entry_is_stopped},
};

pub(crate) fn task_progress_at(state_dir: &Path, root: &Path, task: &TaskEntry) -> Result<String> {
    let title = short_text(&task_display_text(task), 100);
    let mut summary = format!("{title}\n");
    let Some(session) = codex_session_for_task(task) else {
        summary.push_str(if task_entry_is_stopped(task) {
            "Stopped before starting."
        } else {
            "No agent session is linked to this task yet."
        });
        return Ok(summary);
    };
    if !state_dir.join(AGENT_DB_FILE).is_file() {
        summary.push_str("No saved agent state is available.");
        return Ok(summary);
    }
    let root = fs::canonicalize(root)?;
    let store = open_agent_store_at(state_dir)?;
    let Some(project) = store
        .list_projects_blocking()?
        .into_iter()
        .find(|p| p.path == root)
    else {
        summary.push_str("This project is not registered with the agent.");
        return Ok(summary);
    };
    let control = store.session_control_blocking(project.id, &session)?;
    let journal = store.git_finalization_blocking(project.id, &session)?;
    let state = if task_entry_is_stopped(task) {
        "stopped".to_string()
    } else {
        control
            .as_ref()
            .map(|c| c.state.database_value().replace('_', " "))
            .unwrap_or_else(|| "idle".to_string())
    };
    summary.push_str(&format!("Agent: {state}"));
    if let Some(journal) = journal {
        summary.push_str(&format!(" | Git: {}", journal.state.status_label()));
    }
    let snapshot = control.as_ref().and_then(|control| {
        let mailbox = steering::mailbox_for_log(Path::new(control.stderr_path.as_deref()?));
        let snapshot: ProgressSnapshot =
            steering::read_json(&mailbox.join("progress.json")).ok()?;
        (snapshot.target.project_id == project.id
            && snapshot.target.session_id == session
            && snapshot.target.run_token.as_str() == control.run_token.as_deref()?
            && snapshot.target.mailbox == mailbox
            && control
                .child_pid
                .is_none_or(|pid| pid == snapshot.target.child_pid))
        .then_some(snapshot)
    });
    if let Some(snapshot) = snapshot {
        summary.push_str(&format!(
            "\nRecorded: {}\nLatest activity: {}",
            snapshot.recorded_at,
            short_text(&snapshot.activity, 180)
        ));
        if let Some(update) = snapshot.update {
            summary.push_str(&format!("\nLatest update: {}", short_text(&update, 400)));
        }
    } else {
        summary.push_str(
            "\nNo progress details recorded for this run yet. Press l on the board for logs.",
        );
    }
    Ok(summary)
}

#[cfg(test)]
mod tests;
