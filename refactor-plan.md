# CLT Refactor Design/Build Plan

Status: Stage A in progress; Phases 0–6 are complete and Phases 7–14 remain queued

## Early work already present

Before the planning-only boundary was clarified, a small part of Phase 0 was implemented in the worktree. The uncommitted changes currently:

- make unit-test builds resolve agent state to a process-unique temporary directory;
- pin Turso to `=0.7.0` while the private WAL-layout workaround is present;
- add a stable Ubuntu/macOS GitHub Actions workflow for format, strict Clippy, and tests.

Those changes passed formatting, strict Clippy, and 392 tests. They are intentionally left in place for later review; this planning pass does not continue, commit, or otherwise treat Phase 0 as accepted.

## Goal

Refactor the single-file CLT binary into a small launcher plus cohesive internal modules without changing user-visible behavior, persisted data, or agent safety guarantees. The work is deliberately split into two stages: first establish a hermetic regression boundary and move code mechanically; then improve internal APIs only after the extracted system is demonstrably equivalent.

## Review snapshot

At the start of this plan, `src/main.rs` contains 50,953 lines: approximately 31,785 lines of production code and 19,168 lines of tests. All 391 tests are in one root `tests` module. The main concentrations are:

- The inline `agent_store` module, about 7.4K lines, spanning migrations, projects, workers, leases, sessions, runs, model settings, and Git-finalization journals.
- The 2.2K-line `tui_view_with_active_board` controller, which combines state, rendering, I/O, service recovery, process control, and key handling.
- Large scheduler, runner, worker-recovery, and Git-finalization procedures containing correctness-critical state transitions.
- Direct task-storage dependencies on agent state and Git policy.
- Repeated blocking wrappers that construct Tokio runtimes around store operations.

The existing test suite is broad and valuable, and the initial isolated baseline passes all 391 tests along with formatting and strict Clippy. However, tests that mutate session-linked tasks can resolve the live user agent database when `CLT_AGENT_STATE_DIR` is not overridden. The repository also has no CI, and `test_flow.sh` mutates the current repository board without reliable assertions or fail-fast behavior.

## Non-goals

- No new product features or CLI commands.
- No intentional changes to messages, help text, exit behavior, task semantics, scheduling policy, or TUI controls during mechanical extraction.
- No database migration, task-file migration, or service-protocol revision.
- No broadly reusable Rust API in this program; that requires a separate consumer-driven design.
- No MSRV promise. CI follows current stable Rust.

## Compatibility contract

Every phase must preserve:

- The installed `clt` binary and the existing Clap command names, visible and hidden arguments, parsing behavior, stdout/stderr, and exit behavior.
- Markdown and folder-backed task stores, nested boards, task ordering, archive behavior, backup files, mutation locks, atomic temporary-file rules, and terminal `codex:<session-id>` markers.
- Agent database schema versions and migration SQL, state-directory resolution, environment variables, model/provider configuration, prompt text, service labels and unit contents, log paths, and worker protocol values.
- Lease, compare-and-set, heartbeat, process-group, session-control, and managed Git finalization invariants, including fail-closed recovery.
- Existing test coverage; no existing test may be ignored or weakened to make an extraction pass.

## Target architecture

The final crate has a thin binary and one intentionally small public entry point:

```text
src/main.rs -> clt_rs::run()
src/lib.rs  -> public launcher; all other modules private or pub(crate)

cli -----------------------> application services
tui state/update/effects --> application services
application ---------------> task + agent orchestration
agent orchestration -------> task read API + store/git/platform/process adapters
task ----------------------> std filesystem only
tui render ----------------> cached TuiApp state only
```

The intended module families are:

- `cli`: Clap types, shell integration, and command dispatch.
- `application`: workflows that compose task storage with agent/Git policy.
- `task`: typed status/model, task text and marker parsing, Markdown/folder storage, locking, atomic mutation, ordering, archive, and nested-board behavior.
- `agent`: domain models, Codex configuration, persistence, service/process adapters, Git preparation/finalization, scheduling, workers, runner, and session control.
- `tui`: application state, pane-specific updates, explicit effects, terminal ownership, and pure render components.

Final dependency rules:

- `task` never imports `agent` or `tui`.
- CLI and TUI do not query Turso or invoke Git/service/process commands directly.
- Persistence maps rows to agent-domain types and does not expose database row details to the TUI.
- Render functions perform no filesystem, database, network, Git, service-manager, or process operations.
- Production modules use explicit imports; no wildcard parent imports remain.
- The only supported public Rust item is `clt_rs::run() -> anyhow::Result<()>`.

## Implementation phases

### Stage A: safety and behavior-preserving extraction

