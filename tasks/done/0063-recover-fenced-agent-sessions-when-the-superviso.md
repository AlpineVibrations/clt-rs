Recover fenced agent sessions when the supervisor crashes without a reap marker, and expose enough status for the UI to stop or resume the affected session (BUG, AGENT, SESSION-CONTROL, CRASH-RECOVERY, HIGH)

Completion note:

COMPLETED 2026-09-01: Removed live agent-database polling from the child-owning supervisor, routed connected session controls through the runner's existing lifeline, and added panic containment that stops and reaps the owned Codex process group before reporting shutdown proof. Added a FENCED Agent Projects state whose log view retains the exact persisted session target and s/i/c controls when no lease survives. Documented the recovery behavior. Checks: `cargo fmt --all -- --check`; `cargo check --locked`; focused supervisor panic, fenced UI, and crashed-owner tests; `cargo test --locked` (324 passed); `cargo clippy --locked --all-targets -- -D warnings`; `git diff --check`.
