Resume unfinished automated tasks after idle interactive sessions and fix moves past managed Git tasks

COMPLETED 2026-10-07: Idle interactive visits to unfinished automated Doing tasks hand the same session back to exec, with state-checked reservations preserving concurrent and explicit stops. Paused projects retain queued resumes. Markdown moves no longer run destination storage-conversion checks against unrelated managed tasks. Added lifecycle, stop-race, paused-worker, and destination-journal regressions; updated features, architecture, and changelog. Verification: required rustfmt check and strict Clippy passed; full all-targets/all-features test suite passed (710 application tests, 1 ignored, plus library, architecture and CLI suites); additional paused-handback test passed. Release build installed locally; used corrected binary to move requested lls-server-www UI-06 from Doing to Todo.

RELEASE 2026-10-07: Bumped package and lockfile to 0.8.2, published the release notes in CHANGELOG.md, and updated installation examples.

clt:manual codex:01a1168a-7ae6-79e2-8df4-6eeafbe8824d
