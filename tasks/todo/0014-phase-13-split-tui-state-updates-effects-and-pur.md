Phase 13: Split TUI state, updates, effects, and pure rendering (REFACTOR, TUI).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-13-tui-stateupdateeffectrender-separation)

Dependency: Phase 12.

Build outcome: `TuiApp` owns state, pane handlers return explicit effects, effect execution performs I/O, and rendering consumes cached state only.

Acceptance: Reducer and render tests cover pane transitions and key actions; render code has no filesystem, database, network, Git, service, or process operations.
