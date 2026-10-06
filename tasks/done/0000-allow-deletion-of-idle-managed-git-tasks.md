Allow deletion of idle cancelled managed Git tasks and recover cannatrace-api-rs

COMPLETED 2026-10-06: Fixed explicit deletion of idle WORKING managed Git tasks using a durable, generation-fenced cancellation under the board lock and project lease. Preserve original Git boundaries, conversation history, staging and unrelated task paths; keep automated deletion, live owners, launch boundaries and sealed proof fenced. Interrupted removals leave the exact session stopped for a safe retry. Updated feature, architecture and changelog documentation. Checks: required rustfmt gate, strict all-target/all-feature Clippy, and full locked all-target/all-feature test suite (761 passed, 1 ignored).

clt:manual codex:01a11274-8302-7203-a9d4-b85384dea08c
