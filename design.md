## Task Management CLI Design Specification

### 1. Overview
This CLI application, `lls-cli-task`, will provide a simple, file-system-backed task management system. It operates by initializing a standardized task structure in the current working directory.

### 2. Directory Structure
Upon initial execution in any directory (`<current_dir>`), the CLI must:
1. Create a subdirectory named `tasks/` inside `<current_dir>`.
2. Create and initialize three Markdown files inside `tasks/`:
    * `todo.md`: For tasks not yet started.
    * `doing.md`: For tasks currently in progress.
    * `done.md`: For completed tasks.

**Initial Content:**
Each markdown file should start with appropriate headers (e.g., `# To Do Tasks`, `# In Progress`, `# Completed Tasks`).

### 3. Core Functionality (CLI Commands)
The CLI needs commands to interact with tasks:
*   `lls-cli-task init`: Initializes the `tasks/` directory and the three markdown files if they don't exist.
*   `lls-cli-task add <task_description> [optional_metadata]`: Creates a new task and adds it to `todo.md`.
*   `lls-cli-task status <task_id>`: Allows moving a task from one list to another (e.g., `status todo -> doing <task_id>`).
*   `lls-cli-task list`: Displays an overview of all tasks across the three files.

### 4. View Layer (TUI/Kanban View)
The primary interaction view, accessible via a dedicated command (e.g., `lls-cli-task view`), must use `ratatui` to render a Kanban board representation of the tasks.

**Kanban Layout:**
The screen will be divided into three visible columns corresponding to the state:
1.  **To Do:** Tasks read from `tasks/todo.md`.
2.  **In Progress:** Tasks read from `tasks/doing.md`.
3.  **Done:** Tasks read from `tasks/done.md`.

Tasks within each column must display key information (e.g., Title, ID).

### 5. Implementation Notes
*   **State Persistence**: All task data must be persisted in the respective Markdown files (`todo.md`, `doing.md`, `done.md`).
*   **Task Identification**: A unique ID or index must be assigned to each task to manage status transitions accurately.
*   **Markdown Parsing**: The CLI must reliably parse tasks from Markdown content to extract actionable data (Title, Status, Metadata).

---
### Implementation Summary (Completed)
The project has been fully implemented with the following technical choices:
- **CLI Framework**: `clap` (derive) for command-line argument parsing.
- **TUI Framework**: `ratatui` with `crossterm` backend for the Kanban view.
- **Error Handling**: `anyhow` for flexible error propagation.
- **ID Management**: Implemented a global ID scanner that finds the maximum ID across all three state files to ensure uniqueness when adding new tasks.
- **Persistence**: Direct file I/O using `std::fs` to maintain Markdown compatibility.

**Status**: Fully Implemented.