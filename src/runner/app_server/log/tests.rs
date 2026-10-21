use super::EventLog;
use serde_json::{Value, json};

fn completed(log: &mut EventLog, item: Value) -> String {
    log.event("item/completed", &json!({"item": item}))
}

#[test]
fn command_transcript_keeps_newlines_and_prints_streamed_output_once() {
    let mut log = EventLog::default();
    let mut item = json!({
        "id": "command-one", "type": "commandExecution", "status": "inProgress",
        "cwd": "/project", "command": "python3 - <<'PY'\nprint('café')\nPY",
        "commandActions": [{"command": "duplicated command metadata"}],
        "aggregatedOutput": null, "exitCode": null, "durationMs": null
    });
    let mut transcript = log.event("item/started", &json!({"item": item}));
    for delta in ["caf", "é\nsecond", " line"] {
        transcript.push_str(&log.event(
            "item/commandExecution/outputDelta",
            &json!({"itemId": "command-one", "delta": delta}),
        ));
    }
    item["status"] = json!("completed");
    item["aggregatedOutput"] = json!("café\nsecond line");
    item["exitCode"] = json!(0);
    item["durationMs"] = json!(1127);
    transcript.push_str(&completed(&mut log, item));
    transcript.push_str(&completed(
        &mut log,
        json!({
            "type": "reasoning", "id": "reason-one", "summary": [], "content": []
        }),
    ));
    transcript.push_str(&completed(
        &mut log,
        json!({
            "type": "agentMessage", "text": "Done.\nAll checks passed."
        }),
    ));
    assert_eq!(
        transcript,
        "exec\nin /project\npython3 - <<'PY'\nprint('café')\nPY\ncafé\nsecond line\nexec: completed (exit code 0) in 1.13s\ncodex\nDone.\nAll checks passed.\n"
    );
}

#[test]
fn aggregate_fallback_and_failure_details_survive_without_streamed_output() {
    let mut log = EventLog::default();
    // A different command's output and empty deltas must not hide this result.
    log.event(
        "item/commandExecution/outputDelta",
        &json!({
            "itemId": "other-command", "delta": "other output"
        }),
    );
    log.event(
        "item/commandExecution/outputDelta",
        &json!({
            "itemId": "failed-command", "delta": ""
        }),
    );
    let text = completed(
        &mut log,
        json!({
            "type": "commandExecution", "id": "failed-command", "command": "cargo test",
            "cwd": "/project", "status": "failed", "exitCode": 101,
            "aggregatedOutput": "error: compilation failed\n  src/main.rs:12\n"
        }),
    );
    assert_eq!(
        text,
        "\nexec\nin /project\ncargo test\nerror: compilation failed\n  src/main.rs:12\nexec: failed (exit code 101)\n"
    );
}

#[test]
fn summaries_patches_and_tool_results_are_readable_without_protocol_metadata() {
    let mut log = EventLog::default();
    let items = [
        json!({"type": "reasoning", "summary": ["Checking the log\nChecking the tests"], "content": ["duplicate detail"]}),
        json!({"type": "plan", "text": "Fix the formatter\nRun the regression"}),
        json!({"type": "fileChange", "status": "completed", "changes": [{
            "path": "src/main.rs", "kind": {"type": "update"}, "diff": "@@ -1 +1 @@\n-old\n+new"
        }]}),
        json!({"type": "mcpToolCall", "server": "docs", "tool": "read", "status": "failed",
            "arguments": {"opaque": "do not print"}, "error": {"message": "Document unavailable"}}),
        json!({"type": "dynamicToolCall", "tool": "lookup", "status": "completed", "contentItems": [
            {"type": "inputText", "text": "first result\nsecond result"},
            {"type": "inputImage", "imageUrl": "do not print"}
        ]}),
        json!({"type": "functionCallOutput", "name": "check", "output": "Passed\nFinished"}),
        json!({"type": "dynamicToolCall", "tool": "lookup", "status": "completed", "success": false,
            "contentItems": [{"type": "inputText", "text": "Lookup unavailable"}]}),
        json!({"type": "futureItem", "status": "failed", "payload": "do not print"}),
    ];
    let transcript: String = items
        .into_iter()
        .map(|item| completed(&mut log, item))
        .collect();
    assert!(transcript.contains("thinking\nChecking the log\nChecking the tests\n"));
    assert!(transcript.contains("plan\nFix the formatter\nRun the regression\n"));
    assert!(transcript.contains("update src/main.rs\n@@ -1 +1 @@\n-old\n+new\n"));
    assert!(transcript.contains("docs.read: failed\nDocument unavailable\n"));
    assert!(transcript.contains("lookup: completed\nfirst result\nsecond result\n"));
    assert!(transcript.contains("check\nPassed\nFinished\n"));
    assert!(transcript.contains("lookup: failed\nLookup unavailable\n"));
    assert!(transcript.contains("futureItem: failed\n"));
    assert!(!transcript.contains("do not print"));
    assert!(!transcript.contains("duplicate detail"));
    assert!(!transcript.contains("\\n"));
    assert_eq!(
        log.event(
            "error",
            &json!({"error": {"message": "Connection lost\nRetry later"}})
        ),
        "Codex error\nConnection lost\nRetry later\n"
    );
}

#[test]
fn a_new_turn_does_not_inherit_command_or_output_tracking() {
    let mut log = EventLog::default();
    let item = json!({"type": "commandExecution", "id": "reused", "command": "check",
        "aggregatedOutput": "new output", "status": "completed"});
    log.event("item/started", &json!({"item": item}));
    log.event(
        "item/commandExecution/outputDelta",
        &json!({"itemId": "reused", "delta": "old output"}),
    );
    log.start_turn();
    assert_eq!(
        completed(&mut log, item),
        "\nexec\ncheck\nnew output\nexec: completed\n"
    );
}
