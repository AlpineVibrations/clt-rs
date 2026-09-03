# Fixes to Port into the Recovery Refactor

## User-accepted external completion

Reference: master commit `c4a8f9c` (`Accept external completion on user Done moves`).

Release marker: this fix is included in the master `0.5.5` patch release.

A human moving a session-linked task to Done must count as explicit acceptance of externally completed work when its managed Git journal is still `WORKING`. Under a short project fence, CLT should verify the task identity and journal generation, refuse the move if a worker, live session, or project lease still owns the task, cancel the obsolete journal, clear its owner and idle resume control, move the task to Done, and release the fence. The CLI/TUI should report that the task was externally completed rather than claiming CLT proved its Git commit.

Keep the fail-closed boundary: this override must not cancel sealed `FINALIZING`/tracking or `PUSH-PENDING` proof. If CLT is interrupted after cancelling the journal but before moving the board entry, the scheduler must recognize that external-completion cancellation and must not resume the stale session.

The reference commit includes CLI/TUI handling, the transactional store operation, scheduler recovery protection, documentation, and tests for idle acceptance, live-owner/lease rejection, sealed-proof rejection, and the interrupted-board-move case.

## Rebuildable Turso agent registry

After the recovery refactor is complete, make Turso shared-WAL failures recoverable instead of allowing an ownership panic to disable the agent workflow. CLT is staying on Turso, but the agent registry should be treated as rebuildable runtime state rather than an irreplaceable source of truth. The task boards and Git repositories remain authoritative for work and completed code.

Persist the small non-reconstructable subset outside the database in atomically written files:

- registered project paths and user preferences, including enabled state, Git mode, provider/model, reasoning effort, and fast mode;
- enough identity and service metadata to stop or reconcile an active worker safely; and
- every nonterminal Git launch/finalization journal, including its frozen HEAD, branch, destination, worktree baseline, task/session identity, generation, and commit/push state.

Run records, scan timestamps, failure backoff, daemon check-ins, expired leases, terminal worker records, and log indexes may be discarded or reconstructed. Logs already live in the filesystem. For the Turso files, preserve `agent.db` and `agent.db-wal` as one durability unit because committed changes may exist only in the WAL. Treat `agent.db-tshm` and `agent.db-shm` as derived coordination/index files that may be rebuilt only after proving that every database user has stopped.

Add a recovery path with these boundaries:

1. Make `clt agent stop` independent of opening a healthy agent database.
2. On a shared-WAL ownership/frame-index panic, stop retrying indefinitely and report a recovery-required state.
3. Add `clt agent recover` that drains or safely stops the scheduler and workers, proves exclusive database access, and atomically quarantines the original database bundle instead of deleting it.
4. First preserve the database and WAL while rebuilding only the derived Turso coordination state, then reopen and run `PRAGMA integrity_check`.
5. If that cannot produce a healthy store, create a fresh registry from the external configuration, task/Git state, worker manifests, and Git journals. Fail closed for any finalization that cannot be reconstructed safely.
6. Retain the quarantined bundle for history and forensic recovery.

Do not merely suppress the Turso assertion, delete a live `-tshm`, discard the WAL, or remove the long-lived checkpoint pin in isolation. The pin currently prevents the earlier auto-checkpoint/stale-index failure, so replacing it requires regression coverage for checkpoint pressure, simultaneous daemon/TUI/worker access, abrupt process death, coordination-file rebuild, and interrupted Git finalization.
