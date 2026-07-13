Add the clt agent command group and top-level subcommands (AGENT, P1).

Design reference: `codex-agent.md` command design.

Acceptance criteria:
- Add an `agent` subcommand group to the Clap command tree.
- Include subcommands for `register`, `unregister`, `projects`, `run`, `daemon`, `start`, `stop`, `status`, and `logs`.
- Keep the existing no-args TUI behavior unchanged.
- Stub unimplemented agent commands with clear placeholder errors if needed.
- Add focused CLI parsing tests or equivalent coverage where the project already supports tests.
