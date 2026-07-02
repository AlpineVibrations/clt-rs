Add agent leases and stale lease recovery (AGENT, P1).

Design reference: `codex-agent.md` locking and safety.

Acceptance criteria:
- Add Turso-backed leases so only one agent run can own a project at a time.
- Add stale lease expiration based on a configurable timeout.
- Add a repo-local lock directory during Codex execution.
- Release leases and local locks on successful, failed, and interrupted runs where possible.
- Make lease behavior testable without spawning Codex.

Completion note:
- Added configurable agent lease timeout handling, stale lease recovery coverage, and repo-local `.codex-task-loop.lock` protection around Codex execution.
- Verified with `cargo fmt` and `cargo test` (53 tests passed).
