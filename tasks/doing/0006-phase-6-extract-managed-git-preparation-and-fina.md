Phase 6: Extract managed Git preparation and finalization (REFACTOR, AGENT, GIT).

Reference: [CLT Refactor Design/Build Plan](../../refactor-plan.md#phase-6-managed-git-lifecycle)

Dependency: Phase 5.

Build outcome: Git preflight, frozen state, tree projection, manifest proof, publication, leases, journals, and recovery live behind the managed-Git module.

Acceptance: Existing Git lifecycle and crash-recovery tests pass without changing fail-closed semantics or repository command behavior.

Completion note:

COMPLETED 2026-09-03: Added the internal `managed_git` module for finalization leases and retry state, Working-link recovery, Git preflight and frozen baselines, projected task trees, staged-manifest and commit proof, frozen publication, and journal reconciliation. Checks: `cargo fmt --all -- --check`; `cargo clippy --locked --all-targets --all-features -- -D warnings`; `cargo test --locked --all-targets --all-features` (396 library tests and 7 CLI integration tests passed); `git diff --check`.

Blocked note:

BLOCKED 2026-09-03: The extraction and all gates pass, but the frozen baseline has Phase 2–5's `src/lib.rs` and supporting modules as untracked files. Phase 6 necessarily edits `src/lib.rs` to register `managed_git` and remove the extracted implementation, so Git cannot stage a buildable Phase 6 delta without also committing older task payloads. External CLT journal recovery must resolve the earlier WORKING histories before this task can be sealed safely. codex:01a06531-8219-7302-8377-b13af936bd40
