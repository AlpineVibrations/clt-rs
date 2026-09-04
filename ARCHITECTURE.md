# Architecture

CLT is an application crate with one supported Rust entry point:

```text
src/main.rs -> clt_rs::run() -> cli::run()
```

`src/main.rs` only launches the library. `src/lib.rs` declares private modules and exposes
`run() -> anyhow::Result<()>`; no domain, persistence, scheduler, or TUI type is part of the
public crate API.

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
| `scheduler` | Pure scheduling decisions, scans, cooldowns, lease acquisition, and daemon passes | agent store and worker orchestration |
| `worker` | Worker reservation, dispatch, heartbeat, reconciliation, task/session linking, and result recording | scheduler decisions, runner, platform |
| `runner` | Codex prompt/command construction, gated launch, supervision, logs, and outcome classification | process adapters and session/store services |
| `session_control` | Stop, resume, interrupt, interactive handoff, and guardian lifecycle | agent store, runner, platform |
| `tui` | `TuiApp` state, pane reducers, explicit effects, terminal ownership, and pure rendering | application services and cached snapshots |

## Boundary rules

- `task` does not depend on agent or TUI code.
- CLI and TUI call application/store facades; they do not contain SQL.
- TUI render functions read cached `TuiApp` state and perform no I/O.
- Scheduler decision functions are separate from acquisition and worker effects.
- Persistent agent commands use the store blocking adapter's durable update boundary. A writer lock and dirty marker cover the DB-to-snapshot interval; live stores hold shared access until every Turso handle is dropped. Recovery takes exclusive access, preserves DB and WAL together, and refuses ambiguous reconstruction.
- Turso rows are mapped to agent-domain records inside `agent::repositories`.
- The pinned Turso core carries a local reader-ownership fix under `vendor/`; its provenance and patch are documented there. Keep the checkpoint pin and partial-checkpoint, overlapping-store, and interrupted-WAL regressions when updating the dependency.
- Production modules use explicit imports. There is no crate-root prelude or transitional
  re-export layer.

## Tests

Unit tests live under the module that owns the behavior, for example `src/task/tests.rs`,
`src/agent/tests.rs`, and `src/tui/tests.rs`. Shared fixture helpers are in
`src/test_support.rs` and are compiled only for tests. `tests/cli.rs` remains a black-box
integration suite for the installed command contract.

The required verification gates are:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```
