# Fixes to Port into the Recovery Refactor

## User-accepted external completion

Reference: master commit `c4a8f9c` (`Accept external completion on user Done moves`).

Release marker: this fix is included in the master `0.5.5` patch release.

A human moving a session-linked task to Done must count as explicit acceptance of externally completed work when its managed Git journal is still `WORKING`. Under a short project fence, CLT should verify the task identity and journal generation, refuse the move if a worker, live session, or project lease still owns the task, cancel the obsolete journal, clear its owner and idle resume control, move the task to Done, and release the fence. The CLI/TUI should report that the task was externally completed rather than claiming CLT proved its Git commit.

Keep the fail-closed boundary: this override must not cancel sealed `FINALIZING`/tracking or `PUSH-PENDING` proof. If CLT is interrupted after cancelling the journal but before moving the board entry, the scheduler must recognize that external-completion cancellation and must not resume the stale session.

The reference commit includes CLI/TUI handling, the transactional store operation, scheduler recovery protection, documentation, and tests for idle acceptance, live-owner/lease rejection, sealed-proof rejection, and the interrupted-board-move case.
