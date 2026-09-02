Phase 5: Extract OS service adapters and process supervision (REFACTOR, AGENT, PLATFORM).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-5-platform-and-process-adapters)

Dependency: Phase 4.

Build outcome: launchd/systemd management, executable discovery, process groups, child termination/reaping, and terminal supervision are isolated platform adapters.

Acceptance: Platform `cfg` behavior, service definitions, identifiers, process fencing, and terminal restoration tests remain unchanged.
