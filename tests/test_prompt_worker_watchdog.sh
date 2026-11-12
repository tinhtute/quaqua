#!/usr/bin/env bash
# Watchdog shell suite for the event-driven worker watchdog.
# herdr is stubbed on PATH: it logs calls and answers agent get/wait/prompt/
# send-keys from FAKE_* vars. HOME and QUAQUA_RUNS_DIR are temp dirs; no real
# herdr, agent, or home is touched.
# Timing cases use the virtual clock: the stub's `agent wait` advances
# PROMPT_WORKER_CLOCK_FILE through PROMPT_WORKER_SLEEP_CMD, exactly as the real
# blocking herdr call does.
# Usage: tests/test_prompt_worker_watchdog.sh   (exit 0 = pass)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
bin="${QUAQUA_BIN:-$root/target/debug/quaqua}"
(cd "$root" && cargo build --locked --quiet) || fail "cannot build quaqua"
export QUAQUA_BIN="$bin"
export PATH="$root/target/debug:$PATH"
T=$(mktemp -d "${TMPDIR:-/tmp}/test-prompt-watchdog.XXXXXX")
T=$(cd "$T" && pwd -P)
trap 'rm -rf "$T"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

mkdir -p "$T/bin" "$T/home/quaqua/runs" "$T/repo"
export QUAQUA_RUNS_DIR="$T/home/quaqua/runs"
git -C "$T/repo" init -q
echo initial > "$T/repo/changed.txt"
git -C "$T/repo" add changed.txt
git -C "$T/repo" -c user.email=t@t -c user.name=t commit -qm init

