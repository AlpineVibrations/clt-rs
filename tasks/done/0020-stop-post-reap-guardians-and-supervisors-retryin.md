Stop post-reap guardians and supervisors retrying when agent registry recovery is required (BUG, HIGH)

Also fix the shared-WAL reader release race and automatically repair idle registry coordination on the next open, retaining the original bundle and requiring exclusive access, stopped processes and an integrity check.

Release note 2026-09-05: Bumped CLT to 0.6.6 in Cargo.toml and Cargo.lock and documented the recovery fixes in CHANGELOG.md.

COMPLETED 2026-09-05: Retired shared reader metadata before releasing its OS lock, stopped post-reap guardian/supervisor retries on recovery markers, and added exclusive coordination-only automatic repair on the next registry open. Original DB/WAL bundles are retained; live users, interrupted writes and failed repairs require manual recovery. Checks: cargo fmt --all -- --check; cargo clippy --locked --all-targets --all-features -- -D warnings; RUST_TEST_THREADS=4 cargo test --locked --all-targets --all-features (505 passed); vendored Turso shared_wal_coordination tests (45 passed). The first unrestricted parallel run hit two process-cleanup timing failures; the complete four-thread run passed.
