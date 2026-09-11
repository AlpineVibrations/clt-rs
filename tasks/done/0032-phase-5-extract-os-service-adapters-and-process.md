Phase 5: Extract OS service adapters and process supervision (REFACTOR, AGENT, PLATFORM).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-5-platform-and-process-adapters)

Dependency: Phase 4.

Build outcome: launchd/systemd management, executable discovery, process groups, child termination/reaping, and terminal supervision are isolated platform adapters.

Acceptance: Platform `cfg` behavior, service definitions, identifiers, process fencing, and terminal restoration tests remain unchanged.

Completion note:

COMPLETED 2026-09-03: Added the internal `platform` module for launchd/systemd service management and definitions, worker service launching, executable discovery, Unix process groups, child termination/reaping, and interactive terminal foreground control. Checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (396 library tests and 7 CLI integration tests passed); `git diff --check`.

BLOCKED 2026-09-03: Implementation and all gates pass, but the frozen baseline has Phase 2–4's `src/lib.rs` as an untracked file and Phase 5 necessarily changes it to declare `platform` and remove the extracted code. Git cannot stage only Phase 5's delta or a buildable task commit without also absorbing older task payloads, so external CLT journal recovery must resolve the earlier WORKING histories before this task can be sealed safely.

BLOCKED 2026-09-03: Reverified the platform extraction and all locked gates while the Phase 0 and Phase 2–4 journals remain WORKING. Phase 5 still cannot stage its `src/lib.rs` delta without consuming the earlier sessions' untracked baseline; resolve those journals or add supported baseline ownership transfer before sealing this task.

COMPLETED 2026-09-03: Manual recovery preserved the verified Phase 5 implementation in combined rescue commit `f881c4f`; the obsolete per-task WORKING journal was cancelled after the platform extraction became part of that recovery boundary.

codex:01a06525-5bff-7672-bd08-fb6cb3dfb97b
