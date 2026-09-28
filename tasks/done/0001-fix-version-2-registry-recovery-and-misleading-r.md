Fix version-2 registry recovery and misleading registration on registry errors [BUG]

COMPLETED 2026-09-28: Accepted v1/v2 recovery manifests, hid registration prompts until successful registry refresh, and added regressions. Recovered the live registry from its original DB/WAL with integrity verification and preserved quarantine. Checks: 684 tests passed, 1 ignored; rustfmt passed. Full Clippy has two pre-existing vendor nonstandard_macro_braces errors reproduced at baseline 8c4a422; queued follow-up clt-follow-up:01a0e974-886a-7df0-a38e-b312edbf216b.

RELEASED LOCALLY 2026-09-28: Prepared and installed patch version 0.7.2 from the optimized build. Stable Rust 1.97.1 checks passed: cargo fmt --all -- --check; strict all-target/all-feature Clippy; 684 tests, 1 ignored; isolated release-binary version/task/registry smoke checks. Updated the dated changelog and release command versions.

codex:01a0e974-886a-7df0-a38e-b312edbf216b
