# Architecture

CLT publishes one Cargo package containing the command and its patched database engine.
The command has one application entry point:

```text
src/main.rs -> run() -> cli::run()
```

`src/main.rs` includes `src/lib.rs`, which declares the application's private modules
and its `run() -> anyhow::Result<()>` entry point. The application's code remains in
the Rust 2024 binary target. The package's `clt_database` library target compiles the
vendored Turso core, SDK kit and local Rust API with the same package-wide Rust 2024
edition, without deprecated target overrides. Both targets ship in the same
`clt-rs` archive; there are no separately published database forks or nested Cargo
packages.

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
| `supervisor` | Read-only blocked-work assessments, evidence-bound decisions, bounded exact-session retries | agent store, task services, runner |
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
- Supervisor evidence includes each candidate session's retry eligibility, so
  an explicit stop/resume invalidates stale ownership decisions without binding
  heartbeat timestamps or interchangeable idle recovery tokens. Review
  acquisition accepts stopped and resume-requested controls only when they have
  no child, interactive holder or pending interactive launch. Regression tests
  cover the existing task's stop/resume controls through review and exact-session
  retry in both board layouts, preserving Git history, journal and staged work.
- Scheduler decision functions are separate from acquisition and worker effects.
- Direct-session ownership is persisted as `clt:manual` beside the task's terminal conversation marker. Creation publishes directly into Doing; claiming writes ownership before moving; handoff moves into Todo before removing ownership. The project board lock serializes board changes and exact-session selection. Independent manual claims and automated work may coexist; scheduler scans, resume acquisition, blocked recovery and worker result inference exclude manual tasks. Interactive and Git recovery checks protect the selected conversation without fencing unrelated manual work. Creation and status moves preserve neighboring folder paths. Editing and moves to Backlog preserve claims; moves into Todo release the selected claim after publishing its destination. Done and deletion also release it. A conversation with prior automated work keeps its existing journal and controls.
- Completion can recover absent launch environment from the exact runtime `CODEX_THREAD_ID` and its running session-control record. The project, child liveness, unique token ownership and selected task session are checked before ordinary Git sealing/resealing; journal transactions still recheck the token. Stopped, interactive, missing and ambiguous records cannot supply an automated context. Explicit malformed launch environment remains an error. CLI tests cover recovery without launch variables and rejection of another task's completion.

