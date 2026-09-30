Reserve directly started Codex tasks until an explicit automation handoff. [FEATURE]

Completion note:
COMPLETED 2026-09-30: Added clt start and clt claim to publish manual ownership with the current Codex session, and clt handoff to return work to Todo while preserving that conversation. Manual claims fence scheduler acquisition, interrupted Doing recovery, and interactive launches; task edits and ordinary moves preserve them. Added MANUAL display, documentation, and bundled skill guidance. Verified Markdown and folder-backed lifecycles, concurrent starts, stale scheduler scans, nested boards, existing worker/history/unfinished-launch rejection, and completion release. Full suite: 711 passed, one existing ignored smoke test; final focused ownership tests, strict Clippy, rustfmt, and diff checks passed.

COMPLETED 2026-09-30 (follow-up): Moving a manual task to Todo through the TUI or CLI now releases the claim automatically and preserves the planning conversation. Released Terraqua’s Height Mask Todo and confirmed a worker resumed its original session. Verified TUI transitions in both storage modes, CLI handoff, and scheduler eligibility; full suite passed 712 tests with one existing ignored smoke test, plus strict Clippy, rustfmt, and diff checks.

clt:manual codex:01a0f290-e236-78e0-a4cd-f9e9893b75ad
