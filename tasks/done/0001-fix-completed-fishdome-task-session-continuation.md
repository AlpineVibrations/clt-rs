Fix completed Fishdome task session continuation reporting missing agent output (BUG, TUI)

Completion note:

Release note: Included in patch release 0.6.12; updated Cargo.toml, Cargo.lock, and the dated changelog entry.

COMPLETED 2026-09-13: Restored completed-task output from exact session-control log paths when stale worker recovery left no session-linked run history. The c shortcut can reserve an inactive queued session only with its exclusive lease, matching run generation, and no unfinished worker; leaving interactive use preserves the session as stopped. Continuation errors now replace the open task log. Verified Fishdome's completed FISHING-11 task retained its valid session marker and final response; no Fishdome task or session was restarted. Checks: cargo fmt --all -- --check; cargo clippy --locked --all-targets --all-features -- -D warnings; cargo test --locked --all-targets --all-features (550 unit, 4 architecture, 14 CLI tests passed); git diff --check.