#### Phase 0: Hermetic tests and CI

Prevent tests from resolving the user's live agent state by default, give each subprocess integration test an isolated state directory, and add current-stable Linux/macOS CI for locked format, strict Clippy, and the full suite. Pin `turso` to the validated `=0.7.0` release while CLT depends on its private shared-WAL layout.

#### Phase 1: Black-box CLI contracts

Add temporary-workspace tests for help, shell integration, init, add, list, status, done, delete, expand, invalid input, output streams, and exit status. Retire `test_flow.sh` only after equivalent asserted coverage exists.

#### Phase 2: Launcher library and CLI module

Add `src/lib.rs` with `pub fn run()`, reduce `src/main.rs` to the launcher, and isolate Clap definitions and dispatch while preserving visible and hidden command contracts.

#### Phase 3: Task domain and storage

Extract task models, text/marker parsing, Markdown/folder adapters, nested boards, ordering, locking, and atomic mutations. Leave agent-aware mutation policy in the application layer.

#### Phase 4: Agent domain, configuration, and persistence

Extract domain records, provider/Codex configuration, Turso connection management, migrations, WAL recovery, and the existing store facade. Keep migration SQL and transaction behavior unchanged.

#### Phase 5: Platform and process adapters

Extract launchd/systemd service management, executable discovery, Unix process groups, child termination/reaping, and terminal supervision with the existing platform `cfg` behavior.

#### Phase 6: Managed Git lifecycle

Extract Git preflight/synchronization, baseline capture, tree projection, manifest proof, frozen-destination publication, leases, journal reconciliation, and recovery without changing the state machine.

#### Phase 7: Scheduler and worker lifecycle

Extract project scanning, cooldowns, selection, reconciliation, global capacity, lease acquisition, worker reservation/dispatch/heartbeat/finalization, and daemon loops.

#### Phase 8: Codex runner and sessions

Extract prompt and command construction, execution gates, supervisors, session registration/linking, logs, interactive handoffs, stop/resume/interrupt control, and outcome recording.

#### Phase 9: Cohesive TUI extraction

Move all remaining TUI state, panels, input handling, rendering, terminal management, and event-loop behavior into the `tui` family. Delete the transitional monolith after every production item has an owner.

### Stage B: internal boundary improvements

Stage B begins only after all Stage A checks and compatibility tests pass.

#### Phase 10: Typed task services

Introduce internal `TaskStatus`, `TaskBoard`, and managed-task workflow types. Keep serialized status names unchanged while removing task-storage access to agent persistence.

#### Phase 11: Cohesive agent repositories and runtime ownership

Divide persistence into projects/models, workers/leases, sessions/runs, and Git-journal repositories behind one facade. Replace per-operation Tokio runtime construction with one owned blocking adapter while preserving the daemon's asynchronous boundary.

#### Phase 12: Explicit agent orchestration stages

Separate scheduler decisions from effectful acquisition/reconciliation. Split runner launch, supervision, session linking, and result recording. Replace large parameter clusters with internal request/result structures and inject clock/process/service dependencies where recovery tests require determinism.

#### Phase 13: TUI state/update/effect/render separation

Introduce `TuiApp`, route pane events through update handlers that return explicit effects, execute effects outside rendering, update cached snapshots, and make render functions pure.

#### Phase 14: Final architecture cleanup

Move unit tests beside their owners, retain true cross-module behavior as integration tests, narrow visibility, remove transitional re-exports and wildcard imports, record the final module map, and close the stale feature entry.

## Verification gates

Every phase must pass:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

The test environment must be isolated from the user's home agent state. Stage A extraction changes must also pass the black-box CLI contract suite and show no intentional diff in the compatibility contract.

Final acceptance additionally requires:

- At least the original 391 tests remain enabled and passing.
- Current-stable Linux and macOS CI is green.
- Prior-schema migration, multiprocess WAL, board concurrency, worker fencing, session recovery, Git finalization, and TUI update/render paths remain covered.
- Manual Linux/macOS smoke checks cover TUI navigation and editing, terminal restoration, project switching, and agent service start/stop.
- `src/main.rs` contains launcher boilerplate only, and `clt_rs::run()` is the sole public API.
- No schema, task-format, CLI, or service/worker-protocol changes are present.

## Board and delivery rules

- The implementation tasks live as top-level Todo entries in dependency order so the scheduler can see them; they are not nested subtasks.
- Phase numbers remain at the start of task titles so ordering survives file renumbering.
- The initial review gate was satisfied by the committed task definitions and early Phase 0 baseline; later phases may run in dependency order from the recorded board state.
- Each completed phase records its exact changes and checks in its task before moving to Done.
- Existing unrelated Backlog tasks are preserved.
