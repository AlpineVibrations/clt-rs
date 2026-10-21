//! Translate root-thread events into the plain-text transcript used by CLI and TUI logs.
use std::collections::HashSet;

use serde_json::Value;

#[derive(Default)]
pub(super) struct EventLog {
    started_commands: HashSet<String>,
    streamed_items: HashSet<String>,
    line_open: bool,
}

impl EventLog {
    pub(super) fn start_turn(&mut self) {
        self.started_commands.clear();
        self.streamed_items.clear();
    }

    pub(super) fn event(&mut self, method: &str, params: &Value) -> String {
        let mut text = String::new();
        let delta = method.ends_with("/outputDelta");
        match method {
            "item/started" if params["item"]["type"] == "commandExecution" => {
                let item = &params["item"];
                if item["id"]
                    .as_str()
                    .is_none_or(|id| self.started_commands.insert(id.to_string()))
                {
                    command_header(&mut text, item);
                }
            }
            "item/completed" => {
                let item = &params["item"];
                let id = item["id"].as_str().unwrap_or_default();
                let started = self.started_commands.remove(id);
                let streamed = self.streamed_items.remove(id);
                completed_item(&mut text, item, started, streamed);
            }
            "error" => {
                line(&mut text, "Codex error");
                line(&mut text, error_message(&params["error"]));
            }
            _ if delta => {
                if let Some(output) = params["delta"].as_str().filter(|s| !s.is_empty()) {
                    if let Some(id) = params["itemId"].as_str() {
                        self.streamed_items.insert(id.to_string());
                    }
                    text.push_str(output);
                }
            }
            _ => {}
        }
        // A command may end without a newline. Keep the next heading separate
        // without inserting newlines between chunks of its streamed output.
        if !text.is_empty() {
            if self.line_open && !delta {
                text.insert(0, '\n');
            }
            self.line_open = !text.ends_with('\n');
        }
        text
    }
}

fn line(output: &mut String, text: &str) {
    if !text.is_empty() {
        output.push_str(text);
        if !text.ends_with('\n') {
            output.push('\n');
        }
    }
}

fn command_header(output: &mut String, item: &Value) {
    line(output, "exec");
    if let Some(cwd) = item["cwd"].as_str() {
        line(output, &format!("in {cwd}"));
    }
    line(output, item["command"].as_str().unwrap_or_default());
}

fn status(output: &mut String, label: &str, item: &Value) {
    let state = if item["success"] == false {
        "failed"
    } else {
        item["status"].as_str().unwrap_or("completed")
    };
    let mut text = format!("{label}: {state}");
    if let Some(code) = item["exitCode"].as_i64() {
        text.push_str(&format!(" (exit code {code})"));
    }
    if let Some(ms) = item["durationMs"].as_u64() {
        text.push_str(&format!(" in {:.2}s", ms as f64 / 1000.0));
    }
    line(output, &text);
}

fn error_message(error: &Value) -> &str {
    error["message"]
        .as_str()
        .or_else(|| error.as_str())
        .unwrap_or_default()
}

fn content_text(output: &mut String, content: &Value) {
    if let Some(text) = content.as_str() {
        line(output, text);
    } else if let Some(parts) = content.as_array() {
        for part in parts {
            line(output, part["text"].as_str().unwrap_or_default());
        }
    }
}

fn completed_item(output: &mut String, item: &Value, started: bool, streamed: bool) {
    let kind = item["type"].as_str().unwrap_or("item");
    match kind {
        "agentMessage" | "plan" => {
            if let Some(text) = item["text"].as_str().filter(|s| !s.is_empty()) {
                line(
                    output,
                    if kind == "agentMessage" {
                        "codex"
                    } else {
                        "plan"
                    },
                );
                line(output, text);
            }
        }
        "reasoning" => {
            let mut text = String::new();
            for field in ["summary", "content"] {
                if let Some(parts) = item[field].as_array() {
                    for part in parts {
                        line(&mut text, part.as_str().unwrap_or_default());
                    }
                }
                if !text.is_empty() {
                    break;
                }
            }
            if !text.is_empty() {
                line(output, "thinking");
                line(output, &text);
            }
        }
        "commandExecution" => {
            if !started {
                command_header(output, item);
            }
            // Completion repeats the output already delivered by outputDelta.
            // Some peers only send the aggregate, so retain that fallback.
            if !streamed {
                line(
                    output,
                    item["aggregatedOutput"].as_str().unwrap_or_default(),
                );
            }
            status(output, "exec", item);
        }
        "fileChange" => {
            status(output, "patch", item);
            if let Some(changes) = item["changes"].as_array() {
                for change in changes {
                    let path = change["path"].as_str().unwrap_or("unknown path");
                    let kind = change["kind"]["type"].as_str().unwrap_or("update");
                    line(output, &format!("{kind} {path}"));
                    line(output, change["diff"].as_str().unwrap_or_default());
                }
            }
        }
        "mcpToolCall" | "dynamicToolCall" => {
            let tool = item["tool"].as_str().unwrap_or("tool");
            let server = item["server"]
                .as_str()
                .or_else(|| item["namespace"].as_str());
            let label =
                server.map_or_else(|| tool.to_string(), |server| format!("{server}.{tool}"));
            status(output, &label, item);
            content_text(output, &item["result"]["content"]);
            content_text(output, &item["contentItems"]);
            line(output, error_message(&item["error"]));
        }
        "functionCallOutput" => {
            line(output, item["name"].as_str().unwrap_or("tool"));
            content_text(output, &item["output"]);
        }
        "userMessage" => {
            line(output, "user");
            content_text(output, &item["content"]);
        }
        "webSearch" => {
            line(output, "web search");
            line(output, item["query"].as_str().unwrap_or_default());
        }
        "contextCompaction" => line(output, "Context compacted"),
        // Future item types should remain visible without dumping opaque JSON,
        // tool arguments, or media data into the user's transcript.
        _ => status(output, kind, item),
    }
}

#[cfg(test)]
mod tests;
