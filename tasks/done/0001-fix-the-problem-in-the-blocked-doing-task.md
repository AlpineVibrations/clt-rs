fix the problem in the blocked doing task

Completion note: COMPLETED 2026-07-16: Prevented overlapping daemon scheduler passes and configured a five-second busy timeout for every Turso agent-store connection, eliminating duplicate runs and transient database-lock failures under rapid polling. Checks: `cargo fmt -- --check`; focused daemon cooldown test repeated 20 times; `cargo test -- --test-threads=1` (121 passed); `git diff --check`.