cat > "$T/bin/herdr" <<'STUB'
#!/usr/bin/env bash
echo "herdr $*" >> "$CALLS"
gone_now() {
  n=$(grep -c "^herdr agent get" "$CALLS" 2>/dev/null || true)
  [ "${FAKE_GONE:-0}" = 1 ] && return 0
  [ -n "${FAKE_GONE_AFTER:-}" ] && [ "$n" -gt "$FAKE_GONE_AFTER" ] && return 0
  [ -n "${FAKE_GONE_AT:-}" ] && [ "$n" -eq "$FAKE_GONE_AT" ] && return 0
  return 1
}
agent_json() {
  local hook="" harness="" session=""
  [ "${FAKE_HOOK:-0}" = 1 ] && hook=',"screen_detection_skipped":true'
  [ -n "${FAKE_HARNESS:-}" ] && harness=",\"agent\":\"$FAKE_HARNESS\""
  [ -n "${FAKE_SESSION_FILE:-}" ] && session=",\"agent_session\":{\"kind\":\"path\",\"value\":\"$FAKE_SESSION_FILE\"}"
  printf '{"result":{"agent":{"agent_status":"%s","cwd":"%s","pane_id":"%%1"%s%s%s}}}\n' "$1" "${FAKE_CWD:-$HOME}" "$hook" "$harness" "$session"
}
advance() {
  local ms="$1"; local sec=$(( (ms + 999) / 1000 ))
  [ "$sec" -lt 1 ] && sec=1
  if [ -n "${PROMPT_WORKER_SLEEP_CMD:-}" ]; then
    "$PROMPT_WORKER_SLEEP_CMD" "$sec"
  else
    sleep "$sec"
  fi
}
write_report() {
  printf 'Outcome: COMPLETE\nfake summary\n\n## Lessons\n- none\n' > "$HOME/quaqua/runs/$FAKE_NAME.md"
}
touch_session() {
  [ -z "${FAKE_SESSION_FILE:-}" ] && return 0
  [ "${FAKE_TOUCH_SESSION:-0}" = 1 ] || return 0
  printf '%s\n' "$(date +%s%N)" >> "$FAKE_SESSION_FILE"
}
progress_side_effects() {
  if [ "${FAKE_DIFF_PROGRESS:-0}" = 1 ] && [ -n "${FAKE_CWD:-}" ] && [ -d "$FAKE_CWD/.git" ]; then
    n=$(cat "$FAKE_CWD/.diffcnt" 2>/dev/null || echo 0); n=$(( n + 1 ))
    echo "$n" > "$FAKE_CWD/.diffcnt"
    echo "diff-$n" >> "$FAKE_CWD/changed.txt"
  fi
  if [ "${FAKE_ADVANCE_COST:-0}" = 1 ] && [ -n "${PROMPT_WORKER_COST_FILE:-}" ]; then
    cstep=$(cat "$HOME/.coststep" 2>/dev/null || echo 0); cstep=$(( cstep + 1 ))
    echo "$cstep" > "$HOME/.coststep"
    if [ "$cstep" -eq 1 ]; then echo "0.85" > "$PROMPT_WORKER_COST_FILE"
    elif [ "$cstep" -eq 2 ]; then echo "1.15" > "$PROMPT_WORKER_COST_FILE"
    elif [ "$cstep" -ge 3 ]; then write_report; fi
  fi
}
case "${1:-} ${2:-}" in
  "api snapshot")
    echo '{"snapshot":{"workspaces":[],"tabs":[],"panes":[],"layouts":[],"agents":[]}}'; exit 0 ;;
  "agent get")
    gone_now && { printf '{"error":{"code":"agent_not_found"}}\n' >&2; exit 1; }
    agent_json "${FAKE_STATUS:-working}"; exit 0 ;;
  "agent list")
    printf '{"result":{"agents":[{"name":"cos"}]}}\n'; exit 0 ;;
  "agent wait")
    t=1000; prev=""
    for a in "$@"; do [ "$prev" = "--timeout" ] && t="$a"; prev="$a"; done
    advance "$t"
    gone_now && { printf '{"error":{"code":"agent_not_found"}}\n' >&2; exit 1; }
    progress_side_effects
    touch_session
    agent_json "${FAKE_WAIT_STATUS:-${FAKE_STATUS:-working}}"; exit 0 ;;
  "agent prompt")
    case "${FAKE_PROMPT:-ok}" in
      blocked) printf '{"error":{"code":"agent_blocked"}}\n' >&2; exit 1 ;;
      stalled)
        [ "${FAKE_WRITE_ON_KEYS:-0}" = 1 ] || [ "${FAKE_WRITE:-0}" = 0 ]
        printf '{"error":{"code":"agent_prompt_stalled"}}\n' >&2; exit 1 ;;
    esac
    if [ "${FAKE_DELAY:-0}" != 0 ]; then
      ( sleep "$FAKE_DELAY"; write_report ) &
    elif [ "${FAKE_PARTS:-0}" = 1 ]; then
      printf 'part1\n' > "$HOME/quaqua/runs/$FAKE_NAME.md"
      ( sleep 0.3; printf 'Outcome: COMPLETE\npart2\n\n## Lessons\n- none\n' >> "$HOME/quaqua/runs/$FAKE_NAME.md" ) &
    elif [ "${FAKE_WRITE:-0}" = 1 ]; then
      write_report
    fi
    agent_json working; exit 0 ;;
  "agent send-keys")
    [ "${FAKE_WRITE_ON_KEYS:-0}" = 1 ] && write_report
    exit 0 ;;
  "agent read") exit 0 ;;
esac
exit 0
STUB
chmod +x "$T/bin/herdr"
export HERDR_BIN_PATH="$T/bin/herdr"
export HERDR_SOCKET_PATH="$T/herdr.sock"
export PATH="$T/bin:$PATH"

# virtual clock: the stub's agent wait advances the clock by its timeout.
CLK=(PROMPT_WORKER_CLOCK_FILE="$T/clock" PROMPT_WORKER_SLEEP_CMD="$T/bin/advance-clock")
printf '%s\n' 1700000000 > "$T/clock"
cat > "$T/bin/advance-clock" <<'ADV'
#!/usr/bin/env bash
n=$(cat "$PROMPT_WORKER_CLOCK_FILE")
printf '%s\n' "$(( n + ${1:-1} ))" > "$PROMPT_WORKER_CLOCK_FILE"
ADV
chmod +x "$T/bin/advance-clock"

brief='Goal: do the thing
Hard constraints: keep tests passing
Current design (open to challenge): via the existing helper'

