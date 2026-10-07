Bump the patch release to 0.6.8 for orphaned Codex supervision recovery (RELEASE)

Completion note:
COMPLETED 2026-09-09: Bumped the CLT package and lockfile to 0.6.8 and finalized release notes for orphaned Codex supervisor reattachment and active-session c takeover. Verified locked offline Cargo metadata reports 0.6.8, rebuilt the debug CLI and checked its help command, and passed cargo fmt --all -- --check and git diff --check. The included implementation already passed strict Clippy, the release build, and all 538 unit/architecture/CLI tests before this metadata-only release bump.
