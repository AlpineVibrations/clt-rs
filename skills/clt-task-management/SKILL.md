---
name: clt-task-management
description: Manage project tasks with the clt file-system-backed Kanban CLI, including safe pre-task Git synchronization, backlog triage, initialization, task creation and listing, status transitions, outcome notes, deletion, folder-backed tasks, and nested boards. Use when Codex needs to inspect, create, track, update, complete, or organize tasks in a project that uses clt or tasks/backlog.md, tasks/todo.md, tasks/doing.md, and tasks/done.md.
---

# Skills: Project Task Management with `clt`

This document defines the skills and operational procedures for an agent to manage project tasks using the `clt` (lls-cli-task) tool.

## Overview
The project uses a file-system-backed Kanban system. By default, the tool automatically detects the git repository root and locates the `tasks/` directory there to keep task management centralized across the project. Tasks are stored in Markdown status files by default:
- `tasks/backlog.md`: Captured tasks that are not ready to start.
- `tasks/todo.md`: Tasks to be started.
- `tasks/doing.md`: Tasks currently in progress.
- `tasks/done.md`: Completed tasks.

Statuses can also be folders instead of Markdown files:
- `tasks/backlog/`: Each direct file or subfolder is one backlogged task.
- `tasks/todo/`: Each direct file or subfolder is one todo task.
- `tasks/doing/`: Each direct file or subfolder is one active task.
- `tasks/done/`: Each direct file or subfolder is one completed task.

For folder-backed statuses, `clt` displays the first sentence of each task file while preserving the full file content for longer notes. A task subfolder with its own `backlog`, `todo`, `doing`, and `done` stores is a nested subtask board in the TUI.

## Start-of-Task Git Sync

For ordinary interactive work, before moving a task to `doing` or editing files for a new task in an existing Git repository, inspect the checkout:

```bash
git status --short --branch
```

If the worktree and index are clean, HEAD is attached to a branch, and that branch has an upstream, update it before starting the task:

```bash
git pull --ff-only
```

Do this before the CLT status transition because moving a task to `doing` changes the task board and makes the checkout dirty. Use fast-forward-only so the startup sync cannot create a merge commit, rebase local commits, or disturb user work. If the checkout is dirty, detached, has no upstream, or cannot fast-forward, do not stash, discard, commit, switch branches, or integrate changes solely to make the pull succeed. Continue from the current checkout when safe and mention that the startup pull was skipped or could not complete.

The automated CLT Git workflow is different: do not run the inspection/sync procedure above from inside a released automated run. The intended checkout, branch, and upstream must be configured before scheduling. A newly created Todo may remain unstaged: before spawning or releasing Codex, CLT requires the index to match `HEAD`, performs the safe fast-forward-only startup sync unless an older `WORKING` journal requires preserving its history, and checkpoints a dirty task board in a dedicated `CLT Agent` commit without including unrelated worktree changes. It captures the resulting `HEAD`, branch, worktree baseline, and upstream configuration and persists that server-owned launch state. Any spawned child remains gated until its remaining session fences are registered. CLT releases the agent only after preparation succeeds. Commit-and-push mode requires an attached branch with one configured upstream at that boundary.

After release, inspect the provided state but do not pull, fetch/synchronize, merge, rebase, switch branches, reset history, reconfigure the upstream, or push. Move the selected task to Doing before implementation; CLT rechecks the frozen state and binds it to the session's `WORKING` journal at that transition.

If CLT reports an unconsumed pre-registration launch boundary, do not try to replace it or clean/unregister the project. CLT never overwrites that snapshot, including when a reaped child exited before announcing its session. It reclaims the record only after proving the exact worker terminal, finding no session-control row for its run token, and matching the checkout and Git mode to the frozen state; otherwise recovery fails closed.

A dirty worktree is expected in shared repositories where a person, an interactive session, or an independent worker may have changes in progress. It is not a blocker by itself. Treat the initial status and diff as the baseline, preserve pre-existing changes, and continue with non-conflicting work. Another change in the same file is also not automatically a blocker: re-read the affected area and keep both changes when the intended combined result is clear. Mark the task blocked only when the required edits genuinely conflict and the correct result cannot be determined safely.

The shared Git index is different from the shared worktree: it is a cooperative ownership boundary during automated finalization. People, interactive sessions, and parallel tools must not stage or unstage changes while the active automated task owns the index. CLT can detect many unexpected index/baseline changes, but Git cannot identify which actor staged a new clean-file change.

