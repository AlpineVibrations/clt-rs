Add per-task thinking overrides with /think, Todo t cycling, and bracketed labels.

Completion note:
COMPLETED 2026-10-25: Added all supported per-task thinking levels anywhere in task text, Todo-only t cycling back to defaults, and bracketed labels. Overrides apply to fresh/resumed task launches and new planning conversations without changing project/model defaults. Preserved task metadata, session links, nested boards, and managed Git identity. Updated documentation and released the local version as 0.9.4; installed it and reloaded the running scheduler.

Validation: cargo fmt --all -- --check; cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings; cargo test --locked --all-targets --all-features (901 passed, 1 ignored); CARGO_NET_OFFLINE=true python3 scripts/check_release.py --allow-dirty (archive build and smoke checks passed); cargo install --path . --locked --offline --force; installed and scheduler binaries both report clt 0.9.4. The first full suite encountered an intermittent same-process WAL-open error in the existing scheduler polling test; the current-revision isolated test, 11 isolated starting-revision runs, and the complete suite retry passed.

clt:manual codex:01a175f6-a4ee-7281-8a68-073319980026
