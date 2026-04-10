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
*   `lls-cli-task status <transition> <task_index>`: Allows moving a task from one list to another (e.g., `status todo->doing 1`).
*   `lls-cli-task list`: Displays an overview of all tasks across the three files, numbered by their current index.

### 4. View Layer (TUI/Kanban View)
The primary interaction view, accessible via a dedicated command (e.g., `lls-cli-task view`), must use `ratatui` to render a Kanban board representation of the tasks.

**Kanban Layout:**
The screen will be divided into three visible columns corresponding to the state:
1.  **To Do:** Tasks read from `tasks/todo.md`.
2.  **In Progress:** Tasks read from `tasks/doing.md`.
3.  **Done:** Tasks read from `tasks/done.md`.

Tasks within each column must display their description.

### 5. Implementation Notes
*   **State Persistence**: All task data must be persisted in the respective Markdown files (`todo.md`, `doing.md`, `done.md`).
*   **Task Identification**: Tasks are identified by their 1-based index within their current list. This allows for a clean markdown format without stored IDs.
*   **Markdown Parsing**: The CLI must reliably parse tasks from Markdown content (lines starting with `- `).

---
### Implementation Summary
The project has been implemented with the following technical choices:
- **CLI Framework**: `clap` (derive) for command-line argument parsing.
- **TUI Framework**: `ratatui` with `crossterm` backend for the Kanban view.
- **Error Handling**: `anyhow` for flexible error propagation.
- **Indexing**: Implemented a dynamic index-based system where the position of the task in the file determines its ID for that session.
- **Persistence**: Direct file I/O using `std::fs` to maintain Markdown compatibility.

**Status**: Updated to Index-Based System.