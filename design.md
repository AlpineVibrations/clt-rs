## Task Management CLI Design Specification

### 1. Overview
This CLI application, `lls-cli-task`, will provide a simple, file-system-backed task management system. It operates by initializing a standardized task structure in the current working directory.

### 2. Directory Structure
The CLI resolves the task root directory as follows:
1. If the `--local` flag is provided, use the current working directory.
2. Otherwise, attempt to find the git repository root using `git rev-parse --show-toplevel`.
3. If not in a git repository, fallback to the current working directory.

Once the root directory (`<root>`) is determined, the CLI must:
1. Create a subdirectory named `tasks/` inside `<root>`.
2. Create and initialize four Markdown files inside `tasks/`:
    * `backlog.md`: For captured tasks that are not ready to start.
    * `todo.md`: For tasks not yet started.
    * `doing.md`: For tasks currently in progress.
    * `done.md`: For completed tasks.

**Initial Content:**
Each markdown file should start with appropriate headers (e.g., `# To Do Tasks`, `# In Progress`, `# Completed Tasks`).

### 3. Core Functionality (CLI Commands)
The CLI needs commands to interact with tasks. All commands support the optional `--local` flag to override git root detection.

*   `lls-cli-task init`: Initializes the `tasks/` directory and the four markdown files if they don't exist.
*   `lls-cli-task add <task_description> [optional_metadata]`: Creates a new task and adds it to `todo.md`.
*   `lls-cli-task status <transition> <task_index>`: Allows moving a task from one list to another (e.g., `status todo->doing 1`).
*   `lls-cli-task list`: Displays an overview of all tasks across the four files, numbered by their current index.

### 4. View Layer (TUI/Kanban View)
The primary interaction view, accessible via a dedicated command (e.g., `lls-cli-task view`), must use `ratatui` to render a Kanban board representation of the tasks.

**Kanban Layout:**
The screen defaults to three visible columns, with a fourth Backlog column available on demand:
1.  **Backlog:** Tasks read from `tasks/backlog.md`; hidden by default and toggled with `B`.
2.  **To Do:** Tasks read from `tasks/todo.md`.
3.  **In Progress:** Tasks read from `tasks/doing.md`.
4.  **Done:** Tasks read from `tasks/done.md`.

Pressing `b` moves the selected task into Backlog. Pressing `0` reveals and focuses Backlog; `1`, `2`, and `3` retain their existing To Do, Doing, and Done mappings.

Tasks within each column must display their description.

### 5. Implementation Notes
*   **State Persistence**: All task data must be persisted in the respective Markdown files (`backlog.md`, `todo.md`, `doing.md`, `done.md`).
*   **Task Identification**: Tasks are identified by their 1-based index within their current list. This allows for a clean markdown format without stored IDs.
*   **Markdown Parsing**: The CLI must reliably parse tasks from Markdown content (lines starting with `- `).

### 5.1 Codex Agent Registry
The `clt agent` command group coordinates Codex automation across many registered project roots. Each project keeps task state in its own repository-local `tasks/` board, while agent runtime state lives in the shared agent registry database.

Registered projects must store:
*   Project path, display name, and enabled state.
*   Last scan/run/success/failure timestamps and failure count.
*   A per-project `git_commit_enabled` setting.

Agent scheduling must:
*   Run at most one Codex task per project at a time.
*   Skip paused projects, projects without pending `todo` work, and projects with active leases.
*   Ignore `backlog` tasks until a user promotes them to `todo`.
*   Prompt Codex to inspect the task board, move one task to `doing`, complete it, run relevant checks, update the task, mark it done when completed, and stop after one task.
*   Append the `$git-commit` skill instruction only when `git_commit_enabled` is true for that project.

The agent CLI must expose:
*   `clt agent pause [path]` and `clt agent resume [path]` for the project enabled state.
*   `clt agent git-commit enable [path]` and `clt agent git-commit disable [path]` for the per-project commit prompt setting.

The TUI agent projects pane must show:
*   A project enabled column toggled with `Space`.
*   A `GIT` column toggled with `g`.
*   `ON`/`OFF` values for both toggles.

### 5.2 Agent Support Files
Repo-root agent guidance files use the `skill-*.md` naming pattern so users can easily hand them to coding agents.

Current support files:
*   `skill-clt.md`: task-board workflow guidance for using `clt` safely.
*   `skill-git-commit.md`: Git commit and optional push workflow guidance.

---
### Implementation Summary
The project has been implemented with the following technical choices:
- **CLI Framework**: `clap` (derive) for command-line argument parsing.
- **TUI Framework**: `ratatui` with `crossterm` backend for the Kanban view.
- **Error Handling**: `anyhow` for flexible error propagation.
- **Indexing**: Implemented a dynamic index-based system where the position of the task in the file determines its ID for that session.
- **Persistence**: Direct file I/O using `std::fs` to maintain Markdown compatibility.

**Status**: Updated to Index-Based System.

### 6. Metadata Best Practices
Metadata is optional. 
To maintain a scannable and searchable board, metadata should be concise and standardized. Since metadata is stored in parentheses `(metadata)`, the following patterns are recommended:

*   **Tag-Based**: Use short, uppercase tags separated by commas for quick filtering (e.g., `BUG, HIGH, AUTH`).
*   **ID-Based**: Prefix with a hash for external tracking (e.g., `#124, P1`).
*   **Owner-Based**: Use handles for assignment (e.g., `@alice, LOW`).

These patterns ensure that agents and humans can easily `grep` or search for specific priorities or categories across the markdown files.
