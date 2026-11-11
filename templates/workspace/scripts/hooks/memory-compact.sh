#!/usr/bin/env bash
# memory-compact.sh — the CoS pre-compaction flush (G4).
#
# Two modes, both driven by the Claude Code hook JSON on stdin:
#
#   --pre-compact (default)  PreCompact. Writes memory/.compact/<session_id>.md
#                            and prints the compaction steer on stdout; Claude
#                            Code appends a PreCompact hook's stdout to the
#                            compact instructions (exit 0). Exit 2 would block
#                            the compaction, so this script never returns 2.
#   --session-start          SessionStart. When source=compact, prints the newest
#                            snapshot as hookSpecificOutput.additionalContext;
#                            silent for every other source.
#   --dry-run                Build the snapshot, print it on stderr and write
#                            nothing.
#
# Invariants:
#   - always exits 0: a hook that fails must never fail or block a compaction;
#   - the only path written is memory/.compact/ (memory/ itself is never created);
#   - a missing input shortens the snapshot instead of failing it.
#
# The snapshot carries paths and one-line states, never file bodies: SessionStart
# re-injects it into the next session's context.
set -uo pipefail

MODE=--pre-compact
DRY=0
for arg in "$@"; do
  case "$arg" in
    --pre-compact) MODE=--pre-compact ;;
    --session-start) MODE=--session-start ;;
    --dry-run) DRY=1 ;;
  esac
done

INPUT=""
if [ ! -t 0 ]; then
  INPUT=$(cat 2>/dev/null || true)
fi

# json_str <key>: the string value of a top-level hook field, empty when absent.
# The hook JSON is flat and its values are ids and paths, so a quote-bounded
# match is enough; a value carrying an escaped quote is the one known miss.
json_str() {
  printf '%s' "$INPUT" \
    | grep -o "\"$1\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" \
    | head -n 1 \
    | sed -e 's/^[^:]*:[[:space:]]*"//' -e 's/"$//'
}

# trunc <max-bytes>: copy stdin, cutting at a line boundary once the byte count
# would pass <max-bytes>. Cutting on lines keeps multi-byte characters whole;
# the 48 bytes reserved for the marker keep the result inside <max-bytes>.
# LC_ALL=C pins `length` to bytes: gawk would count characters in a UTF-8
# locale and let the file grow past the cap.
trunc() {
  LC_ALL=C awk -v max="$1" '
    { n += length($0) + 1
      if (n > max - 48) { print "[memory-compact: snapshot truncated]"; exit }
      print }'
}

# json_escape: stdin to a JSON string body (no surrounding quotes). Every
# U+0000–U+001F control becomes \u00XX — an ESC byte from a pasted ANSI log
# would otherwise make the SessionStart output invalid JSON.
json_escape() {
  awk 'BEGIN {
         ORS = ""
         for (i = 0; i < 32; i++) ctl[sprintf("%c", i)] = sprintf("\\u%04x", i)
       }
       { if (NR > 1) printf "\\n"
         s = $0
         out = ""
         n = length(s)
         for (i = 1; i <= n; i++) {
           c = substr(s, i, 1)
           if (c == "\\") out = out "\\\\"
           else if (c == "\"") out = out "\\\""
           else if (c in ctl) out = out ctl[c]
           else out = out c
         }
         printf "%s", out }'
}

# ── workspace root ─────────────────────────────────────────────────────────
# CLAUDE_PROJECT_DIR is what the settings entry runs under; the hook cwd is the
# fallback. Never $0: the shipped copy lives under templates/workspace.
ROOT=""
if [ -n "${CLAUDE_PROJECT_DIR:-}" ] && [ -d "${CLAUDE_PROJECT_DIR:-}" ]; then
  ROOT=$CLAUDE_PROJECT_DIR
else
  HOOK_CWD=$(json_str cwd)
  if [ -n "$HOOK_CWD" ] && [ -d "$HOOK_CWD" ]; then ROOT=$HOOK_CWD; else ROOT=$PWD; fi
fi

# ── quaqua binary (transcripts tail) ───────────────────────────────────────
QUAQUA="${QUAQUA_BIN:-}"
if [ -z "$QUAQUA" ] && command -v quaqua >/dev/null 2>&1; then
  QUAQUA=quaqua
elif [ -z "$QUAQUA" ] && [ -n "${CARGO_MANIFEST_DIR:-}" ] \
  && [ -x "$(dirname "$CARGO_MANIFEST_DIR")/../target/debug/quaqua" ]; then
  QUAQUA="$(dirname "$CARGO_MANIFEST_DIR")/../target/debug/quaqua"
elif [ -z "$QUAQUA" ] && [ -x "$HOME/.cargo/bin/quaqua" ]; then
  QUAQUA="$HOME/.cargo/bin/quaqua"
fi
if [ -n "$QUAQUA" ] && ! command -v "$QUAQUA" >/dev/null 2>&1 && [ ! -x "$QUAQUA" ]; then
  QUAQUA=""
fi

