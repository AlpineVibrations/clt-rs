to stay in alignment with B opening backlog view A should open archive view and a should add the task to archive

Completion note:

COMPLETED 2026-07-21: Split the archive shortcuts so `a` moves the selected task into a content-preserving archive and `A` opens or closes the archive view; updated TUI guidance and README documentation. Checks: `cargo fmt -- --check`; `cargo test archive -- --test-threads=1` (4 passed); `cargo clippy --all-targets -- -D warnings`; `cargo test -- --test-threads=1` (137 passed); `git diff --check`.
