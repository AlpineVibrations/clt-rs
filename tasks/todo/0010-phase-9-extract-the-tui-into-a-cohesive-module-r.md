Phase 9: Extract the TUI into a cohesive module (REFACTOR, TUI).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-9-cohesive-tui-extraction)

Dependency: Phase 8.

Build outcome: All TUI state, panels, input, rendering, terminal management, and event-loop behavior live in the TUI module family; the transitional monolith is removed.

Acceptance: TUI helper/render tests and the complete compatibility suite pass with unchanged keyboard and display behavior.