run() {
  local name=$1; shift
  local clock=("${CLK[@]}") a
  for a in "$@"; do [[ $a == REAL_CLOCK=1 ]] && clock=(); done
  : > "$T/calls"
  rc=0
  CALLS="$T/calls" HOME="$T/home" PATH="$T/bin:$PATH" HERDR_BIN_PATH="$T/bin/herdr" HERDR_SOCKET_PATH="$T/herdr.sock" FAKE_NAME="$name" FAKE_CWD="$T/repo" \
    env "${clock[@]}" "$@" "${QUAQUA_BIN:-quaqua}" worker prompt "$name" "$brief" >"$T/out" 2>"$T/err" || rc=$?
}
run_brief() {
  local name=$1 btext=$2; shift 2
  local clock=("${CLK[@]}") a
  for a in "$@"; do [[ $a == REAL_CLOCK=1 ]] && clock=(); done
  : > "$T/calls"
  rc=0
  CALLS="$T/calls" HOME="$T/home" PATH="$T/bin:$PATH" HERDR_BIN_PATH="$T/bin/herdr" HERDR_SOCKET_PATH="$T/herdr.sock" FAKE_NAME="$name" FAKE_CWD="$T/repo" \
    env "${clock[@]}" "$@" "${QUAQUA_BIN:-quaqua}" worker prompt "$name" "$btext" >"$T/out" 2>"$T/err" || rc=$?
}
resume() {
  local name=$1; shift
  local vars=() wargs=() a clock=("${CLK[@]}")
  for a in "$@"; do [[ $a == REAL_CLOCK=1 ]] && clock=(); done
  while (( $# )); do
    if [[ $1 =~ ^[A-Za-z_][A-Za-z0-9_]*= ]]; then vars+=("$1"); else wargs+=("$1"); fi
    shift
  done
  : > "$T/calls"
  rc=0
  CALLS="$T/calls" HOME="$T/home" PATH="$T/bin:$PATH" HERDR_BIN_PATH="$T/bin/herdr" HERDR_SOCKET_PATH="$T/herdr.sock" FAKE_NAME="$name" FAKE_CWD="$T/repo" \
    env "${clock[@]}" "${vars[@]}" "${QUAQUA_BIN:-quaqua}" worker prompt --resume "$name" "${wargs[@]}" >"$T/out" 2>"$T/err" || rc=$?
}
nudges() { grep -c 'agent prompt.*Time check' "$T/calls" || true; }

# ── --resume arg validation ─────────────────────────────────────────────────
rc=0; CALLS="$T/calls" HOME="$T/home" PATH="$T/bin:$PATH" \
  "${QUAQUA_BIN:-quaqua}" worker prompt --resume >"$T/out" 2>"$T/err" || rc=$?
[[ $rc -eq 1 ]] || fail "--resume no name: exit $rc, want 1"
grep -q '^# Usage:' "$T/err" || fail "--resume no name: no usage on stderr"
rc=0; CALLS="$T/calls" HOME="$T/home" PATH="$T/bin:$PATH" \
  "${QUAQUA_BIN:-quaqua}" worker prompt --resume 'bad_name' >"$T/out" 2>"$T/err" || rc=$?
[[ $rc -eq 1 ]] || fail "--resume bad name: exit $rc, want 1"
for bad in '--extend 0' '--extend abc' '--extend'; do
  rc=0; CALLS="$T/calls" HOME="$T/home" PATH="$T/bin:$PATH" \
    "${QUAQUA_BIN:-quaqua}" worker prompt --resume sude-ext $bad >"$T/out" 2>"$T/err" || rc=$?
  [[ $rc -eq 1 ]] || fail "--resume $bad: exit $rc, want 1"
done
echo "ok: --resume arg validation -> exit 1, usage on stderr"

# ── resume happy path: file already there, no prompt ever sent ──────────────
printf 'Outcome: COMPLETE\ndone\n\n## Lessons\n- none\n' > "$T/home/quaqua/runs/sude-rdone.md"
resume sude-rdone
[[ $rc -eq 0 ]] || fail "sude-rdone: exit $rc: $(cat "$T/err")"
[[ "$(cat "$T/out")" == "$T/home/quaqua/runs/sude-rdone.md" ]] \
  || fail "sude-rdone: stdout not the runs path"
grep -q 'agent prompt' "$T/calls" && fail "sude-rdone: --resume still prompted"
echo "ok: --resume -> waits without re-prompting, prints runs path"

# ── resume waits for the file, cap still exit 2 ──────────────────────────────
resume sude-rnever PROMPT_WORKER_MAX_SEC=2 PROMPT_WORKER_POLL_SEC=1 \
  PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 2 ]] || fail "sude-rnever: exit $rc, want 2: $(cat "$T/err")"