# ── SessionStart ───────────────────────────────────────────────────────────
if [ "$MODE" = --session-start ]; then
  [ "$(json_str source)" = compact ] || exit 0
  dir=$ROOT/memory/.compact
  [ -d "$dir" ] || exit 0
  newest=""
  for f in "$dir"/*.md; do
    [ -f "$f" ] || continue
    if [ -z "$newest" ] || [ "$f" -nt "$newest" ]; then newest=$f; fi
  done
  [ -n "$newest" ] || exit 0
  body=$(trunc 9000 < "$newest")
  [ -n "$body" ] || exit 0
  printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"%s"}}\n' \
    "$(printf '%s\n' "$body" | json_escape)"
  exit 0
fi

# ── PreCompact ─────────────────────────────────────────────────────────────
SID=$(json_str session_id | tr -cd 'A-Za-z0-9._-')
[ -n "$SID" ] || SID="unknown-$(date +%Y%m%dT%H%M%S)"
TRIGGER=$(json_str trigger)
OUT=$ROOT/memory/.compact/$SID.md

# The `## Current State and Next Action` block of every active plan.
plans_section() {
  local f block
  for f in "$ROOT"/.harness/plans/active/*.md; do
    [ -f "$f" ] || continue
    block=$(awk '/^## Current State and Next Action/{p=1;next} /^## /{p=0} p' "$f" \
      | sed -e '/^[[:space:]]*$/d' | head -n 20)
    [ -n "$block" ] || continue
    printf '### %s\n%s\n' "${f#"$ROOT"/}" "$block"
  done
}

# The non-done rows of projects/INDEX.md. The status column is field 5; a row
# that cannot be parsed counts as open (one row too many beats a hidden project).
index_section() {
  local f=$ROOT/projects/INDEX.md
  [ -f "$f" ] || return 0
  awk -F'|' '
    /^[[:space:]]*\|/ {
      slug = $2; status = $5
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", slug)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", status)
      if (slug == "" || slug == "slug" || slug == "_example_") next
      if (status ~ /^-+$/) next
      if (status ~ /^done/) next
      printf "- %s: %s\n", slug, status
      if (++n >= 20) exit
    }' "$f"
}

worktree_section() {
  command -v git >/dev/null 2>&1 || return 0
  git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1 || return 0
  git -C "$ROOT" worktree list 2>/dev/null | head -n 10
}

log_section() {
  local f=$ROOT/memory/$(date +%F).md
  [ -f "$f" ] || return 0
  tail -n 10 "$f"
}

# The last 5 user messages of this session, flattened by `transcripts tail`.
# The per-line cut is 400 bytes (`-b`, bytes in every cut implementation), then
# `iconv -c` drops the partial character the cut may have left. `cut -c` would
# be implementation- and locale-dependent: on this host two `cut` binaries
# disagree, and the byte-based one splits a Vietnamese character in half.
prompt_section() {
  local tp
  tp=$(json_str transcript_path)
  [ -n "$tp" ] || return 0
  [ -n "$QUAQUA" ] || return 0
  "$QUAQUA" transcripts tail --session "$tp" --last 5 2>/dev/null \
    | cut -b1-400 | iconv -c -f UTF-8 -t UTF-8 2>/dev/null
}

body() {
  local s
  printf '# Compaction snapshot — %s\n' "$(date -Is)"
  printf -- '- session: %s\n' "$SID"
  if [ -n "$TRIGGER" ]; then printf -- '- trigger: %s\n' "$TRIGGER"; fi
  printf -- '- workspace: %s\n' "$ROOT"

  s=$(plans_section)
  if [ -n "$s" ]; then
    printf '\n## Active plans: current state and next action\n%s\n' "$s"
  fi
  s=$(index_section)
  if [ -n "$s" ]; then
    printf '\n## Projects not done\n%s\n' "$s"
  fi
  s=$(worktree_section)
  if [ -n "$s" ]; then
    printf '\n## Worktrees\n%s\n' "$s"
  fi
  s=$(log_section)
  if [ -n "$s" ]; then
    printf '\n## Today'"'"'s memory log (last 10 lines)\n%s\n' "$s"
  fi
  s=$(prompt_section)
  if [ -n "$s" ]; then
    printf '\n## Last user messages\n%s\n' "$s"
  fi
}

# The R9 steer: what the compaction summary must keep, and where the detail is.
steer() {
  printf 'Compaction steer (memory-compact): pre-compaction snapshot at %s\n' "$1"
  printf 'Keep these in the summary, verbatim where they are paths, ids or SHAs:\n'
  printf '1. decisions made this session and why (one line each)\n'
  printf '2. open items, each with its owner or its next action\n'
  printf '3. the exact next action: the command or the file to touch first\n'
  printf '4. every correction line from memory/corrections.md for this session\n'
  printf '5. file paths, branch names and commit SHAs you will need again\n'
}

BODY=$(body)

if [ "$DRY" = 1 ]; then
  printf '%s\n' "$BODY" >&2
  printf 'would write %s\n' "$OUT" >&2
  steer "$OUT"
  exit 0
fi

# memory/ belongs to the workspace; this script never creates it.
if [ ! -d "$ROOT/memory" ]; then
  steer "(not written: $ROOT/memory does not exist)"
  exit 0
fi

mkdir -p "$ROOT/memory/.compact" 2>/dev/null || exit 0
# Write beside the target and rename, so a crash leaves no half snapshot and a
# SessionStart never injects one.
TMP=$OUT.tmp
if printf '%s\n' "$BODY" | trunc 8192 > "$TMP" 2>/dev/null; then
  mv -f "$TMP" "$OUT" 2>/dev/null || rm -f "$TMP" 2>/dev/null
else
  rm -f "$TMP" 2>/dev/null
fi

steer "$OUT"
exit 0
