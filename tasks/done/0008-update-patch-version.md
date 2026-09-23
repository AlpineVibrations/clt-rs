update patch version

Completion note:

COMPLETED 2026-09-20: Bumped the crate and lockfile from 0.6.18 to 0.6.19 and moved the unreleased changelog entries into the matching dated release section. Checks: `cargo test --locked --test cli` (14 passed), `cargo metadata --locked --no-deps --format-version 1` with manifest/lockfile/changelog consistency assertions, both `--version` and `-V` reporting `clt 0.6.19` without opening a board or registry, `python3 scripts/check_release.py --allow-dirty` (archive build, source audit, and packaged version/task/registry smoke checks passed), and `git diff --check`. codex:01a0bf97-0c90-77f1-8c53-65caf6c2cdc6
