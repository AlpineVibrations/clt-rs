---
name: git-commit
description: Start repository tasks from the latest safe upstream state, then commit and optionally push Git changes with a safe, direct workflow. Use when beginning a new task in a Git repository or when the user asks to commit, save changes, create a git commit, push changes, finish Git work, or mentions /commit. Pull only when the checkout can be updated cleanly, inspect the diff, respect existing staged changes, stage only the intended logical change, generate a clear commit message from the staged diff, and sync again before pushing when requested.
---

# Git Commit Workflow

For ordinary manual or external work, use shell/Bash Git commands for inspection, staging, committing, rebasing, and pushing. Prefer non-interactive commands. Do not change Git config. In a released automated CLT run, use Git only for the permitted inspection, staging, and sealed local commit; never rebase or push.

For ordinary manual work, the caller may request either a commit-only or commit-and-push workflow. In automated CLT commit-and-push mode, Codex is authorized only to create the sealed local commit; CLT alone owns publication.

## Start-of-Task Sync

For ordinary interactive work, before editing files for each new task in an existing Git repository, inspect the checkout:

```bash
git status --short --branch
```

If the worktree and index are clean, HEAD is attached to a branch, and that branch has an upstream, update it before starting work:

```bash
git pull --ff-only
```

Use fast-forward-only here so the startup sync cannot create a merge commit, rebase local commits, or disturb user work. If the checkout is dirty, detached, has no upstream, or cannot fast-forward, do not stash, discard, commit, switch branches, or integrate changes solely to make the pull succeed. Continue from the current checkout when safe and mention that the startup pull was skipped or could not complete.

Do not run that startup procedure from inside a released automated CLT task. For a fresh task in `commit` or `commit-and-push` mode, the intended checkout, branch, and upstream must be configured before scheduling. A newly created Todo may remain unstaged: before spawning or releasing Codex, CLT requires the index to match `HEAD`, performs the safe fast-forward-only startup sync unless an older `WORKING` journal requires preserving its history, and checkpoints a dirty task board in a dedicated `CLT Agent` commit without including unrelated worktree changes. It captures the resulting commit, branch, worktree baseline, and upstream configuration and persists that server-owned launch state. Any spawned child remains gated until its remaining session fences are registered. It releases the agent only after preparation succeeds. Commit-and-push requires an attached branch with exactly one configured upstream at that boundary.

After release, inspect the frozen state but do not pull, fetch/synchronize, merge, rebase, switch branches, reset history, reconfigure the upstream, or push. Move the selected Todo task to Doing before implementation so CLT can recheck and bind the frozen launch record to the session's `WORKING` journal.

An unconsumed pre-registration launch boundary is immutable. Do not retry by recapturing it from the current checkout or by cleaning/unregistering the project. Even if a reaped child exits before announcing a session, CLT preserves the record until the exact worker is terminal, no session-control row owns the run token, and the checkout and Git mode still match; otherwise it fails closed.

## Shared Dirty Worktrees

A dirty worktree is expected when a person, an interactive session, or an independent worker has changes in progress. Dirtiness alone is never a reason to block or abandon the current task.

- Treat the status and diff observed before editing as the baseline, and preserve those pre-existing changes.
- Continue with non-conflicting work even when unrelated files are modified.
- A pre-existing change in the same file is not automatically a conflict. Re-read the affected area, apply the task's change against the current contents, and preserve both changes when the combined result is clear.
- Stop only for a real conflict: the same behavior or lines require incompatible outcomes and the correct combined result cannot be determined safely.
- At commit time, stage only the current task's paths or hunks. Leave unrelated unstaged changes in place. When a file contains mixed changes, use patch staging and verify the cached diff before committing.

Unstaged work can safely coexist because CLT records a baseline. The Git index is a cooperative boundary during an automated finalization: humans, interactive sessions, and parallel tools sharing the checkout must not stage or unstage until it settles. CLT rejects a fresh run with pre-existing staged changes and detects many later changes, but Git does not record which actor staged a new clean-file change.