- Managed Git launch verification fences HEAD, branch, upstream destination, and the index while allowing concurrent unstaged and untracked worktree edits. Activation separately validates the selected task against the starting commit; terminal-worker launch reclamation retains worker/session ownership fences. The original worktree baseline remains recorded, and finalization seals only the reviewed index.
- Project Git mode is a mutable default for subsequent launches. Settings writes preserve leases, workers, session modes, launch records, and journals. Task commands resolve the recorded session mode, including Git off; only launch preparation may promote previously unmanaged work to a managed phase. Resume preparation uses the worker's selected mode even if the default changes before registration. Unconsumed launch recovery validates the old boundary using its original mode before starting work with the new default, including Git off. Regressions cover both settings entry points, active owners, journal states, CLI task moves in both board layouts, and commit/publication behavior after settings edits.
- `managed_git::repair` handles only a selected Markdown Done entry displaced within an otherwise exact sealed commit. Under the finalization lease and board lock it verifies the journal, current branch tip, shared index, and advertised remote refs. Normal hooks/signing run in a detached temporary worktree; the resulting full tree, parent, identity and trailer must match the immutable seal. The real index is locked and only that entry's position is normalized, preserving other staged content. Index publication precedes a compare-and-swap branch update, so a retry after interruption can recognize the already-corrected staging. Other mismatches fail closed with concrete diagnostics. Tests exercise shared work, repeat reconciliation, publication, code/neighbor changes, hooks, locks, and scheduler roll-forward ahead of supervisor holds.
- Two consecutive exact-session failures or timeouts also supply supervisor candidates without relying on task BLOCKED notes; successful runs break the sequence. A Wait decision records a blocker before deferring such a task. Sealed supervisor reviews follow deterministic Git reconciliation. Evidence binds the board, seal generation and current HEAD; each candidate receives its proof diagnostic and saved repair error independently of truncated board context. Retry activation accepts the exact provisional Done session without rewriting its entry. Held decisions do not prevent Git proof from rolling forward, and repaired completion clears obsolete holds before scheduling another task.
- Git finalizer acquisition, renewal, and ownership checks share one session-control predicate in `agent::repositories::workers_leases`. Shared interactive reservations and exact guardians without an unfinished Git journal do not fence another task's commit proof or publication. Exclusive owners, automated workers, mismatched guardian tokens, and shared sessions with unfinished Git journals retain their fences; ordinary scheduling remains unchanged. Regression tests cover completion and local-remote publication with an older conversation open in both board layouts.
- Idle interactive reservations classify an unfinished automated Doing task under the board lock and compare the observed session state during reservation. Those tasks use the existing exec handback disposition, including queued resumes; concurrent stops, terminal journals, planning, completed, manual, and shared sessions retain their own controls. A disabled project leaves the exact resume request queued. Status moves check destination conversion guards only when moving a path-backed task could actually convert the destination store; ordinary Markdown moves preserve neighboring task journals.
- Completed context opening in `session_control::completed` checks the exact guardian and reads the task without moving it or writing ownership markers. Its prompt clears automated child ownership and scopes the conversation to discussion until the user requests work. Legacy `clt:interactive-done` entries still recover after guardian exit or proven absence; idle cleanup also handles missing or queued controls, removes obsolete manual claims, and preserves bound unfinished Git proof. Scheduler reconciliation precedes the manual-task gate and refreshes its board scan, so legacy markers cannot wedge later work. Terminal Git journals remain unchanged.
- User deletion and external completion share a session-, generation-, and nullable-identity-fenced cancellation transaction for idle `WORKING` journals. Under the board lock, deletion can cancel an idle session alongside workers and leases proven to belong to other conversations; it durably stops only the selected session before removing evidence and preserves unrelated ownership. Unknown worker/lease ownership and unconsumed launch boundaries remain project-wide fences. Other cancellations require an idle project and use a temporary project lease through the filesystem mutation. Failed activation may have no bound task identity; cancellation compares NULL exactly instead of requiring users to repair metadata. Idle unbound journals linked uniquely to Done can be retired automatically while preserving conversation links, checkout state, active owners, launch boundaries and sealed proof. Deletion preserves its Git boundaries and suppresses interrupted-work recovery if removal fails. Automated children retain the ordinary mutation guard. Regression tests cover both board layouts, deletion alongside another run, dead workers, changed journal generations/identities and sealed proof.
- Superseded WORKING recovery in `managed_git` holds the board lock, rejects any old session marker or partial seal, and requires one matching Done task in a different completed session. It rechecks that session's retained commit, parent, tree, task identity and publication configuration. The repository transaction compares both complete journal snapshots and all project ownership before cancelling only the obsolete attempt and clearing its exact cooldown. Scheduling runs this before supervisor holds and retry backoff; explicit reconciliation shares the same path. Regression tests cover both layouts, shared staging, live owners, incomplete proof, ambiguous tasks, seals and changed snapshots.
- Explicit missing-journal recovery lives in `application::git_recovery`. Its preview binds the failed run and task contents; the executor holds the project board lock and a repository-owned idle-project lease, stops the obsolete session, then publishes a stopped task move before detaching its old link and queueing a fresh attempt. Existing journals, launch records, owners, files, index and commits are preserved. Already-Done recovery only accepts the user's completion and clears the obsolete resume request. Automated runs cannot invoke this escape from a lost contract; their original fail-closed checks remain in place.
- Explicit session-selected restart shares the recovery transaction but binds the selected session's latest run independently of later supervisor assessments. Only idle unsealed or cancelled attempts can restart; sealed and verified proof remain fenced. Other WORKING journals must have uniquely linked blocked/stopped Todo entries. The old record and session remain historical, and the stopped publication moves only the selected task first before detaching its link for a fresh attempt. Other folder paths, files, index and commits are preserved.
- Fresh launch prompts identify the selected Todo and recheck its identity against the frozen commit under the board lock before spawning. Failed activation recovery proves task absence directly from that commit, using the newest exact-session or exact-worker terminal run. It accepts failed, blocked and timed-out results, missing run session IDs, and cleared idle controls. An unbound WORKING journal and a unique unfinished task remain required; the latest blocker must concern the missing-task activation failure before it is cleared. The recovery transaction rechecks run, journal, controls and project ownership before retiring the attempt; automatic recovery preserves queue order. The next scheduler pass scans again and prepares a fresh launch boundary. Stale unbound attempts also recover when the exact session's latest run finished failed, blocked or timed out and HEAD advanced on the same branch. Recovery precedes supervisor holds, accepts absent controls or the matching idle resume token, and transactionally rechecks the latest run, worker, enabled project and ownership. It rechecks the observed HEAD under the board lock, preserves task order and history, and queues fresh launch preparation. Explicit stops, live owners, new launch records, bound work and sealed proof do not qualify.
- Persistent Codex writer-conflict recovery lives in `application::git_recovery` and runs after stale-owner reconciliation, before supervisor holds. Two consecutive exact-session failures must have bounded, retained logs proving resume startup failed with the writer-conflict diagnostic and no agent output. The board lock rechecks the task and logs; the recovery transaction rechecks the latest two runs, terminal workers, enabled project, exact resume token, journal generation and all idle-owner fences. Only unfinished ready tasks with unsealed WORKING journals or recorded Git-off mode qualify. Recovery preserves queue order, files, index, commits and historical journals and queues fresh verification; it never changes Codex persistence or locks. Sealed proof and explicit stops retain their original recovery contracts.
- Branch-change recovery uses that application workflow with the reviewed branch and exact journal attached to its preview. The scheduler detects mismatched unverified journals under the finalization lease, records a deduplicated diagnostic, releases that lease, and automatically runs recovery through the board lock and idle-project reservation. The repository transaction rechecks the journal and owners, then cancels only the selected unverified attempt while retaining its boundary and manifest and stopping the old session. Linked provisional Done work is queued for fresh verification; orphaned attempts leave the current board unchanged, including tasks with replacement session links. The next scheduler pass re-scans and captures a fresh boundary. Paused projects, stopped sessions, verified commits and conflicting ownership remain fenced. The runner also checks the branch before spawning Codex as a last-line guard.
- Todo launch selection follows board order and skips blocked or stopped tasks. The runner resumes the first ready task’s attached conversation, validates its unique task link, distinguishes a first planning launch from idle prior automation, and holds the board lock through gated session registration. Planning sessions begin a fresh Git boundary; returned automated Todo tasks use resume preparation and the exact-session recovery prompt, retaining their existing journal. Selection and session classification are revalidated under the board lock before gated registration. After registering the exact child and binding its saved journal, CLT moves a ready returned Todo to Doing before releasing that child, without applying fresh-launch checkout checks to the older boundary; stopped/busy controls and missing-journal checks remain authoritative.
- Supervisor settings and the current per-project decision live in migration 20 and registry snapshot version 3 (older snapshots restore with no supervisor reviews). A scheduler gate runs before ordinary recovery can manufacture resume requests or activate queued work. Blocked Doing tasks retain assessment priority; a saved Wait decision defers an idle unsealed Doing task into blocked Todo before updating its evidence. Explicit stops and sealed proof are not deferred. Old Doing Wait holds receive one new assessment. Todo-only blockers yield to ready Todo work in both supervisor and legacy recovery selection, clearing obsolete holds without reordering the board. WORKING link repair preserves blocked/stopped Todo placement; both journal selection and passive session-resume selection yield queued blocked work to ready Todo, without clearing its resume request or proof. Reviewer log evidence is scoped to blocked candidates’ exact sessions; pending sealed Git transactions retain priority. Review workers use the ordinary durable lease, process supervision and result lifecycle, with Git off, a read-only sandbox, and a 180-second limit. Reviewer sessions never attach to task entries. Saved decisions precede retry activation; the retry worker applies the unblock note and resumes the original session with its journal and review direction. Evidence and eligibility are rechecked at acquisition and decision application. Held decisions stop unchanged scans; explicit retry clears the decision and budget. TUI rendering reads cached supervisor settings, and focus isolates the global controls from project edits.
- Scheduler passes persist missing/uninitialized/unreadable board scans and skip those projects before recovery reads, so unavailable storage cannot initialize a replacement board or block unrelated jobs. Platform startup bounds launchd unload/bootstrap retries and retains permanent failure diagnostics.
- Project and existing-board probes preserve metadata errors; only not-found errors indicate missing storage. TUI snapshots distinguish local readability from the daemon's saved scan, without overwriting its access evidence. Reconnection tests restore the same registered path and verify scheduling resumes with the original board.
- Persistent agent commands use the store blocking adapter's durable update boundary. A writer lock and dirty marker cover the DB-to-snapshot interval; live stores hold shared access until every Turso handle is dropped. Recovery takes exclusive access, preserves DB and WAL together, and refuses ambiguous reconstruction.
- An interrupted snapshot export may be repaired automatically from the original DB/WAL under exclusive access. Automatic dirty recovery rechecks process ownership from committed database rows before checkpointing and publishing a fresh snapshot; it never reconstructs from the stale external snapshot. Scheduler recovery runs on a blocking worker after failed pass handles are dropped and only without active inline runs. Interrupted repairs resume from the same validated quarantine under exclusive access, recheck committed process ownership, and retain dirty-snapshot evidence. Failed retries retain that original quarantine and progress marker without creating duplicate backups or reconstructing automatically. Snapshot loading and platform recovery share the supported-version check.
- Snapshot lock conflicts retry only the snapshot transaction, with fresh connections and bounded attempts, while preserving that writer boundary. Completed mutations are never replayed by snapshot retries.
- Registry writer-lock acquisition is bounded. Ordinary store operations retain checkpoint pins only while their futures run, explicitly rolling back on completion; idle handles do not pin old WAL generations. Under the writer lock, opens before migrations and durable updates before mutation/dirty marking attempt engine-managed WAL truncation at 64 MiB. Real readers may defer it; the 128 MiB admission guard remains a fallback for legacy clients and long reads. Recovery retains its lifetime pin until integrity verification, releases only that pin, truncates through the engine, and verifies integrity again before publishing a snapshot. Idle opens also retain exclusive maintenance; live stores and worker/session processes defer that path. Successful routine exclusive maintenance deletes only its own backup after verified teardown and durable snapshot publication; manual and failed recovery bundles remain quarantined. Recovery bypasses the admission guard to permit checkpointing.
- Turso rows are mapped to agent-domain records inside `agent::repositories`.
- The pinned Turso core carries a local reader-ownership fix under `vendor/`; its provenance and patch are documented there. Keep operation checkpoint pins and the partial-checkpoint, overlapping-store, live cross-process WAL truncation, and interrupted-WAL regressions when updating the dependency.
- Production modules use explicit imports. There is no crate-root prelude or transitional
  re-export layer.

