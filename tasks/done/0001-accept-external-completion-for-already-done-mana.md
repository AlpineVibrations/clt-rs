Accept external completion for already-Done managed tasks BUG: Reconcile an idle WORKING journal without changing the completed task or Git history.

Completion note:
COMPLETED 2026-09-23: Routed already-Done completion through idle managed-journal acceptance and preserved task entries and ordering on same-status transitions. Documented recovery from a normal terminal. Checks: 14 CLI tests, 7 external-completion unit tests, 3 resealing tests, and cargo fmt --all -- --check passed.