## Default Flow

```bash
git status --short --branch
git diff --stat
git diff
git diff --staged --stat
git diff --staged
git log --oneline -5
```

Then:

1. Decide what belongs in the commit.
2. Stage the intended files.
3. Verify the staged diff.
4. Commit with a message based on the staged diff.
5. Push only when explicitly requested for ordinary manual/external work. In automated CLT mode, stop after the verified local commit and let CLT publish it.

## Safety Rules

- Follow repo instructions in `AGENTS.md`, `README`, `CONTRIBUTING`, or project docs when present.
- Do not create or switch branches unless the user asks or the repo explicitly requires it.
- If the repo has no branch rule, committing on the current branch is acceptable, including `main` or `master`.
- Do not push unless the user asks during ordinary manual/external work. Never push from an automated CLT task, even when its mode is commit-and-push.
- Do not force-push unless the user explicitly asks; use `--force-with-lease`, never plain `--force`.
- Do not amend commits unless the user asks.
- Do not skip hooks with `--no-verify` unless the user explicitly asks.
- Never commit secrets, credentials, tokens, private keys, `.env` files, local config, logs, caches, or temporary/generated output unless the user explicitly asks.

## Branch Instructions

These instructions apply to ordinary interactive work. In an automated CLT Git-enabled run, the branch was selected and frozen before the agent was released; do not create or switch branches.

Repo instructions may specify a required branch for a feature, bugfix, or plan. Check `AGENTS.md`, project docs, feature plans, and design docs for branch guidance when they are relevant to the work.

If a repo doc or the user names a required branch:

- Use that branch for the work and commit.
- If already on that branch, continue.
- If the branch exists locally, switch to it with `git switch <branch>`.
- If the branch does not exist, create it from the current base with `git switch -c <branch>`.
- Do not invent a feature branch name when no branch is specified.

If a branch is not specified in some way, then it should be done on master or main branch.

If uncommitted work already exists on a different branch, inspect status first. Switch only when the move is clearly safe; otherwise ask before moving work across branches.

Watch especially for:

```text
.env
.env.*
credentials.json
*.pem
*.key
*.crt
*.log
.DS_Store
node_modules/
dist/
build/
coverage/
.cache/
```

## Staging

Treat already staged changes as intentional. Inspect staged and unstaged changes before adding anything else.

If changes are already staged:

- Use the staged diff as the commit scope by default.
- Mention unstaged or untracked changes only if they look relevant.
- Add more files only when they clearly belong to the same logical change or the user asked to commit everything.

The exception is an automated CLT finalization. Pre-existing staged changes are not implicitly part of the task commit. Isolate the current task's paths or hunks and stop if that cannot be done without disturbing or committing another actor's staged work.

If nothing is staged:

- Stage all changes with `git add -A` only when they form one logical commit.
- Stage specific files when the work is mixed or risky:

```bash
git add path/to/file1 path/to/file2
```

After staging, always verify:

```bash
git diff --staged --stat
git diff --staged
```

If unrelated changes are present, commit only the requested or coherent set and leave the rest untouched.

Pre-existing unstaged changes do not prevent a commit. Use path-specific or patch staging to isolate the completed task. Do not require the whole worktree to become clean before committing.

A Todo or other task-board edit added after an automated CLT run starts may also remain unstaged. Preserve it and continue finalization: CLT proves the exact staged task tree, so the concurrent board edit stays outside the sealed commit. Use path-specific or patch staging for the selected task's transition instead of adding the whole board.

## CLT Task Updates

If the work is tracked with `clt`, update the current task through `clt` before staging and include the resulting task-board changes in the same commit as the implementation. Check both supported layouts:

- Markdown files: `tasks/todo.md`, `tasks/doing.md`, and `tasks/done.md`.
- Task folders: `tasks/todo/`, `tasks/doing/`, and `tasks/done/`.

