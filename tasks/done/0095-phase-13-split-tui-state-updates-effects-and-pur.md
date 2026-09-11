Phase 13: Split TUI state, updates, effects, and pure rendering (REFACTOR, TUI).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-13-tui-stateupdateeffectrender-separation)

Dependency: Phase 12.

Build outcome: `TuiApp` owns state, pane handlers return explicit effects, effect execution performs I/O, and rendering consumes cached state only.

Acceptance: Reducer and render tests cover pane transitions and key actions; render code has no filesystem, database, network, Git, service, or process operations.

Completion note:
COMPLETED 2026-09-03: Added a single `TuiApp` state owner, cached task/archive/model-environment/clock snapshots, pure pane reducers with explicit effects, dedicated effect executors, and an immutable whole-app render entry point. Added reducer, cached-navigation, pure-render, and architecture boundary tests. Checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (415 passed).
