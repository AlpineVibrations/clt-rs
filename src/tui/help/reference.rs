//! Keyboard shortcuts grouped by the screen or action they belong to.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Topic {
    #[default]
    Tasks,
    Projects,
    Models,
    Archive,
    Dialogs,
    TextInput,
    General,
}

type Section = (&'static str, &'static [(&'static str, &'static str)]);

impl Topic {
    pub(super) const ALL: [Self; 7] = [
        Self::Tasks,
        Self::Projects,
        Self::Models,
        Self::Archive,
        Self::Dialogs,
        Self::TextInput,
        Self::General,
    ];

    pub(super) fn adjacent(self, delta: isize) -> Self {
        Self::ALL[(self as isize + delta).rem_euclid(Self::ALL.len() as isize) as usize]
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Tasks => "Task board",
            Self::Projects => "Agent Projects",
            Self::Models => "Models",
            Self::Archive => "Archive",
            Self::Dialogs => "Dialogs",
            Self::TextInput => "Text input",
            Self::General => "General",
        }
    }

    // Keep the reference aligned with the pane reducers, execute_tui_key_effect,
    // handle_input_key and progress::task_summary_key when adding commands.
    pub(super) fn sections(self) -> &'static [Section] {
        match self {
            Self::Tasks => &[
                (
                    "Navigate",
                    &[
                        (
                            "Up, Down",
                            "Select the previous or next task, wrapping at the ends",
                        ),
                        (
                            "Left, Right",
                            "Switch to the previous or next visible column",
                        ),
                        (
                            "0, 1, 2, 3",
                            "Focus Backlog, Todo, Doing or Done. 0 also shows Backlog",
                        ),
                        ("Esc", "Close a log or clear the task selection"),
                        ("Backspace", "Return to the parent board"),
                    ],
                ),
                (
                    "Create and edit",
                    &[
                        ("Space", "Create a task at the current selection"),
                        (
                            "Enter",
                            "Message a running Doing agent, open subtasks or edit the task. With no selection, create a task",
                        ),
                        ("e", "Edit the selected task"),
                        ("n, +", "Create a subtask under the selected task"),
                    ],
                ),
                (
                    "Monitor and control",
                    &[
                        ("s", "Open the read-only task progress summary"),
                        (
                            "v, V",
                            "Inspect task details, scope, dependencies and other metadata",
                        ),
                        (
                            "l, L",
                            "Toggle task output, or project output with no task selected",
                        ),
                        ("Shift+S", "Stop or start the selected task"),
                        (
                            "i",
                            "Take over a live or stopped session. Automation resumes when you return",
                        ),
                        (
                            "c",
                            "Plan a Todo or open its linked session, including CLI tasks. Takes over an active run",
                        ),
                        (
                            "t",
                            "Cycle the selected Todo's thinking override, including the default",
                        ),
                        ("r, R", "Retry the current project"),
                    ],
                ),
                (
                    "Move and organize",
                    &[
                        ("b", "Move the task to Backlog"),
                        ("B", "Show or hide the Backlog column"),
                        ("a", "Move the task to the archive"),
                        ("A", "Open the read-only archive"),
                        ("Delete", "Permanently delete the selected task"),
                        (
                            "m",
                            "Enter Reorganize mode to move tasks with the arrow keys",
                        ),
                        ("Shift+Up, Shift+Down", "Reorder the task within its column"),
                        ("Ctrl+P, Ctrl+N", "Reorder the task within its column"),
                        (
                            "Shift+Left, Shift+Right",
                            "Move the task between visible columns",
                        ),
                        (
                            "4, 5, 6, 7, 8, 9",
                            "Move the task to that numbered position in its column",
                        ),
                    ],
                ),
                (
                    "Reorganize mode",
                    &[
                        ("Up, Down", "Reorder the selected task"),
                        ("Left, Right", "Move the task between visible columns"),
                        ("m, Esc", "Exit Reorganize mode"),
                    ],
                ),
            ],
            Self::Projects => &[
                (
                    "Project list",
                    &[
                        ("Up, Down", "Select a project"),
                        (
                            "Enter",
                            "Open the project's task board, or register the current project on the add row",
                        ),
                        (
                            "Space",
                            "Toggle the selected project on or off, or register it on the add row",
                        ),
                        ("Delete", "Remove the selected project after confirmation"),
                        ("Esc", "Close a log or return to the active task board"),
                    ],
                ),
                (
                    "Automation",
                    &[
                        (
                            "g, G",
                            "Cycle project Git mode through off, commit and push",
                        ),
                        ("m", "Cycle the project's model target"),
                        ("f, F", "Toggle project Fast mode"),
                        ("t, T", "Cycle project thinking"),
                        (
                            "r, R",
                            "Retry the failed project or confirm missing Git-record recovery",
                        ),
                        (
                            "v, V",
                            "Read the supervisor decision, task identity and recovery instructions",
                        ),
                        (
                            "s, S",
                            "Stop or resume the selected session. With output open, control that exact session",
                        ),
                        (
                            "l, L",
                            "Toggle the selected project's live or latest output",
                        ),
                        (
                            "i",
                            "Take over the session shown in the log. Automation resumes when you return",
                        ),
                        (
                            "c",
                            "Open the session shown in the log interactively, taking over if active",
                        ),
                        ("u", "Select or deselect global supervisor settings"),
                    ],
                ),
                (
                    "Supervisor settings",
                    &[
                        ("Space", "Toggle the supervisor on or off"),
                        ("m", "Cycle the supervisor model"),
                        ("t, T", "Cycle supervisor thinking"),
                        ("f, F", "Toggle supervisor Fast mode"),
                        ("u, Esc, Enter, Down", "Return to the project list"),
                    ],
                ),
                (
                    "Supervisor details",
                    &[
                        ("Up, Down", "Scroll one line"),
                        ("PageUp, PageDown", "Scroll one page"),
                        ("Home, End", "Go to the first or last page"),
                        (
                            "v, V, Enter, Esc",
                            "Close the read-only supervisor decision",
                        ),
                    ],
                ),
            ],
            Self::Models => &[
                (
                    "Browse and search",
                    &[
                        ("Left, Right", "Switch focus between providers and models"),
                        ("Up, Down", "Select a row, wrapping at the ends"),
                        ("PageUp, PageDown", "Move one page"),
                        ("Home, End", "Select the first or last row"),
                        (
                            "/",
                            "Search model names and IDs. Submit empty input to clear the search",
                        ),
                        (
                            "M, Tab, Shift+Tab, Esc",
                            "Return to the previous pane. Esc closes a log first",
                        ),
                    ],
                ),
                (
                    "Models",
                    &[
                        ("Space", "Enable or disable the selected model or provider"),
                        ("f, F", "Toggle the model's favorite status"),
                        ("t, T", "Cycle the model's thinking level"),
                        ("d, D", "Set the CLT default model"),
                        ("c, C", "Set the Codex default model and its thinking level"),
                        ("r, R", "Discover or refresh the selected provider's models"),
                        ("a, A", "Manually add a model ID to the selected provider"),
                    ],
                ),
                (
                    "Providers",
                    &[
                        (
                            "n, N",
                            "Add a provider. Choose 1 OpenAI, 2 OpenRouter, 3 Ollama, 4 LM Studio or 5 custom endpoint",
                        ),
                        (
                            "k, K",
                            "Store or replace the provider API key. Submit empty input to clear it",
                        ),
                        (
                            "x, X, Delete",
                            "Remove the selected non-built-in provider and its key",
                        ),
                    ],
                ),
            ],
            Self::Archive => &[(
                "Browse archived tasks",
                &[
                    (
                        "Up, Down",
                        "Select the previous or next archived task, wrapping at the ends",
                    ),
                    (
                        "v, V",
                        "Inspect the selected archived task's details and metadata",
                    ),
                    (
                        "A",
                        "Close the read-only archive and return to the task board",
                    ),
                ],
            )],
            Self::Dialogs => &[
                (
                    "Task progress",
                    &[
                        ("s", "Refresh the progress snapshot"),
                        ("Up, Down", "Scroll one line"),
                        ("Esc, Enter", "Close the summary"),
                        ("q", "Quit CLT"),
                    ],
                ),
                (
                    "Task details",
                    &[
                        ("Up, Down", "Scroll one line"),
                        ("PageUp, PageDown", "Scroll one page"),
                        ("Home, End", "Go to the first or last page"),
                        (
                            "v, V, Enter, Esc",
                            "Close the details. Reopen to see the latest cached metadata",
                        ),
                    ],
                ),
                (
                    "Confirmation prompts",
                    &[
                        (
                            "y, n",
                            "Initialize a task board at startup or continue without one",
                        ),
                        ("Ctrl+C", "Cancel initialization and exit"),
                        (
                            "y, n, Esc",
                            "Confirm or cancel project removal or Git recovery",
                        ),
                        ("1, 2, 3, 4, 5", "Choose a preset or custom model provider"),
                        ("Esc", "Cancel provider selection"),
                    ],
                ),
            ],
            Self::TextInput => &[
                (
                    "Submit and cancel",
                    &[
                        (
                            "Enter",
                            "Submit the task, message, search or current provider or model field",
                        ),
                        ("Esc", "Cancel input"),
                        ("Ctrl+C", "Cancel a task or agent-message prompt"),
                    ],
                ),
                (
                    "Move the cursor",
                    &[
                        ("Left, Right", "Move by character"),
                        ("Up, Down", "Move between wrapped rows"),
                        ("Home, Ctrl+A", "Go to the start of input"),
                        ("End, Ctrl+E", "Go to the end of input"),
                        ("Ctrl+Left, Alt+Left, Alt+B", "Move to the previous word"),
                        ("Ctrl+Right, Alt+Right, Alt+F", "Move to the next word"),
                    ],
                ),
                (
                    "Edit text",
                    &[
                        ("Backspace, Delete", "Delete the previous or next character"),
                        (
                            "Ctrl+Backspace, Alt+Backspace, Ctrl+W",
                            "Delete the previous word",
                        ),
                        ("Ctrl+Delete, Alt+Delete", "Delete the next word"),
                        ("Ctrl+U", "Clear input"),
                        ("Ctrl+K", "Delete to the end of input"),
                        (
                            "Paste",
                            "Insert text. Task and message prompts retain multiline pastes",
                        ),
                    ],
                ),
            ],
            Self::General => &[
                (
                    "Switch screens",
                    &[
                        (
                            "Tab, Shift+Tab",
                            "Switch between Tasks and Agent Projects or return from Models",
                        ),
                        (
                            "M",
                            "Open Models from the task board or Agent Projects, or return from Models",
                        ),
                        ("h, H, ?", "Open keyboard help"),
                        ("q", "Quit CLT"),
                    ],
                ),
                (
                    "Browse help",
                    &[
                        ("Left, Right", "Switch to the previous or next topic"),
                        ("Tab, Shift+Tab", "Switch to the next or previous topic"),
                        ("/", "Search all help topics by shortcut or action"),
                        ("Up, Down", "Scroll one line"),
                        ("PageUp, PageDown", "Scroll one page"),
                        (
                            "Home, End",
                            "Go to the first or last page while browsing a topic",
                        ),
                        (
                            "h, H, ?, Enter, Esc",
                            "Close help while browsing. Esc exits search first",
                        ),
                    ],
                ),
            ],
        }
    }
}