## Tests

`application::activity` resolves worker activity using the exact worker token,
registered session and task marker, including nested boards. It never selects a
task from queue position alone. TUI refresh snapshots and CLI status share this
read-only projection; rendering uses the cached activity. Worker attachment
checks the frozen Todo identity, and the task writer refuses to replace another
session's marker. Recovery prompts include the exact session and linked task.
Deletion may coexist with read-only supervisor workers because review decisions
recheck board evidence under the same board lock. Selected-session owners,
unregistered implementation launches and sealed Git proof remain fenced.
Regressions cover cross-task attachment, both board layouts, review-time deletion,
task moves and nested activity, and visible ownership alongside errors and logs.

`runner::app_server` adapts implementation launches to a private JSONL stdio
server behind the existing exec gate and session supervisor. The adapter keeps
the registered child PID and process group, inherits launch environment and Git
identity, and translates model/provider/reasoning/Fast/goal configuration without
changing the prompt or launch record. Reviews retain their structured exec path.
Root-thread events produce the existing session/settings header, live stderr and
final stdout. Goal continuations stay server-owned; failure, interruption and
disconnect cannot become successful completion.

`runner::app_server::log` formats root-thread events into the plain-text transcript
shared by CLI and TUI logs. It keeps command output streaming, suppresses repeated
completion aggregates by item ID, retains aggregate-only output and failures, and
omits empty reasoning and opaque protocol metadata. Formatter regressions and the
private-peer integration test verify readable output without changing final stdout,
session/settings headers, steering or thread filtering. Historical logs are unchanged.

