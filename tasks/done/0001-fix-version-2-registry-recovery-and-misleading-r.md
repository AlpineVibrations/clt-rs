Fix version-2 registry recovery and misleading registration on registry errors [BUG]

COMPLETED 2026-09-28: Accepted v1/v2 recovery manifests, hid registration prompts until successful registry refresh, and added regressions. Recovered the live registry from its original DB/WAL with integrity verification and preserved quarantine. Checks: 684 tests passed, 1 ignored; rustfmt passed. Full Clippy has two pre-existing vendor nonstandard_macro_braces errors reproduced at baseline 8c4a422; queued follow-up clt-follow-up:01a0e974-886a-7df0-a38e-b312edbf216b.

codex:01a0e974-886a-7df0-a38e-b312edbf216b