A Todo or other task-board edit added after the automated run starts may remain unstaged. Preserve it and continue finalization: CLT's exact staged-tree proof excludes that concurrent board work from the sealed task commit. Stage only the selected task's status transition, its explicit follow-up, or related hunks; never use a whole-board add when it would absorb the concurrent edit.

## Core Workflow
The agent must adhere to the following state transition pipeline:
`Backlog` → `Todo` → `Doing` → `Done`

1. **Capture/Triage**: Put work that is not ready in `backlog`. Backlog tasks are not eligible for automated agent runs.
2. **Identify/Create**: Add actionable requirements or bugs to `todo`, or promote a ready backlog task to `todo`.
3. **Activate**: Inspect the selected task and applicable repository instructions, then move it from `todo` to `doing` before editing implementation files. For ordinary interactive work, finish pre-task Git sync/branch selection first. In an automated Git-enabled run, CLT already completed and froze that preparation before release; do not repeat or alter it.
4. **Complete**: Once the task is verified and finished, move it from `doing` to `done`.

For an automated CLT run whose project Git mode is `commit` or `commit-and-push`, the final move has stronger semantics. `clt done` records a durable finalization intent before it moves the board entry. The entry in the Done store is provisional while CLT reports it as `FINALIZING`; it becomes terminal only after CLT proves the task-specific commit, and, in push mode, proves that commit is present on the configured upstream. The same linked Codex session resumes an interrupted finalization. Do not select another task, create a replacement task, or move the provisional entry back to Doing.

## Command Reference

### 1. Initialization
If the `tasks/` directory is missing, initialize the system:
```bash
clt init
```
Use the default Markdown-file mode for normal agent task tracking. Only initialize folder-backed statuses when the user explicitly asks for expanded tasks or the project has already adopted that format.
```bash
clt init --folders
```
To force initialization in the current working directory instead of the git root ( not used most the time ), use:
```bash
clt --local init
```

To expand existing Markdown status files into folder-backed task files:
```bash
clt expand        # Expand backlog.md, todo.md, doing.md, and done.md
clt expand backlog
clt expand todo   # Expand one status
```
Expansion preserves the original Markdown file as `status.md.bak`.

### 2. Adding Tasks
Add a new task to the `todo` list.
```bash
clt add "Task description" ["Optional metadata"]
```

### 3. Listing Tasks
Always list the relevant status before performing index-based operations to ensure the correct `task_index` is used. Prefer status-scoped listings so unrelated tasks do not consume context.
```bash
clt list todo     # List only todo tasks
clt list doing    # List only doing tasks
clt list done     # List only done tasks
clt list backlog  # List backlog only when the current work requires it
clt list          # List all statuses only when a whole-board view is necessary
```

**Sample output:**
```
--- TODO ---
1. Fix login bug
2. Add dark mode
```

Each section lists tasks with a 1-based index scoped to that status. An empty section displays the header with no items beneath it. Always use the index relative to its section — index `1` in `BACKLOG`, index `1` in `TODO`, and index `1` in `DOING` refer to different tasks.

Folder-backed tasks still use the same status-scoped indexes. `clt list` marks folder tasks that contain nested boards with `[subtasks]`.

### 4. Managing Task Status
Move tasks between lists using their 1-based index.

**Move to In Progress:**
```bash
clt status todo <index> doing
```

**Backlog or promote a task:**
```bash
clt status todo <index> backlog
clt status backlog <index> todo
```

**Mark as Done:**
```bash
clt done doing <index>
```
*(Alternatively: `clt status doing <index> done`)*

In an automated Git-enabled run, use this command only after the implementation and completion note are verified, all file-mutating formatters/hooks have run, and the implementation plus active Doing task have been staged and inspected. CLT projects the selected task's Done move and seals the exact resulting full repository tree, then treats the worktree's Done entry as provisional until Git finalization succeeds. Stage only the resulting board transition and include it in the same task commit. If a later commit hook changes or rejects files, stage the complete correction, list Done to confirm its current index, and run `clt done done <index>` to reseal the provisional entry before retrying that one commit.

### Independent failures and follow-ups

A failed command does not automatically mean the implementation is blocked. First establish whether the task's acceptance criteria are satisfied and its relevant checks pass. For a separate pre-existing or environment-only failure, reproduce the same failure on the frozen starting revision in an isolated directory, without switching or resetting the active checkout. Record the revision, commands, matching failure, passing task checks, and what is needed to unblock the independent work. If acceptance or independence is uncertain, keep the original task blocked and do not commit incomplete work.

When the original task is complete, record the independent work with:

