# Skills: Project Task Management with `clt`

This document defines the skills and operational procedures for an agent to manage project tasks using the `clt` (lls-cli-task) tool.

## Overview
The project uses a file-system-backed Kanban system located in the `tasks/` directory. Tasks are stored in Markdown files:
- `tasks/todo.md`: Tasks to be started.
- `tasks/doing.md`: Tasks currently in progress.
- `tasks/done.md`: Completed tasks.

## Core Workflow
The agent must adhere to the following state transition pipeline:
`Todo` → `Doing` → `Done`

1. **Identify/Create**: Add new requirements or bugs to the `todo` list.
2. **Activate**: When starting work on a task, move it from `todo` to `doing`.
3. **Complete**: Once the task is verified and finished, move it from `doing` to `done`.

## Command Reference

### 1. Initialization
If the `tasks/` directory is missing, initialize the system:
```bash
clt init
```

### 2. Adding Tasks
Add a new task to the `todo` list.
```bash
clt add "Task description" ["Optional metadata"]
```

### 3. Listing Tasks
Always list tasks before performing index-based operations to ensure the correct `task_index` is used.
```bash
clt list          # List all tasks across all statuses
clt list todo     # List only todo tasks
clt list doing    # List only doing tasks
clt list done     # List only done tasks
```

### 4. Managing Task Status
Move tasks between lists using their 1-based index.

**Move to In Progress:**
```bash
clt status todo <index> doing
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

- **Verify Indices**: Task indices are dynamic. Always run `clt list <status>` immediately before a `status`, `done`, or `delete` command to avoid modifying the wrong task.
- **Atomic Transitions**: Only move one task to `doing` at a time to maintain focus and clear project state.
- **Metadata Usage**: Use the metadata field for tracking issue numbers, priority, or assignees. Use standardized, comma-separated tags for better scannability (e.g., `clt add "Fix memory leak" "BUG, HIGH"`).
- **Consistency**: Ensure every significant change or feature implementation is tracked as a task. If a task is too large, break it into smaller sub-tasks in the `todo` list.

## Interactive View
For a visual representation of the board, the tool provides a TUI (Terminal User Interface). While agents primarily use the CLI, the TUI is the primary interface for human collaborators.
```bash
clt