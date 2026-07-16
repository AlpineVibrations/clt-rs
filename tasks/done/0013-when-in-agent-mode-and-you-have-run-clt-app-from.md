when in agent mode and you have run clt app from a project fodler that is not registered then at the top of the list of registered projects should be line for adding the current project with enter button or space button

Completed: Added a top agent-pane registration row for the current unregistered project, wired Enter/Space to register it, and covered the row/selection behavior with tests. Ran `cargo fmt`, `cargo fmt --check`, focused cargo tests for agent-panel/current-project registration behavior, and `cargo test`.