grep -q 'WATCHDOG exit=2' "$T/err" || fail "sude-rnever: no WATCHDOG line"
echo "ok: --resume no file -> exit 2 at cap with WATCHDOG line"

# ── STALL: static diff and static session -> exit 3 ─────────────────────────
run sude-stall PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=2 \
  PROMPT_WORKER_BUDGET_SEC=9999 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 3 ]] || fail "sude-stall: exit $rc, want 3: $(cat "$T/err")"
grep -q 'WATCHDOG exit=3 stall' "$T/err" || fail "sude-stall: no stall line: $(cat "$T/err")"
echo "ok: static diff + session -> exit 3 stall"

# ── progress resets the stall clock: session writes outlive STALL_SEC ───────
: > "$T/session.jsonl"
run sude-busy FAKE_SESSION_FILE="$T/session.jsonl" FAKE_TOUCH_SESSION=1 \
  PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=1 \
  PROMPT_WORKER_BUDGET_SEC=3 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-busy: exit $rc, want 4 (session writes must beat stall): $(cat "$T/err")"
grep -q 'WATCHDOG exit=4' "$T/err" || fail "sude-busy: no budget line"
echo "ok: session progress -> no stall, exit 4 at budget"

# ── BUDGET: one nudge past the pct mark, then exit 4 ────────────────────────
run sude-budget PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
  PROMPT_WORKER_BUDGET_SEC=3 PROMPT_WORKER_NUDGE_PCT=30 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-budget: exit $rc, want 4: $(cat "$T/err")"
[[ $(nudges) -eq 1 ]] || fail "sude-budget: $(nudges) nudges, want 1: $(cat "$T/calls")"
grep -q 'WATCHDOG nudge sent' "$T/err" || fail "sude-budget: no nudge notice"
echo "ok: budget -> one nudge past the pct mark, exit 4"

# A watchdog stop is a worker lifecycle end: `status --metrics` must show it
# without waiting for a cleanup (exit 4 left no row before).
tele="$T/home/quaqua/runs/telemetry.jsonl"
rows=$(grep -c '"rec":"worker".*"name":"sude-budget".*"exit":4' "$tele" 2>/dev/null || true)
[[ $rows -eq 1 ]] || fail "sude-budget: $rows exit-4 telemetry rows after the stop, want 1"
QUAQUA_RUNS_DIR="$T/home/quaqua/runs" HOME="$T/home" "${QUAQUA_BIN:-quaqua}" status --metrics --since 2020-01-01T00:00:00Z 2>/dev/null \
  | grep -Eq '^sude-budget .* 4 ' || fail "sude-budget: stopped worker missing from status --metrics"
echo "ok: exit 4 -> worker row in status --metrics"

resume sude-budget PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
  PROMPT_WORKER_BUDGET_SEC=2 PROMPT_WORKER_NUDGE_PCT=30 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-budget resume: exit $rc, want 4: $(cat "$T/err")"
[[ $(nudges) -eq 0 ]] || fail "sude-budget resume: re-nudged: $(cat "$T/calls")"
echo "ok: --resume -> no second nudge"

# ── grace: session progress at the deadline -> one +25%, then exit 4 ────────
: > "$T/session.jsonl"
run sude-grace FAKE_SESSION_FILE="$T/session.jsonl" FAKE_TOUCH_SESSION=1 \
  PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=1 \
  PROMPT_WORKER_BUDGET_SEC=4 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-grace: exit $rc, want 4: $(cat "$T/err")"