```bash
clt list doing
clt follow-up doing <index> "Fix existing lint warnings" --evidence "Failure evidence; starting revision and reproduction; remaining work"
```

The command queues one actionable Todo task with a `clt-follow-up:<parent-session-id>` reference. It preserves the parent and existing board order, does not attach the parent's `codex:` marker, and does not start another task or session. Repeating the same command is safe; edit an existing follow-up instead of creating duplicates. This command requires a session-linked Doing parent. Record only the independent remaining work, never the parent's unfinished acceptance criteria.

Add a COMPLETED note to the original task with the follow-up reference and validation evidence. In automated Git mode, stage the linked follow-up together with the verified implementation and original Doing task before `clt done`; include all of them and the resulting Done move in the one sealed commit. CLT permits that linked addition while continuing to reject unrelated staged board edits. If a hook requires corrections, include the complete payload when resealing. Stop after finalizing the original task; the queued follow-up is eligible for a fresh Todo run with its own session and Git start journal. Report the parent as completed with follow-up work queued, not as a failed or blocked run.

Only add `--blocked "Unavailable dependency or input; what restores it"` when an actual obstacle prevents starting the follow-up; this records it as blocked in Doing for later recovery. Ordinary implementation work, pre-existing warnings, and bugs that the follow-up itself should fix are actionable Todo work, not blockers.

### 5. Deleting Tasks
Remove a task that is no longer relevant.
```bash
clt delete <status> <index>
```

## Operational Guidelines for Agents

- **Root Awareness**: Be aware that `clt` operates relative to the git root by default. If you need to manage tasks in a specific subdirectory that is not the git root, use the `--local` flag.
- **Verify Indices**: Task indices are dynamic. Always run `clt list <status>` immediately before a `status`, `done`, or `delete` command to avoid modifying the wrong task.
- **Keep Listings Scoped**: During normal task execution, list only the status needed for the current decision. Do not load the backlog unless the user asks for it or the work specifically requires backlog triage, inspection, promotion, or a whole-board diagnosis. Large unrelated backlogs consume context and can distract from actionable `todo` and `doing` work.
- **Preserve Existing Tasks**: Never delete, reorder, or rewrite `clt` tasks unless explicitly asked. Other people may add todos while you are working, and those are real tasks, not noise.
- **Backlog Is Not Actionable**: Do not start or automatically select backlog tasks. Work on one only after the user or project workflow promotes it to `todo`.
- **Default Storage Mode**: Use regular Markdown-file mode for agent-created task lists unless the user explicitly asks for expanded folder-backed tasks. Do not run `clt init --folders` or `clt expand` just because a task has some detail.
- **Folder-Backed Tasks**: When a status is already a folder, edit the task file for detailed notes. Keep the first sentence suitable for list and TUI display. Managed Git automation preserves a directory-backed task's existing path and order during status moves. It rejects a folder-backed Todo-to-Markdown Doing or folder-backed Doing-to-Markdown Done route before launch; expand and commit the destination layout first. Exact source/destination duplicates left by a crash are repaired without reordering unrelated tasks, while ambiguous copies fail closed.
- **Outcome Notes**: Before changing a task's status after a work attempt, record the outcome in the task. For a Markdown-backed status, append the note to the task's existing line. For a folder-backed status, preserve the first sentence and add a `Completion note:` or `Blocked note:` section to the task file.
- **Status Transitions**: After recording the outcome note, use `clt status` or `clt done` to change the task's status; never move or rename task files directly, because `clt` preserves board ordering and storage behavior.
- **Completion Notes**: Before moving a verified task to `done`, add `COMPLETED YYYY-MM-DD:` followed by a concise summary of what changed and the checks or tests that ran. Do not use a completion note as a substitute for verification.
- **Automated Git Finalization**: In an automated Git-enabled run, `clt done` starts a persisted `FINALIZING` transaction. Create one ordinary task commit with the exact sealed full tree, parent boundary, task identity, CLT Agent identity, and one `CLT-Task: codex:<session-id>` trailer, inspect it, and exit without pushing. A commit, hook, timeout, or process failure leaves the same task and Codex session resumable; never start another task or manufacture a second completion commit.
- **Commit-and-Push Ownership**: Configure the intended upstream and any push override before scheduling. CLT resolves and freezes `branch.<name>.pushRemote`, then `remote.pushDefault`, then the upstream remote, together with the concrete push URL and upstream merge ref. After local proof, CLT alone sends the immutable exact OID to that URL/ref with an explicit non-force refspec; implicit push routing is ignored, while normal pre-push hooks and signed-push policy still apply. Never run `git push` in an automated CLT task. CLT retries `PUSH-PENDING` scheduler-side without Codex and blocks later project work until remote proof succeeds or the state is resolved externally.
- **Provisional Done Entries**: A task's physical presence in the Done store is not terminal while CLT reports it as `FINALIZING` or `PUSH-PENDING`. Do not move it backward after a successful local commit. Recovery verifies existing Git state and rolls forward, including when the commit was created or CLT's publication succeeded immediately before a crash.
- **Missing Journals Fail Closed**: If completed-task evidence survives but its frozen start journal is lost, do not reconstruct or commit from memory. CLT refuses to guess the exact-one-commit boundary; preserve the checkout and report the recovery error.
- **Blocked Working Backoff**: A durably blocked task may retain its `WORKING` journal while recovery backs off. CLT can run another ready Todo during that interval without discarding the blocked task's session or history, and it skips startup sync to keep the older proof boundary reachable. `FINALIZING` and `PUSH-PENDING` never yield to later project work.
- **Independent Failures**: Use `clt follow-up` when acceptance is satisfied and a separate failure is evidenced as pre-existing or environment-only. Commit the verified original task and linked follow-up together; never leave finished implementation uncommitted solely because that independent failure remains. Do not work on the follow-up in the same run.
- **Blocked Notes**: If a task cannot be completed safely, add `BLOCKED YYYY-MM-DD:` followed by the blocker, what was attempted, and what is needed to continue. Do not move a blocked task to `done`; preserve its current status unless the user or project policy directs another transition. Normal automated selection skips a blocked task even when it remains in `todo`.
- **Unblocked Notes**: When a recorded blocker is resolved but the task still needs the normal Todo workflow, add `UNBLOCKED YYYY-MM-DD:` with the resolution and move the same task to `todo`. The automated scheduler treats the latest dated `BLOCKED`, `UNBLOCKED`, or `COMPLETED` state note as current, so blocker history can remain in the task.
- **Atomic Transitions**: Only move one task to `doing` at a time to maintain focus and clear project state.
- **Metadata Usage**: Use the metadata field for tracking issue numbers, priority, or assignees. Use standardized, comma-separated tags for better scannability (e.g., `clt add "Fix memory leak" "BUG, HIGH"`).
- **Consistency**: Ensure every significant change or feature implementation is tracked as a task. If a task is too large, break it into smaller sub-tasks in the `todo` list.

