Automatically recover interrupted Codex sessions with persistent active-writer conflicts

COMPLETED 2026-10-12: Automatically recover two consecutive exact-session active-writer startup failures through the guarded idle-project restart. Preserve files, staging, commits, queue order and session/journal history; retain stopped/manual/live-owner and sealed-proof safeguards, including exact recovery-generation checks. Verified rustfmt, strict Clippy, and the full all-targets/all-features suite (732 application tests passed, one ignored, plus database, architecture and CLI tests).

clt:manual codex:01a13199-6ec7-7bd2-83f7-8e9724eb6bb0
