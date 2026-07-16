#!/usr/bin/env bash
set -euo pipefail

MAX_TASKS="${MAX_TASKS:-0}"      # 0 = unlimited
TIMEOUT="${TIMEOUT:-45m}"
LOG_DIR="${LOG_DIR:-.codex-task-runs}"
SLEEP_BETWEEN="${SLEEP_BETWEEN:-2}"

mkdir -p "$LOG_DIR"

if command -v timeout >/dev/null 2>&1; then
  TIMEOUT_CMD="timeout"
elif command -v gtimeout >/dev/null 2>&1; then
  TIMEOUT_CMD="gtimeout"
else
  echo "Missing timeout command."
  echo "On macOS, run: brew install coreutils"
  exit 1
fi

LOCK_DIR=".codex-task-loop.lock"

if ! mkdir "$LOCK_DIR" 2>/dev/null; then
  echo "Another codex task loop appears to be running: $LOCK_DIR"
  exit 1
fi

printf 'clt-script\n' > "$LOCK_DIR/owner"
printf 'clt-script-%s\n' "$$" > "$LOCK_DIR/holder"
printf '%s\n' "$$" > "$LOCK_DIR/pid"
date +%s > "$LOCK_DIR/created_at"

cleanup() {
  rm -rf "$LOCK_DIR"
}
trap cleanup EXIT

i=0

while true; do
  i=$((i + 1))

  if [ "$MAX_TASKS" -gt 0 ] && [ "$i" -gt "$MAX_TASKS" ]; then
    echo "Reached MAX_TASKS=$MAX_TASKS. Stopping."
    exit 0
  fi

  echo "----------------------------------------"
  if [ "$MAX_TASKS" -gt 0 ]; then
    echo "Codex task run $i of $MAX_TASKS"
  else
    echo "Codex task run $i"
  fi
  echo "----------------------------------------"

  RUN_ID="$(date +%Y%m%d-%H%M%S)-$i"
  OUT_FILE="$LOG_DIR/$RUN_ID.out"
  ERR_FILE="$LOG_DIR/$RUN_ID.err"

  PROMPT=$(cat <<'EOF'
You are working in this repo.

Use the existing task-management CLI tooling: clt.

Your job for this run:

1. Inspect the task board using the task CLI.
2. Pick the next available TODO / ready task.
3. If there are no available tasks, say exactly: NO_TASKS_LEFT
4. If there is a task:
   - move it to doing
   - complete that task
   - run the relevant checks/tests
   - update the task using the task CLI
   - mark it done if completed
   - include a concise note with what changed and what commands/tests ran
5. Stop after that one task.
6. Do not start another task.
7. Exit when finished.

Safety rules:
- Do not overwrite unrelated user changes.
- Before making edits, inspect the current repo state.
- If the task is blocked or cannot be completed safely, update the task with a concise blocked note instead of forcing it.
EOF
)

  set +e

  # Automated task runs need Git metadata access and cannot pause for approval.
  # Only run this loop in a trusted checkout or an externally isolated environment.
  "$TIMEOUT_CMD" "$TIMEOUT" codex \
    --sandbox danger-full-access \
    --ask-for-approval never \
    exec -C "$PWD" "$PROMPT" \
    > >(tee "$OUT_FILE") \
    2> >(tee "$ERR_FILE" >&2)

  STATUS=$?

  set -e

  echo "Codex exit status: $STATUS"
  echo "Output log: $OUT_FILE"
  echo "Error log:  $ERR_FILE"

  if grep -q "NO_TASKS_LEFT" "$OUT_FILE"; then
    echo "No tasks left. Stopping."
    exit 0
  fi

  if [ "$STATUS" -eq 124 ]; then
    echo "Codex timed out after $TIMEOUT. Stopping."
    echo "A task may have been left in doing state. Check the task board."
    exit 124
  fi

  if [ "$STATUS" -ne 0 ]; then
    echo "Codex failed. Stopping."
    exit "$STATUS"
  fi

  echo "Task run $i complete. Starting next fresh Codex context."
  sleep "$SLEEP_BETWEEN"
done
