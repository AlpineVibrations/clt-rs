Implement clt agent status and logs commands (AGENT, P2).

Design reference: `codex-agent.md` command design and logging.

Acceptance criteria:
- `clt agent status` shows registry path, project count, active leases, and recent run state.
- `clt agent logs` prints recent agent run logs.
- Add useful filters if they are straightforward, such as `--project` or `--run`.
- Do not require the background service to be running in order to inspect stored state.
- Keep output readable in plain terminals.

Completion note:
- Implemented `clt agent status` and `clt agent logs` against the existing Turso registry, including active leases, recent run records, and log tail output.
- Added focused tests for active lease queries, recent run queries, and log tailing.
- Ran `cargo fmt`, `cargo test`, `cargo build`, and CLI smoke checks with temporary `CLT_AGENT_STATE_DIR` values.
