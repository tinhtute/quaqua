#!/bin/sh
# cos-inbox.sh — Claude Code hook for CoS lead mail notification.
# Runs `quaqua mail inbox --for cos` and injects the batched digest (<=20 lines).
# On UserPromptSubmit: injects additionalContext.
# On Stop: blocks once with the digest so the CoS reports it.
# Must be fast (<300 ms), silent when nothing new, and never fail the turn on error.

set -u

# Ensure any unexpected error exits cleanly with 0 (never fail the turn)
trap 'exit 0' ERR 2>/dev/null || true

INPUT=""
if [ ! -t 0 ]; then
  INPUT=$(cat 2>/dev/null || true)
fi

EVENT="UserPromptSubmit"
if [ "${1:-}" = "--stop" ] || echo "$INPUT" | grep -q '"hook_event_name"[[:space:]]*:[[:space:]]*"Stop"'; then
  EVENT="Stop"
elif [ "${1:-}" = "--user-prompt-submit" ]; then
  EVENT="UserPromptSubmit"
fi

# On Stop: if stop_hook_active is true, Claude is already continuing from a previous block.
# Do not block again (block at most once per turn).
if [ "$EVENT" = "Stop" ]; then
  if echo "$INPUT" | grep -q '"stop_hook_active"[[:space:]]*:[[:space:]]*true'; then
    exit 0
  fi
fi

# Locate quaqua binary
QUAQUA="${QUAQUA_BIN:-}"
if [ -z "$QUAQUA" ] && command -v quaqua >/dev/null 2>&1; then
  QUAQUA="quaqua"
elif [ -z "$QUAQUA" ] && [ -n "${CARGO_MANIFEST_DIR:-}" ]; then
  QUAQUA="$(dirname "$CARGO_MANIFEST_DIR")/../target/debug/quaqua"
elif [ -z "$QUAQUA" ] && [ -x "$HOME/.cargo/bin/quaqua" ]; then
  QUAQUA="$HOME/.cargo/bin/quaqua"
fi

if [ -z "$QUAQUA" ] || (! command -v "$QUAQUA" >/dev/null 2>&1 && [ ! -x "$QUAQUA" ]); then
  exit 0
fi

# Run the one digest renderer; --for cos advances only when the caller is cos.
RAW_OUT=$("$QUAQUA" mail inbox --for cos 2>/dev/null || true)

if [ -z "$RAW_OUT" ]; then
  exit 0
fi

# The renderer already caps at 20 lines; keep the cap if a caller ever widens it.
DIGEST=$(printf '%s\n' "$RAW_OUT" | head -n 20)

# Check if digest is empty after stripping whitespace
if [ -z "$(printf '%s' "$DIGEST" | tr -d '[:space:]')" ]; then
  exit 0
fi

# Output JSON for Claude Code hook
if [ "$EVENT" = "Stop" ]; then
  REASON="New lead mail received:
$DIGEST"
  if command -v jq >/dev/null 2>&1; then
    jq -n --arg ctx "$DIGEST" --arg reason "$REASON" '{
      decision: "block",
      reason: $reason,
      hookSpecificOutput: {
        hookEventName: "Stop",
        additionalContext: $ctx
      }
    }' 2>/dev/null || exit 0
  else
    python3 -c '
import json, sys
ctx = sys.argv[1]
reason = sys.argv[2]
obj = {
    "decision": "block",
    "reason": reason,
    "hookSpecificOutput": {
        "hookEventName": "Stop",
        "additionalContext": ctx
    }
}
print(json.dumps(obj))
' "$DIGEST" "$REASON" 2>/dev/null || exit 0
  fi
else
  if command -v jq >/dev/null 2>&1; then
    jq -n --arg ctx "$DIGEST" '{
      hookSpecificOutput: {
        hookEventName: "UserPromptSubmit",
        additionalContext: $ctx
      }
    }' 2>/dev/null || exit 0
  else
    python3 -c '
import json, sys
ctx = sys.argv[1]
obj = {
    "hookSpecificOutput": {
        "hookEventName": "UserPromptSubmit",
        "additionalContext": ctx
    }
}
print(json.dumps(obj))
' "$DIGEST" 2>/dev/null || exit 0
  fi
fi

exit 0
