Restore Tab toggle and open Models from Agent Projects with m (TUI, UX)

COMPLETED 2026-08-11: Restored Tab as a direct Kanban/Agent Projects toggle, moved Models entry to lowercase m, retained quick per-project target cycling on Shift+M, and updated the in-app help, README, changelog, and regression coverage. Checks: cargo fmt; cargo test -- --test-threads=1 (176 passed); cargo clippy --all-targets -- -D warnings; git diff --check; manual TUI path verified Tab to Agent Projects, Tab back to Kanban, m to Models, and m back to Agent Projects.
