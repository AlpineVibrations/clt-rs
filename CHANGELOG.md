# Changelog

All notable changes to this project are documented in this file.

This project follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) style sections and uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) for releases.

## [Unreleased]

### Fixed

- Show the full `INTERACTIVE` agent status in Agent Projects and explain why queued tasks wait when a project is reserved.

## [0.7.0] - 2026-09-24

### Added

- Add `clt skills install` to install bundled Codex skills into the user's `.agents/skills` directory on macOS, Linux, and Windows, with per-file overwrite prompts and a `--force` option.

### Fixed

- Allow Codex planning conversations on stopped Todo tasks while preserving their stop marker and session link; press `s` to explicitly make the task available for automation afterward.

### Changed

- Teach the task-management skill to link tasks created by standalone Codex sessions to their current conversation, preserving terminal marker placement and existing task identities.
- Keep README focused on installation and quick start; move detailed usage into `features.md`, preserve feature proposals in `docs/FEATURE_IDEAS.md`, and record the documentation split in project and agent guidance.

## [0.6.22] - 2026-09-24

### Fixed

- Allow unfinished tasks started with Git off to resume with commit or commit-and-push after Git is enabled while paused, preserving their session, existing implementation, and staged work.
- Persist session Git modes and recover older unmanaged sessions from verified launch logs, while retaining protection against missing managed Git journals.

## [0.6.21] - 2026-09-23

### Fixed

- Cancel failed agent launches while preserving their diagnostics and allowing the scheduler to continue.
- Show agent enablement in the Kanban console title for registered projects.

## [0.6.20] - 2026-09-21

### Fixed

- Allow Todo planning conversations while another Codex session is queued for recovery without a worker or interactive owner; preserve the queued session and its Git journal.

## [0.6.19] - 2026-09-20

### Added

- Press `c` on an unlinked Todo to create a Codex planning conversation with its full task context, save its session link, and return to the same Todo. Linked Todos can reopen their conversation, and normal automation can still claim a planned task.

- Let the Models page store a provider API key in the CLT registry with `k`. A stored key is injected into the Codex child as that provider's `env_key`, outranks the environment variable, is used for model discovery, and is shown only as its source (`clt`, `env`, `env-missing`, or `none`). Keys are kept in the owner-only agent state directory and retained by registry recovery snapshots.

### Fixed

- Preserve normal Shift capitalization and keyboard-layout text while adding or editing tasks; restore extended shortcut reporting when text entry closes.

- Allow automated Git tasks to start with existing staged changes; preserve staged entries through task-board checkpoints, freeze the launch index, and skip startup pulls in dirty checkouts.

- Keep the Kanban console title's agent status visible for registered projects whose agent is `OFF`; omit it only for unregistered projects.
- Show the registered project's configured Codex model, reasoning effort, and fast setting alongside the agent status in the Kanban console title, naming the resolved target in parentheses when the project inherits the CLT default.
- Remove the task board's `d` and `D` shortcuts to prevent accidental permanent deletion; keep `Delete` available for intentional deletion and `a` for archiving.
- Allow task commits and resealing while unrelated unstaged edits or new files appear, preserving that work outside the exact staged commit.
- Allow manual completion of an idle session-linked task after its text was edited or its work committed externally, while preserving active-owner and sealed-proof checks.
- Retire task-less managed Git journals left behind by a run that ended before claiming a task; the project no longer wedges with `reason=active_lease` or a permanent `WORKING` journal, and a resumed run adopts such a journal so it can still be retired later.

## [0.6.16] - 2026-09-16

### Fixed

- Allow CLI unregister and TUI project removal with unfinished Git finalizations or launch boundaries. Clear the project's stored agent state without changing its files or Git checkout, while retaining active worker and lease checks.

## [0.6.15] - 2026-09-16

### Added

- Add `clt --version` and `clt -V` to report the package version without opening a task board or agent registry.

### Fixed

