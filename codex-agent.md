# Codex Agent Feature

## Goal

Add a first-class `clt agent` command group that lets `clt` manage Codex automation across many registered projects while keeping each project's tasks in its own repository-local `tasks/` board.

The user-facing model should stay simple:

```bash
clt agent register .
clt agent projects
clt agent run --once
clt agent daemon
clt agent start
clt agent stop
clt agent status
clt agent logs
```

There should be one installed app: `clt`. The background worker is part of `clt`, not a separate product or required second binary.

## Product Principles

- Keep `clt` as the only command users need to remember.
- Keep project task state file-backed, local, human-readable, and git-friendly.
- Use a central registry only for cross-project coordination and runtime state.
- Make the foreground/debug workflow and the background/service workflow use the same scheduler code.
- Keep the daemon scheduler responsive while Codex runs are active; long-running child processes must not block registry polling or status output.
- Run at most one Codex task per project at a time.
- Prefer conservative defaults: low concurrency, explicit registration, clear logs, and visible failure states.

## Storage Model

Task data remains in each project:

```text
<project>/
  tasks/
    todo/
    doing/
    done/
```

The agent registry lives outside any one project:

```text
~/.local/state/clt/agent.db
```

On macOS this can map to:

```text
~/Library/Application Support/clt/agent.db
```

The code should centralize path selection behind a small state-dir helper so Linux, macOS, and tests can use different roots without spreading platform checks through the scheduler.

## Turso SQL Registry

The first version should use Turso's pure-Rust SQLite-compatible database crate for a durable registry and execution ledger.

This choice is intentional:

- It keeps `cargo install` friendly for users who do not have a C compiler.
- It preserves a familiar SQL schema and migration model.
- It avoids storing opaque serialized Rust objects as the long-term data format.
- It keeps future schema evolution in normal SQL migrations instead of ad hoc object migrations.

Turso is currently beta, so all database access should sit behind an internal `AgentStore` module or trait. Scheduler, service, and Codex execution code should call store methods rather than embedding SQL directly. If the storage engine needs to change later, the blast radius should stay inside that store layer.

Suggested tables:

```sql
schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at TEXT NOT NULL
);

projects (
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  registered_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_scan_at TEXT,
  last_run_at TEXT,
  last_success_at TEXT,
  last_failure_at TEXT,
  failure_count INTEGER NOT NULL DEFAULT 0
);

runs (
  id INTEGER PRIMARY KEY,
  project_id INTEGER NOT NULL REFERENCES projects(id),
  status TEXT NOT NULL,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  exit_code INTEGER,
  log_dir TEXT,
  stdout_path TEXT,
  stderr_path TEXT,
  summary TEXT
);

leases (
  project_id INTEGER PRIMARY KEY REFERENCES projects(id),
  holder TEXT NOT NULL,
  acquired_at TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
```

The schema can be adjusted during implementation, but the store should support these behaviors from the start:

- Register and unregister projects.
- Enable and disable projects.
- List project status.
- Record every Codex run.
- Recover from crashed workers by expiring stale leases.
- Back off projects that repeatedly fail.

## Command Design

### `clt agent register [path]`

Registers a project path. Defaults to the current directory. The command should resolve the same project root that normal `clt` uses unless `--local` is provided.

Expected behavior:

- Confirm the project has an initialized `tasks/` board.
- Store a canonical absolute path.
- Use the directory name as the display name by default.
- Be idempotent when the project is already registered.

### `clt agent unregister [path]`

Removes a project from the registry. It should not delete project tasks or logs.

### `clt agent projects`

Lists registered projects with enabled state, last run status, last success, last failure, and basic pending-task signal.

### `clt agent run --once`

Runs one scheduler pass in the foreground. This is the easiest way to test the feature and the right primitive for cron-like usage.

Expected behavior:

- Load enabled projects.
- Find projects with available `todo` tasks and no active lease.
- Start Codex runs up to the configured concurrency limit.
- Stop when all runs started by the current pass have completed.

### `clt agent daemon`

Runs the scheduler loop in the foreground. This command is what service managers should execute.

Expected behavior:

- Poll registered projects.
- Continue polling and printing active-run status while Codex child processes are running.
- Respect concurrency limits.
- Use leases.
- Apply failure backoff.
- Write logs.
- Handle Ctrl-C cleanly.

### `clt agent start`

Installs or starts the platform service that runs `clt agent daemon`.

On macOS, this should use launchd. On Linux, this should use systemd user services. Initial implementation can support the current platform first, but the command design should leave room for both.

### `clt agent stop`

Stops the background service.

### `clt agent status`

Shows whether the service is running, where the registry lives, current projects, active leases, and recent run results.

