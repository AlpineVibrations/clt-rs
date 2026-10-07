Fix managed Git recovery after a checkout branch change [BUG]

Completion note:
COMPLETED 2026-10-02: Detect branch changes before resuming managed Codex work and retain the diagnostic through worker finalization. Add explicit branch recovery that preserves files, staging, commits and the retired journal, and queues remaining work for a fresh attempt; provisional Done work is reverified. Added recovery, ownership, interrupted-write, UI and no-child-launch regressions. Checks passed: rustfmt required gate; cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings; cargo test --locked --all-targets --all-features (732 passed, 1 ignored).

Release note:
COMPLETED 2026-10-02: Bumped CLT to 0.7.8, updated the dated changelog and release instructions, and installed the local build; installed version verified as clt 0.7.8.

clt:manual codex:01a0ff4a-2380-7952-812f-786fb8c7026e
