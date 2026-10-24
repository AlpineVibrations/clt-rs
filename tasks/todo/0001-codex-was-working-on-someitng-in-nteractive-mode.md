Coordinate dependencies and overlapping work between manual and automated tasks.

Planning status: agreed scope. Keep this task in Todo for planning and discussion; begin implementation or an automated task run only when explicitly requested. Updating this plan does not authorize starting work.

## Problem and intent

A directly opened Codex session claimed a task displayed as [CLI], and CLT started another automated task in the same project. Running independent tasks together is desirable, but ownership alone does not establish that their changes are independent. Either task may need the other's unfinished work or change behavior that the other relies on.

Keep useful parallel work moving while making dependencies, potential conflicts, and scheduling decisions visible. The current Git checks and completion workflow are working reasonably well after earlier excessive blocking. Preserve that progress: add focused coordination without redesigning the Git workflow or restoring broad project blockers.

## Agreed scope

1. Shared awareness of ongoing work
   - Give both automated and participating direct/manual sessions the relevant other active tasks, their owners, intended changes, and declared scope before implementation starts.
   - Show this context in CLT, distinguishing a manual ownership claim from an actually running session.
   - Refresh relevant context when another task starts, completes, or changes scope. Preserve each task's existing conversation and ownership.

2. Explicit dependencies
   - Let a task declare that it depends on another task using a stable task identifier, independent of list position, wording, and conversation ID. References must survive task edits, reordering, and status moves.
   - Keep dependent work in Todo until its prerequisites are truly complete, including required Git finalization or publication. A provisional entry in Done is not sufficient.
   - Reject self-dependencies and cycles. Surface missing or deleted prerequisites with an actionable explanation instead of silently treating them as complete.
   - Reevaluate eligibility when prerequisites complete, preserving explicit stops and existing ownership controls. Make eligible work available to normal scheduling without a new manual unblock step.

3. Assessment of overlapping changes
   - Before admitting another implementation task, compare its requirements and planned files, modules, interfaces, and shared resources with relevant ongoing work and current changes.
   - Different files can still have a behavioral dependency; edits to the same file can be compatible. Neither file separation nor file overlap is conclusive by itself.
   - Use explicit dependencies as scheduling rules. Use automated analysis to identify possible missing dependencies or conflicts, with concrete evidence and a reason rather than a claim that every dependency can be inferred.
   - Allow independent or understood compatible work to proceed. Defer a task for a known conflicting change or unmet prerequisite. A concrete unresolved concern should identify what needs review; incomplete metadata or generic uncertainty must not become an indefinite project-wide hold.

4. Visible, actionable decisions
   - Show the affected task, related task, reason, and what permits progress: for example, "Waiting for task A" or "Potential overlap with task A: scheduler behavior."
   - Distinguish a dependency wait, a potential conflict needing assessment, and ordinary execution/Git state. Keep unrelated ready tasks eligible.
   - Avoid repeated assessments or approval requests when the relevant evidence has not changed. Clear coordination waits when their specific cause is resolved.

5. Coordination during execution and finalization
   - Reassess when either task expands its scope or discovers a dependency. Coordinate or defer the affected work before making incompatible edits; preserve existing work and conversation identity.
   - Make participating manual sessions and automated workers follow the same coordination rules. Be clear that a shared checkout and cooperative protocol cannot guarantee isolation from uncoordinated external edits.
   - Coordinate ownership of shared Git staging and finalization so one session cannot accidentally stage or commit another task's work. Build on existing index, sealing, finalization, and recovery behavior; extend it only where this coordination requires it.
   - Keep any new coordination wait narrowly scoped and recoverable after its owner exits or its cause is resolved. Preserve existing valid Git safeguards and explicit stops.

## Throughput and compatibility requirements

- Preserve the ability to run independent manual and automated work together, including concurrent planning and task-board edits.
- A dirty worktree, unrelated unstaged/untracked edits, or compatible same-file changes are not blockers by themselves.
- Avoid adding a blanket project lock merely because a manual task exists. Do not require every task to wait for every other task.
- Preserve working Git completion, publication, and recovery behavior. Do not weaken correctness checks or add redundant global fences to solve task-level coordination.
- Existing tasks without dependency or scope metadata remain usable; assess concrete concerns without forcing a board-wide migration before work can continue.
- Document user-facing behavior in features.md, record the implemented change under Unreleased in CHANGELOG.md, and update ARCHITECTURE.md for new boundaries and verification commands. Keep README.md short.

## Acceptance criteria

- An unrelated manual task and automated task can progress together, and both can see the other task's ownership and intended work.
- A task needing another task's unfinished API stays in Todo with a visible prerequisite reference; completion of the prerequisite makes it eligible automatically, subject to normal scheduling and explicit stops.
- A provisional Done prerequisite remains unsatisfied until its required finalization finishes.
- Two tasks proposing incompatible changes to the same behavior receive a specific conflict explanation and do not silently proceed with those incompatible edits. Potential overlap across different files is considered too.
- Compatible changes in one file and unrelated dirty work do not unnecessarily prevent progress or completion.
- A scope change during execution triggers reassessment of the affected tasks without stopping unrelated work.
- Dependency references survive edits, moves, and reordering; self-dependencies, cycles, and missing references are handled explicitly.
- Staging/finalization coordination preserves unrelated work and the current Git completion/recovery guarantees. New coordination state does not leave stale, unexplained holds after its cause is resolved.
- Verification covers both successful parallel progress and necessary waiting, including regressions for the permissive Git behavior that currently keeps work moving.

## Possible next step

Separate Git worktrees per task may provide stronger editing isolation in a later task. They are outside this implementation scope and are not a prerequisite for delivering the coordination above. Worktrees would still need dependency tracking and integration checks.

codex:01a16c21-5a61-72d2-aaf7-55e2976e9caa
