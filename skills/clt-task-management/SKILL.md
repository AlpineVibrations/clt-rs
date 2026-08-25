---
name: clt-task-management
description: Manage project tasks with the clt file-system-backed Kanban CLI, including backlog triage, initialization, task creation and listing, status transitions, outcome notes, deletion, folder-backed tasks, and nested boards. Use when Codex needs to inspect, create, track, update, complete, or organize tasks in a project that uses clt or tasks/backlog.md, tasks/todo.md, tasks/doing.md, and tasks/done.md.
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

## Core Workflow
The agent must adhere to the following state transition pipeline:
`Backlog` → `Todo` → `Doing` → `Done`

1. **Capture/Triage**: Put work that is not ready in `backlog`. Backlog tasks are not eligible for automated agent runs.
2. **Identify/Create**: Add actionable requirements or bugs to `todo`, or promote a ready backlog task to `todo`.
3. **Activate**: When starting work on a task, move it from `todo` to `doing`.
4. **Complete**: Once the task is verified and finished, move it from `doing` to `done`.

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
- **Folder-Backed Tasks**: When a status is already a folder, edit the task file for detailed notes. Keep the first sentence suitable for list and TUI display.
- **Outcome Notes**: Before changing a task's status after a work attempt, record the outcome in the task. For a Markdown-backed status, append the note to the task's existing line. For a folder-backed status, preserve the first sentence and add a `Completion note:` or `Blocked note:` section to the task file.
- **Status Transitions**: After recording the outcome note, use `clt status` or `clt done` to change the task's status; never move or rename task files directly, because `clt` preserves board ordering and storage behavior.
- **Completion Notes**: Before moving a verified task to `done`, add `COMPLETED YYYY-MM-DD:` followed by a concise summary of what changed and the checks or tests that ran. Do not use a completion note as a substitute for verification.
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

## Interactive View
For a visual representation of the board, the tool provides a TUI (Terminal User Interface). While agents primarily use the CLI, the TUI is the primary interface for human collaborators.
```bash
clt
```

The Backlog column is hidden by default. Press `b` to move the selected task to Backlog, `B` to show or hide the column, or `0` to show and focus it. Keys `1`, `2`, and `3` focus Todo, Doing, and Done.