Inspect the board changes even when implementation files are already staged:

```bash
git status --short -- tasks/
git diff -- tasks/
git diff --staged -- tasks/
```

Stage the current task's content and status transition, including both sides of a move or deletion. Use `git add -A -- tasks/` only when every task-board change belongs to the same logical commit; otherwise stage the exact task paths and leave unrelated task changes untouched.

For managed Git automation, directory-backed status moves preserve the existing task path and order rather than converting or renumbering it. CLT rejects folder-backed Todo-to-Markdown Doing and folder-backed Doing-to-Markdown Done routes before release. Exact session-linked duplicates left by a crash are repaired; ambiguous copies remain fenced.

## Automated CLT Finalization

When an automated CLT prompt enables `commit` or `commit-and-push` mode, `clt done` starts a durable task finalization. The task may already appear in the Done store, but that move is provisional while CLT reports `FINALIZING`.

CLT already completed its scheduler-owned startup preparation and branch/upstream validation, persisted the server-owned launch state, and only then released this automated run. That preparation may deliberately preserve the current commit when an older `WORKING` journal depends on it. Do not repeat the preparation. Move the selected committed Todo task to Doing before implementation; CLT rechecks the starting commit, branch, baseline, and upstream configuration and binds the session journal at that seam. Do not pull, fetch/synchronize, merge, rebase, switch branches, reset, reconfigure the destination, or rewrite history afterward.

Before `clt done`, run every available formatter, linter, signing check, and hook check that can mutate files. Then stage the verified implementation plus the active Doing task with its dated completion note and terminal session marker. Inspect the staged diff, and leave all unrelated baseline work unstaged. `clt done` uses a private index to project that task into Done and seals the exact resulting full repository tree, not merely the changed paths or patch. It then moves the worktree entry provisionally. Stage only the resulting board transition and inspect the complete staged diff again before committing.

Create exactly one normal commit containing all of the following:

- the implementation and tests or documentation belonging to the task;
- its `COMPLETED YYYY-MM-DD:` note;
- the complete task-board transition into Done; and
- one exact commit trailer identifying the linked task session: `CLT-Task: codex:<session-id>`.

Read the terminal `codex:<session-id>` marker from the task entry and preserve it in the board change. Add the trailer as a separate commit-message paragraph, for example:

```bash
git commit \
  -m "Clear summary message" \
  -m "CLT-Task: codex:019abcde-1234-7890-abcd-0123456789ab"
```

Use ordinary `git commit` so repository hooks and signing behavior remain active. Do not use `commit-tree`, an alternate index, amend, a merge commit, or a second board-only commit to simulate finalization. After the command returns, inspect the created commit. CLT will accept only the exact sealed full tree and manifest parent together with the matching task identity, CLT Agent author/committer identity, and one exact trailer.

If a commit hook changes files or fails after sealing, fix and stage the complete corrected payload, run `clt list done` to confirm the current index, then run `clt done done <index>` to reseal that provisional Done entry. Inspect the new staged diff and retry the one commit. If the process stops, CLT resumes this exact session and checks whether the intended commit already exists before creating anything. Never move the provisional Done entry back to Doing merely because acknowledgement was lost, and never duplicate a commit that CLT can prove already succeeded. If completed-task evidence exists but the frozen start journal was lost, stop: CLT deliberately fails closed because it cannot reconstruct the exact-one-commit boundary safely.

After creating and inspecting the exact task commit, exit without pushing. In commit-and-push mode CLT proves the local commit, performs publication itself, and retries `PUSH-PENDING` without resuming Codex. A blocked `WORKING` journal may yield to another Todo during blocked-recovery backoff while its history is preserved; `FINALIZING` and `PUSH-PENDING` block later project work.

## Failed checks and completed implementation

