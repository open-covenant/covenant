#!/usr/bin/env bash
# Spawn a fresh agent session in a new terminal window, pointed at the
# autonomy loop entry doc (docs/autonomous-development.md) and ready to
# call `autonomy-continue.mjs` for the next unblocked task.
#
# Used when the current autonomous run has accumulated enough state that a
# clean context will produce better-quality work on the next task slice.
#
# Trust state for the project folder is assumed cached by the configured
# agent client. If the script is run for a brand-new folder, the operator may
# need to accept the prompt manually the first time.
#
# Usage:
#   scripts/handover.sh                                 # current dir
#   scripts/handover.sh path/to/dir                     # specific project dir
#   AGENT_CMD='claude --dangerously-skip-permissions' scripts/handover.sh
#
# AGENT_CMD is invoked as a literal command in a non-interactive subshell, so
# zsh aliases do not resolve here.

set -euo pipefail

usage() {
  cat <<'EOF'
usage: handover.sh [path]

Spawn a fresh agent session in a new terminal window, pointed at the
autonomy loop entry doc (docs/autonomous-development.md) and ready to
run autonomy-continue.

  path  optional project directory (defaults to cwd). Must contain
        docs/autonomous-development.md and agent-os/scripts/.

Environment:
  AGENT_CMD                       agent client command (default: claude ...)
  COVENANT_HANDOVER_TRUST_DELAY   seconds to wait before sending the
                                  trust-prompt Enter keystroke (default: 3)
EOF
}

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
esac

DIR="${1:-$(pwd)}"
DIR_ABS="$(cd "$DIR" && pwd)"
ENTRY_DOC="$DIR_ABS/docs/autonomous-development.md"
AGENT_CMD="${AGENT_CMD:-${CLAUDE_CMD:-claude --model claude-opus-4-7 --effort max --dangerously-skip-permissions}}"

if [ ! -f "$ENTRY_DOC" ]; then
  echo "handover.sh: no docs/autonomous-development.md at $ENTRY_DOC" >&2
  echo "  Run handover.sh from the repo root (or pass it as the first argument)." >&2
  exit 1
fi

AGENT_BIN="${AGENT_CMD%% *}"
if ! command -v "$AGENT_BIN" >/dev/null 2>&1; then
  echo "handover.sh: '$AGENT_BIN' not found on PATH." >&2
  echo "  Set AGENT_CMD to the agent client binary + flags, e.g.:" >&2
  echo "  AGENT_CMD='claude --dangerously-skip-permissions' scripts/handover.sh" >&2
  exit 1
fi

# --- Session lock bootstrap ---
#
# Generate a fresh session-id, write it to <repo-root>/.covenant-session-id,
# and pass it to the spawned agent via $COVENANT_SESSION_ID. The repo's
# pre-commit hook (hooks/pre-commit) reads both the env var and the file
# and refuses commits when they do not match, so an older session that
# stays alive (operator forgot to close the previous Terminal window)
# cannot land work after this one starts.
#
# Also (idempotently) sets `core.hooksPath = hooks` on the repo so the
# tracked hook actually fires. Operator action: zero. The hook chains to
# any existing global covenant-hooks/pre-commit so the identity / leakage
# checks still run.

REPO_ROOT="$(git -C "$DIR_ABS" rev-parse --show-toplevel 2>/dev/null || echo "$DIR_ABS")"
LOCK_PATH="$REPO_ROOT/.covenant-session-id"

if command -v uuidgen >/dev/null 2>&1; then
  SESSION_ID="$(uuidgen | tr '[:upper:]' '[:lower:]')"
else
  # Fallback: 32 hex chars from /dev/urandom. Same entropy class as
  # uuidgen for our purposes (it's a single-process opaque token).
  SESSION_ID="$(head -c 16 /dev/urandom | xxd -p)"
fi
printf '%s\n' "$SESSION_ID" >"$LOCK_PATH"
chmod 0600 "$LOCK_PATH" 2>/dev/null || true