### `clt agent logs`

Shows recent agent logs. Useful options later:

```bash
clt agent logs --follow
clt agent logs --project .
clt agent logs --run <id>
```

## Codex Run Behavior

Each project run should execute a prompt equivalent to the current shell script:

1. Inspect the task board using `clt`.
2. Pick the next available ready/todo task.
3. If no task exists, print `NO_TASKS_LEFT`.
4. Move exactly one task to `doing`.
5. Complete that task.
6. Run relevant checks/tests.
7. Update the task board.
8. Mark the task done if completed.
9. Stop after one task.

The Rust implementation should invoke Codex with:

```bash
codex exec --sandbox workspace-write "<prompt>"
```

The prompt should live in Rust as a template or in a bundled text file. It should be easy to update without touching scheduler logic.

## Scheduling Rules

Default settings should be intentionally modest:

```text
max_global_jobs = 12
max_jobs_per_project = 1
poll_interval_seconds = 30
run_timeout_minutes = 45
success_cooldown_seconds = 5
failure_backoff_seconds = 300
lease_timeout_minutes = 60
```

Later, these can be configurable through a config file or command flags.

The scheduler should:

- Skip disabled projects.
- Skip projects without initialized tasks.
- Skip projects with an active non-expired lease.
- Skip projects in failure backoff.
- Prefer projects with pending `todo` tasks.
- Treat `NO_TASKS_LEFT` as a clean idle result.
- Stop or mark failure on timeout.

## Async Daemon Architecture

The daemon should use a Tokio runtime and split scheduling from run execution:

- The foreground daemon loop wakes every `poll_interval_seconds` and scans every enabled project, even while previous Codex runs are still active.
- The scheduler acquires DB leases and spawns run supervisors as Tokio tasks up to `max_global_jobs` and `max_jobs_per_project`.
- Active run supervisors own the Codex child process, stream or capture stdout/stderr, emit heartbeat/status output, enforce the timeout, record the run result, and release the DB lease.
- The main scheduler loop keeps an in-memory active-run table keyed by project id and reconciles it with DB leases each pass.
- If capacity is full, pending projects should be reported as deferred instead of being invisible while another project is running.
- `Ctrl-C` should initiate graceful shutdown: stop starting new runs, signal active Codex children, record interrupted/failed results, and release leases owned by the current daemon where possible.

`CLT_AGENT_MAX_GLOBAL_JOBS` should allow operators to lower or raise the global capacity without recompiling. Even when the configured capacity is full, the daemon should remain observably alive and continue to scan other projects every poll interval.

## Logging

Each run should have durable logs. A reasonable location:

```text
~/.local/state/clt/runs/<project-slug>/<run-id>.out
~/.local/state/clt/runs/<project-slug>/<run-id>.err
```

The project can also continue to support repo-local `.codex-task-runs/` later if that is useful, but central logs make `clt agent logs` straightforward.

## Locking And Safety

Use the Turso-backed DB lease as the source of truth for agent coordination. The Rust daemon should not use repo-local lock directories as a hard gate; stale file locks can block valid work and are not necessary when all first-class agent runs coordinate through the central registry.

The agent must never delete or rewrite unrelated project files. Codex runs should inherit the same safety prompt currently used by the shell script.

## MVP Implementation Plan

1. Add `agent` subcommands to the Clap command tree.
2. Add state-dir resolution and Turso-backed registry initialization.
3. Implement project registration, unregistration, and listing.
4. Implement `run --once` with one-project-at-a-time scheduling.
5. Add Codex process execution with timeout and log capture.
6. Add leases and stale lease recovery.
7. Add `daemon` loop.
8. Add `status` and `logs`.
9. Add platform `start` and `stop` service management.
10. Document the feature in the README.

## Post-MVP Async Scheduler Plan

1. Refactor the foreground daemon into a Tokio event loop that can poll the registry while runs are active.
2. Move Codex execution into supervised async run tasks with timeout, heartbeat, result recording, and lease release.
3. Track active jobs in memory and report deferred projects when capacity is full.
4. Add shutdown handling that terminates active children and releases current-daemon leases cleanly.
5. Extend tests to cover concurrent active runs, serial capacity with continued polling, stale active-run reconciliation, and shutdown cleanup.

## Open Questions

- Should `register` auto-run `clt init` when tasks are missing, or should it ask the user to initialize explicitly?
- Should the agent ever resume tasks already in `doing`, or only pick from `todo`?
- Should there be a project priority field for scheduling order?
- Should logs be central-only, project-local-only, or both?
- Should a run that leaves a task in `doing` be marked as blocked, failed, or needs-review?
