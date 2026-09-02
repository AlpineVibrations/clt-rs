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
*   A per-project Git mode: off, commit, or commit-and-push.
*   Durable task-level Git finalization records keyed to the linked Codex session. These records track the starting branch and HEAD, worktree baseline, frozen upstream destination, required Git mode, durable task identity, exact sealed commit tree, locally proven commit OID, publication state, ownership generation, acknowledgement, and the latest recovery error.
*   Immutable pre-registration Git launch boundaries keyed to a project and worker run token. They preserve the server-owned snapshot between gated process release and atomic session registration and are not interchangeable with later checkout state.

Agent scheduling must:
*   Run at most one Codex task per project at a time.
*   Skip paused projects, projects without unblocked `todo` work or blocked work ready for recovery, and projects with active leases.
*   Ignore `backlog` tasks until a user promotes them to `todo`.
*   Prompt Codex to inspect the task board, move one task to `doing`, complete it, run relevant checks, update the task, mark it done when completed, and stop after one task.
*   Enable Codex goals for automated runs. When the selected task's first non-whitespace token is exactly `/goal`, prompt Codex to create a persistent goal from the remaining non-empty task content without including the directive itself.
*   Reconsider one currently blocked `todo` or `doing` task before fresh Todo work whenever its recovery backoff permits. Leave unmarked `doing` work alone; after an unresolved recovery, allow ready Todo work during the backoff and then check a blocker first again.
*   Append the `$git-commit` skill instruction only when the project Git mode requires a commit.
*   Before spawning or releasing a fresh Git-enabled Codex process, require the intended checkout, branch, and upstream configuration to be complete and require the index to match `HEAD`. Perform the safe fast-forward-only startup sync unless an older `WORKING` journal depends on the current history; preserve that commit instead of invalidating the older proof boundary. Capture the resulting exact `HEAD`, branch, worktree baseline, and upstream configuration. Persist this server-owned launch state before release; any spawned child remains gated until its remaining session fences are registered, and preparation or persistence failure must prevent Codex from executing. Recheck the frozen state when Todo moves to Doing and bind it to a `WORKING` journal before changing the board. The selected task's durable identity must already exist in the starting commit, so an uncommitted newly-created Todo is not eligible.
*   Treat the frozen launch state as scheduler-owned. After release, the agent may inspect Git and create the sealed task commit, but it must never push, pull, fetch/synchronize, merge, rebase, switch branches, reset history, or reconfigure the upstream.
*   Never overwrite or recapture an unconsumed pre-registration launch boundary. A supervised child that exits before session registration must be reaped without deleting this evidence; its exact worker becomes terminal first. Reclaim the record automatically only when that terminal generation is proven, no session-control row owns the run token, and the Git mode and checkout still match the snapshot. Otherwise fail closed and block new work. Project unregister and history cleanup must refuse while any such boundary remains.
*   Validate managed board storage before release. A folder-backed Todo requires folder-backed Doing, and folder-backed Doing requires folder-backed Done. Directory-backed managed transitions must rename the existing path without conversion or reordering; an exact crash duplicate across the transition is repaired, while ambiguous or nonidentical duplicates fail closed.
*   In a Git-enabled automated run, persist finalization intent before moving the task into Done. Treat that file-backed Done entry as provisional until one task-specific commit with the exact `CLT-Task: codex:<session-id>` trailer is proven.
*   At `clt done`, require the implementation and active Doing task to be staged, project the selected task's move into Done in a private index, reject raw changes elsewhere in the task board, and seal the exact resulting full repository tree. Accept only the one-parent CLT task commit whose complete tree, parent boundary, task identity, author/committer identity, and trailer match that seal; a path list or semantically similar rewrite is not sufficient proof.
*   If a hook mutates or rejects the sealed payload, require the corrected complete payload to be staged and the provisional Done entry to be explicitly resealed before retrying the same single commit.
*   In commit-and-push mode, resolve and freeze the effective push remote using `branch.<name>.pushRemote`, then `remote.pushDefault`, then the upstream remote. Require one concrete push URL and a `refs/heads/...` upstream merge ref. After local commit proof, CLT alone publishes that immutable exact OID to the URL/ref with an explicit non-force refspec; implicit push routing is never consulted at publication time, while configured pre-push and signed-push policy remains in force. Keep the task in `PUSH-PENDING` until bounded CLT-owned push and remote query/fetch proof succeed.
*   Retry `PUSH-PENDING` scheduler-side without launching or resuming Codex, and block later project work while it remains pending. A non-fast-forward or ambiguous result stays pending for another scheduler attempt or explicit external recovery.
*   Serialize scheduler-owned recovery with a short, renewable, exact-holder finalizer lease acquired in the same transaction that verifies active workers and session controls. Recheck that fence around board mutations, remote commands, and terminal journal changes. Never delete an expired lease belonging to a still-live controlled session, and never allow a shared writable interactive session across a Git-enabled lease, launch boundary, or nonterminal journal.
*   Give unresolved `FINALIZING` state priority over queued work and resume the same linked Codex session. A nonblocked `WORKING` journal is also resumed exactly, but a durably blocked `WORKING` task may yield to another Todo during its recovery backoff while its journal and reachable history are preserved and startup sync is skipped. Crash recovery must roll forward idempotently, adopt an already-proven commit or publication, and must not create a second completion commit, reset unrelated work, or move a successfully committed task backward. If completed-task evidence survives but its start journal does not, fail closed because the exact-one-commit boundary cannot be reconstructed safely.
*   Treat the shared Git index as a cooperative concurrency boundary. A fresh run rejects pre-existing staged changes and CLT verifies exact staged and baseline state, but Git does not record which actor staged a clean-file change during the run. Other actors sharing the checkout must not stage or unstage while automated finalization owns the index.

The agent CLI must expose:
*   `clt agent pause [path]` and `clt agent resume [path]` for the project enabled state.
*   `clt agent git-commit enable [path]`, `clt agent git-commit push [path]`, and `clt agent git-commit disable [path]` for the per-project Git mode.

The TUI agent projects pane must show:
*   A project enabled column toggled with `Space`.
*   A `GIT` column toggled with `g`.
*   `OFF`, `COM`, and `PUSH` values for Git mode.

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
