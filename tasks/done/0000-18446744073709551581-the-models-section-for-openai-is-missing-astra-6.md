the models section for openai is missing astra 6.2

Completion note:

COMPLETED 2026-11-25: Added GPT-6.2 Astra to the built-in OpenAI Models catalog for fresh and upgraded registries. Passed `cargo fmt --all -- --check`, `cargo clippy --no-deps --locked --all-targets --all-features -- -D warnings`, `cargo test --locked --test architecture --test cli`, focused catalog, migration and Models tests, and a terminal smoke test. `cargo test --locked --all-targets --all-features` had one independent registry writer timeout. The exact registry test passed alone and the timeout reproduced on isolated `db76f307c3b970eb080506bfa4fecb44f5c95a26` with `cargo test --locked --bin clt -- --test-threads 32`. Queued `clt-follow-up:01a2132c-0c6d-7f91-aea3-c485dfbf9f64`.

<!-- clt-coordination: {"id":"f4220dde-0b9f-4801-9014-3ad416d733c9","depends_on":[],"scope":{"intent":"Expose GPT-6.2 Astra in the built-in OpenAI Models catalog","files":["src/agent.rs","src/agent/tests.rs","src/worker/tests.rs","features.md","CHANGELOG.md"],"modules":["agent registry model catalog"],"interfaces":[],"resources":[],"behavior":{"openai.model_catalog":"Seed GPT-6.2 Astra in new and existing registries while preserving user preferences"}},"reviews":[]} --> codex:01a2132c-0c6d-7f91-aea3-c485dfbf9f64
