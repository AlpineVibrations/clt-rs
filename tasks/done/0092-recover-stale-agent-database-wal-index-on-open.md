Recover stale agent database WAL index on open

Outcome: Recovered versioned Turso shared-WAL indexes only when no live peer holds the coordination map, and pinned long-lived agent-store readers to prevent the auto-checkpoint restart that produced the stale index. Added exact corruption, live-peer deferral, checkpoint-pressure, and multiprocess regression coverage. Verified with 314 tests, Clippy with warnings denied, formatting, diff checks, a forced install, and the live agent database.
