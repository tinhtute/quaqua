#!/usr/bin/env bash
# Test for templates/workspace/scripts/hooks/memory-compact.sh (G4: R8, R9, R10).
# Drives the shipped script with real hook JSON in a temp workspace; never the
# live ~/quaqua or ~/.claude.
# Usage: tests/test_memory_compact.sh   (exit 0 = pass)
set -euo pipefail

T=$(mktemp -d "${TMPDIR:-/tmp}/test-memory-compact.XXXXXX")
trap 'rm -rf "$T"' EXIT

here="$(cd "$(dirname "$0")" && pwd)"
hook="$(cd "$here/.." && pwd)/templates/workspace/scripts/hooks/memory-compact.sh"
[[ -x $hook ]] || { echo "FAIL: $hook is not executable" >&2; exit 1; }

fail() { echo "FAIL: $*" >&2; exit 1; }

# ── fixture workspace ─────────────────────────────────────────────────────
ws=$T/ws
mkdir -p "$ws/memory" "$ws/projects" "$ws/.harness/plans/active"
today=$(date +%F)
cat > "$ws/memory/$today.md" <<'EOF'
- [alpha] an older line
- [alpha] memory tail line
EOF
cat > "$ws/projects/INDEX.md" <<'EOF'
# Projects

| slug | repo | owner | status | one-line goal |
|------|------|-------|--------|---------------|
| _example_ | ~/code/example | cos | idle | copy `_template.md` to add a project |
| open-proj | ~/Lab/x | cos | G5 done; next G4 fan-out | an open project |
| done-proj | ~/Lab/y | cos | done: merged 2026-11-11 | a closed project |
EOF
cat > "$ws/.harness/plans/active/alpha.md" <<'EOF'
# Alpha

## Current State and Next Action
- active_phase: build
- exact_next_action: run the next action line
EOF

# a real repo, so `git worktree list` has something to print
git -C "$ws" init -q -b main
git -C "$ws" -c user.email=t@t -c user.name=t add -A
git -C "$ws" -c user.email=t@t -c user.name=t commit -qm init
git -C "$ws" worktree add -q "$T/wt2" -b side

# stub quaqua: `transcripts tail` answers, anything else fails. The third
# message is 500 Vietnamese characters, so the per-line cut is exercised.
LONG=$(printf '\u1eaf%.0s' {1..500})
export LONG
mkdir -p "$T/bin"
cat > "$T/bin/quaqua" <<'EOF'
#!/bin/sh
case "$*" in
  "transcripts tail"*) printf 'first user message\nsecond user message\n%s\n' "$LONG" ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$T/bin/quaqua"
printf '{"type":"user"}\n' > "$ws/t.jsonl"

# A PATH with the tools the script needs and no quaqua, plus an empty HOME, so
# the "binary is missing" case cannot fall back to the host's install.
mkdir -p "$T/pathdir" "$T/home"
for t in awk sed grep head date tr cut tail mkdir mv rm cat git bash; do
  ln -s "$(command -v "$t")" "$T/pathdir/$t"
done

export CLAUDE_PROJECT_DIR=$ws
export QUAQUA_BIN=$T/bin/quaqua

pre_json=$(jq -nc --arg sid s1 --arg tp "$ws/t.jsonl" --arg cwd "$ws" \
  '{session_id:$sid, transcript_path:$tp, cwd:$cwd, hook_event_name:"PreCompact", trigger:"auto", custom_instructions:null}')
start_json=$(jq -nc --arg cwd "$ws" \
  '{session_id:"s1", cwd:$cwd, hook_event_name:"SessionStart", source:"compact"}')

# ── 1. --dry-run writes nothing ───────────────────────────────────────────
out=$(printf '%s' "$pre_json" | bash "$hook" --dry-run 2>"$T/dry.err")
[[ ! -e $ws/memory/.compact ]] || fail "--dry-run created memory/.compact"
grep -q 'would write' "$T/dry.err" || fail "--dry-run did not name its target"
grep -q 'Compaction steer' <<<"$out" || fail "--dry-run printed no steer"
echo "ok: --dry-run prints the snapshot and writes nothing"