## End-to-End Workflow Example

The following shows a complete task lifecycle from creation to completion.

**1. Add a new task:**
```bash
clt add "Fix memory leak in parser" "BUG, HIGH"
```

**2. Verify it appears in todo:**
```bash
clt list todo
```
```
--- BACKLOG ---

--- TODO ---
1. Fix memory leak in parser
```

**3. Check nothing is already in progress before activating:**
```bash
clt list doing
```
```
--- DOING ---
```

**4. Move the task to doing (use the index confirmed in step 2):**
```bash
clt status todo 1 doing
```

**5. Confirm the transition:**
```bash
clt list doing
```
```
--- DOING ---
1. Fix memory leak in parser
```

**6. After completing and verifying the work, record the outcome in the task:**

For this Markdown-backed example, update the existing line in `tasks/doing.md`:
```markdown
- Fix memory leak in parser — COMPLETED 2026-07-13: Corrected parser ownership; checks: `cargo test parser`.
```

For a folder-backed task, add the same information beneath a `Completion note:` heading in its task file.

**7. List the status again, then mark the confirmed task done:**
```bash
clt list doing
```
```bash
clt done doing 1
```

**8. Verify the final state:**
```bash
clt list done
```
```
--- DONE ---
1. Fix memory leak in parser — COMPLETED 2026-07-13: Corrected parser ownership; checks: `cargo test parser`.
```

For an automated Git-enabled run, CLT checkpoints the task board before activation so the selected task is present in the frozen starting commit. Continue with the `$git-commit` workflow after step 7. The task remains logically `FINALIZING` until CLT verifies the required commit or commit-and-push result; only then is step 8 a terminal Done state.

## Interactive View
For a visual representation of the board, the tool provides a TUI (Terminal User Interface). While agents primarily use the CLI, the TUI is the primary interface for human collaborators.
```bash
clt
```

The Backlog column is hidden by default. Press `b` to move the selected task to Backlog, `B` to show or hide the column, or `0` to show and focus it. Keys `1`, `2`, and `3` focus Todo, Doing, and Done.
