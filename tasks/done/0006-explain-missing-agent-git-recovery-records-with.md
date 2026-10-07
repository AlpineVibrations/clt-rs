Explain missing agent Git recovery records with readable, actionable TUI errors.

Completion note:
COMPLETED 2026-09-30: Replaced missing-Git-record jargon in the agent view with a short headline and pause, task-review, and manual-completion guidance. Kept project rows to one line and made l show the saved failure even before log creation, while retaining exact-session controls and task output. Documented recovery and recorded the user-facing change. Verified 80/120-column rendering, saved diagnostics and session identity; required rustfmt, Clippy, and full locked all-target/all-feature tests passed (47 library, 647 application, 4 architecture, 19 CLI; one application test ignored).

COMPLETED 2026-09-30: Added actual task recovery through Agent Projects r with y/n confirmation and clt agent recover-task. User-confirmed recovery preserves files, staging, commits, old logs and session-mode evidence, queues unfinished work as a fresh conversation with review instructions, and accepts already-Done tasks without rerunning them. Added idle ownership and surviving-journal fences, exact task/run revalidation, interrupted-move recovery, visible confirmation, paused/Done-only error access, and automated-run refusal. Required formatting, Clippy, full locked all-target/all-feature tests passed (47 library, 654 application, 4 architecture, 20 CLI; one application test ignored), plus the final session-mismatch regression.

clt:manual codex:01a0f295-740b-76e3-a849-c446a1c76b89
