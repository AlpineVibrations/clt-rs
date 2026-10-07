Fix stopping linked tasks without live session controls and Todo planning startup.

Completion note:
COMPLETED 2026-09-24: Task stop/start now persists for saved Codex links without CLT control records and idle Todo planning sessions. New planning conversations can open alongside another project run while preserving its lease and exact session. Updated feature documentation and bundled skill guidance. Validation: required rustfmt and Clippy gates passed; cargo test --locked --all-targets --all-features passed (681 tests, 1 optional smoke test ignored); installed Codex planning durability smoke passed separately; release TUI stop/start smoke passed. Cleared stale local binary build fingerprints before CLI verification.

codex:01a0d488-bf3e-7812-816e-e37a7b8374ca
