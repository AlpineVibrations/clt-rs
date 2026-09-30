# Architecture

CLT publishes one Cargo package containing the command and its patched database engine.
The command has one application entry point:

```text
src/main.rs -> run() -> cli::run()
```

`src/main.rs` includes `src/lib.rs`, which declares the application's private modules
and its `run() -> anyhow::Result<()>` entry point. The application's code remains in
the Rust 2024 binary target. The package's `clt_database` library target compiles the
vendored Turso core with its upstream Rust 2021 edition and includes the SDK kit and
local Rust API as modules. Both targets ship in the same `clt-rs` archive; there are
no separately published database forks or nested Cargo packages.

The engine's feature configuration is fixed in `build.rs` to match the previously
used Turso dependency. Application modules import its local API through
`clt_database::turso`; database implementation types stay out of application facades.

## Module map

| Module | Responsibility | Principal dependencies |
| --- | --- | --- |
| `cli` | Clap command definitions and command dispatch | application services, task commands, scheduler, TUI |
| `application` | User-facing workflows that combine task state with agent and Git policy | task, agent facade, managed Git, platform |
| `task` | Typed statuses, task parsing, Markdown/folder storage, locks, ordering, archive, nested boards | standard-library filesystem only |
| `agent` | Agent domain records, configuration, migrations, and the store facade | repositories, one owned Tokio blocking adapter |
| `agent::recovery` | Atomic external registry snapshots, lifetime/write locks, quarantine and exclusive reconstruction | agent store and filesystem |
| `agent::repositories` | Projects/models, workers/leases, sessions/runs, and Git-journal persistence | Turso and agent-domain records |
| `platform` | launchd/systemd, executable snapshots, process groups, and terminal/process adapters | operating-system APIs |
| `managed_git` | Git preflight, immutable launch boundaries, commit proof, publication, and recovery | task services and agent journals |
| `manual_sessions` | Atomic direct-session task creation, claiming, and explicit handoff | task board, application moves, agent ownership checks |
| `scheduler` | Pure scheduling decisions, scans, cooldowns, lease acquisition, and daemon passes | agent store and worker orchestration |
| `worker` | Worker reservation, dispatch, heartbeat, reconciliation, task/session linking, and result recording | scheduler decisions, runner, platform |
| `runner` | Codex prompt/command construction, gated launch, supervision, logs, and outcome classification | process adapters and session/store services |
| `session_control` | Stop, resume, interrupt, interactive handoff, guardian lifecycle, and durable Todo planning conversations via the local Codex app-server | agent store, runner, platform |
| `session_recovery` | Reattach supervision to a surviving automated process, retain its logs and generation, and deliver controls through stable OS identities | agent store, platform, scheduler |
| `skills` | Embed bundled Codex skills and install them into the user's home agents directory | standard-library filesystem and terminal I/O |
| `tui` | `TuiApp` state, pane reducers, explicit effects, terminal ownership, and pure rendering | application services and cached snapshots |

## Boundary rules

- `task` does not depend on agent or TUI code.
- CLI and TUI call application/store facades; they do not contain SQL.
- TUI render functions read cached `TuiApp` state and perform no I/O.
- Scheduler decision functions are separate from acquisition and worker effects.
- Direct-session ownership is persisted as `clt:manual` beside the task's terminal conversation marker. Creation publishes directly into Doing; claiming writes ownership before moving; handoff moves into Todo before removing ownership. The project board lock serializes claims with scheduler and interactive lease acquisition. Manual tasks anywhere on an unfinished board fence the project, including stale scheduler snapshots and interrupted Doing recovery. Editing and ordinary moves preserve claims; Done and deletion release them. Claims require an idle project and a conversation without prior automated work, leaving existing automated Git journals to their established controls.
- Todo launch selection follows board order and skips blocked or stopped tasks. The runner resumes the first ready task’s attached conversation, validates its unique task link and lack of prior automated work, and holds the board lock through gated session registration. Planning sessions begin a fresh Git boundary; interrupted automated sessions retain their existing journal.
- Scheduler passes persist missing/uninitialized/unreadable board scans and skip those projects before recovery reads, so unavailable storage cannot initialize a replacement board or block unrelated jobs. Platform startup bounds launchd unload/bootstrap retries and retains permanent failure diagnostics.
- Project and existing-board probes preserve metadata errors; only not-found errors indicate missing storage. TUI snapshots distinguish local readability from the daemon's saved scan, without overwriting its access evidence. Reconnection tests restore the same registered path and verify scheduling resumes with the original board.
- Persistent agent commands use the store blocking adapter's durable update boundary. A writer lock and dirty marker cover the DB-to-snapshot interval; live stores hold shared access until every Turso handle is dropped. Recovery takes exclusive access, preserves DB and WAL together, and refuses ambiguous reconstruction.
- An interrupted snapshot export may be repaired automatically from the original DB/WAL under exclusive access. Automatic dirty recovery rechecks process ownership from committed database rows before checkpointing and publishing a fresh snapshot; it never reconstructs from the stale external snapshot. Scheduler recovery runs on a blocking worker after failed pass handles are dropped and only without active inline runs. Failed repairs retain their quarantine and progress marker to prevent repeated destructive retries.
- Snapshot lock conflicts retry only the snapshot transaction, with fresh connections and bounded attempts, while preserving that writer boundary. Completed mutations are never replayed by snapshot retries.
- Registry writer-lock acquisition is bounded. Exclusive recovery verifies the original database, releases only its maintenance checkpoint pin, truncates the WAL through the engine, and verifies integrity again before publishing a snapshot. Idle opens request this maintenance above 64 MiB; live stores and worker/session processes defer it. Successful routine maintenance deletes only its own backup after verified teardown and durable snapshot publication; manual and failed recovery bundles remain quarantined. A 128 MiB WAL admission guard under the writer lock rejects ordinary opens before migrations and every durable update before polling its future or creating the dirty marker. Already-open stores obey the same guard; recovery bypasses it to permit checkpointing.
- Turso rows are mapped to agent-domain records inside `agent::repositories`.
- The pinned Turso core carries a local reader-ownership fix under `vendor/`; its provenance and patch are documented there. Keep the checkpoint pin and partial-checkpoint, overlapping-store, and interrupted-WAL regressions when updating the dependency.
- Production modules use explicit imports. There is no crate-root prelude or transitional
  re-export layer.

## Tests

Unit tests live under the module that owns the behavior, for example `src/task/tests.rs`,
`src/agent/tests.rs`, and `src/tui/tests.rs`. Shared fixture helpers are in
`src/test_support.rs` and are compiled only for tests. `tests/cli.rs` remains a black-box
integration suite for the installed command contract.

`clt skills install` is dispatched before task-root discovery. Its installer reads
the same embedded skill text that the agent prompt fallback uses and resolves the
user home from `HOME` on Unix or `USERPROFILE` (with a drive/path fallback) on
Windows. Unit tests cover overwrite decisions; the CLI suite checks that the
command works without a task board.

The required verification gates are:

```bash
rustfmt --edition 2024 --check build.rs src/main.rs src/lib.rs tests/architecture.rs tests/cli.rs
cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```
