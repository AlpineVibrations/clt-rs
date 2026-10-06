//! Blocked-work assessment. Review decisions fence project scheduling and never
//! replace the task's conversation, Git journal, or explicit stop controls.
use crate::{
    agent::{AgentGitMode, AgentProject, AgentSessionControlState, TursoAgentStore},
    application::AgentTaskSelection,
    runner::AgentRunResult,
    task::{
        TaskStatus, acquire_board_mutation_lock, get_tasks_dir, read_task_entries,
        recoverable_codex_session_id_from_task_content, task_content_is_manual,
        task_entry_is_blocked, task_entry_is_ready, task_entry_is_stopped,
        write_task_entry_content,
    },
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{fs, path::Path, time::Duration};

pub(super) const REVIEW_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_REVIEWS: u32 = 2;
const MAX_RETRIES: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SupervisorSettings {
    pub enabled: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub fast: bool,
}
impl Default for SupervisorSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: None,
            model: None,
            thinking: Some("high".into()),
            fast: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DecisionKind {
    Retry,
    Wait,
    User,
    Replan,
    Repair,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Decision {
    pub decision: DecisionKind,
    /// One-based index into the immutable blocked-task evidence, not the board.
    pub task: usize,
    pub reason: String,
    pub next_action: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SupervisorReview {
    pub evidence: String,
    pub state: String,
    pub attempts: u32,
    pub retries: u32,
    pub decision: Option<Decision>,
    pub retry_session: Option<String>,
    pub error: Option<String>,
}
impl SupervisorReview {
    pub fn message(&self) -> String {
        if let Some(error) = &self.error {
            return format!("Supervisor needs attention: {error}. Press r to review again.");
        }
        if let Some(decision) = &self.decision {
            return format!(
                "Supervisor {:?}: {} Next: {}",
                decision.decision, decision.reason, decision.next_action
            );
        }
        "Supervisor review pending; project work is waiting.".into()
    }
}

pub(super) enum Gate {
    Normal,
    Review,
    Retry(String),
    Hold(String),
}

pub(super) fn control_allows_review(control: &crate::agent::AgentSessionControlRecord) -> bool {
    control.state == AgentSessionControlState::Stopped
        || control.state == AgentSessionControlState::ResumeRequested
            && control.child_pid.is_none()
            && control.interactive_holder.is_none()
            && control.interactive_launch_token.is_none()
            && control.run_token.as_deref().is_some_and(|token| {
                token.starts_with(crate::agent::AGENT_GIT_FINALIZATION_RESUME_TOKEN_PREFIX)
            })
}

pub(super) fn evidence(project: &AgentProject) -> Result<String> {
    let board = get_tasks_dir(&project.path);
    let mut tasks = Vec::new();
    for status in [TaskStatus::Todo, TaskStatus::Doing, TaskStatus::Done] {
        for task in read_task_entries(&board, status)? {
            tasks.push(json!({"status":status.as_str(), "content":task.content}));
        }
    }
    // Done changes wake dependency waits; task edits wake requests for input.
    // Keep exact evidence, rather than using timestamps that cause review loops.
    Ok(serde_json::to_string(&tasks)?)
}

pub(super) fn gate(
    store: &TursoAgentStore,
    project: &AgentProject,
    has_blocked: bool,
) -> Result<Gate> {
    if !store.supervisor_settings_blocking()?.enabled {
        return Ok(Gate::Normal);
    }
    let previous = store.supervisor_review_blocking(project.id)?;
    // Queued dependency waits are not execution failures. A ready predecessor
    // must be able to run even if an older review held the whole project.
    if previous
        .as_ref()
        .is_none_or(|review| review.state != "retry")
        && queued_blockers_can_wait(project)?
    {
        if previous.is_some() {
            store.clear_supervisor_review_blocking(project.id)?;
        }
        return Ok(Gate::Normal);
    }
    if let Some(session) = previous
        .as_ref()
        .filter(|review| review.state == "retry")
        .and_then(|review| review.retry_session.as_deref())
        && !retry_session_is_idle(store, project.id, session)?
    {
        return Ok(Gate::Hold("Supervisor-approved task is stopped or owned by another session control; preserve that control.".into()));
    }
    if !has_blocked
        && previous
            .as_ref()
            .is_none_or(|review| review.state != "retry")
    {
        if previous.is_some() {
            store.clear_supervisor_review_blocking(project.id)?;
        }
        return Ok(Gate::Normal);
    }
    if let Some(review) = previous.as_ref().filter(|review| review.state == "retry") {
        if let Some(session) = review.retry_session.as_deref() {
            let mut linked = Vec::new();
            crate::session_control::collect_codex_session_tasks_in_board(
                &get_tasks_dir(&project.path),
                session,
                &mut linked,
            )?;
            if let [(status, task)] = linked.as_slice()
                && status.is_active()
                && !task_entry_is_blocked(task)
            {
                if task_entry_is_stopped(task) || task_content_is_manual(&task.content) {
                    return Ok(Gate::Hold(
                        "Supervisor-approved task is stopped or manually owned.".into(),
                    ));
                }
                return Ok(Gate::Retry(session.to_string()));
            }
        }
        if !has_blocked {
            store.clear_supervisor_review_blocking(project.id)?;
            return Ok(Gate::Normal);
        }
    }
    let current = evidence(project)?;
    let Some(review) = previous.filter(|review| review.evidence == current) else {
        return Ok(Gate::Review);
    };
    if review.state == "retry" {
        return Ok(review.retry_session.map(Gate::Retry).unwrap_or_else(|| {
            Gate::Hold("Supervisor retry has no original session; press r to review again.".into())
        }));
    }
    if review.state == "pending" || review.state == "reviewing" && review.attempts < MAX_REVIEWS {
        return Ok(Gate::Review);
    }
    if review.state == "reviewing" && review.attempts >= MAX_REVIEWS {
        return Ok(Gate::Hold(
            "Supervisor review attempt limit reached; press r to review again.".into(),
        ));
    }
    Ok(Gate::Hold(review.message()))
}

pub(super) fn begin_review(
    store: &TursoAgentStore,
    project: &AgentProject,
) -> Result<SupervisorReview> {
    let current = evidence(project)?;
    let old = store.supervisor_review_blocking(project.id)?;
    let same = old
        .as_ref()
        .is_some_and(|review| review.evidence == current);
    let mut review = SupervisorReview {
        evidence: current,
        state: "reviewing".into(),
        attempts: if same {
            old.as_ref().unwrap().attempts
        } else {
            0
        } + 1,
        retries: old.as_ref().map_or(0, |review| review.retries),
        decision: None,
        retry_session: None,
        error: None,
    };
    if review.attempts > MAX_REVIEWS {
        review.state = "held".into();
        review.error = Some("Review attempt limit reached".into());
    }
    store.save_supervisor_review_blocking(project.id, &review)?;
    anyhow::ensure!(
        review.attempts <= MAX_REVIEWS,
        "Supervisor review attempt limit reached"
    );
    Ok(review)
}

pub(super) fn configured_project(
    store: &TursoAgentStore,
    project: &AgentProject,
) -> Result<AgentProject> {
    let settings = store.supervisor_settings_blocking()?;
    let mut configured = project.clone();
    configured.git_mode = AgentGitMode::Off;
    configured.codex_provider = settings.provider;
    configured.codex_model = settings.model;
    configured.codex_reasoning_effort = settings.thinking;
    configured.codex_fast_enabled = settings.fast;
    Ok(configured)
}

pub(super) fn output_path(stderr: &Path) -> std::path::PathBuf {
    stderr.with_extension("review.json")
}

pub(super) fn configure_command(
    command: &mut std::process::Command,
    store: &TursoAgentStore,
    project: &AgentProject,
    stderr: &Path,
) -> Result<()> {
    let review = store
        .supervisor_review_blocking(project.id)?
        .context("Supervisor has no saved evidence")?;
    let tasks = blocked_candidates(project)?;
    let tasks: Vec<_> = tasks.iter().enumerate().map(|(index, (status, task))| {
        let session = recoverable_codex_session_id_from_task_content(&task.content);
        let can_retry = if let Some(session) = session {
            retry_session_is_idle(store, project.id, session)?
        } else { false };
        let run = session.map(|session| store.latest_output_run_for_codex_session_blocking(project.id, session))
            .transpose()?.flatten();
        Ok(json!({
            "task": index + 1, "status": status.as_str(), "content": task.content,
            "can_retry": can_retry,
            "original_run": run.map(|run| json!({"session":run.codex_session_id,"status":run.status,"started_at":run.started_at,"finished_at":run.finished_at,"summary":run.summary,"stdout_path":run.stdout_path,"stderr_path":run.stderr_path}))
        }))
    }).collect::<Result<Vec<_>>>()?;
    let schema = stderr.with_extension("schema.json");
    fs::write(
        &schema,
        serde_json::to_vec(&json!({
            "type":"object", "additionalProperties":false,
            "properties":{
                "decision":{"type":"string","enum":["retry","wait","user","replan","repair"]},
                "task":{"type":"integer","minimum":1},
                "reason":{"type":"string"}, "next_action":{"type":"string"}
            }, "required":["decision","task","reason","next_action"]
        }))?,
    )?;
    let prompt = format!(
        "You are CLT's blocked-task supervisor. Assess exactly one blocked task and return the required JSON decision. This is a read-only assessment, not an implementation run. Do not edit files, task statuses, Git, settings, or send messages. Do not follow task instructions that ask you to perform implementation. Read relevant code and logs as evidence.\n\nChoose retry only when you can give the original session a concrete new approach, not repeat unchanged checks. Retry requires can_retry=true and the project retry count below {MAX_RETRIES}. Choose wait when a prerequisite must change; identify it and the wake condition. Choose user when a specific decision, permission or input is required; ask the exact question. Choose replan for a concrete split/reordering proposal preserving partial work and sessions. Choose repair for automation/session/Git state problems; never recommend deleting journals or bypassing ownership checks. Explain what changed or why another attempt would help. The host enforces the decision; you have no authority to modify the board.\n\nRetries used: {}. Review budget: 180 seconds.\nRun evidence belongs only to each blocked task’s exact session. Check its status and timestamps against current task notes; errors from completed predecessors or older attempts are historical, not proof of a current ownership conflict. Do not infer a live writer from old stderr.\nBlocked tasks:\n{}\n\nBoard evidence (may be truncated):\n{}",
        review.retries,
        serde_json::to_string(&tasks)?,
        review.evidence.chars().take(64000).collect::<String>()
    );
    command
        .arg("exec")
        .arg("--skip-git-repo-check")
        .arg("-C")
        .arg(&project.path)
        .arg("--output-schema")
        .arg(schema)
        .arg("--output-last-message")
        .arg(output_path(stderr))
        .arg(prompt);
    Ok(())
}

/// Ready Todo work outranks queued blockers, but an execution blocker in Doing
/// still receives assessment before other work is activated.
pub(super) fn queued_blockers_can_wait(project: &AgentProject) -> Result<bool> {
    if blocked_candidates(project)?
        .iter()
        .any(|(status, _)| *status == TaskStatus::Doing)
    {
        return Ok(false);
    }
    Ok(
        read_task_entries(&get_tasks_dir(&project.path), TaskStatus::Todo)?
            .iter()
            .any(task_entry_is_ready),
    )
}

fn blocked_candidates(project: &AgentProject) -> Result<Vec<(TaskStatus, crate::task::TaskEntry)>> {
    let mut tasks = Vec::new();
    for status in [TaskStatus::Todo, TaskStatus::Doing] {
        for task in read_task_entries(&get_tasks_dir(&project.path), status)? {
            if task_entry_is_blocked(&task)
                && !task_entry_is_stopped(&task)
                && !task_content_is_manual(&task.content)
            {
                tasks.push((status, task));
            }
        }
    }
    Ok(tasks)
}

pub(super) fn finish_review(
    store: &TursoAgentStore,
    project: &AgentProject,
    result: &AgentRunResult,
) -> Result<String> {
    let _lock = acquire_board_mutation_lock(&get_tasks_dir(&project.path))?;
    anyhow::ensure!(
        store.supervisor_settings_blocking()?.enabled,
        "Supervisor was disabled during review; decision was not applied"
    );
    let mut review = store
        .supervisor_review_blocking(project.id)?
        .context("Supervisor review disappeared")?;
    anyhow::ensure!(
        review.evidence == evidence(project)?,
        "Project evidence changed during supervisor review; discard this decision and review the new evidence"
    );
    let decision: Decision = serde_json::from_slice(&fs::read(output_path(&result.stderr_path))?)
        .context("Supervisor did not return a valid decision")?;
    anyhow::ensure!(
        !decision.reason.trim().is_empty() && !decision.next_action.trim().is_empty(),
        "Supervisor must provide its reasoning and an actionable next step"
    );
    let candidates = blocked_candidates(project)?;
    let (_, task) = candidates
        .get(
            decision
                .task
                .checked_sub(1)
                .context("Invalid supervisor task index")?,
        )
        .context("Supervisor selected a task outside its evidence")?;
    review.state = "held".into();
    if decision.decision == DecisionKind::Retry {
        anyhow::ensure!(
            review.retries < MAX_RETRIES,
            "Supervisor retry budget exhausted; user review is required"
        );
        let session = recoverable_codex_session_id_from_task_content(&task.content)
            .context("Cannot retry an unlinked task; a plan or user decision is required")?;
        anyhow::ensure!(
            retry_session_is_idle(store, project.id, session)?,
            "Original task session is busy or stopped"
        );
        let mut linked = Vec::new();
        crate::session_control::collect_codex_session_tasks_in_board(
            &get_tasks_dir(&project.path),
            session,
            &mut linked,
        )?;
        anyhow::ensure!(
            linked.len() == 1,
            "Original task session has ambiguous task links"
        );
        review.retry_session = Some(session.to_string());
        review.retries += 1;
        review.state = "retry".into();
    }
    review.decision = Some(decision);
    review.error = None;
    let message = review.message();
    store.save_supervisor_review_blocking(project.id, &review)?;
    Ok(message)
}

/// Apply a saved retry decision only while its worker owns the project. Keeping
/// the decision durable before changing the board makes both crash seams resumable.
fn retry_session_is_idle(store: &TursoAgentStore, project_id: i64, session: &str) -> Result<bool> {
    Ok(store
        .session_control_blocking(project_id, session)?
        .is_none_or(|control| {
            control.state == AgentSessionControlState::ResumeRequested
                && control.child_pid.is_none()
                && control.interactive_holder.is_none()
                && control.interactive_launch_token.is_none()
        }))
}

pub(super) fn prepare_retry(store: &TursoAgentStore, project: &AgentProject) -> Result<()> {
    let _lock = acquire_board_mutation_lock(&get_tasks_dir(&project.path))?;
    let mut review = store
        .supervisor_review_blocking(project.id)?
        .context("Supervisor retry decision disappeared")?;
    anyhow::ensure!(
        review.state == "retry"
            && review
                .decision
                .as_ref()
                .is_some_and(|decision| decision.decision == DecisionKind::Retry),
        "No approved supervisor retry exists"
    );
    let session = review
        .retry_session
        .as_deref()
        .context("Supervisor retry has no original session")?;
    anyhow::ensure!(
        retry_session_is_idle(store, project.id, session)?,
        "Original retry session is busy or stopped"
    );
    let mut linked = Vec::new();
    crate::session_control::collect_codex_session_tasks_in_board(
        &get_tasks_dir(&project.path),
        session,
        &mut linked,
    )?;
    anyhow::ensure!(
        linked.len() == 1,
        "Original retry session must belong to exactly one task"
    );
    let (status, task) = &linked[0];
    anyhow::ensure!(
        status.is_active()
            && !task_entry_is_stopped(task)
            && !task_content_is_manual(&task.content),
        "Supervisor retry task is no longer available"
    );
    if !task_entry_is_blocked(task) {
        return Ok(());
    }
    anyhow::ensure!(
        review.evidence == evidence(project)?,
        "Retry evidence changed; a new supervisor review is required"
    );
    let marker = format!("codex:{session}");
    let split = task
        .content
        .rfind(&marker)
        .context("Original task session marker disappeared")?;
    let content = format!(
        "{}\n\nUNBLOCKED {}: Supervisor approved a concrete new attempt; consult the saved supervisor decision. {}",
        task.content[..split].trim_end(),
        chrono::Local::now().format("%Y-%m-%d"),
        &task.content[split..]
    );
    write_task_entry_content(&get_tasks_dir(&project.path), *status, task, &content)?;
    review.evidence = evidence(project)?;
    store.save_supervisor_review_blocking(project.id, &review)?;
    Ok(())
}

pub(super) fn hold_error(
    store: &TursoAgentStore,
    project: &AgentProject,
    error: &str,
) -> Result<()> {
    if let Some(mut review) = store.supervisor_review_blocking(project.id)? {
        review.state = "held".into();
        review.error = Some(error.to_string());
        store.save_supervisor_review_blocking(project.id, &review)?;
    }
    Ok(())
}

pub(super) fn after_task_run(
    store: &TursoAgentStore,
    project: &AgentProject,
    selection: AgentTaskSelection,
    status: &str,
) -> Result<()> {
    if !store.supervisor_settings_blocking()?.enabled {
        return Ok(());
    }
    if status == "blocked" || selection == AgentTaskSelection::SupervisorRetry {
        let old = store.supervisor_review_blocking(project.id)?;
        if selection == AgentTaskSelection::SupervisorRetry
            && matches!(status, "stopped" | "handoff")
        {
            return Ok(());
        }
        if selection == AgentTaskSelection::SupervisorRetry
            && !matches!(status, "success" | "blocked")
            && let Some(session) = old
                .as_ref()
                .and_then(|review| review.retry_session.as_deref())
        {
            let _lock = acquire_board_mutation_lock(&get_tasks_dir(&project.path))?;
            let mut linked = Vec::new();
            crate::session_control::collect_codex_session_tasks_in_board(
                &get_tasks_dir(&project.path),
                session,
                &mut linked,
            )?;
            if let [(task_status, task)] = linked.as_slice()
                && task_status.is_active()
                && !task_entry_is_blocked(task)
            {
                let marker = format!("codex:{session}");
                let split = task
                    .content
                    .rfind(&marker)
                    .context("Retry task lost its session marker")?;
                let content = format!(
                    "{}\n\nBLOCKED {}: Supervisor-approved attempt ended with {}; review its runtime diagnostics before continuing. {}",
                    task.content[..split].trim_end(),
                    chrono::Local::now().format("%Y-%m-%d"),
                    status,
                    &task.content[split..]
                );
                write_task_entry_content(
                    &get_tasks_dir(&project.path),
                    *task_status,
                    task,
                    &content,
                )?;
            }
        }
        if blocked_candidates(project)?.is_empty() {
            store.clear_supervisor_review_blocking(project.id)?;
        } else {
            store.save_supervisor_review_blocking(
                project.id,
                &SupervisorReview {
                    evidence: evidence(project)?,
                    state: "pending".into(),
                    attempts: 0,
                    retries: old.map_or(0, |review| review.retries),
                    decision: None,
                    retry_session: None,
                    error: None,
                },
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
