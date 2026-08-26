in the models tui page on a provider in the model list page up and page down and homme and end shold work for hte list. there shold also be a search.

Completion note:

COMPLETED 2026-08-26: Added viewport-aware PageUp/PageDown and Home/End navigation plus case-insensitive `/` search across model names and IDs; updated TUI guidance and regression coverage. Checks: `cargo fmt -- --check`; `cargo test tui_models`; `cargo test` (269 passed); `git diff --check`. codex:01a03eac-b6aa-7a71-b2a9-012d5ae42542