- Bundle the patched Turso engine and local Rust API directly in the single `clt-rs` package, so crates.io installs retain the registry fixes without separately publishing fork crates.
- Reject builds whose database core lacks the required CLT patch marker, verify the actual publishable archive in CI on Linux and macOS, and remove unused vendored sync packages and standalone package scaffolding.

## [0.6.14] - 2026-09-14

### Fixed

- Prevent idle registry clients from rebuilding the shared WAL index from an outdated local scan, restoring stale leases or settings, and corrupting database pages after another process commits.
- Preserve the original Turso index-page failure when an update requires recovery, and stop further database operations on the affected handle.

## [0.6.13] - 2026-09-13

### Fixed

- Allow Git task completion when a concurrent user commit already includes the frozen baseline changes or implementation, preserving the original journal and validating the remaining staged task against the current parent.
- Keep completed-task and project logs visible when a later Git-recovery acknowledgement has no output paths, using the recorded output from the same Codex session.

## [0.6.12] - 2026-09-13

### Fixed

- Restore selected-task output from its exact session record when a stale worker never saved run history. Allow `c` to open an inactive session queued for recovery after acquiring exclusive ownership, and keep it stopped on return. Show continuation errors instead of hiding them behind an open log.

## [0.6.11] - 2026-09-11

### Fixed

- Save the exact task-to-session link when automated runs with Git automation off move a task to Doing, so the selected task's log and session controls are immediately available.

## [0.6.10] - 2026-09-11

### Added

- Show the current project's agent status after the Kanban console title when its registered agent is enabled.

### Fixed

- Detect damaged agent registry indexes during normal use and automatically repair supported worker-index damage when the registry is idle, preserving task history and a backup of the original database.
- Coordinate stale-service restarts across open TUIs with a shared cooldown, and explicitly release registry and restart locks even when child processes inherit their file handles.
- Require a saved session before resuming a Doing task after a worker or lease expires, preventing the scheduler from claiming manually started tasks.
- Prevent concurrent managed Git projections from failing on temporary directory name collisions while preserving private permissions and automatic cleanup.
- Allow ordinary user commits, including patch-version bumps, and CLT board checkpoints while an agent task is in progress. Preserve the original launch journal and seal or reseal the task against the current branch parent without discarding concurrent work.

## [0.6.9] - 2026-09-10

### Fixed

- Label the console explicitly as `Log View` while agent output is open, clear previous console feedback when opening logs, and prevent Agent Projects refresh errors from covering the displayed output.

## [0.6.8] - 2026-09-09

### Fixed

- Reattach supervision to surviving automated Codex processes after their worker exits, preserving the session, current work, and output while restoring stop and interactive takeover controls. Replacement supervisors use exact run claims and generation-safe OS signals; unfinished sessions resume only after the prior process group exits.
- Let `c` take over the selected active automated session using the same guarded handoff as `i`.

## [0.6.7] - 2026-09-09

### Fixed

- Disable the default elapsed-time cutoff for Codex tasks. Runs continue until completion or explicit stop while renewable leases and process supervision remain active; a positive `CLT_AGENT_RUN_TIMEOUT_SECONDS` is now an opt-in deadline, and `0` means unlimited.
- Resume the saved Codex session of an unfinished Doing task after a timeout releases its worker and lease, preserving stopped sessions and respecting failure backoff.
- Validate `NO_TASKS_LEFT` against the board before accepting an idle run. Remaining ready Todo work, an unfinished linked task, or an unreadable board records a failure with backoff instead of repeatedly launching fresh sessions after the success cooldown.

## [0.6.6] - 2026-09-05

### Added

- Automatically repair idle agent registry coordination on the next open, preserving the original database bundle and requiring exclusive access, stopped workers and sessions, and a successful integrity check. Interrupted updates and repairs requiring database reconstruction retain explicit recovery guidance.

### Fixed

- Clear Turso shared-WAL reader metadata before releasing its OS lock, preventing another process from reclaiming the slot during cleanup and triggering an ownership panic or losing its reader metadata.
- Exit interactive guardians and disconnected automated supervisors after reaping Codex when registry recovery is required, releasing database access instead of retrying finalization indefinitely.

