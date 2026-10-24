//! Portable task coordination metadata. No scheduler or process state lives here.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const PREFIX: &str = "<!-- clt-coordination: ";
const SUFFIX: &str = " -->";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Scope {
    pub intent: String,
    pub files: Vec<String>,
    pub modules: Vec<String>,
    pub interfaces: Vec<String>,
    pub resources: Vec<String>,
    /// Desired observable behavior, e.g. scheduler.order = fifo.
    pub behavior: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Review {
    pub other: String,
    pub evidence: String,
    pub compatible: bool,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Metadata {
    pub id: String,
    pub depends_on: Vec<String>,
    pub scope: Scope,
    pub reviews: Vec<Review>,
}

fn span(content: &str) -> Result<Option<(usize, usize)>> {
    let Some(start) = content.find(PREFIX) else {
        return Ok(None);
    };
    let end = content[start..]
        .find(SUFFIX)
        .context("Unterminated CLT coordination metadata")?
        + start
        + SUFFIX.len();
    anyhow::ensure!(
        !content[end..].contains(PREFIX),
        "Duplicate CLT coordination metadata"
    );
    Ok(Some((start, end)))
}

pub(crate) fn read(content: &str) -> Result<Metadata> {
    let Some((start, end)) = span(content)? else {
        return Ok(Metadata::default());
    };
    let metadata: Metadata =
        serde_json::from_str(&content[start + PREFIX.len()..end - SUFFIX.len()])
            .context("Invalid CLT coordination metadata; repair it with clt coordinate")?;
    anyhow::ensure!(
        uuid::Uuid::parse_str(&metadata.id).is_ok(),
        "Invalid stable task ID"
    );
    Ok(metadata)
}

pub(crate) fn strip(content: &str) -> String {
    match span(content) {
        Ok(Some((start, end))) => format!("{}{}", &content[..start], &content[end..])
            .trim()
            .to_string(),
        _ => content.to_string(),
    }
}

pub(crate) fn write(content: &str, metadata: &Metadata) -> Result<String> {
    span(content)?;
    let stopped = super::task_content_is_stopped(content);
    let session = super::recoverable_codex_session_id_from_task_content(content);
    let body = strip(&super::task_content_without_recoverable_codex_session(
        super::task_content_without_stop_marker(content),
    ));
    // A comment must remain one token group even when an intent contains HTML.
    let json = serde_json::to_string(metadata)?.replace('>', "\\u003e");
    let mut content = format!("{} {PREFIX}{json}{SUFFIX}", body.trim_end());
    if let Some(session) = session {
        content = super::task_content_with_codex_session(&content, session);
    }
    if stopped {
        content.push_str(" clt:stopped");
    }
    Ok(content)
}

/// The task editor edits prose; IDs, dependencies and scope have their own command.
pub(crate) fn preserve(original: &str, edited: &str) -> Result<String> {
    let old = read(original)?;
    if old.id.is_empty() {
        return Ok(edited.to_string());
    }
    let supplied = read(edited)?;
    anyhow::ensure!(
        supplied.id.is_empty() || supplied.id == old.id,
        "A stable task ID cannot be changed by editing its description"
    );
    write(edited, &old)
}
