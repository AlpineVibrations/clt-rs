# lls-cli-task

A simple, file-system-backed task management CLI written in Rust. It uses Markdown files for persistence and provides both a command-line interface and a TUI Kanban board.

## Features

- **File-based Persistence**: Tasks are stored in `tasks/todo.md`, `tasks/doing.md`, and `tasks/done.md`.
- **Kanban TUI**: A visual board view powered by `ratatui`.
- **Simple CLI**: Easy commands to add, move, and list tasks.

## Installation

Ensure you have Rust and Cargo installed.

```bash
git clone <repository-url>
cd cli-task
cargo build --release
```

## Usage

### Initialization
Initialize the task directory structure:
```bash
cargo run -- init
```

### Adding Tasks
Add a new task to the To Do list:
```bash
cargo run -- add "My first task" "optional metadata"
```

### Moving Tasks
Change the status of a task using the `source->dest` transition format:
```bash
cargo run -- status todo->doing 1
cargo run -- status doing->done 1
```

### Listing Tasks
Get a quick overview of all tasks:
```bash
cargo run -- list
```

### Kanban View
Open the interactive TUI Kanban board:
```bash
cargo run -- view
```
*(Press 'q' to quit the TUI view)*