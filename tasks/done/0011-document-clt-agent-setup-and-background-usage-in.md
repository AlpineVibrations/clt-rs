Document clt agent setup and background usage in the README (AGENT, P2).

Design reference: `codex-agent.md`.

Acceptance criteria:
- Add README documentation for registering projects.
- Explain `run --once`, `daemon`, `start`, `stop`, `status`, and `logs`.
- Document where agent state and logs live.
- Mention that the agent registry uses Turso's pure-Rust SQL database so `cargo install` does not require SQLite or a C compiler for the registry layer.
- Mention conservative concurrency and one-task-per-project behavior.
- Keep existing `clt` task/TUI documentation intact.
