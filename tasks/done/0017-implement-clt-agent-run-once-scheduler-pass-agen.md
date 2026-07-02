Implement clt agent run --once scheduler pass (AGENT, P1).

Design reference: `codex-agent.md` command design and scheduling rules.

Acceptance criteria:
- Add `clt agent run --once` as a foreground scheduler pass.
- Load enabled registered projects from the registry.
- Pick eligible projects with pending tasks and no active lease.
- Respect a conservative global concurrency limit, defaulting to one.
- Record run outcomes in the registry.

Completion note:
- Implemented `clt agent run --once` scheduler pass with enabled-project scanning, pending-task selection, active lease checks, default single-run concurrency, durable run outcome recording, and regression tests.
- Ran `cargo fmt` and `cargo test`.