grep -q 'WATCHDOG grace +1s (progressing)' "$T/err" \
  || fail "sude-grace: no grace line: $(cat "$T/err")"
echo "ok: progress at the deadline -> grace +1s (25% of 4s), exit 4 after it"

# ── no progress at the deadline -> exit 4 with no grace ────────────────────
run sude-quiet PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
  PROMPT_WORKER_BUDGET_SEC=3 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-quiet: exit $rc, want 4: $(cat "$T/err")"
! grep -q 'WATCHDOG grace' "$T/err" || fail "sude-quiet: grace granted on static work: $(cat "$T/err")"
echo "ok: static worker at the deadline -> exit 4 at once, no grace"

resume sude-grace PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-grace resume: exit $rc, want 4: $(cat "$T/err")"
! grep -q 'WATCHDOG grace' "$T/err" || fail "sude-grace resume: grace granted twice: $(cat "$T/err")"
echo "ok: grace granted once per task (resume does not re-grant)"

# ── diff progress earns bounded auto-extend (<= +50% budget) ────────────────
run sude-diffext FAKE_DIFF_PROGRESS=1 PROMPT_WORKER_POLL_SEC=1 \
  PROMPT_WORKER_STALL_SEC=10 PROMPT_WORKER_BUDGET_SEC=4 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-diffext: exit $rc, want 4 (bounded at +50%): $(cat "$T/err")"
grep -q 'WATCHDOG grace +1s (progressing)' "$T/err" || fail "sude-diffext: no grace line"
grep -q 'WATCHDOG auto-extend' "$T/err" || fail "sude-diffext: no auto-extend"
echo "ok: diff progress earns auto-extend (bounded to <= +50% budget) -> exit 4"

# ── gracet never pushes past the cap -> exit 2 ─────────────────────────────
run sude-gracecap FAKE_DIFF_PROGRESS=1 PROMPT_WORKER_POLL_SEC=1 \
  PROMPT_WORKER_STALL_SEC=1 PROMPT_WORKER_BUDGET_SEC=4 PROMPT_WORKER_MAX_SEC=5
[[ $rc -eq 2 ]] || fail "sude-gracecap: exit $rc, want 2 (cap): $(cat "$T/err")"
grep -q 'WATCHDOG grace' "$T/err" || fail "sude-gracecap: grace never granted: $(cat "$T/err")"
echo "ok: grace clamped by the cap -> exit 2 at the cap"

# ── COST BUDGET: 80% then 100% once, report accepted ───────────────────────
cost_brief='Goal: test cost budget
Hard constraints: none
Current design: test
Cost budget: $1.00
Time budget: 5 min'
echo "0.50" > "$T/cost"
run_brief sude-cost "$cost_brief" FAKE_ADVANCE_COST=1 PROMPT_WORKER_COST_FILE="$T/cost" \
  PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 0 ]] || fail "sude-cost: exit $rc, want 0: $(cat "$T/err")"
[[ $(grep -c 'WATCHDOG cost 80%' "$T/err") -eq 1 ]] || fail "sude-cost: cost 80% not once"
[[ $(grep -c 'WATCHDOG cost 100%' "$T/err") -eq 1 ]] || fail "sude-cost: cost 100% not once"
echo "ok: cost crossing 80% then 100% printed once each, report accepted"

# ── 100% + timers.cost_buffer (default 120%): ESCALATE once + one herdr notification ────────────────
echo "2.50" > "$T/cost"
run_brief sude-cost200 "$cost_brief" PROMPT_WORKER_COST_FILE="$T/cost" \
  PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $(grep -c 'WATCHDOG cost 120%.*ESCALATE' "$T/err") -eq 1 ]] || fail "sude-cost200: ESCALATE not once: $(cat "$T/err")"
[[ $(grep -c '^herdr notification show quaqua: sude-cost200 at 120% cost' "$T/calls") -eq 1 ]] || fail "sude-cost200: no single notification: $(cat "$T/calls")"
echo "ok: 120% cost -> ESCALATE line and one herdr notification"

