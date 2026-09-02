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
    backlog/
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

Removes a project from the registry. It should not delete project tasks or logs, and it must refuse while a nonterminal Git finalization or unconsumed pre-registration launch boundary remains.

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
4. Move exactly one task to `doing`. With Git automation enabled, the task must already exist in `HEAD`; the scheduler has already synchronized and frozen the checkout before releasing Codex. The command rechecks that launch state and binds it to a durable `WORKING` journal before this board mutation. Codex must not pull, synchronize, or switch branches itself.
5. Complete that task.
6. Run relevant checks/tests.
7. Update the task board.
8. Mark the task done if completed. In a Git-enabled automated run this starts a provisional, persisted `FINALIZING` transaction rather than immediately making the task terminal.
9. Create one normal task commit whose full tree exactly matches the sealed implementation, completion note, and projected board transition, with one exact `CLT-Task: codex:<session-id>` trailer. Never push from Codex, including in commit-and-push mode.
10. Stop after one task. CLT proves the local commit and, when configured, publishes it itself. Local-finalization interruption resumes the same session; `PUSH-PENDING` publication does not.

The Rust implementation should invoke Codex with:

```bash
codex \
  --sandbox danger-full-access \
  --ask-for-approval never \
  exec \
  -C "<project-path>" \
  "<prompt>"
```

This removes the Codex command sandbox so automated runs can update Git metadata and
cannot pause waiting for an approval response. Only enable the agent for trusted
repositories or run it inside an externally isolated container or VM.

The prompt should live in Rust as a template or in a bundled text file. It should be easy to update without touching scheduler logic.

## Durable Git Finalization Contract

For a fresh project run in `commit` or `commit-and-push` mode, checkout selection and required branch/upstream configuration must be complete before scheduling; detached HEAD is rejected. Before spawning or releasing Codex, the scheduler requires the Git index to match `HEAD` and performs the safe fast-forward-only startup sync unless an older `WORKING` journal still depends on the current history. In that case it deliberately preserves the current commit so the older task remains provable. It captures the resulting `HEAD`, attached branch, unstaged/untracked baseline, and upstream configuration and persists that server-owned launch record. Any spawned child remains behind its launch gate until the remaining session fences are registered. If synchronization, capture, persistence, or registration fails, Codex must not execute the agent prompt.

The launch record initially exists before a Codex session can be registered. This unconsumed pre-registration boundary is immutable: neither the same run nor a replacement worker may overwrite or recapture it from a later checkout. If Codex exits before announcing a session, its supervisor first reaps the exact child and terminalizes that worker generation without erasing the boundary. CLT reclaims it automatically only when the exact worker is terminal, no session-control row owns its run token, and the checkout and Git mode match the frozen snapshot. Any uncertainty or changed checkout fails closed and prevents later project work. Unregister and clean also refuse to erase this evidence.

The selected Todo task must already have the same durable identity in the frozen starting commit. This makes task creation and task execution separate commit boundaries: a user or prior workflow commits the task definition once, then the automated run makes the single implementation-and-completion commit.

The Todo-to-Doing command verifies the server-owned launch state has not changed and binds it to the session's durable `WORKING` journal before moving the task. Once released, Codex may inspect Git, implement the task, and create the sealed commit. It never pushes and must not run a startup pull, fetch/synchronize, merge, rebase, switch branches, reset history, or reconfigure the destination; those are CLT-owned operations, not agent work.

Managed folder-backed moves preserve representation and order. When the source task is a path, Todo-to-Doing and Doing-to-Done rename that same file or directory into a directory-backed destination without conversion or renumbering. Prelaunch rejects Todo-directory to Doing-Markdown and Doing-directory to Done-Markdown layouts. If a crash leaves identical session-linked source and destination copies, recovery removes the duplicate and completes the intended state without disturbing unrelated tasks; mismatched or ambiguous copies remain fenced.

When implementation and checks are complete, Codex runs all known file-mutating formatters and hook checks, records the completion note, and stages the implementation plus the active Doing task and its terminal session marker. `clt done` verifies the baseline, task identity, branch, history, and staged task scope. In a private temporary index it projects only that task into Done and records the resulting full repository tree as the immutable commit manifest. The worktree's Done entry is provisional; its physical location is not success.

