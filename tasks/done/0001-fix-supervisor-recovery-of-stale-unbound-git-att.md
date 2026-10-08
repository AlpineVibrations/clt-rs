Fix supervisor recovery of stale unbound Git attempts

Completion note:
COMPLETED 2026-10-08: Recover terminal unbound attempts after HEAD advances before activation, including blocked attempts with no session control, before supervisor holds. Preserve task order, worktree, index, commits and historical journals; recheck stops, ownership, latest run and checkout before retirement. Verified formatting, Clippy, the full all-targets/all-features test suite, recovery race regressions, and the release build. Installed the patched binary. Recovered the reported lls-server-www task through the supported restart command and verified its fresh worker successfully bound the task at a new Git checkpoint.

clt:manual codex:01a11c8f-4e8c-7bb1-b66e-1d74d7b81e28