# ── on_cost_cap = "kill" (#48): one interrupt at 100%, exit 5, no cleanup ───
sed 's/^\[kinds.sude\]$/[kinds.sude]\non_cost_cap = "kill"/' "$root/templates/workspace/team.toml" > "$T/team-kill.toml"
grep -q 'on_cost_cap = "kill"' "$T/team-kill.toml" || fail "sude-costkill: team fixture not patched"
echo "0.50" > "$T/cost"; echo 0 > "$T/home/.coststep"
run_brief sude-costkill "$cost_brief" FAKE_ADVANCE_COST=1 PROMPT_WORKER_COST_FILE="$T/cost" \
  FAKE_HARNESS=omp QUAQUA_TEAM_TOML="$T/team-kill.toml" PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 5 ]] || fail "sude-costkill: exit $rc, want 5: $(cat "$T/err")"
grep -q 'WATCHDOG cost 100%.*| STOP' "$T/err" || fail "sude-costkill: no STOP line: $(cat "$T/err")"
grep -q 'cost cap reached' "$T/err" || fail "sude-costkill: unclear exit reason: $(cat "$T/err")"
[[ $(grep -c '^herdr agent send-keys' "$T/calls") -ge 1 ]] || fail "sude-costkill: no interrupt sent: $(cat "$T/calls")"
! grep -qE 'pane close|agent close|worktree remove' "$T/calls" || fail "sude-costkill: cleanup ran: $(cat "$T/calls")"
[[ ! -s "$T/out" ]] || fail "sude-costkill: stdout not empty: $(cat "$T/out")"
echo "ok: kill kind at 100% -> interrupt, STOP line, exit 5, pane kept"

# ── --resume / --extend arithmetic ─────────────────────────────────────────
run sude-noext PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
  PROMPT_WORKER_BUDGET_SEC=2 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-noext run1: exit $rc, want 4: $(cat "$T/err")"
resume sude-noext PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-noext resume: exit $rc, want 4: $(cat "$T/err")"
echo "ok: --resume without --extend -> exit 4 fast (allowance unchanged)"

resume sude-noext --extend 4 PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
  PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-noext extend: exit $rc, want 4: $(cat "$T/err")"
grep -q 'WATCHDOG extend +4s' "$T/err" || fail "sude-noext extend: no extend line"
echo "ok: --extend 4 -> waits 4s more, then exit 4"

run sude-extcap PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
  PROMPT_WORKER_BUDGET_SEC=2 PROMPT_WORKER_MAX_SEC=4
[[ $rc -eq 4 ]] || fail "sude-extcap run1: exit $rc, want 4: $(cat "$T/err")"
resume sude-extcap --extend 300 PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
  PROMPT_WORKER_MAX_SEC=4
[[ $rc -eq 2 ]] || fail "sude-extcap: exit $rc, want 2: $(cat "$T/err")"
grep -q 'WATCHDOG extend +300s' "$T/err" || fail "sude-extcap: no extend line"
echo "ok: --extend past the cap -> still exit 2"

# ── blocked -> exit 5, no nudge ────────────────────────────────────────────
run sude-blocked FAKE_STATUS=blocked PROMPT_WORKER_POLL_SEC=1 \
  PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 5 ]] || fail "sude-blocked: exit $rc, want 5: $(cat "$T/err")"
grep -q 'WATCHDOG exit=5' "$T/err" || fail "sude-blocked: no blocked line"
[[ $(nudges) -eq 0 ]] || fail "sude-blocked: nudged a blocked agent"
echo "ok: agent_status=blocked -> exit 5, no nudge"

# ── idle without report -> exit 3 ──────────────────────────────────────────
run sude-idle FAKE_STATUS=idle PROMPT_WORKER_POLL_SEC=1 \
  PROMPT_WORKER_IDLE_SEC=1 PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 3 ]] || fail "sude-idle: exit $rc, want 3: $(cat "$T/err")"
grep -q 'agent idle' "$T/err" || fail "sude-idle: no idle reason: $(cat "$T/err")"
echo "ok: agent idle without run file -> exit 3"

# ── omp without a hook gets the fallback idle; with a hook the short idle ──
run sude-omp-fallback FAKE_STATUS=idle FAKE_HARNESS=omp \
  PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_IDLE_SEC=1 PROMPT_WORKER_IDLE_FALLBACK_SEC=3 \
  PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 3 ]] || fail "sude-omp-fallback: exit $rc, want 3: $(cat "$T/err")"
