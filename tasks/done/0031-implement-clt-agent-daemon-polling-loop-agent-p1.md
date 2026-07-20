Implement clt agent daemon polling loop (AGENT, P1).

Design reference: `codex-agent.md` command design and scheduling rules.

Acceptance criteria:
- Add `clt agent daemon` as a long-running foreground worker.
- Poll registered projects at a configurable interval.
- Reuse the same scheduler path as `run --once`.
- Apply success cooldown and failure backoff.
- Handle Ctrl-C or termination cleanly enough to avoid abandoned active state when possible.

Completion note:
- Added the foreground daemon loop, configurable poll interval, scheduler pass summary reuse, success cooldown, and failure backoff.
- Ran `cargo fmt` and `cargo test` (57 tests passed).
