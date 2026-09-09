Queue actionable follow-ups in Todo and keep successful parent runs from reporting false failures (BUG, FOLLOW-UP)

Completion note:
COMPLETED 2026-09-05: Added actionable Todo follow-ups with --evidence, explicit blockers, clear queue messages, and updated agent prompts and bundled skills. Extended Git sealing to linked Todo additions and empty folder-backed comparison scopes; verified parent success and fresh follow-up Git startup while rejecting unrelated board changes. Checks: cargo test --locked --all-targets -- --test-threads=1 (498 passed); cargo clippy --locked --all-targets --no-deps -- -D warnings; cargo fmt --all -- --check; git diff --check. Installed with cargo install --path . --locked --offline. Preserved Meshdock cleanup history, recorded UNBLOCKED, returned it to Todo, cleared failed-start cooldown, and restarted the scheduler.

Release note:
Bumped the package and lockfile to 0.6.5 and added release notes for the verified follow-up fix.