grep -q 'elapsed=[3-9]s' "$T/err" || fail "sude-omp-fallback: left before fallback"
echo "ok: omp without hook -> fallback idle grace"

run sude-omp-hook FAKE_STATUS=idle FAKE_HARNESS=omp FAKE_HOOK=1 \
  PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_IDLE_SEC=1 PROMPT_WORKER_IDLE_FALLBACK_SEC=5 \
  PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 3 ]] || fail "sude-omp-hook: exit $rc, want 3: $(cat "$T/err")"
grep -q 'elapsed=[1-2]s' "$T/err" || fail "sude-omp-hook: hook state did not use IDLE_SEC"
echo "ok: omp with hook -> IDLE_SEC directly"

# ── prompt landing: stalled prompt, one enter, then working -> exit 0 ─────
run sude-landok FAKE_PROMPT=stalled FAKE_WRITE_ON_KEYS=1 FAKE_STATUS=working \
  PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_IDLE_SEC=9999 \
  PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 0 ]] || fail "sude-landok: exit $rc, want 0: $(cat "$T/err")"
grep -q 'agent send-keys sude-landok enter' "$T/calls" || fail "sude-landok: no recovery enter"
[[ "$(cat "$T/out")" == "$T/home/quaqua/runs/sude-landok.md" ]] || fail "sude-landok: no runs path"
echo "ok: stalled prompt -> enter recovers, report accepted (exit 0)"

run sude-landstall FAKE_PROMPT=stalled FAKE_STATUS=idle FAKE_KEYS_STATUS=idle \
  PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_IDLE_SEC=9999 \
  PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 3 ]] || fail "sude-landstall: exit $rc, want 3: $(cat "$T/err")"
grep -q 'prompt not landed' "$T/err" || fail "sude-landstall: no prompt-not-landed reason"
[[ $(grep -c 'agent send-keys sude-landstall enter' "$T/calls") -eq 1 ]] \
  || fail "sude-landstall: enter not sent exactly once"
echo "ok: stalled prompt stays idle -> one enter, exit 3"

run sude-pblock FAKE_PROMPT=blocked
[[ $rc -eq 5 ]] || fail "sude-pblock: exit $rc, want 5: $(cat "$T/err")"
echo "ok: agent_blocked on submit -> exit 5"

# ── agent gone mid-run -> exit 3 ────────────────────────────────────────────
run sude-gone FAKE_GONE_AFTER=1 PROMPT_WORKER_POLL_SEC=1 \
  PROMPT_WORKER_AGENT_GONE_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
  PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 3 ]] || fail "sude-gone: exit $rc, want 3: $(cat "$T/err")"
grep -q 'agent gone' "$T/err" || fail "sude-gone: no gone reason"
echo "ok: agent_not_found mid-run -> exit 3 agent gone"

# ── confirmed down before prompting -> exit 1, nothing sent (#69) ──────────
run sude-down FAKE_GONE=1
[[ $rc -eq 1 ]] || fail "sude-down: exit $rc, want 1: $(cat "$T/err")"
grep -q 'no live worker' "$T/err" || fail "sude-down: unclear refusal: $(cat "$T/err")"
! grep -q '^herdr agent prompt' "$T/calls" || fail "sude-down: prompted: $(cat "$T/calls")"
echo "ok: confirmed-down worker -> exit 1, no prompt"

# ── WATCHDOG line carries status/quiet/diff (no pane ctx) ──────────────────
run sude-stat PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=1 \
  PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 3 ]] || fail "sude-stat: exit $rc, want 3"
grep -q 'elapsed=[0-9]' "$T/err" || fail "sude-stat: no elapsed"
grep -q 'status=working' "$T/err" || fail "sude-stat: no status"
grep -q 'quiet=' "$T/err" || fail "sude-stat: no quiet"
grep -q '| diff=' "$T/err" || fail "sude-stat: no diff"
! grep -q 'ctx=' "$T/err" || fail "sude-stat: pane ctx still printed"
echo "ok: WATCHDOG line has elapsed/status/quiet/diff, no pane ctx"