if [ -d "$REPO_ROOT/hooks" ]; then
  current_hooks_path="$(git -C "$REPO_ROOT" config --get core.hooksPath 2>/dev/null || true)"
  if [ "$current_hooks_path" != "hooks" ]; then
    git -C "$REPO_ROOT" config core.hooksPath hooks
  fi
fi

PROMPT='Read docs/autonomous-development.md to anchor on the loop, then run `node agent-os/scripts/autonomy-continue.mjs` to pick up the next unblocked task. Drive it through the workflow states (proposed → triaged → planned → in_progress → self_review → cross_review → validation → ready → integrated) using `node agent-os/scripts/autonomy-transition.mjs <id> <state> --actor <role> --note "<why>"`. Run the task'\''s declared verification commands plus `bash agent-os/scripts/validate.sh --scripts`. Commit with the Open Covenant Automation identity (`node agent-os/scripts/configure-git-identity.mjs` configures it) and a conventional-commit subject; push to origin/main when integrated. Do not stop unless every candidate is blocked or a true blocker appears.'

# Build a temporary launch script. Putting the multi-line shell command in a
# tempfile lets us avoid double-escaping into AppleScript / xterm -e.
LAUNCH=$(mktemp -t covenant-handover-XXXXXX)
chmod 0700 "$LAUNCH"
cat >"$LAUNCH" <<EOF
#!/bin/zsh -i
# Auto-removed after launch; the new agent session captures the handover
# from HANDOVER.md, not this file.
cd $(printf '%q' "$DIR_ABS")
export COVENANT_SESSION_ID=$(printf '%q' "$SESSION_ID")
exec ${AGENT_CMD} $(printf '%q' "$PROMPT")
EOF

case "$(uname -s)" in
  Darwin)
    # `do script` is a fire-and-forget command (returns the new tab id).
    # The trust-folder prompt may render before the agent client reads the prompt
    # argument, so we send a `return` keystroke after a short delay. If the
    # folder was already trusted, the Enter is harmless: it lands as an empty
    # submission, which is a no-op.
    TRUST_DELAY="${COVENANT_HANDOVER_TRUST_DELAY:-3}"
    /usr/bin/osascript <<APPLE
tell application "Terminal"
  activate
  do script "$LAUNCH; rm -f $LAUNCH"
end tell
delay $TRUST_DELAY
tell application "System Events" to keystroke return
APPLE
    ;;
  Linux)
    SPAWNED=0
    for t in gnome-terminal kitty alacritty wezterm xterm; do
      command -v "$t" >/dev/null 2>&1 || continue
      case "$t" in
        gnome-terminal) "$t" --working-directory="$DIR_ABS" -- /bin/sh -c "$LAUNCH; rm -f $LAUNCH" & SPAWNED=1; break ;;
        kitty)          "$t" --directory="$DIR_ABS" /bin/sh -c "$LAUNCH; rm -f $LAUNCH" & SPAWNED=1; break ;;
        alacritty)      "$t" --working-directory="$DIR_ABS" -e /bin/sh -c "$LAUNCH; rm -f $LAUNCH" & SPAWNED=1; break ;;
        wezterm)        "$t" start --cwd "$DIR_ABS" -- /bin/sh -c "$LAUNCH; rm -f $LAUNCH" & SPAWNED=1; break ;;
        xterm)          "$t" -e "/bin/sh -c '$LAUNCH; rm -f $LAUNCH'" & SPAWNED=1; break ;;
      esac
    done
    if [ "$SPAWNED" -eq 0 ]; then
      echo "handover.sh: no supported terminal emulator on PATH" >&2
      rm -f "$LAUNCH"
      exit 1
    fi
    ;;
  *)
    echo "handover.sh: unsupported platform: $(uname -s)" >&2
    rm -f "$LAUNCH"
    exit 1
    ;;
esac

echo "handover.sh: launched ${AGENT_CMD} in a new terminal at $DIR_ABS"
echo "  next session will read docs/autonomous-development.md and run autonomy-continue.mjs"
echo "  session-id: $SESSION_ID (written to $LOCK_PATH)"
echo "  any older session attempting a commit will be refused by hooks/pre-commit"
