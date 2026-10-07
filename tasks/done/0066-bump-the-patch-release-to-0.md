Bump the patch release to 0.6.7 for agent timeout and recovery fixes (RELEASE)

Completion note:
COMPLETED 2026-09-09: Bumped the CLT package and lockfile to 0.6.7 and finalized the release changelog for unlimited default runs, saved-session recovery, and contradictory no-task backoff. Preserved the existing Unix-only crash-recovery test guard during commit review. Validation: cargo metadata --offline --locked --no-deps --format-version 1 confirms 0.6.7; cargo fmt --all -- --check and git diff --check pass. The included implementation was already verified with cargo clippy --locked --all-targets --all-features -- -D warnings and the complete 510-test suite in the preceding task.