## [0.6.5] - 2026-09-05

### Fixed

- Queue actionable follow-ups in Todo with clear scheduling guidance, reserving blocked Doing follow-ups for explicit obstacles. Ordinary cleanup work can start with its own Git journal instead of repeatedly failing interrupted-task recovery.
- Allow a verified task commit to include its linked Todo follow-up, including on an otherwise empty folder-backed board, while preserving unrelated task content and commit checks.
- Resolve both registered and requested project paths during orphan Git journal recovery, so macOS path aliases do not block cleanup or scheduling the next task.
- Select and reveal the newly completed task at the top of Done when moving it in the TUI, including when the Done list was scrolled down.
- Recognize Shift+M when terminals report lowercase `m` with a Shift modifier, so the Models page opens and closes consistently from Tasks and Agent Projects while plain `m` still cycles the project model.

## [0.6.3] - 2026-09-04

### Added

- Added `clt follow-up` to record an independent blocked Doing task alongside a verified implementation in the same sealed task commit, with prompt and skill guidance to distinguish pre-existing failures from incomplete acceptance criteria.
- Keep the displayed agent run's model and thinking effort visible in the log footer, using its recorded startup settings for both live and completed output.

### Fixed

- Refresh the matching remote-tracking ref after verifying an automated push, so Git no longer reports already-published task commits as unpushed. Separate fetch/push repositories and concurrent fetches retain their own tracking state.
- Place managed Git completions at the top of folder-backed Done lists without renaming unrelated tasks or invalidating their sealed Git proof. Repeated completions, interrupted moves, and manual reordering preserve the displayed order.

## [0.6.2] - 2026-09-04

### Fixed

- Retire idle, unbound Git journals with no task marker or sealed proof before scheduling, so an older project's abandoned session cannot repeatedly block new work after the checkout advances.
- Added `clt agent reconcile [PATH]` to apply the same guarded cleanup to a registered project, including while it is paused.
- Apply orphan cleanup before CLI and TUI project removal so unused journals do not prevent unregistering a project; linked tasks and sealed Git proof remain protected.

## [0.6.1] - 2026-09-04

### Fixed

- Fixed registry reader ownership failures when opening an existing database from the TUI or CLI, including after a partial checkpoint or interrupted WAL write and before the agent service has started.

## [0.6.0] - 2026-09-04

### Added

- Added external registry snapshots and `clt agent recover` with exclusive service drain, preserved DB/WAL quarantine, coordination repair, and fail-closed reconstruction of Git journals.