Codex then stages the real board transition and creates exactly one ordinary, one-parent commit. CLT accepts it only when the complete committed tree equals the seal and the task identity, manifest parent, CLT Agent author/committer identity, and one exact `CLT-Task: codex:<session-id>` trailer all agree. A second board-only commit, an equivalent same-path rewrite, or an earlier unproven CLT Agent commit does not satisfy the contract. If a hook changes files or fails after sealing, Codex stages the complete corrected payload and runs `clt done done <index>` to reseal the provisional entry before retrying that same one-commit operation.

Commit-and-push resolves Git's effective push remote at launch using `branch.<name>.pushRemote`, then `remote.pushDefault`, then the upstream remote, and freezes the chosen remote, its one concrete push URL, and the upstream `refs/heads/...` merge ref. After proving the local commit, CLT—not Codex—pushes that immutable exact OID to the URL/ref with an explicit non-force refspec, so implicit routing, default refspecs, and later configuration changes cannot redirect publication. Normal pre-push hooks and configured signed-push policy still apply. `PUSH-PENDING` becomes terminal only after CLT independently queries and fetches that frozen destination and proves containment. A successful push exit or stale local remote-tracking ref is insufficient.

The journal makes recovery a roll-forward state machine. `FINALIZING` local work resumes the exact session, checks for an already-created commit, and performs only the first unproven local step. `PUSH-PENDING` is different: the scheduler retries CLT's bounded publication without launching or resuming Codex and blocks later work in that project until settlement. Scheduler-owned reconciliation uses a transactionally acquired, renewable exact-holder lease; live workers or controls prevent acquisition, and ownership is rechecked around every mutation and remote side effect. Shared writable interactive sessions are refused while Git launch or finalization proof is active. A durably blocked `WORKING` journal may yield to another Todo while blocked-recovery backoff is active; CLT preserves its journal and reachable history and skips startup synchronization for the later task. It never creates a replacement completion commit merely because acknowledgement was interrupted. If completed-task evidence exists but the frozen start journal has disappeared, CLT fails closed rather than guessing the original branch, parent, baseline, or ownership boundary.

Dirty unstaged work is supported through the captured baseline. The shared Git index is intentionally a cooperative boundary: CLT refuses pre-existing staged changes at fresh launch and later verifies the exact seal, but Git provides no actor ownership for a clean-file change staged concurrently. Humans, interactive sessions, and parallel tools sharing that checkout must leave the index untouched while automated finalization owns it.

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
- Before starting fresh `todo` work, revisit one blocked task from either `todo` or `doing` whenever its recovery backoff permits and re-evaluate whether the blocking conditions still exist.
- Either finish that blocked task, requeue the same task with a newer `UNBLOCKED YYYY-MM-DD:` note after resolving its blocker, or refresh its blocked note.
- Back off an unresolved blocked-task recovery so the monitor does not create a tight Codex retry loop, while allowing ready Todo work to proceed during that recovery-specific backoff.
- Do not treat unmarked `doing` tasks as abandoned work without a stale or expired agent lease.
- Ignore backlog-only projects until a task is promoted to `todo`.
- Keep fresh Git-enabled Codex execution gated until scheduler-owned startup synchronization and durable launch-state persistence both succeed.
- Resume unresolved `FINALIZING` work in its exact Codex session and roll forward from the first unproven local-commit step.
- Retry `PUSH-PENDING` entirely inside CLT without starting Codex, and do not run a later project task until publication settles.
- Resume a ready `WORKING` journal exactly. If its linked task is durably blocked and blocked-recovery backoff is active, preserve the journal and history but allow another ready Todo; skip startup sync so the older boundary remains reachable.
- Treat a provisional Done entry, process exit, HEAD movement, or successful push exit alone as insufficient proof. Match the exact sealed tree, parent, task marker, and `CLT-Task` trailer, and in push mode prove exact-OID containment at the frozen remote destination.
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
