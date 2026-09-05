Release interactive agent leases immediately when no Codex session starts or the holder process exits; never report a lease-only reservation as a running agent. (AGENT, LEASE, TUI, BUG, HIGH)

## Completion note

COMPLETED 2026-09-02: Added generation-aware interactive-holder liveness for guarded recovery and TUI status, immediate exact-lease cleanup when a reaped guardian has lost its session row, and daemon reclamation for dead lease-only interactive reservations while retaining matching session fences. Documented renewable one-hour lease semantics and the corrected FENCED/STALE states. Checks: `cargo fmt -- --check`; five focused regression tests; `cargo test` (395 passed); `cargo clippy --all-targets -- -D warnings`; `cargo build --release`.