- Added persisted daemon project-scan errors to the Agent Projects pane, with red `ERROR` rows and actionable macOS Full Disk Access guidance for inaccessible external drives.
- Added `n` and `+` TUI shortcuts that create a Todo subtask under the selected task, automatically expand Markdown-backed parent storage, and open the resulting nested board.
- Added durable per-run agent workers: macOS uses one-shot launchd jobs and Linux uses transient user services, with persisted launch contracts, fenced worker records, heartbeats, crash recovery, and idempotent run finalization.
- Added `/goal`-prefixed task support for automated Codex runs, including explicit goals feature enablement and prompt guidance that removes the directive from the persistent goal objective.
- Added confirmed `Delete`-key removal for registered projects in the Agent Projects pane without deleting project files.
- Added a TUI Models page with provider presets, custom Responses-compatible endpoints, enabled model targets, favorites, a CLT-wide default, and per-project provider/model overrides.
- Added `x`/`Delete` removal for non-built-in providers on the Models page, including dependent model, selection, and Codex configuration cleanup.
- Added explicit, backup-protected Codex `config.toml` actions for custom provider definitions and the user's top-level default while keeping API keys exclusively in environment variables.
- Added an idle Done-or-blocked-task `c` shortcut to resume that task's Codex session interactively with workspace-write access, including while another Codex task is using the project, and return to the same board after Codex exits.
- Added task-level `s` controls to stop a selected task's linked active Codex session and later queue that exact session ID for automated `codex exec resume`, without stopping the agent service.
- Added task-level `i` interruption to stop a selected task's linked active Codex process, open the same ID in interactive Codex, and automatically restart that session in `codex exec resume` mode after exit.
- Added blocked-task monitoring that revisits one existing blocker at a time and backs off unresolved recovery attempts.
- Added the current local time to the agent projects pane's top border before the daemon status.
- Added `clt shell-init bash|zsh` integration so quitting after opening a registered project can change the calling shell to that project's directory.
- Added a first-class Backlog status for Markdown- and folder-backed boards, including CLI listing and status transitions.
- Added a hidden-by-default Backlog TUI column: `b` sends the selected task to Backlog, `B` toggles the column, and `0` reveals and focuses it.
- Added folder-backed status support: `tasks/backlog/`, `tasks/todo/`, `tasks/doing/`, and `tasks/done/` can now contain one task per file or subfolder.
- Added `clt init --folders` for fresh folder-backed task stores.
- Added `clt expand [status]` to migrate Markdown status files into folder-backed task files.
- Added nested subtask board navigation in the TUI for folder tasks that contain their own `backlog`, `todo`, `doing`, and `done` stores.
- Added first-sentence summaries for long task files while preserving full task content on moves.
- Added a multi-project Codex agent registry with `register`, `unregister`, `pause`, `resume`, scheduler, daemon, service, status, and log commands.
- Added a full-screen TUI agent projects pane for switching between registered project boards, toggling projects `ON` or `OFF`, and seeing `todo`/`doing` counts.
- Added per-project `git-commit` skill toggles for Codex agent runs through `clt agent git-commit enable|disable` and the TUI agent projects pane.
- Added per-project commit-and-push automation through `clt agent git-commit push`; the TUI now cycles Git automation through `OFF`, `COM`, and `PUSH` modes.
- Added live/latest agent output viewing directly from the active Kanban board with `l`.
- Added `skill-git-commit.md` guidance for safe agent-driven Git commits.

### Changed

- Task titles now use only the actionable `[STOPPED]` session prefix; active CLT work remains visible in the Agent Projects runtime column without a redundant `[CLT]` task prefix.
- `clt agent start` now snapshots the current executable into an immutable generation, while `clt agent stop` stops only the scheduler and leaves already-dispatched workers running on their original binary generation.
- Codex session IDs are now attached while automated work is active, stored with generation-safe live process and log metadata, and reused by unambiguous stop, interactive handoff, and interrupted/blocked recovery through `codex exec resume`.
- Automated commit and commit-and-push runs now use the isolated `CLT Agent <clt-agent@localhost>` Git author and committer identity without modifying Git configuration.
- Made embedded `codex:<session-id>` task markers the sole interactive-resume link, removed mutable task-text associations from the agent database, and made marker persistence failures visible as failed runs.
- Made the TUI task-board console help show task controls instead of Agent Projects controls.
- Changed the portable `r` task-reorganization shortcut into a sticky mode with yellow task-board borders and a visible mode title; arrows keep reorganizing tasks until `r` or `Esc` exits.
- Made local OpenAI-compatible endpoint setup preset-led and self-discovering: Ollama and LM Studio load their model catalogs automatically, custom endpoints generate their provider IDs, the API-root prompt explains `/v1` and rejects complete operation URLs, `/models` results are presented as explicit opt-in choices, and model discovery can be refreshed from the Models page.
- Replaced the plain GPT-5.6 entry in the built-in OpenAI catalog with the explicit GPT-5.6 Sol model while preserving existing selections and defaults during migration.
- Added aligned, labeled provider/model columns to the Models page, replacing unexplained favorite stars with `FAV` values and marking CLT and Codex defaults independently.
- Restored `Tab` as a direct Kanban/Agent Projects toggle; lowercase `m` retains quick per-project target cycling, while uppercase `M` opens the Models page from either Tasks or Agent Projects and returns to the originating pane.
- Git commit-and-push automation now pulls with the user's configured merge or rebase strategy instead of forcing a rebase.
- Moving a folder-backed task into a Markdown-backed status now expands the destination status to a folder and preserves the old Markdown file as `status.md.bak`.
- Right-aligned the hidden Backlog count and shortcut in the task console title.
- Updated the terminal title to show the active project when using the TUI.
- Renamed the agent task workflow guide from `clt-skill.md` to `skill-clt.md`.
- Automated Codex agent runs now use `danger-full-access` with approvals disabled so non-interactive tasks can update Git metadata.

