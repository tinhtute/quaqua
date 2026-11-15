#!/usr/bin/env bash
# Sandbox test for pipeline.sh: arg validation, the plan-ready pause (exit 5)
# + --resume, the auto review gate (>150 changed lines or >1 top-level dir),
# clean / disputed / rounds-exhausted / missing-Verdict outcomes, watchdog
# exit propagation, and one telemetry.jsonl line per stop. git and herdr are
# stubbed on PATH (they log calls; herdr fabricates runs files from FAKE_*
# vars keyed on the 'Pipeline phase: X' marker in the prompt). HOME is a temp
# dir. No real herdr, agent, repo, or home touched.
# Usage: tests/test_pipeline.sh   (exit 0 = pass)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
src="$(cd "$here/.." && pwd)/scripts"
# The binary under test: the repo build (so the suite exercises the working
# tree, like tests/test_pipeline_grid.sh), else `quaqua` on PATH. QUAQUA_BIN
# picked by the caller always wins.
root="$(cd "$here/.." && pwd)"
bin="${QUAQUA_BIN:-$root/target/debug/quaqua}"
(cd "$root" && cargo build --locked --quiet) || fail "cannot build quaqua"
export QUAQUA_BIN="$bin"
T=$(mktemp -d "${TMPDIR:-/tmp}/test-pipeline.XXXXXX")
T=$(cd "$T" && pwd -P)  # normalize trailing slashes and macOS /tmp symlinks
trap 'rm -rf "$T"' EXIT

fail() { echo "FAIL: $*" >&2
  { echo '--- out ---'; cat "$T/out"
    echo '--- err ---'; tail -20 "$T/err"
    echo '--- pfile ---'; tail -30 "$T"/home/quaqua/runs/pipeline-*.md
    echo '--- calls ---'; tail -20 "$T/calls"; } >&2 2>/dev/null
  exit 1; }

# ── sandbox: stub git/herdr, temp HOME/quaqua/runs, dummy repo + brief ──────
mkdir -p "$T/bin" "$T/repo" "$T/home/quaqua/runs"
export QUAQUA_RUNS_DIR="$T/home/quaqua/runs"
# git stub: rev-parse HEAD is keyed on the -C dir's basename; call #1 is the
# spawn-time base, call #2 the post-build tip, call #3+ the post-fix tip.
# FAKE_BASE_SHA/FAKE_HEAD_SHA/FAKE_HEAD2_SHA shape each; FAKE_PORCELAIN feeds
# `status --porcelain` (empty = clean tree). No branch exists (show-ref fails)
# unless FAKE_BRANCHES names it — spawn-worker.sh reuses a branch cleanup kept
# only when it is really there. A worktree added at an explicit sha pins HEAD to
# that commit for every later read: real git does not move a checkout between
# two reads, and the read counter would fake a move on a reviewer's worktree
# (baseline taken at spawn, compared at acceptance).
cat > "$T/bin/git" <<'STUB'
#!/usr/bin/env bash
echo "git $*" >> "$CALLS"
dir='' prev=''
for a in "$@"; do [ "$prev" = -C ] && dir=$a; prev=$a; done
cf="$GITN-${dir##*/}"; n=$(cat "$cf" 2>/dev/null || echo 0)
case " $* " in
  *"status --porcelain"*)   printf '%s\n' "${FAKE_PORCELAIN:-}" ;;
  *"diff --numstat"*)       printf '%s\n' "${FAKE_NUMSTAT:-}" ;;
  *"diff --name-only"*)     printf '%s\n' "${FAKE_NAMES:-}" ;;
  *"show-ref --verify"*)
    for b in ${FAKE_BRANCHES:-}; do case " $* " in *" refs/heads/$b "*) exit 0 ;; esac; done
    exit 1 ;;
  *"worktree add "*)
    wargs=(); wskip=0
    for a in "$@"; do
      if (( wskip )); then wskip=0; continue; fi
      case "$a" in -C|-b) wskip=1 ;; -*) ;; *) wargs+=("$a") ;; esac
    done
    if [[ ${wargs[3]:-} =~ ^[0-9a-f]{7,40}$ ]]; then
      printf '%s\n' "${wargs[3]}" > "$GITN-$(basename "${wargs[2]}").pin"
    else
      rm -f "$GITN-$(basename "${wargs[2]}").pin"
    fi ;;
  *"rev-parse HEAD"*)
    if [ -f "$cf.pin" ]; then cat "$cf.pin"; exit 0; fi
    n=$((n+1)); echo "$n" > "$cf"
    if (( n <= ${FAKE_BASECALLS:-1} )); then echo "${FAKE_BASE_SHA:-ba5eba5}";
    elif (( n == 2 )); then echo "${FAKE_HEAD_SHA:-c0ffee1deadbeef0000000000000000000000}";
    else echo "${FAKE_HEAD2_SHA:-f00dd00dbabe0000000000000000000000}"; fi ;;
esac
exit 0
STUB
cat > "$T/bin/herdr" <<'STUB'
#!/usr/bin/env bash
echo "herdr $*" >> "$CALLS"
case "${1:-} ${2:-}" in
  "agent get")
    printf '{"result":{"agent":{"agent_status":"%s","cwd":"%s","pane_id":"%%1"}}}\n' \
      "${FAKE_STATUS:-working}" "${FAKE_CWD:-$HOME}" ;;
  "agent read") printf '%s\n' "${FAKE_PANE:-working}" ;;
  "agent prompt")
    w=$3; content=''
    # builder reports carry the '## Lessons' tail (prompt-worker.sh exit 6
    # without it, rules/_all/brief.md); reviewer reports carry 'Verdict:'.
    case "$4" in
      *"phase: PLAN"*)   content=${FAKE_PLAN_MD:-$'fake plan: step 1\n\n## Lessons\n- none'} ;;
      *"phase: BUILD"*)  content=${FAKE_BUILD_MD:-$'fake build report: committed + tests pass\n\n## Lessons\n- none'} ;;
      *"phase: FIX"*)    content=${FAKE_FIX_MD:-$'fake fix report: findings addressed\n\n## Lessons\n- none'} ;;
      *"phase: REVIEW"*) content=${FAKE_REVIEW_MD:-'Verdict: clean'} ;;
    esac
    if [ -n "$content" ] && [ "${FAKE_SILENT:-0}" != 1 ]; then
      if [ "${FAKE_DELAY:-0}" != 0 ]; then
        if [ -n "${PROMPT_WORKER_CLOCK_FILE:-}" ]; then
          Tdir="$(cd "$(dirname "$0")/.." && pwd)"
          now_clk=$(cat "$PROMPT_WORKER_CLOCK_FILE" 2>/dev/null || echo 0)
          printf '%s %s\n' "$(( now_clk + FAKE_DELAY ))" "$HOME/quaqua/runs/$w.md" > "$Tdir/delayed_file"
          printf '%s\n' "$content" > "$Tdir/delayed_content"
        else
          ( sleep "$FAKE_DELAY"; printf '%s\n' "$content" > "$HOME/quaqua/runs/$w.md" ) &
        fi
      else
        printf '%s\n' "$content" > "$HOME/quaqua/runs/$w.md"
      fi
    fi
    exit "${FAKE_RC:-0}" ;;
  "pane split") printf '{"result":{"pane":{"pane_id":"%%1"}}}\n' ;;
  "pane move")
    # GRID_MOVE_FAIL makes the move a no-op (changed=false) so a repair failure
    # can be exercised; otherwise the pane lands in a stub tab.
    if [ -n "${GRID_MOVE_FAIL:-}" ]; then
      printf '{"result":{"move_result":{"changed":false,"reason":"same_tab"}}}\n'
    else
      printf '{"result":{"move_result":{"changed":true,"pane":{"pane_id":"%%9","tab_id":"w1:t9"}}}}\n'
    fi
    exit 0 ;;
  "pane wait-output") exit 1 ;;   # no first-run trust dialog
  "api snapshot")
    # the pipeline's grid check is in-process: GRID_FAIL fails every check
    if [ -n "${GRID_FAIL:-}" ]; then printf '{"error":{"message":"backend down"}}\n' >&2; exit 1; fi
    if [ -n "${GRID_SNAPSHOT:-}" ]; then cat "$GRID_SNAPSHOT"; else printf '{"snapshot":{"workspaces":[],"tabs":[],"panes":[],"layouts":[],"agents":[]}}\n'; fi
    exit 0 ;;