`session_control::steering` owns a private mailbox beside each run's logs. The
TUI and owning worker recheck the unique Doing task and the saved session,
PID, run token and active turn before publication/delivery. Only the worker
writes `turn/steer`, guarded by `expectedTurnId`. Atomic per-request receipts
acknowledge the successful RPC response; queued files and transport failures
never imply acceptance. Stale runs cannot route messages into their successors.
Composer work runs off the UI thread and never calls task editing APIs. Protocol,
mailbox and TUI regressions cover both board layouts, nested boards, stale turns,
changed ownership, disconnects, drafts, goal continuations and task preservation;
a supervised private-peer integration test covers active delivery and cancellation.

Blocked worker completion returns only its uniquely linked, unsealed Doing task
to Todo under the board lock after session control settles. It preserves unrelated
tasks, journal state, explicit stops, manual ownership and live session owners.
WORKING link repair recognizes session markers before the stop suffix and keeps
blocked/stopped Todo placement in linked, missing-marker and duplicate-move
recovery. Idle resume requests also honor stopped task markers, even without
ready Todo work, and do not reserve the project against other ready tasks.
Direct claims reserve individual conversations and can coexist with active work
and Git proof. Regressions cover both board layouts, independent concurrent claims,
exact-session exclusion, preserved task paths, and scheduler finalization alongside
manual work and an older interactive conversation.

Unit tests live under the module that owns the behavior, for example `src/task/tests.rs`,
`src/agent/tests.rs`, and `src/tui/tests.rs`. Shared fixture helpers are in
`src/test_support.rs` and are compiled only for tests. `tests/cli.rs` remains a black-box
integration suite for the installed command contract.

The runner records the requested Fast setting as the first line of each automated
run's stderr log. The log footer reads that launch record and Codex's complete
startup header from the displayed run, preserving historical settings independently
of current project settings. Missing records stay unknown; task/output text cannot
replace the recorded settings.

`clt skills install` is dispatched before task-root discovery. Its installer reads
the same embedded skill text that the agent prompt fallback uses and resolves the
user home from `HOME` on Unix or `USERPROFILE` (with a drive/path fallback) on
Windows. Unit tests cover overwrite decisions; the CLI suite checks that the
command works without a task board.

The required verification gates are:

```bash
cargo fmt --all -- --check
cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

The supervisor stop/resume recovery regressions can also be run directly:

```bash
cargo test --locked --bin clt supervisor::tests
```