# ── 2. PreCompact writes the snapshot and the steer (R8, R9) ──────────────
out=$(printf '%s' "$pre_json" | bash "$hook" --pre-compact)
snap=$ws/memory/.compact/s1.md
[[ -f $snap ]] || fail "no snapshot at $snap"
grep -q 'exact_next_action: run the next action line' "$snap" \
  || fail "the plan's next action is missing from the snapshot"
grep -q 'open-proj' "$snap" || fail "the open project row is missing"
if grep -q 'done-proj' "$snap"; then fail "a done project leaked into the snapshot"; fi
grep -q 'memory tail line' "$snap" || fail "today's memory tail is missing"
grep -q 'first user message' "$snap" || fail "the transcript tail is missing"
# 500 characters in, a 400-byte excerpt out: 133 whole characters, never half of
# one (a byte-based cut without the iconv repair would leave a broken sequence).
[[ $(grep -o $'\u1eaf' "$snap" | wc -l) -eq 133 ]] \
  || fail "the long message was not cut at 400 bytes"
iconv -f UTF-8 -t UTF-8 < "$snap" > /dev/null || fail "the snapshot is not valid UTF-8"
grep -q "$T/wt2" "$snap" || fail "the worktree list is missing"
grep -q "$snap" <<<"$out" || fail "the steer does not name the snapshot path"
[[ ${#out} -le 2000 ]] || fail "the steer is ${#out} chars (cap 2000)"
grep -q 'exact next action' <<<"$out" || fail "the steer omits the must-keep items"
echo "ok: PreCompact writes memory/.compact/s1.md and a ${#out}-char steer"

# ── 3. nothing outside memory/.compact/ is written ────────────────────────
before=$(cd "$ws" && find . -path ./.git -prune -o -path ./memory/.compact -prune -o -print | sort)
printf '%s' "$pre_json" | bash "$hook" --pre-compact >/dev/null
after=$(cd "$ws" && find . -path ./.git -prune -o -path ./memory/.compact -prune -o -print | sort)
[[ $before == "$after" ]] || fail "the hook wrote outside memory/.compact/"
if compgen -G "$ws/memory/.compact/*.tmp" >/dev/null; then fail "a temp file was left behind"; fi
echo "ok: the only path written is memory/.compact/"

# ── 4. degraded inputs still exit 0 (R8) ──────────────────────────────────
empty=$T/empty
mkdir -p "$empty/memory"
out=$(CLAUDE_PROJECT_DIR=$empty bash "$hook" --pre-compact < /dev/null)
compgen -G "$empty/memory/.compact/unknown-*.md" >/dev/null \
  || fail "an empty workspace wrote no fallback snapshot"
grep -q 'Compaction steer' <<<"$out" || fail "an empty workspace printed no steer"

noq_json=$(jq -nc --arg sid s2 --arg tp "$ws/t.jsonl" --arg cwd "$ws" \
  '{session_id:$sid, transcript_path:$tp, cwd:$cwd, hook_event_name:"PreCompact", trigger:"manual"}')
printf '%s' "$noq_json" | PATH=$T/pathdir HOME=$T/home QUAQUA_BIN= \
  bash "$hook" --pre-compact >/dev/null
[[ -f $ws/memory/.compact/s2.md ]] || fail "a missing quaqua binary lost the snapshot"
if grep -q 'first user message' "$ws/memory/.compact/s2.md"; then
  fail "a missing quaqua binary still produced transcript lines"
fi

out=$(printf 'not json at all' | bash "$hook" --session-start)
[[ -z $out ]] || fail "unparseable stdin produced session-start output"
printf 'not json at all' | bash "$hook" --pre-compact >/dev/null \
  || fail "unparseable stdin failed the pre-compact hook"

# memory/ belongs to the workspace: the hook must not create it.
nomem=$T/nomem
mkdir -p "$nomem"
out=$(printf '%s' "$pre_json" | CLAUDE_PROJECT_DIR=$nomem bash "$hook" --pre-compact)
[[ ! -e $nomem/memory ]] || fail "the hook created memory/ itself"
grep -q 'not written' <<<"$out" || fail "no-memory workspace did not say the snapshot was skipped"
echo "ok: empty workspace, missing binary, garbage stdin and no memory/ all exit 0"

# ── 5. SessionStart injects the newest snapshot (R10) ─────────────────────
out=$(printf '%s' "$start_json" | bash "$hook" --session-start)
[[ $(jq -r '.hookSpecificOutput.hookEventName' <<<"$out") == SessionStart ]] \
  || fail "not a SessionStart additionalContext: $out"
ctx=$(jq -r '.hookSpecificOutput.additionalContext' <<<"$out")
grep -q 'exact_next_action' <<<"$ctx" || fail "the snapshot was not injected"

startup_json=$(jq -nc --arg cwd "$ws" \
  '{session_id:"s1", cwd:$cwd, hook_event_name:"SessionStart", source:"startup"}')
out=$(printf '%s' "$startup_json" | bash "$hook" --session-start)
[[ -z $out ]] || fail "a startup session got a snapshot injection"

emptysnap=$T/emptysnap
mkdir -p "$emptysnap/memory/.compact"
out=$(printf '%s' "$start_json" | CLAUDE_PROJECT_DIR=$emptysnap bash "$hook" --session-start)
[[ -z $out ]] || fail "an empty memory/.compact still injected context"
echo "ok: SessionStart injects on source=compact only"

# A control byte in the snapshot must not make the injection invalid JSON.
esc=$T/esc
mkdir -p "$esc/memory/.compact"
printf 'plain line\n\033[31mred line\033[0m\ttabbed\n' > "$esc/memory/.compact/s3.md"
out=$(printf '%s' "$start_json" | CLAUDE_PROJECT_DIR=$esc bash "$hook" --session-start)
jq -e . >/dev/null <<<"$out" || fail "SessionStart output is not valid JSON with a control byte"
ctx=$(jq -r '.hookSpecificOutput.additionalContext' <<<"$out")
grep -q 'red line' <<<"$ctx" || fail "the control-byte line was dropped"
if printf '%s' "$out" | LC_ALL=C grep -q $'\033'; then
  fail "a raw ESC byte reached the JSON"
fi
echo "ok: control bytes are escaped as \\u00XX"

# ── 6. both caps hold ─────────────────────────────────────────────────────
big=$T/big
mkdir -p "$big/memory/.compact"
{ yes 'a long line of snapshot text for the truncation case' | head -n 400 || true; } \
  > "$big/memory/.compact/s9.md"
out=$(printf '%s' "$start_json" | CLAUDE_PROJECT_DIR=$big bash "$hook" --session-start)
ctx=$(jq -r '.hookSpecificOutput.additionalContext' <<<"$out")
[[ ${#ctx} -le 9000 ]] || fail "additionalContext is ${#ctx} chars (cap 9000)"
grep -q 'snapshot truncated' <<<"$ctx" || fail "no truncation marker at 9000 chars"

# Multi-byte filler: 100 characters are 300 bytes, so a character-counting cap
# would let the file reach ~3x the 8 KiB limit.
cap=$T/cap
mkdir -p "$cap/memory" "$cap/.harness/plans/active"
viet=$(printf '\u1eaf%.0s' {1..100})
for i in 1 2 3 4 5; do
  {
    printf '# Plan %s\n\n## Current State and Next Action\n' "$i"
    for j in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
      printf -- '- item %s: %s\n' "$j" "$viet"
    done
  } > "$cap/.harness/plans/active/p$i.md"
done
cap_json=$(jq -nc --arg sid cap --arg cwd "$cap" \
  '{session_id:$sid, cwd:$cwd, hook_event_name:"PreCompact", trigger:"auto"}')
CLAUDE_PROJECT_DIR=$cap QUAQUA_BIN=$T/bin/quaqua \
  bash "$hook" --pre-compact <<<"$cap_json" >/dev/null
size=$(wc -c < "$cap/memory/.compact/cap.md")
[[ $size -le 8192 ]] || fail "the snapshot is $size bytes (cap 8192)"
grep -q 'snapshot truncated' "$cap/memory/.compact/cap.md" || fail "no truncation marker at 8 KiB"
iconv -f UTF-8 -t UTF-8 < "$cap/memory/.compact/cap.md" > /dev/null \
  || fail "the capped snapshot is not valid UTF-8"
echo "ok: snapshot <= 8192 bytes and additionalContext <= 9000 chars"

echo "PASS: memory-compact.sh"
