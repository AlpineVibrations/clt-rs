//! Small, atomic progress snapshots derived from events the worker already receives.
use std::path::Path;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session_control::steering::{self, SteeringTarget};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ProgressSnapshot {
    pub target: SteeringTarget,
    pub recorded_at: String,
    pub update: Option<String>,
    pub activity: String,
}

impl ProgressSnapshot {
    pub(super) fn new(target: SteeringTarget) -> Self {
        Self {
            target,
            recorded_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            update: None,
            activity: "Starting work on this turn".into(),
        }
    }

    pub(super) fn event(&mut self, method: &str, params: &Value) -> bool {
        let item = &params["item"];
        let completed = method == "item/completed";
        let activity = match method {
            "item/started" | "item/completed" => match item["type"].as_str().unwrap_or_default() {
                "agentMessage" | "plan" if completed => {
                    let text = item["text"].as_str().unwrap_or_default();
                    if text.trim().is_empty() {
                        return false;
                    }
                    self.update = Some(short_text(text, 400));
                    "Posted a progress update".to_string()
                }
                "commandExecution" => format!(
                    "Command {}: {}",
                    if completed {
                        item["status"].as_str().unwrap_or("completed")
                    } else {
                        "running"
                    },
                    short_text(item["command"].as_str().unwrap_or("shell command"), 140)
                ),
                "fileChange" => format!(
                    "File edits {}",
                    if completed {
                        item["status"].as_str().unwrap_or("completed")
                    } else {
                        "in progress"
                    }
                ),
                "mcpToolCall" | "dynamicToolCall" => format!(
                    "Tool {}: {}",
                    if completed {
                        item["status"].as_str().unwrap_or("completed")
                    } else {
                        "running"
                    },
                    short_text(item["tool"].as_str().unwrap_or("tool"), 140)
                ),
                "webSearch" => "Looking up information".into(),
                "contextCompaction" => "Compacting conversation context".into(),
                _ => return false,
            },
            "turn/completed" => format!(
                "Turn {}",
                params["turn"]["status"].as_str().unwrap_or("ended")
            ),
            "error" => "Codex reported an error; see logs for details".into(),
            _ => return false,
        };
        self.activity = activity;
        self.recorded_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        true
    }

    pub(super) fn save(&self, mailbox: &Path) {
        // Observability must never fail the task or its owning protocol connection.
        let _ = steering::write_json(&mailbox.join("progress.json"), self);
    }
}

pub(crate) fn short_text(text: &str, limit: usize) -> String {
    let mut result = String::new();
    let mut count = 0;
    for word in text.split_whitespace() {
        if !result.is_empty() {
            if count >= limit {
                result.push('…');
                return result;
            }
            result.push(' ');
            count += 1;
        }
        for ch in word.chars().filter(|ch| !ch.is_control()) {
            if count >= limit {
                result.push('…');
                return result;
            }
            result.push(ch);
            count += 1;
        }
    }
    result
}
