#!/bin/sh
# cos-inbox.sh — Claude Code hook for CoS lead mail notification.
# Runs `quaqua inbox --for cos` for new unacked mail and injects a <=20-line digest (per R4).
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

FORMAT="hook-ups"
if [ "$EVENT" = "Stop" ]; then
  FORMAT="hook-stop"
fi

exec "$QUAQUA" inbox --for cos --format "$FORMAT"