Classify a failing command before blocking finalization. When the implementation meets the original task's acceptance criteria and its relevant checks pass, a proven independent pre-existing or environment-only failure should not leave finished code uncommitted. Reproduce the failure on the frozen starting revision in an isolated directory without switching or resetting the active checkout; retain the revision, commands, matching output, passing checks, and unblock requirement.

Use `clt list doing` and `clt follow-up doing <index> "Independent remaining work" --blocked "Evidence, baseline reproduction, and unblock requirement"` to record the separate blocked Doing task. It gets a `clt-follow-up:<parent-session-id>` reference, not the parent's terminal `codex:` marker. Include the reference and validation evidence in the original task's COMPLETED note. Stage the follow-up, verified implementation, and original Doing task before sealing; include the complete Done move and follow-up in the same single task commit. Leave unrelated board edits unstaged. Record the follow-up without starting work on it in this run.

If the original implementation is incomplete, a task-relevant check fails, or independence is unproven, keep the original task blocked and preserve its changes. Never use a follow-up to bypass an acceptance criterion, required signing, or commit hooks. Reseal any corrected payload after a hook failure using the normal CLT contract.

## Commit Message

Prefer the repo's existing style from recent commits.

If recent commits use Conventional Commits, use:

```text
feat(scope): add behavior
fix(scope): correct behavior
docs(scope): update documentation
refactor(scope): simplify implementation
test(scope): add coverage
chore(scope): update maintenance files
```

Otherwise use a plain imperative message:

```text
Add Git commit workflow
Fix task status handling
Update setup instructions
```

Rules:

- Base the message on the staged diff, not the user's rough wording.
- Use imperative mood: "add", "fix", "update".
- Keep the subject short and specific, preferably under 72 characters.
- Add a body only when it explains useful context not obvious from the diff.

Commit with:

```bash
git commit -m "Clear summary message"
```

or, when a body is useful:

```bash
git commit -m "Clear summary message" -m "Explain the relevant context."
```

For automated CLT finalization, the required `CLT-Task: codex:<session-id>` trailer is the final message paragraph even when no other body is needed.

## Hook Failures

If a commit hook fails:

1. Read the hook output.
2. Fix the issue when the fix is clear.
3. Re-stage changed files.
4. Run the commit again.

If the fix is unclear, report the failure and ask before continuing. Do not bypass hooks unless explicitly requested.

## Manual Or External Pull And Push

When the user asks to push outside automated CLT finalization, sync first. Use a normal pull so Git honors the user's existing `pull.rebase` or branch configuration:

```bash
git pull --autostash
```

The pull may merge or rebase according to that configuration. Do not pass `--rebase` or `--no-rebase`, and do not change Git configuration, unless the user explicitly asks for a specific strategy.

Outside automated CLT finalization, if conflicts occur:

1. Inspect the conflict.
2. Resolve only when the correct resolution is clear.
3. Continue with `git rebase --continue` for a rebase or `git merge --continue` for a merge.
4. Ask the user when the correct resolution is ambiguous.

Outside automated CLT finalization, then push:

```bash
git push
```

Outside automated CLT finalization, if the branch has no upstream:

```bash
git push -u origin "$(git branch --show-current)"
```

Do not push tags unless the user explicitly asks.

The commands above are never part of an automated CLT run. In automated commit-and-push mode, CLT resolves the effective remote using `branch.<name>.pushRemote`, then `remote.pushDefault`, then the upstream remote, and freezes its one concrete push URL plus the upstream merge ref. After local proof, CLT alone invokes an explicit non-force `<frozen-oid>:<frozen-ref>` publication to the frozen URL, ignoring implicit routing at publication time, and independently proves remote containment. A failure remains `PUSH-PENDING`; the scheduler retries it without Codex and blocks later project work. External recovery may publish deliberately, but the automated agent must never run a push command.

## Final Response

Summarize the result briefly:

```text
Committed changes.

Commit: abc1234 Add Git commit workflow
Branch: main
Files changed: 3
Pushed: no
Checks: not run
```

If nothing was committed, say why and include the current status.
