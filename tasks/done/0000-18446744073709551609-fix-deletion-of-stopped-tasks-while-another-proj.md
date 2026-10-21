Fix deletion of stopped tasks while another project task is running

Completion note:
COMPLETED 2026-10-21: Deletion now cancels only the selected idle WORKING journal when other workers and leases are proven to belong to different sessions. Preserve unrelated tasks, workers, controls, leases, Git journals, staging and commits. Keep active selected owners, unknown ownership, unconsumed launches and sealed proof protected; project-level errors no longer claim the stopped task is running. Added regressions for both board layouts, another running task, exited workers, changed journal generations/identities and protected owners. Updated features, architecture and Unreleased changelog. Verification passed: deletion regressions (7 tests); repository rustfmt check; cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings; cargo test --locked --all-targets --all-features (47 database, 762 application, 4 architecture and 24 CLI tests passed; 1 ignored).

clt:manual codex:01a15eb9-24c1-73d1-ac40-069829dc87e7
