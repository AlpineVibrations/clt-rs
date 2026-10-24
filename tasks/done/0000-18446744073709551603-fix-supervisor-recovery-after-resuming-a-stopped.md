Fix supervisor recovery after resuming a stopped managed task.

The ridge task in Terraqua finished implementation interactively after its
automated session stopped. Leaving that interactive conversation correctly
preserved the stop, but a saved supervisor Repair decision did not notice a
subsequent explicit resume. Read-only review acquisition also rejected the idle
session's original run token.

Bind review evidence to candidate retry eligibility and allow a read-only
review when a stopped or resume-requested session has no live child,
interactive holder or pending interactive launch. Preserve explicit stops,
the original conversation, Git journal, index and unrelated work. Cover resumed
recovery, concurrent control changes, stable idle eligibility and live-owner
fences, including Markdown and folder boards.

Terraqua recovery: resumed the exact ridge session using the existing TUI
controls and appended an UNBLOCKED note after confirming resume_requested.
The installed scheduler picked up the same conversation and its WORKING
journal; no replacement session or direct registry edits were used.
After an interrupted first resume, cleared the project retry delay through
`clt agent retry`. The original session then sealed and committed the ridge
payload as c8d420e9c8ccb9db54c8e2492ee69d712c9e302f. CLT published it and
recorded the original journal as completed, with no error or remaining worker.
The export commit and unrelated unstaged Pond/documentation edits were preserved.

COMPLETED 2026-10-23: Fixed stale ownership assessments after explicit resume
and read-only review admission for idle original run tokens. Added four
regressions covering both board layouts, unchanged Git history/journal/index,
concurrent control changes and live-owner fences. Updated features,
architecture and Unreleased changelog. Verification: cargo fmt --all -- --check;
cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings;
cargo test --locked --all-targets --all-features (881 passed, 1 ignored);
18 targeted supervisor tests passed; git diff --check. The installed scheduler
has not been replaced.

clt:manual codex:01a16c46-3bc3-7320-85ee-fb76ee196b32
