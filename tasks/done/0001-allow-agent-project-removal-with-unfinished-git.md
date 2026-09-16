Allow agent project removal with unfinished Git finalizations. (BUG, AGENT)

Completion note:
COMPLETED 2026-09-16: CLI unregister and TUI removal now abandon pending Git finalizations and launch boundaries without requiring reconciliation or changing project files, tasks, or the Git checkout. Active workers and leases still prevent removal. Updated the README and regression coverage for every pending Git state, retained task and checkout contents, re-registration, and active ownership. Verified the regressions fail before the fix; rustfmt check, Clippy with warnings denied, and all 623 tests pass.

Release note: Bumped the package and lockfile to 0.6.16, added the dated changelog entry, and updated release installation examples.
