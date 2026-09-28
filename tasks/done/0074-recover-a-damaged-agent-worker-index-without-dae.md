Recover a damaged agent worker index without daemon crash loops (BUG, RELIABILITY)

Completion note:

COMPLETED 2026-09-01: Detect Turso's missing-index-entry failure during scheduler recovery, transactionally rebuild and integrity-check only the derived active-worker project index, retry the pass once, preserve daemon check-ins, and keep the daemon alive for later pass retries. Bumped the crate patch version to 0.5.1. Checks: `cargo test` (320 passed), `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check`, and an isolated end-to-end run against the original corrupted database backup that repaired the index, abandoned the stale worker, remained running, and finished with `PRAGMA integrity_check` reporting `ok`.