# ── heartbeat line while waiting ───────────────────────────────────────────
run sude-hb PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_HEARTBEAT_SEC=1 \
  PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=3
[[ $rc -eq 4 ]] || fail "sude-hb: exit $rc, want 4"
grep -q 'WATCHDOG heartbeat' "$T/err" || fail "sude-hb: no heartbeat"
echo "ok: heartbeat prints while waiting"

# ── Lead exemption: <name>-lead idle on a live pipeline is waiting ─────────
sleep 300 & live_pid=$!
true & dead_pid=$!; wait $dead_pid 2>/dev/null || true
pipe_state() { printf 'finished=%s\npipeline_pid=%s\nlead=%s\nlead_pane=%s\n' "$1" "$2" "$3" "$4" \
  > "$T/home/quaqua/runs/pipeline-isl.state"; }
pipe_state 0 "$live_pid" isl-lead '%1'
run isl-lead FAKE_STATUS=idle PROMPT_WORKER_POLL_SEC=1 \
  PROMPT_WORKER_IDLE_SEC=1 PROMPT_WORKER_STALL_SEC=1 \
  PROMPT_WORKER_BUDGET_SEC=4 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "isl-lead: exit $rc, want 4: $(cat "$T/err")"
! grep -q 'exit=3' "$T/err" || fail "isl-lead: exit 3 on a waiting Lead"
echo "ok: -lead + own live pipeline -> idle/stall suppressed, exit 4"

pipe_state 1 "$live_pid" isl-lead '%1'
run isl-lead FAKE_STATUS=idle PROMPT_WORKER_POLL_SEC=1 \
  PROMPT_WORKER_IDLE_SEC=1 PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 3 ]] || fail "isl-lead finished: exit $rc, want 3: $(cat "$T/err")"
grep -q 'agent idle' "$T/err" || fail "isl-lead finished: no idle reason"
echo "ok: -lead + finished=1 pipeline -> idle exit 3"

pipe_state 0 "$dead_pid" isl-lead '%1'
run isl-lead FAKE_STATUS=idle PROMPT_WORKER_POLL_SEC=1 \
  PROMPT_WORKER_IDLE_SEC=1 PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 3 ]] || fail "isl-lead paused: exit $rc, want 3: $(cat "$T/err")"
echo "ok: -lead + dead pipeline pid -> idle exit 3"

pipe_state 0 "$live_pid" beta-lead '%9'
run isl-lead FAKE_STATUS=idle PROMPT_WORKER_POLL_SEC=1 \
  PROMPT_WORKER_IDLE_SEC=1 PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 3 ]] || fail "isl-lead foreign: exit $rc, want 3: $(cat "$T/err")"
echo "ok: foreign pipeline does not protect this Lead"

pipe_state 0 "$live_pid" isl-lead '%1'
run sude-isl FAKE_STATUS=idle PROMPT_WORKER_POLL_SEC=1 \
  PROMPT_WORKER_IDLE_SEC=1 PROMPT_WORKER_STALL_SEC=9999 PROMPT_WORKER_BUDGET_SEC=9999
[[ $rc -eq 3 ]] || fail "sude-isl: exit $rc, want 3 (exemption is Lead-only)"
echo "ok: non-lead worker with live pipeline -> idle exit 3"
kill "$live_pid" 2>/dev/null || true; rm -f "$T/home/quaqua/runs/pipeline-isl.state"

# ── consumed budget persists across --resume ──────────────────────────────
run sude-persist PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
  PROMPT_WORKER_BUDGET_SEC=3 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-persist run1: exit $rc, want 4"
resume sude-persist PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
  PROMPT_WORKER_BUDGET_SEC=4 PROMPT_WORKER_MAX_SEC=60
[[ $rc -eq 4 ]] || fail "sude-persist resume: exit $rc, want 4"
grep -q 'elapsed=[0-2]s' "$T/err" || fail "sude-persist: resume restocked the budget"
echo "ok: --resume continues the same allowance, no fresh clock"

echo "PASS"