### Fixed

- Allowed explicit user Done moves to accept idle externally completed `WORKING` tasks without discarding sealed Git proof.
- Recovered malformed active-worker indexes during independent reservation and scheduler scanning with one guarded retry.
- Retagged abandoned `WORKING` sessions before finalization lease acquisition and kept idle recovery failures visible as `ERROR`.
- Made `clt agent stop` independent of database health and stopped database retries after shared-WAL ownership/frame-index failures.

- Fixed managed Git sealing so a Todo or other task-board edit added during an agent run can remain unstaged and survive outside the exact task commit, while staged unrelated board changes and non-task baseline drift are still rejected.

- Git-enabled scheduling now checkpoints dirty task-board definitions in a dedicated prelaunch commit while preserving unrelated unstaged work, so tasks created in the CLI or TUI no longer fail before Codex starts. Failed pending projects also render as red `ERROR` rows with the stored cause, automatic-retry timing, and an `r` immediate-retry action instead of appearing unexplained as `IDLE`.
- Reclaim orphaned interactive reservations as soon as their generated holder process exits, release the exact lease when a guarded session disappears after reap, and show interactive lease-only states as `FENCED` or `STALE` instead of a false `RUNNING` agent.
- Allowed `c` to reopen a linked Doing task when its exact Codex session is stopped or otherwise idle, while continuing to reject sessions that are still running.
- Rebuild the derived active-worker project index after Turso reports a missing index entry, retry the scheduler pass once, and keep the daemon alive for later retries if a pass still fails.
- Kept TUI startup and keyboard input responsive while agent-panel refreshes or stale-service recovery are slow.
- Made Ctrl-C cancel TUI task creation and editing prompts, matching Escape without saving changes.
- Recheck blocked Todo and Doing tasks before fresh Todo work whenever recovery backoff permits, while allowing ready work to proceed during an unresolved blocker's backoff.
- Distinguished independent scheduler dispatch leases from legacy in-process runs so `clt agent stop` no longer reports a false legacy-run fence during post-reboot worker handoff.
- Moved independent-worker dispatch off the daemon's async runtime so blocking agent-store operations cannot panic and restart the scheduler before the worker service is launched.
- Reclaimed dead or expired agent leases for disabled projects and during unregister, while continuing to protect live, unknown, and independent-worker leases from deletion.
- Prevented scheduler restarts and binary upgrades from duplicating old-worker projects, made worker run recording/project counter updates/lease release one transaction, bounded failed startup and stale-heartbeat recovery behind verified service draining, serialized global worker capacity, treated newer worker protocols as opaque, deferred incompatible migrations without disabling controls, and kept stop and interrupt requests compatible across binary generations through the existing session-control protocol.
- Exact-session recovery now continues from the next unfinished step and requires requested code, file, configuration, or task-board changes to exist and pass relevant checks before marking the linked task done, while still allowing response-only tasks to finish with a response.
- Recovered Codex session markers displaced by completion notes so interactive handback can finish cleanly and the scheduler can continue with ready Todo work.
- Allowed explicitly registered non-Git folders to start and resume automated Codex runs, and allowed projects with run history to be unregistered cleanly.
- Fixed interactive Codex handoff on macOS by preserving the inherited terminal through the guardian and launch gate, handling zombie-only process groups safely, and showing stop, entry, and return-to-exec progress in the TUI.
- Prevented `c` from opening a Codex session that is still occupied by its automated run, and reserved the project while the interactive handoff is active so the scheduler cannot resume it concurrently.
- Prevented stop and interactive handoff races by having the owning runner terminate its own Codex process group, fencing the scheduler with persisted session state, and recovering stale TUI handoffs.
- Made the Kanban agent-output viewer follow the selected task, using live output for the active Doing task and session-linked run history for completed or blocked tasks.
- Fixed prompt construction in the bundled task-runner script under the macOS-provided Bash 3.2.
- Registering the current project from the Agent Projects pane now keeps the cursor on that project after it moves into the alphabetically sorted project list.
- Added portable task-reorganization shortcuts for terminals that do not distinguish Shift+Arrow: `Ctrl-P`/`Ctrl-N` reorder vertically, and `r` toggles a keyboard-driven reorganization mode.
- Documented the Terminal.app profile mappings required to preserve Shift+Up and Shift+Down task reordering through SSH and tmux.
- The agent projects pane now detects and restarts a stale background service while leaving explicitly stopped services alone; Linux services also restart after unexpected clean exits.
- Agent scheduling now reclaims crashed or expired leases and resumes the interrupted `doing` task instead of leaving it stranded.
- Existing folder- or Markdown-backed boards with one or more missing empty status stores are now detected and repaired instead of prompting for initialization.
- Declining the no-board initialization prompt now opens the TUI in the agent projects pane without creating an active task board.
- Agent services now resolve `codex` from `PATH` by default instead of pinning a version-manager-specific executable path, while preserving explicit `CLT_AGENT_CODEX_PATH` overrides.
- `clt agent start` now restarts an existing Linux user service after rewriting its systemd unit so updated environment settings take effect.
- Linux agent service commands now recover the standard user runtime directory when `XDG_RUNTIME_DIR` is missing, avoiding user-bus connection failures in SSH and non-interactive shells.

