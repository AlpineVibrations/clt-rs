Add safe pre-task Git sync to CLT skill

Completion note:

COMPLETED 2026-08-25: Added fast-forward-only startup sync guidance to both bundled skills, kept the installed git-commit copy aligned, and made the Codex runner fixture independent of embedded prompt length. Checks: frontmatter parse, bundled/installed comparison, `git diff --check`, and `cargo test` (196 passed).
