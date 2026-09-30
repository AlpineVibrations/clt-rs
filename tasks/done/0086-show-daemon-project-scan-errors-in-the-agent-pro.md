Show daemon project-scan errors in the Agent Projects UI with actionable external-volume permission guidance (BUG, AGENT, TUI)

Completion note:

COMPLETED 2026-08-27: Persisted each daemon project-scan status and error independently from foreground task counts; Agent Projects now renders enabled scan failures as red `ERROR` rows and shows selected-project recovery guidance, including macOS Full Disk Access instructions for inaccessible `/Volumes` projects and mount checks for missing external drives. Added an additive worker-compatible schema migration, documentation, and regression coverage. Checks: `cargo fmt`; focused daemon-scan tests; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets -- --test-threads=1` (310 passed); `git diff --check`.