esac
exit 0
STUB
chmod +x "$T/bin/git" "$T/bin/herdr"
export HERDR_BIN_PATH="$T/bin/herdr"
export HERDR_SOCKET_PATH="$T/herdr.sock"

cat > "$T/bin/advance-clock" <<'ADV'
#!/usr/bin/env bash
n=$(cat "$PROMPT_WORKER_CLOCK_FILE")
n=$(( n + ${1:-1} ))
printf '%s\n' "$n" > "$PROMPT_WORKER_CLOCK_FILE"
Tdir="$(cd "$(dirname "$0")/.." && pwd)"
if [[ -f "$Tdir/delayed_file" ]]; then
  read -r target_clk target_file < "$Tdir/delayed_file"
  if (( n >= target_clk )); then
    cat "$Tdir/delayed_content" > "$target_file"
    rm -f "$Tdir/delayed_file" "$Tdir/delayed_content"
  fi
fi
ADV
chmod +x "$T/bin/advance-clock"
CLK=(PROMPT_WORKER_CLOCK_FILE="$T/clock" PROMPT_WORKER_SLEEP_CMD="$T/bin/advance-clock")

cat > "$T/brief.md" <<'EOF'
Goal: implement the widget
Hard constraints: keep tests passing, no new deps
Current design (open to challenge): extend the existing renderer
Unknowns: none
Evidence: diff + test output
EOF

# Sandbox roster: the template with tieude on sude's harness, so the default
# "harness" policy has a same-harness pair to refuse (tieude/sude).
sed '/^\[kinds\.tieude\]/,/^\[kinds\.sude\]/s/^harness = .*/harness = "omp"/' \
  "$root/templates/workspace/team.toml" > "$T/home/quaqua/team.toml"

tele="$T/home/quaqua/runs/telemetry.jsonl"
runsdir="$T/home/quaqua/runs"

