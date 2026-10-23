Allow agent Git settings to change during active runs while preserving each run’s Git mode.

COMPLETED 2026-10-23: Project Git settings now save for subsequent launches during automated and interactive activity. Active task commands keep their recorded mode, including Git off; managed journals retain their commit/push obligations. Resume preparation and abandoned-launch recovery use the frozen run mode. Updated TUI/CLI feedback, documentation, bundled skills, and regression coverage.

Validation: Formatting and diff checks passed. The focused settings regression passed across worker, interactive, launch, and journal states. The full test and Clippy runs were stopped at the user's request because of limited disk space; remaining build and validation are deferred to the user.

clt:manual codex:01a1691f-fed1-73f3-87fb-55041b5735d7
