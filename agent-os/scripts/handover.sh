#!/usr/bin/env bash
# scripts/handover.sh — spawn a fresh Claude Code session in a new Terminal
# window, pointed at HANDOVER.md.
#
# Used when the current autonomous run has accumulated enough state that a
# clean context will produce better-quality work on the next sprint chunk
# (e.g., MCP spec interpretation, Solana SPL programming, Tailwind migration).
#
# Trust state for the project folder is assumed cached (Claude Code remembers
# per-folder after the first "yes I trust this folder" acceptance). If the
# script is ever run for a brand-new folder, the operator accepts the prompt
# manually that first time.
#
# Usage:
#   scripts/handover.sh                                 # current dir
#   scripts/handover.sh path/to/dir                     # specific project dir
#   CLAUDE_CMD='claude --dangerously-skip-permissions' scripts/handover.sh
#
# CLAUDE_CMD is invoked as a literal command in a non-interactive subshell, so
# zsh aliases like `cc` don't resolve here — we call the `claude` binary
# directly with the same flags the operator's alias carries.

set -euo pipefail

DIR="${1:-$(pwd)}"
DIR_ABS="$(cd "$DIR" && pwd)"
HANDOVER_PATH="$DIR_ABS/HANDOVER.md"
CLAUDE_CMD="${CLAUDE_CMD:-claude --model claude-opus-4-7 --effort max --dangerously-skip-permissions}"

if [ ! -f "$HANDOVER_PATH" ]; then
  echo "handover.sh: no HANDOVER.md at $HANDOVER_PATH" >&2
  echo "  (write it first; the next session reads it as the canonical entry point)" >&2
  exit 1
fi

CLAUDE_BIN="${CLAUDE_CMD%% *}"
if ! command -v "$CLAUDE_BIN" >/dev/null 2>&1; then
  echo "handover.sh: '$CLAUDE_BIN' not found on PATH." >&2
  echo "  Set CLAUDE_CMD to the Claude Code binary + flags, e.g.:" >&2
  echo "  CLAUDE_CMD='claude --dangerously-skip-permissions' scripts/handover.sh" >&2
  exit 1
fi

PROMPT='Read HANDOVER.md, then WORKFLOW.md, then PROJECT_STATE.md, then the tail of SPRINT_LOG.md (the latest "Resume from here" block). Continue the autonomous sprint loop from there. The previous session paused itself for a clean context; pick up exactly where it left off, with the same rules. Do not stop unless a true blocker appears.'

# Build a temporary launch script. Putting the multi-line shell command in a
# tempfile lets us avoid double-escaping into AppleScript / xterm -e.
LAUNCH=$(mktemp -t covenant-handover-XXXXXX)
chmod 0700 "$LAUNCH"
cat >"$LAUNCH" <<EOF
#!/bin/zsh -i
# Auto-removed after launch; the new Claude session captures the handover
# from HANDOVER.md, not this file.
cd $(printf '%q' "$DIR_ABS")
exec ${CLAUDE_CMD} $(printf '%q' "$PROMPT")
EOF

case "$(uname -s)" in
  Darwin)
    # `do script` is a fire-and-forget command (returns the new tab id).
    # The trust-folder prompt may render before Claude reads the prompt
    # argument, so we send a `return` keystroke after a short delay. If the
    # folder was already trusted, the Enter is harmless — it lands in
    # Claude's input box as an empty submission, which is a no-op.
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

echo "handover.sh: launched ${CLAUDE_CMD} in a new terminal at $DIR_ABS"
echo "  → next session will read HANDOVER.md and resume from SPRINT_LOG.md's tail"