## [0.1.10] - 2026-05-11

### Added

- Added scroll handling in TUI task columns so keyboard navigation keeps the selected task visible.
- Added TUI task editing, deletion, help popover, console feedback, cursor-aware text input, and wrapping for selected long tasks.
- Added multiline-aware TUI input navigation for wrapped add/edit prompts, including Up/Down row movement, word jumps/deletes, and Ctrl-A/E/U/K/W shortcuts.
- Added CLI deletion support and single-status task listing.
- Added support for unquoted multi-word task descriptions in `clt add`.
- Added agent task workflow guidance, now published as `skill-clt.md`.

### Changed

- Made the TUI Kanban board the default when running `clt` with no subcommand.
- Made completed tasks appear at the top of the Done column.
- Updated README usage examples to match the current CLI behavior.

### Fixed

- Fixed stale or empty TUI selections causing add, edit, navigation, and move panics.
- Fixed terminal cleanup so raw mode and alternate screen are restored on TUI error paths.
- Fixed task moves so destination write failures do not remove the source task.
- Fixed TUI navigation on empty boards.

[Unreleased]: https://github.com/AlpineVibrations/clt-rs/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/AlpineVibrations/clt-rs/compare/v0.6.22...v0.7.0
[0.6.14]: https://github.com/AlpineVibrations/clt-rs/releases/tag/v0.6.14
[0.6.8]: https://github.com/AlpineVibrations/clt-rs/releases/tag/v0.6.8
[0.6.7]: https://github.com/AlpineVibrations/clt-rs/releases/tag/v0.6.7
[0.6.6]: https://github.com/AlpineVibrations/clt-rs/releases/tag/v0.6.6
[0.6.5]: https://github.com/AlpineVibrations/clt-rs/releases/tag/v0.6.5
[0.6.2]: https://github.com/AlpineVibrations/clt-rs/releases/tag/v0.6.2
[0.6.1]: https://github.com/AlpineVibrations/clt-rs/releases/tag/v0.6.1
[0.6.0]: https://github.com/AlpineVibrations/clt-rs/releases/tag/v0.6.0
[0.1.10]: https://github.com/AlpineVibrations/clt-rs/releases/tag/v0.1.10