# run_pipe [VAR=val...] -- <pipeline.sh args>  →  rc, $T/out, $T/err, $T/calls
run_pipe() {
  local vars=()
  while (($#)) && [[ $1 != -- ]]; do vars+=("$1"); shift; done
  [[ ${1:-} == -- ]] && shift
  : > "$T/calls"
  rc=0
  CALLS="$T/calls" GITN="$T/gitn" HOME="$T/home" PATH="$T/bin:$PATH" QUAQUA_TEST=1 GRID_SKIP=1 \
    HERDR_BIN_PATH="$T/bin/herdr" HERDR_SOCKET_PATH="$T/herdr.sock" \
    PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
    PROMPT_WORKER_IDLE_SEC=9999 PROMPT_WORKER_MAX_SEC=3600 \
    env "${vars[@]}" "${QUAQUA_BIN:-quaqua}" pipeline "$@" >"$T/out" 2>"$T/err" || rc=$?
}

# ── arg validation: refused before any spawn/prompt ─────────────────────────
reject() {  # reject <label> <stderr-match> [VAR=val...] -- <pipeline.sh args>
  local label=$1 match=$2; shift 2
  run_pipe "$@"
  [[ $rc -eq 1 ]] || fail "$label: exit $rc, want 1"
  grep -q "$match" "$T/err" || fail "$label: no '$match' on stderr: $(cat "$T/err")"
  ! grep -q 'agent prompt' "$T/calls" || fail "$label: herdr prompt called: $(cat "$T/calls")"
  echo "ok: $label -> exit 1 ($match)"
}

reject "no args"              '^# Usage:' --
reject "bad task chars"       '^# Usage:' -- Bad_Task "$T/repo" --brief "$T/brief.md"
reject "missing --brief"      'brief.*required' -- isl "$T/repo"
reject "missing brief file"   'brief.*required' -- isl "$T/repo" --brief "$T/nope.md"
printf 'just prose\n' > "$T/nobrief.md"
reject "brief lacks headers"  'Goal.*Hard constraints.*Current design' -- isl "$T/repo" --brief "$T/nobrief.md"
reject "same harness"         'different harnesses' -- isl "$T/repo" --brief "$T/brief.md" --builder tieude --reviewer sude
reject "same kind"            'different harnesses' -- isl "$T/repo" --brief "$T/brief.md" --builder suhuynh --reviewer suhuynh
reject "non-reviewer kind"    'can_review = false in team.toml' -- isl "$T/repo" --brief "$T/brief.md" --builder ni --reviewer tieude
# builder_reviewer_differ = "kind": two kinds on one harness pass, the same kind is refused.
sed 's/^builder_reviewer_differ = .*/builder_reviewer_differ = "kind"/' \
  "$root/templates/workspace/team.toml" > "$T/team-kind.toml"
reject "kind policy: same kind"  'different kinds' QUAQUA_TEAM_TOML="$T/team-kind.toml" -- isl "$T/repo" --brief "$T/brief.md" --builder suhuynh --reviewer suhuynh
run_pipe QUAQUA_TEAM_TOML="$T/team-kind.toml" -- kd "$T/repo" --brief "$T/brief.md" --builder ni --reviewer suhuynh
! grep -q 'must be different' "$T/err" || fail "kind policy: ni/suhuynh refused: $(cat "$T/err")"
echo "ok: kind policy: ni/suhuynh passes the differ check"
reject "bad --review"         'auto|always|skip' -- isl "$T/repo" --brief "$T/brief.md" --review maybe
reject "--rounds 0"           'integer >= 1' -- isl "$T/repo" --brief "$T/brief.md" --rounds 0
reject "builder name >20"     'exceeds 20 chars' -- abcdefghijklmnopq "$T/repo" --brief "$T/brief.md"
reject "reviewer name >20"    'exceeds 20 chars' -- abcdefghijklm "$T/repo" --brief "$T/brief.md"
reject "resume w/o state"     'no pipeline state' -- --resume nope "$T/repo"

# ── clean run: plan pause -> resume -> build -> auto review -> Verdict: clean
run_pipe FAKE_NUMSTAT=$'200\t10\tsrc/a.c\n5\t0\tsrc/b.c' \
         FAKE_NAMES=$'src/a.c\nsrc/b.c' \
         -- isl "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "isl run1: exit $rc, want 5 plan-ready: $(cat "$T/err")"
grep -q 'plan-ready' "$T/err" || fail "isl run1: no plan-ready notice"
[[ -f $runsdir/sude-isl.plan.md ]] || fail "isl run1: plan file not moved aside"
[[ ! -f $runsdir/sude-isl.md ]] || fail "isl run1: runs file not moved"
[[ -f $runsdir/pipeline-isl.state ]] || fail "isl run1: no state file"
grep -q '"task":"isl".*"status":"plan-ready"' "$tele" \
  || fail "isl run1: no plan-ready telemetry line: $(cat "$tele")"
grep -q 'agent start sude-isl --kind omp' "$T/calls" || fail "isl run1: builder not spawned as omp: $(cat "$T/calls")"
! grep -q -- '--permission-mode dontAsk' "$T/calls" || fail "isl run1: builder spawned read-only"
echo "ok: plan-ready -> exit 5, plan parked, state + telemetry written"

# refuse a second fresh start on the same slug (state exists)
run_pipe -- isl "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 1 ]] || fail "isl rerun: exit $rc, want 1"
grep -q 'already has state' "$T/err" || fail "isl rerun: no state error: $(cat "$T/err")"
echo "ok: duplicate slug -> exit 1, --resume hinted"

run_pipe FAKE_NUMSTAT=$'200\t10\tsrc/a.c\n5\t0\tsrc/b.c' \
         FAKE_NAMES=$'src/a.c\nsrc/b.c' \
         FAKE_REVIEW_MD=$'looks good\nVerdict: clean' \
         -- --resume isl "$T/repo"
[[ $rc -eq 0 ]] || fail "isl resume: exit $rc, want 0: $(cat "$T/err")"
grep -q 'status=clean' "$T/out" || fail "isl resume: stdout not clean: $(cat "$T/out")"
# reviewer spawned on the exact committed tip the gate measured (F2), read-only
grep -q 'worktree add -b quaqua/suhuynh-islr1 .* c0ffee1deadbeef' "$T/calls" \
  || fail "isl resume: reviewer not based on the gated head sha: $(cat "$T/calls")"
grep -q 'commit c0ffee1' "$runsdir/pipeline-isl.md" \
  || fail "isl resume: committed head sha not recorded in the report"
grep -qF 'agent start suhuynh-islr1 --kind claude' "$T/calls" \
  || fail "isl resume: reviewer not spawned as claude: $(cat "$T/calls")"
! grep -q 'agent start sude-isl' "$T/calls" || fail "isl resume: builder respawned"
grep -q 'pane close' "$T/calls" || fail "isl resume: reviewer pane not closed"
grep -q 'worktree remove' "$T/calls" || fail "isl resume: reviewer worktree kept"
grep -q 'Verdict: clean' "$runsdir/pipeline-isl.md" || fail "isl: verdict not logged"
grep -q 'status: clean' "$runsdir/pipeline-isl.md" || fail "isl: no final status"
tail -1 "$tele" | grep -q '"status":"clean"' || fail "isl: last telemetry not clean"
tail -1 "$tele" | grep -q '"review":"run"' || fail "isl: telemetry review!=run"
tail -1 "$tele" | grep -q '"rounds":1' || fail "isl: telemetry rounds!=1"
echo "ok: clean -> review ran on builder branch (read-only), cleaned, telemetry clean"

# ── review skipped: small single-dir diff under the auto gate ───────────────
run_pipe -- skp "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "skp run1: exit $rc, want 5"
run_pipe FAKE_NUMSTAT=$'20\t5\tAGENTS.md' FAKE_NAMES='AGENTS.md' \
         -- --resume skp "$T/repo"
[[ $rc -eq 0 ]] || fail "skp resume: exit $rc"
grep -q 'status=clean' "$T/out" || fail "skp: stdout not clean"
! grep -q 'suhuynh' "$T/calls" || fail "skp: reviewer spawned despite skip: $(cat "$T/calls")"
tail -1 "$tele" | grep -q '"review":"skip"' || fail "skp: telemetry review!=skip"
tail -1 "$tele" | grep -q '"rounds":0' || fail "skp: telemetry rounds!=0"
echo "ok: auto gate under threshold -> review skipped, no reviewer spawned"

# ── rounds exhausted: findings both rounds -> blocked, review changed outcome
run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
         -- rex "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "rex run1: exit $rc, want 5"
run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
         FAKE_REVIEW_MD=$'src/a.c:10 wrong\nsrc/a.c:20 missing\nVerdict: 2 findings' \
         -- --resume rex "$T/repo"
[[ $rc -eq 0 ]] || fail "rex resume: exit $rc: $(cat "$T/err")"
grep -q 'status=blocked' "$T/out" || fail "rex: stdout not blocked: $(cat "$T/out")"
grep -q 'rounds exhausted' "$runsdir/pipeline-rex.md" || fail "rex: no exhausted note"
grep -q '## Blockers' "$runsdir/pipeline-rex.md" || fail "rex: no Blockers section"
[[ -f $runsdir/sude-rex.fix1.md ]] || fail "rex: fix1 file not parked"
[[ $(grep -c 'agent start suhuynh-rexr' "$T/calls") -eq 2 ]] \
  || fail "rex: want 2 fresh reviewers: $(grep 'agent start' "$T/calls")"
tail -1 "$tele" | grep -q '"status":"blocked"' || fail "rex: telemetry not blocked"
tail -1 "$tele" | grep -q '"rounds":2' || fail "rex: telemetry rounds!=2"
tail -1 "$tele" | grep -q '"review_changed_outcome":true' \
  || fail "rex: review_changed_outcome!=true"
echo "ok: rounds exhausted -> blocked + Blockers, 2 fresh reviewers, telemetry"

# ── disputed: builder answers findings with a Challenge -> stop as disputed ──
run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
         -- dsp "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "dsp run1: exit $rc, want 5"
run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
         FAKE_REVIEW_MD=$'src/a.c:10 wrong\nVerdict: 1 findings' \
         FAKE_FIX_MD=$'looked into it\n\n## Challenge\nfinding 1 is wrong: the spec says X (evidence)\n\n## Lessons\n- none\n' \
         -- --resume dsp "$T/repo"
[[ $rc -eq 0 ]] || fail "dsp resume: exit $rc"
grep -q 'status=disputed' "$T/out" || fail "dsp: stdout not disputed: $(cat "$T/out")"
grep -q '## Challenge' "$runsdir/pipeline-dsp.md" || fail "dsp: challenge not copied"
grep -q 'finding 1 is wrong' "$runsdir/pipeline-dsp.md" || fail "dsp: challenge text missing"
! grep -q '^## Blockers' "$runsdir/pipeline-dsp.md" || fail "dsp: disputed recorded as a blocker"
! grep -q 'agent start suhuynh-dspr2' "$T/calls" || fail "dsp: round-2 reviewer spawned"
tail -1 "$tele" | grep -q '"status":"disputed"' || fail "dsp: telemetry not disputed"
echo "ok: disputed -> Challenge copied into pipeline file, stops before round 2"

# ── watchdog exit propagates: silent builder -> exit 2, telemetry blocked ────
run_pipe FAKE_SILENT=1 PROMPT_WORKER_MAX_SEC=2 \
         -- wdg "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 2 ]] || fail "wdg: exit $rc, want 2 (watchdog propagation): $(cat "$T/err")"
grep -q 'WATCHDOG exit=2' "$T/err" || fail "wdg: no WATCHDOG line"
grep -q 'status: blocked' "$runsdir/pipeline-wdg.md" || fail "wdg: not logged blocked"
tail -1 "$tele" | grep -q '"status":"blocked"' || fail "wdg: telemetry not blocked"
echo "ok: watchdog exit 2 -> propagated, pipeline blocked, telemetry written"

# ── R3: missing Verdict line -> exit 6 at the wait, pipeline blocked ─────────
# prompt-worker.sh rejects the reviewer report before the pipeline can read it
# (R3), so the phase wait fails with watchdog exit 6 and pipeline.sh reports
# blocked like every other watchdog exit. The pipeline's own missing-Verdict
# check stays as a backstop for reports that arrive out of band.
run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
         -- mvd "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "mvd run1: exit $rc, want 5"
run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
         FAKE_REVIEW_MD='a report with no verdict at all' \
         -- --resume mvd "$T/repo"
[[ $rc -eq 6 ]] || fail "mvd resume: exit $rc, want 6 (malformed report): $(cat "$T/err")"
grep -q 'status=blocked' "$T/out" || fail "mvd: stdout not blocked: $(cat "$T/out")"
grep -q 'watchdog exit 6' "$runsdir/pipeline-mvd.md" \
  || fail "mvd: exit 6 not recorded in the pipeline file"
grep -q 'WATCHDOG malformed-report' "$T/err" || fail "mvd: no malformed-report line"
# the wait never returned a path, so the reviewer was never cleaned up
! grep -q 'pane close' "$T/calls" || fail "mvd: reviewer cleaned up despite the failed wait"
echo "ok: missing Verdict -> watchdog exit 6, pipeline blocked, no cleanup"

# ── --base passthrough: builder worktree starts at the given ref ─────────────
run_pipe -- bas "$T/repo" --brief "$T/brief.md" --base quaqua/integ
[[ $rc -eq 5 ]] || fail "bas: exit $rc, want 5"
grep -q 'rev-parse --verify .*quaqua/integ' "$T/calls" \
  || fail "bas: --base not verified: $(cat "$T/calls")"
grep -q 'worktree add -b quaqua/sude-bas .* quaqua/integ' "$T/calls" \
  || fail "bas: worktree not based on quaqua/integ: $(cat "$T/calls")"
echo "ok: --base -> builder worktree starts at quaqua/integ"

# ── grid: the pipeline arranges its own workers into the project tab ────────
# copied scripts dir; run_pipe2 does NOT set GRID_SKIP so the pipeline's own
# grid check is observable. The check is in-process: one `herdr api snapshot`
# per check (the inner spawn/cleanup hooks are skipped by GRID_SKIP, so an
# `agent list`-free extra snapshot would mean a leak).
mkdir -p "$T/p2/scripts/worker"
cat > "$T/p2/scripts/worker/spawn-worker.sh" << 'EOF'
#!/usr/bin/env bash
exec "${QUAQUA_BIN:-quaqua}" worker spawn "$@"
EOF
chmod +x "$T/p2/scripts/worker/spawn-worker.sh"

run_pipe2() {  # like run_pipe, but against $T/p2/scripts and without GRID_SKIP
  local vars=()
  while (($#)) && [[ $1 != -- ]]; do vars+=("$1"); shift; done
  [[ ${1:-} == -- ]] && shift
  : > "$T/calls"
  rc=0
  CALLS="$T/calls" GITN="$T/gitn" HOME="$T/home" PATH="$T/bin:$PATH" \
    HERDR_BIN_PATH="$T/bin/herdr" HERDR_SOCKET_PATH="$T/herdr.sock" QUAQUA_OWNER=cos \
    PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
    PROMPT_WORKER_IDLE_SEC=9999 PROMPT_WORKER_MAX_SEC=3600 \
    QUAQUA_SCRIPT_DIR="$T/p2/scripts/lead" env "${vars[@]}" "${QUAQUA_BIN:-quaqua}" pipeline "$@" >"$T/out" 2>"$T/err" || rc=$?
}
# The pipeline's grid check is in-process now (one `herdr api snapshot` per
# check); the --only/--tab scoping is covered in crates/quaqua/tests/grid.rs.
nchecks() { grep -c '^herdr api snapshot$' "$T/calls" || true; }

lineno() { grep -n "$1" "$T/calls" | head -1 | cut -d: -f1; }

run_pipe2 FAKE_NUMSTAT=$'200\t10\tsrc/a.c' FAKE_NAMES=$'src/a.c' \
          -- grd "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "grd run1: exit $rc, want 5: $(cat "$T/err")"
[[ "$(nchecks)" -ge 1 ]] || fail "grd run1: no grid check: $(cat "$T/calls")"
[[ $(lineno 'agent start sude-grd') -lt $(lineno '^herdr api snapshot$') ]] \
  || fail "grd run1: grid ran before the builder spawn: $(cat "$T/calls")"
! grep -q 'warning: grid check failed' "$T/err" \
  || fail "grd run1: converged snapshot must not warn: $(cat "$T/err")"
echo "ok: builder spawn -> in-process grid check after the spawn, converged"

run_pipe2 FAKE_NUMSTAT=$'200\t10\tsrc/a.c' FAKE_NAMES=$'src/a.c' \
          FAKE_REVIEW_MD=$'ok\nVerdict: clean' \
          -- --resume grd "$T/repo"
[[ $rc -eq 0 ]] || fail "grd resume: exit $rc: $(cat "$T/err")"
[[ "$(nchecks)" -ge 2 ]] || fail "grd resume: want >=2 grid checks, got $(nchecks)"
echo "ok: resume -> in-process grid checks for the build + review phases"

# GRID_TAB overrides the derived <repo-basename>-workers label
run_pipe2 GRID_TAB=my-tab -- gto "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "gto: exit $rc, want 5: $(cat "$T/err")"
[[ "$(nchecks)" -ge 1 ]] || fail "gto: no grid check: $(cat "$T/calls")"
echo "ok: GRID_TAB overrides the workers tab label"

# A mis-arranged owner population is repaired into GRID_TAB before any prompt:
# two workers of the owner sit in the orchestrator tab, one working + one idle.
# The idle one creates the tab labelled GRID_TAB; the working one is reported
# deferred and never moved (rules/_all/fanout-agent-grid.md).
cat > "$T/misarranged.json" <<'JSON'
{"snapshot":{"workspaces":[{"workspace_id":"w1","label":"repo"}],
 "tabs":[{"tab_id":"w1:t1","workspace_id":"w1","label":"1","pane_count":3}],
 "panes":[{"pane_id":"w1:p1","tab_id":"w1:t1","workspace_id":"w1","agent_status":"idle",
           "tokens":{"grid_role":"orchestrator","grid_owner":"cos","grid_ready":"1"}},
          {"pane_id":"w1:p5","tab_id":"w1:t1","workspace_id":"w1","agent_status":"working","label":"sude-gtm",
           "tokens":{"grid_role":"worker","grid_owner":"cos","grid_group":"my-tab","grid_ready":"1"}},
          {"pane_id":"w1:p6","tab_id":"w1:t1","workspace_id":"w1","agent_status":"idle","label":"sude-gtm2",
           "tokens":{"grid_role":"worker","grid_owner":"cos","grid_group":"my-tab","grid_ready":"1"}}],
 "layouts":[],"agents":[{"name":"sude-gtm","pane_id":"w1:p5","agent_status":"working"},
                        {"name":"sude-gtm2","pane_id":"w1:p6","agent_status":"idle"}]}}
JSON
run_pipe2 GRID_TAB=my-tab GRID_SNAPSHOT="$T/misarranged.json" \
          -- gtm "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "gtm: exit $rc, want 5 (plan-ready): $(cat "$T/err")"
! grep -q 'warning: grid check failed' "$T/err" \
  || fail "gtm: a repairable layout must not warn: $(cat "$T/err")"
grep -q 'pane move w1:p6 --new-tab --label my-tab --no-focus' "$T/calls" \
  || fail "gtm: idle worker not moved into GRID_TAB: $(cat "$T/calls")"
! grep -q 'pane move w1:p5' "$T/calls" \
  || fail "gtm: the working worker must never move: $(cat "$T/calls")"
grep -q 'deferred: w1:p5 (working)' "$T/err" \
  || fail "gtm: working worker not reported deferred: $(cat "$T/err")"
move_line=$(lineno 'pane move w1:p6')
prompt_line=$(lineno 'agent prompt sude-gtm')
[[ -n "$move_line" && -n "$prompt_line" && "$move_line" -lt "$prompt_line" ]] \
  || fail "gtm: repair not before the prompt: $(cat "$T/calls")"
echo "ok: mis-arranged owner population repaired into GRID_TAB before the prompt"

# without GRID_TAB the repair labels the tab it creates <repo>-workers
run_pipe2 GRID_SNAPSHOT="$T/misarranged.json" -- gtd "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "gtd: exit $rc, want 5: $(cat "$T/err")"
grep -q 'pane move w1:p6 --new-tab --label repo-workers --no-focus' "$T/calls" \
  || fail "gtd: derived workers tab label not used: $(cat "$T/calls")"
echo "ok: without GRID_TAB the repair labels the created tab <repo>-workers"

# a failing grid PAUSES the pipeline before any prompt (F12, MANDATORY rule)
run_pipe2 GRID_FAIL=1 GRID_CHECK_TIMEOUT_SEC=0 -- gfx "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "gfx: exit $rc, want 5 (grid pause): $(cat "$T/err")"
grep -q 'warning: grid check failed' "$T/err" \
  || fail "gfx: no grid-failure warning: $(cat "$T/err")"
! grep -q 'agent prompt' "$T/calls" \
  || fail "gfx: worker prompted despite grid failure: $(cat "$T/calls")"
grep -q 'status: blocked' "$runsdir/pipeline-gfx.md" \
  || fail "gfx: pause not recorded as blocked"
run_pipe2 -- --resume gfx "$T/repo"
[[ $rc -eq 5 ]] || fail "gfx resume: exit $rc, want 5 plan-ready: $(cat "$T/err")"
grep -q 'agent prompt sude-gfx' "$T/calls" \
  || fail "gfx resume: plan prompt not sent after arrange"
echo "ok: grid failure pauses pre-prompt; resume after arranging continues"

# a repair that cannot move (herdr answers changed=false) also pauses before the
# review prompt: the reviewer's own pane is mis-arranged, so its repair trips
# (the builder is not in this snapshot). The reviewer sits alone in a scratch
# tab, not beside the CoS anchor.
cat > "$T/reviewer-misarranged.json" <<'JSON'
{"snapshot":{"workspaces":[{"workspace_id":"w1","label":"repo"}],
 "tabs":[{"tab_id":"w1:t1","workspace_id":"w1","label":"1","pane_count":1},
         {"tab_id":"w1:t2","workspace_id":"w1","label":"2","pane_count":1}],
 "panes":[{"pane_id":"w1:p1","tab_id":"w1:t1","workspace_id":"w1","agent_status":"idle",
           "tokens":{"grid_role":"orchestrator","grid_owner":"cos","grid_ready":"1"}},
          {"pane_id":"w1:p7","tab_id":"w1:t2","workspace_id":"w1","agent_status":"idle","label":"suhuynh-gfrr1",
           "tokens":{"grid_role":"worker","grid_owner":"cos","grid_group":"repo","grid_ready":"1"}}],
 "layouts":[],"agents":[{"name":"suhuynh-gfrr1","pane_id":"w1:p7","agent_status":"idle"}]}}
JSON
run_pipe2 FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
          -- gfr "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "gfr run1: exit $rc, want 5"
run_pipe2 FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
          GRID_SNAPSHOT="$T/reviewer-misarranged.json" GRID_MOVE_FAIL=1 \
          FAKE_REVIEW_MD=$'ok\nVerdict: clean' -- --resume gfr "$T/repo"
[[ $rc -eq 5 ]] || fail "gfr: exit $rc, want 5 (reviewer-grid pause): $(cat "$T/err")"
grep -q 'warning: grid check failed' "$T/err" || fail "gfr: no grid warning: $(cat "$T/err")"
! grep -q 'agent prompt suhuynh-gfr' "$T/calls" \
  || fail "gfr: reviewer prompted despite grid failure: $(cat "$T/calls")"
run_pipe2 FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
          FAKE_REVIEW_MD=$'ok\nVerdict: clean' -- --resume gfr "$T/repo"
[[ $rc -eq 0 ]] || fail "gfr resume: exit $rc: $(cat "$T/err")"
grep -q 'status=clean' "$T/out" || fail "gfr: not clean after arrange"
echo "ok: reviewer-spawn grid failure pauses pre-prompt; resume completes review"

# state binds the Lead exemption to this pipeline's orchestrator (F6)
run_pipe2 PIPELINE_LEAD=st6-lead HERDR_PANE_ID=w1:p9 -- st6 "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "st6 run1: exit $rc"
grep -q '^lead=st6-lead' "$runsdir/pipeline-st6.state" || fail "st6: lead not recorded"
grep -q '^lead_pane=w1:p9' "$runsdir/pipeline-st6.state" || fail "st6: lead_pane not recorded"
pid1=$(sed -n 's/^pipeline_pid=//p' "$runsdir/pipeline-st6.state")
run_pipe2 FAKE_NUMSTAT=$'20\t5\tAGENTS.md' FAKE_NAMES='AGENTS.md' -- --resume st6 "$T/repo"
pid2=$(sed -n 's/^pipeline_pid=//p' "$runsdir/pipeline-st6.state")
[[ -n $pid1 && -n $pid2 && $pid1 != "$pid2" ]] \
  || fail "st6: --resume did not refresh pipeline_pid ($pid1 -> $pid2)"
echo "ok: state carries lead/lead_pane; --resume refreshes pipeline_pid"

# ── F1: an unfinished phase report never advances (Outcome/## Blockers) ─────
run_pipe FAKE_PLAN_MD=$'plan v1\n\n## Blockers\n- cannot plan without the spec\n\n## Lessons\n- none\n' \
         -- pb1 "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 0 ]] || fail "pb1: exit $rc, want 0"
grep -q 'status=blocked' "$T/out" || fail "pb1: stdout not blocked: $(cat "$T/out")"
grep -q 'cannot plan without the spec' "$runsdir/pipeline-pb1.md" \
  || fail "pb1: blocker evidence not copied"
! grep -q 'phase: BUILD' "$T/calls" || fail "pb1: build prompted despite blocked plan"
echo "ok: blocked plan report -> blocked, evidence copied, no build prompt"

run_pipe -- pb2 "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "pb2 run1: exit $rc, want 5"
run_pipe FAKE_BUILD_MD=$'Outcome: BLOCKED\n\n## Blockers\n- implementation incomplete\n\n## Lessons\n- none\n' \
         -- --resume pb2 "$T/repo"
[[ $rc -eq 0 ]] || fail "pb2 resume: exit $rc"
grep -q 'status=blocked' "$T/out" || fail "pb2: stdout not blocked: $(cat "$T/out")"
grep -q 'implementation incomplete' "$runsdir/pipeline-pb2.md" || fail "pb2: evidence not copied"
! grep -q 'agent start suhuynh' "$T/calls" || fail "pb2: reviewer spawned on a blocked build"
tail -1 "$tele" | grep -q '"status":"blocked"' || fail "pb2: telemetry not blocked"
echo "ok: Outcome: BLOCKED build (nudge-style report) -> blocked, no reviewer"

run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' -- pb3 "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "pb3 run1: exit $rc, want 5"
run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
         FAKE_REVIEW_MD=$'src/a.c:1 wrong\nVerdict: 1 findings' \
         FAKE_FIX_MD=$'tried\n\n## Blockers\n- fix needs schema access\n\n## Lessons\n- none\n' \
         -- --resume pb3 "$T/repo"
[[ $rc -eq 0 ]] || fail "pb3 resume: exit $rc"
grep -q 'status=blocked' "$T/out" || fail "pb3: stdout not blocked: $(cat "$T/out")"
grep -q 'fix needs schema access' "$runsdir/pipeline-pb3.md" || fail "pb3: evidence not copied"
! grep -q 'agent start suhuynh-pb3r2' "$T/calls" || fail "pb3: round-2 reviewer spawned"
echo "ok: blocked fix report -> blocked, no next review round"

# F1b (regression): a '## Blockers' section whose body only says none is
# finished — the r3 run of quaqua-rs stopped falsely on a 'Blockers: none'
# report. Accepts none / None / '- none' (case-insensitive, optional bullet).
run_pipe FAKE_PLAN_MD=$'plan v1\n\n## Blockers\n- none\n\n## Lessons\n- none\n' \
         -- pbz "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "pbz: exit $rc, want 5 plan-ready ('## Blockers: - none' is not a blocker): $(cat "$T/out")"
grep -q 'plan-ready' "$T/err" || fail "pbz: no plan-ready notice"
! grep -q 'status=blocked' "$T/out" || fail "pbz: 'none' blockers wrongly blocked: $(cat "$T/out")"
[[ -f $runsdir/sude-pbz.plan.md ]] || fail "pbz: plan not advanced to plan-ready"

run_pipe FAKE_PLAN_MD=$'plan v2\n\n## Blockers\nNone\n\n## Lessons\n- none\n' \
         -- pbw "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "pbw: exit $rc, want 5 plan-ready ('## Blockers: None' is not a blocker): $(cat "$T/out")"
! grep -q 'status=blocked' "$T/out" || fail "pbw: 'None' blockers wrongly blocked: $(cat "$T/out")"
echo "ok: Blockers 'none'/'None' -> not blocked, plan advances to plan-ready"

# ── F2: no committed snapshot / dirty tree -> resumable blocked, not clean ───
run_pipe -- ncm "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "ncm run1: exit $rc, want 5"
run_pipe FAKE_HEAD_SHA=ba5eba5 -- --resume ncm "$T/repo"   # tip == base: no commits
[[ $rc -eq 3 ]] || fail "ncm: exit $rc, want 3 (resumable block): $(cat "$T/out")"
grep -q 'status=blocked' "$T/out" || fail "ncm: not blocked"
grep -q 'produced no commits' "$runsdir/pipeline-ncm.md" || fail "ncm: no-commit reason missing"
! grep -q 'agent start suhuynh' "$T/calls" || fail "ncm: reviewer spawned with no commits"
echo "ok: zero commits after build -> resumable blocked, no reviewer"

run_pipe -- nop "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "nop run1: exit $rc, want 5"
run_pipe FAKE_HEAD_SHA=ba5eba5 FAKE_BUILD_MD=$'looked carefully\nOutcome: no changes\n\n## Lessons\n- none\n' \
         -- --resume nop "$T/repo"
[[ $rc -eq 0 ]] || fail "nop: exit $rc"
grep -q 'status=clean' "$T/out" || fail "nop: declared no-op did not finish clean: $(cat "$T/out")"
grep -q 'no commits.*no changes' "$runsdir/pipeline-nop.md" || fail "nop: no-op note missing"
! grep -q 'agent start suhuynh' "$T/calls" || fail "nop: reviewer spawned for a no-op"
echo "ok: explicit 'Outcome: no changes' -> clean, review skipped"

run_pipe -- dty "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "dty run1: exit $rc, want 5"
run_pipe FAKE_PORCELAIN=$' M src/a.c\n?? newfile.c' -- --resume dty "$T/repo"
[[ $rc -eq 3 ]] || fail "dty: exit $rc, want 3"
grep -q 'status=blocked' "$T/out" || fail "dty: not blocked"
grep -q 'uncommitted/untracked' "$runsdir/pipeline-dty.md" || fail "dty: dirty evidence not logged"
grep -q '?? newfile.c' "$runsdir/pipeline-dty.md" || fail "dty: untracked file not listed"
! grep -q 'agent start suhuynh' "$T/calls" || fail "dty: reviewer spawned on dirty tree"
echo "ok: dirty worktree (tracked + untracked) -> blocked, evidence logged"

run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' -- fxc "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "fxc run1: exit $rc, want 5"
run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
         FAKE_REVIEW_MD=$'src/a.c:1 wrong\nVerdict: 1 findings' \
         FAKE_HEAD2_SHA=c0ffee1deadbeef0000000000000000000000 \
         -- --resume fxc "$T/repo"   # post-fix tip identical to the reviewed tip
[[ $rc -eq 3 ]] || fail "fxc: exit $rc, want 3: $(cat "$T/out")"
grep -q 'no commits past' "$runsdir/pipeline-fxc.md" || fail "fxc: no-advance reason missing"
echo "ok: fix without a new commit -> resumable blocked (no-op fix is a Challenge)"

# ── F3: a reviewer Challenge reaches the Lead before any verdict ─────────────
run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' -- rch "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "rch run1: exit $rc, want 5"
run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
         FAKE_REVIEW_MD=$'the diff contradicts the spec\n\n## Challenge\nspec section 3 requires Y, diff does X (evidence)\n\nVerdict: clean' \
         -- --resume rch "$T/repo"
[[ $rc -eq 0 ]] || fail "rch: exit $rc"
grep -q 'status=disputed' "$T/out" || fail "rch: reviewer challenge did not dispute: $(cat "$T/out")"
grep -q 'spec section 3 requires Y' "$runsdir/pipeline-rch.md" || fail "rch: challenge not copied"
grep -q 'pane close' "$T/calls" || fail "rch: reviewer not cleaned"
echo "ok: reviewer Challenge + clean verdict -> disputed, copied, reviewer cleaned"

run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' -- rchf "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "rchf run1: exit $rc, want 5"
run_pipe FAKE_NUMSTAT=$'300\t0\tsrc/a.c' FAKE_NAMES='src/a.c' \
         FAKE_REVIEW_MD=$'## Challenge\npremise wrong\n\nVerdict: 2 findings' \
         -- --resume rchf "$T/repo"
[[ $rc -eq 0 ]] || fail "rchf: exit $rc"
grep -q 'status=disputed' "$T/out" || fail "rchf: challenge+findings not disputed"
echo "ok: reviewer Challenge + findings verdict -> still disputed"

# ── F5: a phase never gets more than the whole-pipeline remainder ────────────
run_pipe -- clk "$T/repo" --brief "$T/brief.md" --review skip
[[ $rc -eq 5 ]] || fail "clk run1: exit $rc, want 5"
perl -pi -e 's/^start_elapsed=.*/start_elapsed=7180/' "$runsdir/pipeline-clk.state"
printf '%s\n' 1700000000 > "$T/clock"
run_pipe "${CLK[@]}" PIPELINE_MAX_SEC=7200 -- --resume clk "$T/repo"
grep -q 'budget 20s' "$T/err" \
  || fail "clk: phase budget not clamped to remainder (want 20s): $(cat "$T/err")"
[[ $rc -eq 0 ]] || fail "clk: exit $rc inside the allowance"
echo "ok: phase budget clamped to pipeline remainder (20s of 2700s)"

run_pipe -- clk2 "$T/repo" --brief "$T/brief.md" --review skip
[[ $rc -eq 5 ]] || fail "clk2 run1: exit $rc, want 5"
perl -pi -e 's/^start_elapsed=.*/start_elapsed=7190/' "$runsdir/pipeline-clk2.state"
printf '%s\n' 1700000000 > "$T/clock"
run_pipe "${CLK[@]}" PIPELINE_MAX_SEC=7195 FAKE_DELAY=7 -- --resume clk2 "$T/repo"
[[ $rc -eq 4 ]] || fail "clk2: exit $rc, want 4 (wait over remainder): $(cat "$T/out")"
grep -q 'status=blocked' "$T/out" || fail "clk2: not blocked"
grep -q 'budget 5s' "$T/err" || fail "clk2: remainder budget not applied"
rm -f "$T/delayed_file" "$T/delayed_content"
echo "ok: file landing past the total budget -> watchdog 4 + blocked, never clean"

# ── F7: a report predating the prompt is archived, never consumed ────────────
printf 'stale content from an old task\n' > "$runsdir/sude-sta.md"
run_pipe -- sta "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "sta: exit $rc, want 5"
ls "$runsdir"/sude-sta.md.stale.* >/dev/null 2>&1 || fail "sta: stale report not archived"
grep -q 'stale content' "$runsdir"/sude-sta.md.stale.* || fail "sta: archive lost content"
grep -q 'agent prompt sude-sta ' "$T/calls" || fail "sta: no fresh prompt after archive"
echo "ok: stale report archived before the fresh prompt"

run_pipe FAKE_SILENT=1 PROMPT_WORKER_MAX_SEC=2 -- fst "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 2 ]] || fail "fst run1: exit $rc, want 2: $(cat "$T/err")"
printf 'stale\n' > "$runsdir/sude-fst.md"
perl -e 'utime time-3600,time-3600,@ARGV or die "utime: $!"' "$runsdir/sude-fst.md"
run_pipe FAKE_SILENT=1 PROMPT_WORKER_MAX_SEC=2 -- --resume fst "$T/repo"
ls "$runsdir"/sude-fst.md.stale.* >/dev/null 2>&1 \
  || fail "fst: report older than the recorded prompt time not archived on resume"
echo "ok: resume archives a report older than the recorded prompt time"

# The paired positive case protects against BSD stat being misread as mtime 0:
# a report arriving after a paused wait must be consumed, not always archived.
run_pipe FAKE_SILENT=1 PROMPT_WORKER_MAX_SEC=2 -- cur "$T/repo" --brief "$T/brief.md"
[[ $rc -eq 2 ]] || fail "cur run1: exit $rc, want 2"
printf 'fresh plan arriving after the prompt\n\n## Lessons\n- none\n' > "$runsdir/sude-cur.md"
run_pipe FAKE_SILENT=1 PROMPT_WORKER_MAX_SEC=2 -- --resume cur "$T/repo"
[[ $rc -eq 5 ]] || fail "cur resume: exit $rc, want 5 plan-ready: $(cat "$T/err")"
grep -q 'fresh plan' "$runsdir/sude-cur.plan.md" || fail "cur: fresh report not consumed"
! compgen -G "$runsdir/sude-cur.md.stale.*" >/dev/null || fail "cur: fresh report archived as stale"
! grep -q 'agent prompt' "$T/calls" || fail "cur: resumed wait re-prompted"
echo "ok: resume consumes a fresh post-prompt report without archiving or re-prompting"

# ── telemetry: one jsonl file, two record kinds ─────────────────────────────
# Pipeline stop rows (no `rec`, or "pipeline") carry the lock §2 fields; a
# worker row ("rec":"worker", appended by `worker cleanup`) carries its own
# shape and links back to the task through `pipeline`. Both may appear, so the
# §2 check is scoped to the pipeline rows rather than applied to every line.
while IFS= read -r l; do
  jq -e . <<<"$l" >/dev/null || fail "telemetry line is not jsonl: $l"
  if [[ $(jq -r '.rec // "pipeline"' <<<"$l") == worker ]]; then
    jq -e '.ts and .name and .spend_src' <<<"$l" >/dev/null \
      || fail "worker telemetry row malformed: $l"
  else
    jq -e '.ts and .task and .repo and .builder and .reviewer and .review
           and (.rounds|type=="number") and (.review_changed_outcome|type=="boolean")
           and .status and (.minutes|type=="number")' <<<"$l" >/dev/null \
      || fail "pipeline telemetry row malformed: $l"
  fi
done < "$tele"
# The reviewer's cleanup row is what links a worker to the task it served.
jq -e 'select(.rec=="worker" and .name=="suhuynh-islr1" and .pipeline=="isl")' \
  "$tele" >/dev/null || fail "no worker telemetry row for the isl reviewer: $(cat "$tele")"
echo "ok: telemetry -> jsonl, §2 fields on every pipeline row, worker rows linked"

# ══ real-Git fixture: snapshot binding + spawn resume (F2/F4) ══════════════
# Only herdr is stubbed; git is real, so worktrees, commits and SHAs behave
# like production. The stub makes the builder's commit while its prompt runs
# (FAKE_DO_COMMIT=<wt>), and can fail agent start/wait once per name.
mkdir -p "$T/rgbin" "$T/rg/repo"
git -C "$T/rg/repo" init -q
git -C "$T/rg/repo" -c user.email=t@t -c user.name=t commit -qm init --allow-empty
cat > "$T/rgbin/herdr" <<'STUB'
#!/usr/bin/env bash
echo "herdr $*" >> "$CALLS"
case "${1:-} ${2:-}" in
  "agent get")
    printf '{"result":{"agent":{"agent_status":"working","cwd":"%s","pane_id":"%%1"}}}\n' "$HOME" ;;
  "agent read") printf 'working\n' ;;
  "agent prompt")
    w=$3; content=''
    # same report tails as the stub above ('## Lessons' for builders)
    case "$4" in
      *"phase: PLAN"*)   content=${FAKE_PLAN_MD:-$'fake plan\n\n## Lessons\n- none'} ;;
      *"phase: BUILD"*)  content=${FAKE_BUILD_MD:-$'fake build\n\n## Lessons\n- none'} ;;
      *"phase: FIX"*)    content=${FAKE_FIX_MD:-$'fake fix\n\n## Lessons\n- none'} ;;
      *"phase: REVIEW"*) content=${FAKE_REVIEW_MD:-'Verdict: clean'} ;;
    esac
    case "$4" in
      *"phase: BUILD"*|*"phase: FIX"*)
        if [ -n "${FAKE_DO_COMMIT:-}" ]; then
          ( cd "$FAKE_DO_COMMIT" && echo "$(date +%s)-$$" >> impl.txt \
            && git add impl.txt && git -c user.email=t@t -c user.name=t commit -qm impl ) || true
        fi
        [ -n "${FAKE_DIRTY:-}" ] && echo scratch >> "$FAKE_DO_COMMIT/dirty.txt" ;;
    esac
    if [ -n "$content" ] && [ "${FAKE_SILENT:-0}" != 1 ]; then
      printf '%s\n' "$content" > "$HOME/quaqua/runs/$w.md"
    fi
    exit "${FAKE_RC:-0}" ;;
  "agent start")
    n=$(cat "$T/failed-$3" 2>/dev/null || echo 0)
    limit=${FAKE_FAIL_LIMIT:-2}
    if [ "$3" = "${FAKE_FAIL_START:-}" ] && [ "$n" -lt "$limit" ]; then
      echo $((n + 1)) > "$T/failed-$3"; exit 1
    fi ;;
  "agent wait")
    if [ "$3" = "${FAKE_FAIL_WAIT:-}" ] && [ ! -f "$T/failedw-$3" ]; then
      : > "$T/failedw-$3"; exit 1
    fi ;;
  "pane split") printf '{"result":{"pane":{"pane_id":"%%1"}}}\n' ;;
  "pane wait-output") exit 1 ;;
