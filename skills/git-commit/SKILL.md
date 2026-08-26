---
name: git-commit
description: Start repository tasks from the latest safe upstream state, then commit and optionally push Git changes with a safe, direct workflow. Use when beginning a new task in a Git repository or when the user asks to commit, save changes, create a git commit, push changes, finish Git work, or mentions /commit. Pull only when the checkout can be updated cleanly, inspect the diff, respect existing staged changes, stage only the intended logical change, generate a clear commit message from the staged diff, and sync again before pushing when requested.
---

# Git Commit Workflow

Use shell/Bash Git commands for inspection, staging, committing, rebasing, and pushing. Prefer non-interactive commands. Do not change Git config.

The caller may request either a commit-only or commit-and-push workflow. An automated agent prompt that explicitly selects commit and push authorizes pushing the completed task for that run.

## Start-of-Task Sync

Before editing files for each new task in an existing Git repository, inspect the checkout:

```bash
git status --short --branch
```

If the worktree and index are clean, HEAD is attached to a branch, and that branch has an upstream, update it before starting work:

```bash
git pull --ff-only
```

Use fast-forward-only here so the startup sync cannot create a merge commit, rebase local commits, or disturb user work. If the checkout is dirty, detached, has no upstream, or cannot fast-forward, do not stash, discard, commit, switch branches, or integrate changes solely to make the pull succeed. Continue from the current checkout when safe and mention that the startup pull was skipped or could not complete.

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
5. Push only if requested by the user or explicitly selected by the automated agent prompt, after `git pull --autostash`.

## Safety Rules

- Follow repo instructions in `AGENTS.md`, `README`, `CONTRIBUTING`, or project docs when present.
- Do not create or switch branches unless the user asks or the repo explicitly requires it.
- If the repo has no branch rule, committing on the current branch is acceptable, including `main` or `master`.
- Do not push unless the user asks to push or the automated agent prompt explicitly selects commit and push.
- Do not force-push unless the user explicitly asks; use `--force-with-lease`, never plain `--force`.
- Do not amend commits unless the user asks.
- Do not skip hooks with `--no-verify` unless the user explicitly asks.
- Never commit secrets, credentials, tokens, private keys, `.env` files, local config, logs, caches, or temporary/generated output unless the user explicitly asks.

## Branch Instructions

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

## Hook Failures

If a commit hook fails:

1. Read the hook output.
2. Fix the issue when the fix is clear.
3. Re-stage changed files.
4. Run the commit again.

If the fix is unclear, report the failure and ask before continuing. Do not bypass hooks unless explicitly requested.

## Pull And Push

When the user asks to push or the automated agent prompt explicitly selects commit and push, sync first. Use a normal pull so Git honors the user's existing `pull.rebase` or branch configuration instead of forcing a different integration strategy:

```bash
git pull --autostash
```

The pull may merge or rebase according to that configuration. Do not pass `--rebase` or `--no-rebase`, and do not change Git configuration, unless the user explicitly asks for a specific strategy.

If conflicts occur:

1. Inspect the conflict.
2. Resolve only when the correct resolution is clear.
3. Continue with `git rebase --continue` for a rebase or `git merge --continue` for a merge.
4. Ask the user when the correct resolution is ambiguous.

Then push:

```bash
git push
```

If the branch has no upstream:

```bash
git push -u origin "$(git branch --show-current)"
```

Do not push tags unless the user explicitly asks.

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
