Implement Codex process execution with timeout and log capture (AGENT, P1).

Design reference: `codex-agent.md` Codex run behavior and logging.

Acceptance criteria:
- Invoke `codex exec --sandbox workspace-write` from the target project directory.
- Use the one-task prompt described in the feature doc.
- Capture stdout and stderr to durable central log files.
- Enforce a configurable timeout with a safe default of 45 minutes.
- Treat `NO_TASKS_LEFT` as a clean idle result.

Completion note:
- Implemented `CodexAgentRunner` for `codex exec --sandbox workspace-write` runs from the project root, central stdout/stderr log capture, `CLT_AGENT_RUN_TIMEOUT_SECONDS` with a 45-minute default, timeout killing, and `NO_TASKS_LEFT` idle mapping.
- Added scheduler runner injection and tests for scheduler behavior plus runner log capture.
- Checks: `cargo test agent_`; `cargo test codex_runner_writes_logs_and_treats_no_tasks_left_as_idle`; `cargo test`.