esac
exit 0
STUB
chmod +x "$T/rgbin/herdr"
export HERDR_BIN_PATH="$T/rgbin/herdr"
export HERDR_SOCKET_PATH="$T/herdr.sock"

run_rg() {  # real-git run (only herdr stubbed); sets rc, $T/out, $T/err, $T/calls
  local vars=()
  while (($#)) && [[ $1 != -- ]]; do vars+=("$1"); shift; done
  [[ ${1:-} == -- ]] && shift
  : > "$T/calls"
  rc=0
  CALLS="$T/calls" HOME="$T/home" PATH="$T/rgbin:$PATH" QUAQUA_TEST=1 GRID_SKIP=1 \
    HERDR_BIN_PATH="$T/rgbin/herdr" HERDR_SOCKET_PATH="$T/herdr.sock" \
    PROMPT_WORKER_POLL_SEC=1 PROMPT_WORKER_STALL_SEC=9999 \
    PROMPT_WORKER_IDLE_SEC=9999 PROMPT_WORKER_MAX_SEC=3600 T="$T" \
    env "${vars[@]}" "${QUAQUA_BIN:-quaqua}" pipeline "$@" >"$T/out" 2>"$T/err" || rc=$?
}

# F2: the reviewer worktree is created at exactly the committed tip the gate saw
run_rg -- rgb "$T/rg/repo" --brief "$T/brief.md" --review always
[[ $rc -eq 5 ]] || fail "rgb run1: exit $rc, want 5: $(cat "$T/err")"
wt_rgb="$T/rg/.wt/sude-rgb"
run_rg FAKE_DO_COMMIT="$wt_rgb" FAKE_REVIEW_MD=$'ok\nVerdict: clean' \
       -- --resume rgb "$T/rg/repo"
[[ $rc -eq 0 ]] || fail "rgb resume: exit $rc: $(cat "$T/err")"
grep -q 'status=clean' "$T/out" || fail "rgb: not clean: $(cat "$T/out")"
head_rgb=$(git -C "$wt_rgb" rev-parse HEAD)
grep -q "spawned on $head_rgb" "$runsdir/pipeline-rgb.md" \
  || fail "rgb: reviewer not spawned on the committed tip $head_rgb: $(cat "$runsdir/pipeline-rgb.md")"
grep -q 'review round 1: Verdict: clean' "$runsdir/pipeline-rgb.md" || fail "rgb: review did not run"
grep -q "commit ${head_rgb:0:7}" "$runsdir/pipeline-rgb.md" || fail "rgb: tip not recorded"
echo "ok: real git — reviewer spawned on the exact committed tip (gate SHA == base)"

# F2: uncommitted leftovers are missing from the reviewed snapshot -> blocked
run_rg -- rgd "$T/rg/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "rgd run1: exit $rc, want 5"
wt_rgd="$T/rg/.wt/sude-rgd"
run_rg FAKE_DO_COMMIT="$wt_rgd" FAKE_DIRTY=1 -- --resume rgd "$T/rg/repo"
[[ $rc -eq 3 ]] || fail "rgd: exit $rc, want 3 (dirty tree): $(cat "$T/err")"
grep -q 'status=blocked' "$T/out" || fail "rgd: not blocked"
grep -q 'uncommitted/untracked' "$runsdir/pipeline-rgd.md" || fail "rgd: dirty not logged"
echo "ok: real git — dirty worktree blocked (untracked file kept off the branch)"

# F2: a build with zero commits is blocked, not a clean no-op
run_rg -- rgn "$T/rg/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "rgn run1: exit $rc, want 5"
run_rg -- --resume rgn "$T/rg/repo"
[[ $rc -eq 3 ]] || fail "rgn: exit $rc, want 3: $(cat "$T/err")"
grep -q 'produced no commits' "$runsdir/pipeline-rgn.md" || fail "rgn: no-commit reason missing"
echo "ok: real git — zero commits after build is blocked"

run_rg -- rgp "$T/rg/repo" --brief "$T/brief.md"
[[ $rc -eq 5 ]] || fail "rgp run1: exit $rc, want 5"
run_rg FAKE_BUILD_MD=$'checked\nOutcome: no changes\n\n## Lessons\n- none\n' -- --resume rgp "$T/rg/repo"
[[ $rc -eq 0 ]] || fail "rgp: exit $rc"
grep -q 'status=clean' "$T/out" || fail "rgp: declared no-op not clean: $(cat "$T/out")"
echo "ok: real git — declared no-op finishes clean, no reviewer spawned"

# F4: agent-start failure keeps the worktree; --resume finishes the startup.
# A start failure now takes one configured fallback hop (sude→suhuynh, team.toml)
# before it is a spawn failure, and a claude start timeout is answered with an
# idle wait — so both the start and that wait must fail to reach exit 3.
run_rg FAKE_FAIL_START=sude-rsf FAKE_FAIL_WAIT=sude-rsf -- rsf "$T/rg/repo" --brief "$T/brief.md"
[[ $rc -eq 3 ]] || fail "rsf: exit $rc, want 3 (spawn failure): $(cat "$T/err")"
grep -q 'fallback: sude->suhuynh\|fallback: sude→suhuynh' "$T/err" \
  || fail "rsf: fallback hop not reported: $(cat "$T/err")"
[[ -d $T/rg/.wt/sude-rsf ]] || fail "rsf: worktree should survive the failed start"
run_rg -- --resume rsf "$T/rg/repo"
[[ $rc -eq 5 ]] || fail "rsf resume: exit $rc, want 5 plan-ready: $(cat "$T/err")"
grep -q 'agent start sude-rsf' "$T/calls" || fail "rsf: agent start not retried"
grep -q 'reusing it' "$T/err" || fail "rsf: worktree reuse note missing: $(cat "$T/err")"
echo "ok: failed agent start -> resume reuses worktree, retries start, prompts plan"

# F4 (reviewer): a claude start herdr could not confirm (workspace-trust dialog
# or a slow startup) is answered with an idle wait, so the reviewer fails only
# when that wait never sees idle — the spawn then keeps the worktree; --resume
# retries it
run_rg -- rsv "$T/rg/repo" --brief "$T/brief.md" --review always
[[ $rc -eq 5 ]] || fail "rsv run1: exit $rc, want 5"
run_rg FAKE_DO_COMMIT="$T/rg/.wt/sude-rsv" FAKE_FAIL_START=suhuynh-rsvr1 \
       FAKE_FAIL_WAIT=suhuynh-rsvr1 -- --resume rsv "$T/rg/repo"
[[ $rc -eq 3 ]] || fail "rsv: exit $rc, want 3 (reviewer spawn failure): $(cat "$T/err")"
[[ -d $T/rg/.wt/suhuynh-rsvr1 ]] || fail "rsv: reviewer worktree should survive"
run_rg FAKE_REVIEW_MD=$'ok\nVerdict: clean' -- --resume rsv "$T/rg/repo"
[[ $rc -eq 0 ]] || fail "rsv resume: exit $rc: $(cat "$T/err")"
grep -q 'status=clean' "$T/out" || fail "rsv: not clean after reviewer retry"
echo "ok: reviewer spawn failure -> resume reuses worktree, reviews, finishes clean"

echo "PASS"
