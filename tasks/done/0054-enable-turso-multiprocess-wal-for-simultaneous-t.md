Enable Turso multiprocess WAL for simultaneous TUI and daemon registry access (BUG, HIGH)

Completion note:

COMPLETED 2026-08-21: Enabled Turso's multiprocess WAL coordination for the agent registry and added a regression test that holds the store open while a second test process opens and reads the same database. Installed the locked release build and verified a separate `clt agent status` succeeds while the launchd daemon is active and holds the database files. Checks: `cargo fmt -- --check`; focused cross-process test; `cargo test -- --test-threads=1` (189 passed); `git diff --check`; live daemon/status concurrency check.
