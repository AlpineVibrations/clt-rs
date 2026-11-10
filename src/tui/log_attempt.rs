//! Resolve exact attempts, including logs retained after an interrupted result write.
use super::{
    TuiAgentLogView, TuiAgentProject, TuiCodexSessionTarget, agent_session_control_has_live_log,
};
use crate::{
    agent::{self, AgentSessionControlRecord},
    runner::{preferred_agent_output_path, preferred_recorded_agent_output_path},
};
use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(super) struct Attempt {
    pub started_at: Option<i64>,
    pub outcome: String,
    pub run_id: Option<i64>,
}

impl Attempt {
    pub fn from_run(run: &agent::AgentRunRecord) -> Self {
        Self {
            started_at: run.started_at.parse().ok(),
            outcome: match run.status.as_str() {
                "failure" => "Failed",
                "timeout" => "Timed out",
                "success" => "Succeeded",
                "running" => "Outcome unrecorded",
                "stopped" => "Stopped",
                _ => &run.status,
            }
            .to_string(),
            run_id: Some(run.id),
        }
    }

    pub fn from_path(path: &Path) -> Self {
        Self {
            started_at: token_time(path).map(|time| time.0 as i64),
            outcome: "Outcome unrecorded".into(),
            run_id: None,
        }
    }

    pub fn label(&self, live: bool) -> String {
        let time = self
            .started_at
            .and_then(|at| DateTime::<Utc>::from_timestamp(at, 0))
            .map(|at| {
                at.with_timezone(&Local)
                    .format("%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_else(|| "Time unrecorded".into());
        let mut label = if live {
            format!("LIVE · {time}")
        } else {
            format!("SAVED · {time} · {}", self.outcome)
        };
        if let Some(id) = self.run_id {
            label.push_str(&format!(" · run {id}"));
        }
        label
    }
}

// CLT run tokens start with epoch seconds and nanoseconds. Never use file mtime:
// appending output or copying a saved file must not change the attempt's date.
fn token_time(path: &Path) -> Option<(u64, u32)> {
    let mut parts = path.file_stem()?.to_str()?.split('-');
    let seconds = parts.next()?.parse().ok()?;
    let nanos = parts.next()?.parse().ok()?;
    (nanos < 1_000_000_000).then_some((seconds, nanos))
}

fn matches_path(run: &agent::AgentRunRecord, path: &Path) -> bool {
    [run.stderr_path.as_deref(), run.stdout_path.as_deref()]
        .into_iter()
        .flatten()
        .any(|recorded| Path::new(recorded) == path)
}

pub(super) fn attach(
    store: &agent::TursoAgentStore,
    project_id: i64,
    mut view: TuiAgentLogView,
) -> Result<TuiAgentLogView> {
    let Some(path) = view.path.as_deref() else {
        return Ok(view);
    };
    let mut run = match path.file_stem().and_then(|name| name.to_str()) {
        Some(token) => store.run_for_worker_token_blocking(project_id, token)?,
        None => None,
    }
    .filter(|run| matches_path(run, path));
    if run.is_none()
        && let Some(target) = &view.session_target
    {
        run = store
            .latest_output_run_for_codex_session_blocking(project_id, &target.session_id)?
            .filter(|run| matches_path(run, path));
    }
    if run.is_none() {
        run = store
            .latest_run_for_project_blocking(project_id)?
            .filter(|run| matches_path(run, path));
    }
    view.attempt = Some(
        run.as_ref()
            .map(Attempt::from_run)
            .unwrap_or_else(|| Attempt::from_path(path)),
    );
    Ok(view)
}

pub(super) fn newer_control_path(
    control: &AgentSessionControlRecord,
    run: Option<&agent::AgentRunRecord>,
) -> Option<PathBuf> {
    let path = control
        .stderr_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            control
                .stdout_path
                .as_deref()
                .map(PathBuf::from)
                .filter(|path| path.is_file())
        })?;
    if !path.is_file() {
        return None;
    }
    if agent_session_control_has_live_log(control) {
        return Some(path);
    }
    let Some(run) = run else {
        return Some(path);
    };
    if matches_path(run, &path) {
        return preferred_recorded_agent_output_path(run);
    }
    let recorded_time = preferred_recorded_agent_output_path(run)
        .as_deref()
        .and_then(token_time)
        .or_else(|| run.started_at.parse().ok().map(|at| (at, 0)));
    match (token_time(&path), recorded_time) {
        (Some(retained), Some(recorded)) if retained > recorded => Some(path),
        _ => None,
    }
}

pub(super) fn session_view(
    store: &agent::TursoAgentStore,
    selected: &TuiAgentProject,
    session: &str,
    prefer_final_output: bool,
) -> Result<Option<TuiAgentLogView>> {
    let run = store.latest_output_run_for_codex_session_blocking(selected.project.id, session)?;
    let control = store.session_control_blocking(selected.project.id, session)?;
    let retained = control
        .as_ref()
        .and_then(|control| newer_control_path(control, run.as_ref()));
    let (path, settings, live) = if let Some(path) = retained {
        let control = control.as_ref().expect("retained control");
        let path = if prefer_final_output && !agent_session_control_has_live_log(control) {
            preferred_agent_output_path(
                control.stdout_path.as_deref(),
                control.stderr_path.as_deref(),
            )
            .unwrap_or(path)
        } else {
            path
        };
        (
            Some(path),
            control.stderr_path.as_ref().map(PathBuf::from),
            agent_session_control_has_live_log(control),
        )
    } else {
        (
            run.as_ref().and_then(preferred_recorded_agent_output_path),
            run.as_ref()
                .and_then(|run| run.stderr_path.as_ref())
                .map(PathBuf::from),
            false,
        )
    };
    path.map(|path| {
        let target = Some(TuiCodexSessionTarget::new(
            &selected.project,
            session.to_string(),
        ));
        attach(
            store,
            selected.project.id,
            TuiAgentLogView::new(selected.project.name.clone(), path, settings, live, target)?,
        )
    })
    .transpose()
}

pub(super) fn newer_project_session(
    store: &agent::TursoAgentStore,
    project_id: i64,
) -> Result<Option<String>> {
    let latest = store.latest_run_for_project_blocking(project_id)?;
    // A new launch failure with no output is itself useful evidence. Do not
    // hide it behind an earlier conversation's retained file.
    if latest.as_ref().is_some_and(|run| {
        matches!(run.status.as_str(), "failure" | "timeout")
            && run.stdout_path.is_none()
            && run.stderr_path.is_none()
    }) {
        return Ok(None);
    }
    Ok(store
        .session_controls_for_project_blocking(project_id)?
        .into_iter()
        .filter_map(|control| {
            let path = newer_control_path(&control, latest.as_ref())?;
            Some((
                (
                    agent_session_control_has_live_log(&control),
                    token_time(&path),
                ),
                control.codex_session_id,
            ))
        })
        .max_by_key(|(key, _)| *key)
        .map(|(_, session)| session))
}

#[cfg(test)]
mod tests;
