#!/usr/bin/env bash
# T46 E2E: the switch signal (part `switch`) and clause (d) (part `d`) on the new binary.
# Spends REAL tokens on two harnesses. Run only with anh iu's approval (T48); use --dry-run to read the plan.
# shellcheck disable=SC2015  # `A && pass || fail` is intended: fail exits, pass cannot fail
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: full_pipeline.sh [--dry-run] [--check] [-h|--help] [part...]      parts: switch | d (default: both)

  switch  one real task on the new binary: brief -> fanout to 2 harnesses -> cross-harness
          review bound to a candidate sha -> gate -> merge into the project integration branch;
          every kind/harness/model/effort is checked against qq.db.
  d       a non-claude worker's `git push`, an out-of-band spawn and a Peer's `qq push` are all refused.
  --dry-run   print the planned steps; start no herdr, no daemon, no agent, spend nothing.
  --check     resolve every qq verb and flag this script calls against the binary (usage probes in a
              temp workspace); start no herdr, no daemon, no agent, spend nothing. Exit 1 on a miss.

env: QQ (binary, default target/debug/qq)  E2E_PI_MODEL (model of the pi kind added with `config set`,
     required for a real run)  E2E_PI_EFFORT (default medium)  E2E_TIMEOUT (seconds per wait, default 900)
     E2E_BUILDER_KIND / E2E_REVIEWER_KIND (default sude / pie; must differ in harness)
     E2E_STOP_AFTER (C2 | C3 | C4: end cleanly after that common step; starts no agent, spends no tokens)
USAGE
}

DRY=0
CHECK=0
PARTS=()
for a in "$@"; do
  case "$a" in
    --dry-run) DRY=1 ;;
    --check) CHECK=1 ;;
    -h | --help) usage; exit 0 ;;
    switch | d) PARTS+=("$a") ;;
    *) usage >&2; exit 2 ;;
  esac
done
[ "${#PARTS[@]}" -gt 0 ] || PARTS=(switch d)
want() { local p; for p in "${PARTS[@]}"; do [ "$p" = "$1" ] && return 0; done; return 1; }

REPO_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
QQ=${QQ:-$REPO_ROOT/target/debug/qq}
PI_MODEL=${E2E_PI_MODEL:-}
PI_EFFORT=${E2E_PI_EFFORT:-medium}
TIMEOUT=${E2E_TIMEOUT:-900}
BUILDER=${E2E_BUILDER_KIND:-sude}
REVIEWER=${E2E_REVIEWER_KIND:-pie}
PROJECT=e2e
TASK=e2e1   # the pipeline's task id (a string); its task row is made with `qq task create` in S2b
N=0

# ---- assertion helpers: one assertion per step, evidence on every pass, first failure exits --------
pass() { N=$((N + 1)); printf 'PASS %02d %s | %s\n' "$N" "$1" "$2"; }
fail() { N=$((N + 1)); printf 'FAIL %02d %s | %s\n' "$N" "$1" "$2" >&2; exit 1; }
# expect STEP WANT GOT: equality assertion; evidence is the observed value.
expect() { [ "$2" = "$3" ] && pass "$1" "$3" || fail "$1" "want [$2] got [$3]"; }
# expect_ok STEP EVIDENCE CMD...: CMD must succeed.
expect_ok() { local s=$1 e=$2; shift 2; "$@" >/dev/null 2>&1 && pass "$s" "$e" || fail "$s" "command failed: $*"; }
# expect_refused STEP CMD...: CMD must exit non-zero; its output is the evidence.
expect_refused() {
  local s=$1 out rc=0; shift
  out=$("$@" 2>&1) || rc=$?
  [ "$rc" -ne 0 ] && pass "$s" "exit=$rc $(printf '%s' "$out" | tr '\n' ' ' | cut -c1-160)" \
    || fail "$s" "command was allowed: $*"
}

# ---- the plan (single source for --dry-run and the real run) ---------------------------------------
plan() {
  cat <<PLAN
common
  C1 isolate        temp workspace seeded from a COPY of ~/quaqua; refuse a path inside ~/quaqua
  C2 session        private herdr session (own XDG_CONFIG_HOME), \`qq daemon\` on the copy, \`qq ping\`;
                    a pane tagged grid_role=cos is the only door for CoS calls (identity = herdr pane)
  C3 seed           \`qq config import team.toml\`; roster has the claude builder kind
  C4 second harness \`qq config set kind.$REVIEWER '{harness:pi,model:\$E2E_PI_MODEL,...}'\` (live roster is claude-only)
part switch
  S1 sqlite roster  kind rows for $BUILDER and $REVIEWER in qq.db name two different harnesses
  S2 fixture        git repo + local bare origin + integration branch integ/$PROJECT, brief file
  S2b task row      \`qq task create $PROJECT '<title>' --kind $BUILDER --repo <repo>\` writes a task row (project, title,
                    kind, state open); the same call again returns that row (created=false)
  S3 fanout         \`worker spawn $TASK-b $BUILDER <repo>\` and \`worker spawn $TASK-r1 $REVIEWER <repo>\`: two harnesses
  S4 sqlite spawn   each run row's harness/model/effort equals its kind row (config read from SQLite)
  S5 build          \`pipeline '<json>' --wait\` (plan=false) returns exit 0 (merged); exit 5 = paused for approval ->
                    \`pipeline --resume $TASK --wait\` and wait again (at most 3 pauses); builder reports a candidate sha
  S6 review         reviewer on another harness reviews; its run row carries that same candidate sha
  S7 sha binding    \`merge '<json>'\` refuses a sha other than the reviewed one, accepts the reviewed sha
  S8 gate           the pipeline workflow finished and its journal reached the merge
  S9 merge          the integration branch contains the candidate sha
  S10 sqlite config no run of the task names a kind/harness/model/effort absent from qq.db
part d
  D1 spawn fence    \`worker spawn\` from a pane with no agent identity is refused
  D2 push fence     \`qq gate\` blocks \`git push\` (PreToolUse hook, exit 2)
  D3 worker push    a pi worker told to run \`git push\` leaves the bare origin without the ref
  D4 journal        qq.db journal holds a refusal event (authority:refusals) after D1
  D5 qq push fence  \`qq push\` from a pane with no agent identity is refused; the same \`qq push\` run by the pi
                    worker (a Peer) leaves the bare origin without the ref and journals a refusal on edge
                    push/orchestration naming that worker
cleanup (trap)      stop daemon, stop the herdr session, trash the temp dir
PLAN
}

# ---- --check: every command and flag this script calls, resolved against the binary ---------------------
# Probes are usage errors (no daemon, no herdr, nothing started); the workspace is a throwaway temp dir.
# A verb is "found" when it is in the usage list; each flag/token must appear in that verb's list line or in
# the usage text its bad-argument probe prints.
check_cmds() {
  local tmp list line probe miss=0 verb tok spec
  [ -x "$QQ" ] || { echo "no binary at $QQ (cargo build --locked -p qq-core, or set QQ)" >&2; return 1; }
  tmp=$(mktemp -d /tmp/qqchk.XXXXXX)
  list=$(env -u HERDR_SOCKET_PATH -u HERDR_PANE_ID -u QQ_AGENT QUAQUA_HOME="$tmp" QUAQUA_ROOT="$tmp" "$QQ" 2>&1 || true)
  # spec: verb|probe args (space separated, "-" = none)|tokens that must appear (space separated, "-" = none)
  local specs=(
    "daemon|-|-"
    "ping|-|-"
    "stop|-|-"
    "gate|-|-"
    "config|set|import set"
    "worker|spawn --probe|spawn --base --review <name> <kind> <repo-path>"
    "worker|prompt|prompt <name>"
    "pipeline|-|--rounds --resume --cancel --wait"
    "task|create|task create --kind --disposition --repo"
    "push|-|push <repo> <remote> <branch>"
    "merge|-|merge"
  )
  for spec in "${specs[@]}"; do
    IFS='|' read -r verb probe tok <<<"$spec"
    line=$(printf '%s\n' "$list" | grep -E "^  $verb( |$)" || true)
    if [ -z "$line" ]; then printf 'MISSING verb %-9s not in qq usage list\n' "$verb"; miss=1; continue; fi
    local out=""
    if [ "$probe" != "-" ] && [ "$verb" != "daemon" ]; then
      # shellcheck disable=SC2086  # probe words are split on purpose
      out=$(env -u HERDR_SOCKET_PATH -u HERDR_PANE_ID -u QQ_AGENT QUAQUA_HOME="$tmp" QUAQUA_ROOT="$tmp" timeout 10 "$QQ" $verb $probe 2>&1 || true)
    fi
    local text="$line"$'\n'"$out" bad=""
    if [ "$tok" != "-" ]; then
      for t in $tok; do printf '%s' "$text" | grep -qF -- "$t" || bad="$bad $t"; done
    fi
    if [ -n "$bad" ]; then printf 'MISSING %-9s token(s) not in usage:%s\n' "$verb $probe" "$bad"; miss=1
    else printf 'FOUND   %-9s %s\n' "$verb" "$([ "$tok" = "-" ] && echo "verb in usage list" || echo "tokens ok: $tok")"; fi
  done
  # verbs with a JSON argument: the shape must be refused as bad usage, not as unknown verb
  for spec in "mail send --to --kind --body-file" "grid reconcile --only --tab" "lead spawn --task"; do
    # shellcheck disable=SC2086  # spec words are split on purpose
    set -- $spec
    verb=$1; shift
    line=$(printf '%s\n' "$list" | grep -E "^  $verb( |$)" || true)
    [ -n "$line" ] || { printf 'MISSING verb %-9s not in qq usage list\n' "$verb"; miss=1; continue; }
    local bad=""
    for t in "$@"; do printf '%s' "$line" | grep -qF -- "$t" || bad="$bad $t"; done
    [ -z "$bad" ] && printf 'FOUND   %-9s tokens ok: %s\n' "$verb" "$*" || { printf 'MISSING %-9s token(s) not in usage:%s\n' "$verb" "$bad"; miss=1; }
  done
  trash "$tmp" 2>/dev/null || true
  return $miss
}

if [ "$CHECK" -eq 1 ]; then
  echo "CHECK parts: ${PARTS[*]} (binary $QQ; nothing started, no tokens spent)"
  if check_cmds; then echo "CHECK OK: every command and flag the script calls exists"; exit 0; fi
  echo "CHECK FAILED" >&2; exit 1
fi

if [ "$DRY" -eq 1 ]; then
  echo "DRY RUN parts: ${PARTS[*]} (nothing started, no tokens spent)"
  plan
  exit 0
fi

# ---- preflight ----------------------------------------------------------------------------------------
for need in herdr git sqlite3 python3; do
  command -v "$need" >/dev/null 2>&1 || { echo "missing: $need" >&2; exit 1; }
done
[ -x "$QQ" ] || { echo "no binary at $QQ (cargo build --locked -p qq-core, or set QQ)" >&2; exit 1; }
[ -n "$PI_MODEL" ] || { echo "set E2E_PI_MODEL (model for the pi kind)" >&2; exit 1; }
check_cmds >/dev/null || { echo "qq lacks a verb or flag this script calls; run with --check" >&2; exit 1; }

# ---- C1 isolate -----------------------------------------------------------------------------------------
LIVE=$(cd ~/quaqua && pwd -P)
T=$(mktemp -d /tmp/qqe2e.XXXXXX)      # /tmp: unix socket paths are short
WS=$(cd "$T" && pwd -P)/ws
case "$WS/" in "$LIVE"/*) echo "refusing: workspace $WS is inside $LIVE" >&2; exit 1 ;; esac
SESSION="qqe2e-$$-$RANDOM"
export HERDR_SESSION="$SESSION"   # herdr server, daemon (its pane lookup) and every qq call share this session
export XDG_CONFIG_HOME="$T/cfg"
mkdir -p "$XDG_CONFIG_HOME" "$WS" "$T/calls"
unset HERDR_SOCKET_PATH HERDR_ENV HERDR_PANE_ID HERDR_WORKSPACE_ID HERDR_TAB_ID HERDR_AGENT QQ_AGENT
export QUAQUA_HOME="$WS" QUAQUA_ROOT="$WS"
H() { herdr --session "$SESSION" "$@"; }

DAEMON_PID=""; SERVER_PID=""; cleaned=0
cleanup() {
  [ "$cleaned" -eq 0 ] || return 0
  cleaned=1; set +e
  "$QQ" stop >/dev/null 2>&1
  [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null
  H server stop >/dev/null 2>&1
  [ -n "$SERVER_PID" ] && { sleep 1; kill "$SERVER_PID" 2>/dev/null; }
  case "$T" in /tmp/qqe2e.??????) trash "$T" 2>/dev/null || echo "left $T (trash it by hand)" ;; esac
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

# rsync/tar copy without VCS, build output and live state (runs/ starts empty in the copy).
tar -C "$LIVE" --exclude=./.git --exclude=./runs --exclude=./target --exclude=./node_modules -cf - . | tar -C "$WS" -xf -
mkdir -p "$WS/runs"
[ -f "$WS/team.toml" ] || fail C1 "copy has no team.toml"
pass C1 "workspace=$WS is a copy of $LIVE, outside it"

DB="$WS/runs/qq.db"
q() { sqlite3 -readonly "$DB" "$1"; }
wait_for() { # wait_for STEP SECONDS CMD...
  local s=$1 t=$2 i=0; shift 2
  until "$@" >/dev/null 2>&1; do i=$((i + 2)); [ "$i" -le "$t" ] || fail "$s" "timed out after ${t}s: $*"; sleep 2; done
}
# first "pane_id" value anywhere in a herdr JSON reply on stdin
first_pane() { python3 -c 'import json,sys
def walk(o):
    if isinstance(o,dict):
        if "pane_id" in o: print(o["pane_id"]); raise SystemExit
        for v in o.values(): walk(v)
    elif isinstance(o,list):
        for v in o: walk(v)
walk(json.load(sys.stdin))'; }

# ---- C2 session + daemon ---------------------------------------------------------------------------------
herdr --session "$SESSION" server >"$T/herdr.log" 2>&1 & SERVER_PID=$!
wait_for C2 30 H workspace list
H workspace create --cwd "$WS" --label e2e --no-focus >/dev/null
( cd "$WS" && exec "$QQ" daemon >"$T/daemon.log" 2>&1 ) & DAEMON_PID=$!   # exec: $! is the daemon itself, so cleanup kills it

# Caller identity is the herdr pane the process runs in (a pane tagged grid_role=cos is the CoS; a process in
# no pane, or in a pane with no agent, is refused). So every orchestration call runs inside a pane:
#   as_pane PANE ENVPREFIX CMD...  -> runs CMD in PANE, sets AP_RC / AP_OUT (output file contents).
COS_PANE=$(H pane list | first_pane)
[ -n "$COS_PANE" ] || fail C2 "no pane in the e2e workspace"
H pane report-metadata "$COS_PANE" --source e2e --token grid_role=cos >/dev/null
CALLN=0
as_pane() {
  local pane=$1; shift
  CALLN=$((CALLN + 1))
  local base="$T/calls/$CALLN" i=0
  printf '#!/usr/bin/env bash\ncd %q\n%s > %q 2>&1\necho $? > %q.rc\n' "$WS" "$(printf '%q ' "$@")" "$base.out" "$base" >"$base.sh"
  H pane run "$pane" "bash $base.sh" >/dev/null
  until [ -f "$base.rc" ]; do i=$((i + 1)); [ "$i" -le "$TIMEOUT" ] || fail "as_pane" "no exit status after ${TIMEOUT}s: $*"; sleep 1; done
  AP_RC=$(cat "$base.rc"); AP_OUT=$(cat "$base.out")
}
as_cos() { as_pane "$COS_PANE" "$@"; }
# stop_after STEP: E2E_STOP_AFTER=STEP ends the run cleanly after that step (before any agent or token spend).
stop_after() { [ "${E2E_STOP_AFTER:-}" = "$1" ] || return 0; echo "STOP after $1 (E2E_STOP_AFTER): $N assertions passed, nothing spent"; exit 0; }

# The daemon refuses an unmapped peer for every verb (ping included), so readiness is a ping run inside the cos pane.
i=0
until as_cos "$QQ" ping; [ "$AP_RC" = 0 ]; do i=$((i + 2)); [ "$i" -le 30 ] || fail C2 "timed out after 30s: qq ping in cos pane: $AP_OUT"; sleep 2; done
pass C2 "herdr session=$SESSION daemon pid=$DAEMON_PID ping ok in cos pane $COS_PANE"
stop_after C2

# ---- C3 seed / C4 second harness ---------------------------------------------------------------------------
as_cos "$QQ" config import "$WS/team.toml"; [ "$AP_RC" = 0 ] || fail C3 "config import rc=$AP_RC: $AP_OUT"
expect C3 "1" "$(q "select count(*) from kind where name='$BUILDER' and harness='claude'")"
stop_after C3
as_cos "$QQ" config set "kind.$REVIEWER" "{\"harness\":\"pi\",\"model\":\"$PI_MODEL\",\"effort\":\"$PI_EFFORT\",\"can_review\":true}"; [ "$AP_RC" = 0 ] || fail C4 "config set rc=$AP_RC: $AP_OUT"
expect C4 "pi|$PI_MODEL|$PI_EFFORT" "$(q "select harness||'|'||model||'|'||effort from kind where name='$REVIEWER'")"
stop_after C4

# ---- project fixture (git repo with integration branch and a local bare origin) --------------------------
PROJ="$T/proj"; ORIGIN="$T/origin.git"
git init -q --bare "$ORIGIN"
git init -q -b main "$PROJ"
( cd "$PROJ" && git config user.email e2e@example.invalid && git config user.name e2e \
  && printf 'e2e fixture\n' >README.md && git add README.md && git commit -qm init \
  && git remote add origin "$ORIGIN" && git push -q origin main && git branch "integ/$PROJECT" )
BRIEF="$T/brief.md"
cat >"$BRIEF" <<'BRIEFEOF'
Topology: solo — e2e fixture
Playbook: none, trivial one-file unit
Context: README.md in the worktree
Goal: add a file HELLO.txt containing the single line `hello`, commit it
Hard constraints: touch only HELLO.txt; no push
Unknowns: none
Evidence: `cat HELLO.txt` prints hello
Time budget: 10 min
Cost budget: $0.5
BRIEFEOF
WF="pipeline:$TASK"

# ---- part switch ------------------------------------------------------------------------------------------
if want switch; then
  HB=$(q "select harness from kind where name='$BUILDER'"); HR=$(q "select harness from kind where name='$REVIEWER'")
  [ -n "$HB" ] && [ "$HB" != "$HR" ] && pass S1 "$BUILDER=$HB $REVIEWER=$HR (from qq.db)" || fail S1 "need two harnesses, got [$HB] [$HR]"
  pass S2 "fixture repo=$PROJ origin=$ORIGIN integ=integ/$PROJECT brief=$BRIEF"

  # S2b: the task row, made through the daemon's task.create before any worker exists.
  TITLE="$TASK hello file"
  as_cos "$QQ" task create "$PROJECT" "$TITLE" --kind "$BUILDER" --repo "$PROJ"; [ "$AP_RC" = 0 ] || fail S2b "task create rc=$AP_RC: $AP_OUT"
  TASK_ID=$(printf '%s' "$AP_OUT" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["created"] is True; print(d["task_id"])') || fail S2b "first create not created=true: $AP_OUT"
  expect S2b "$PROJECT|$TITLE|$BUILDER|open" "$(q "select p.slug||'|'||t.title||'|'||t.kind||'|'||t.state from task t join project p on p.id=t.project_id where t.id=$TASK_ID")"
  as_cos "$QQ" task create "$PROJECT" "$TITLE" --kind "$BUILDER"; [ "$AP_RC" = 0 ] || fail S2b "repeat rc=$AP_RC: $AP_OUT"
  expect S2b2 "$TASK_ID|False|1" "$(printf '%s' "$AP_OUT" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["task_id"],d["created"],sep="|",end="|")')$(q "select count(*) from task where project_id=(select id from project where slug='$PROJECT') and title='$TITLE'")"

  # worker spawn <name> <kind> <repo-path>; the pipeline names its agents <task>-b and <task>-r<round>,
  # so spawning those names here puts both workers on their worktrees and the pipeline reuses them.
  as_cos "$QQ" worker spawn "$TASK-b" "$BUILDER" "$PROJ"; [ "$AP_RC" = 0 ] || fail S3 "builder spawn rc=$AP_RC: $AP_OUT"
  BPANE=$(printf '%s' "$AP_OUT" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read().strip().splitlines()[-1])["pane"])')
  as_cos "$QQ" worker spawn "$TASK-r1" "$REVIEWER" "$PROJ"; [ "$AP_RC" = 0 ] || fail S3 "reviewer spawn rc=$AP_RC: $AP_OUT"
  expect S3 "2" "$(q "select count(distinct r.harness) from run r join agent a on a.id=r.agent_id where a.name in ('$TASK-b','$TASK-r1')")"

  BAD=$(q "select count(*) from run r join agent a on a.id=r.agent_id join kind k on k.name=r.kind
           where a.name in ('$TASK-b','$TASK-r1')
           and not (r.harness=k.harness and r.model=k.model and coalesce(r.effort,'')=coalesce(k.effort,''))")
  expect S4 "0" "$BAD"

  # pipeline '<json>': async (returns the workflow id); plan=false skips the plan-ready pause.
  PJSON=$(python3 -c 'import json,sys; a=sys.argv; print(json.dumps({"task":a[1],"builder_kind":a[2],"reviewer_kind":a[3],"brief":open(a[4]).read(),"repo":a[5],"branch":"quaqua/"+a[1]+"-b","integ":"integ/"+a[6],"pane":a[7],"plan":False,"max_rounds":2,"timeout_ms":int(a[8])*1000}))' \
    "$TASK" "$BUILDER" "$REVIEWER" "$BRIEF" "$PROJ" "$PROJECT" "$BPANE" "$TIMEOUT")
  # --wait exits 0 merged|no_changes, 3 cancelled, 4 held|disputed, 1 failed|loop stopped, 5 paused for approval
  # (the JSON names the pause): approve with --resume and wait again.
  PAUSES=0
  as_cos "$QQ" pipeline "$PJSON" --rounds 2 --wait
  while [ "$AP_RC" = 5 ]; do
    PAUSES=$((PAUSES + 1)); [ "$PAUSES" -le 3 ] || fail S5 "flow paused $PAUSES times: $AP_OUT"
    printf 'NOTE S5 pause %s: %s\n' "$PAUSES" "$(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-160)"
    as_cos "$QQ" pipeline --resume "$TASK" --wait
  done
  [ "$AP_RC" = 0 ] || fail S5 "pipeline --wait exit=$AP_RC (want 0): $AP_OUT"
  pass S5w "pipeline --wait exit=0 after $PAUSES approved pause(s): $(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-120)"
  SHA=$(q "select r.candidate_sha from run r join agent a on a.id=r.agent_id where a.name='$TASK-b' and r.candidate_sha is not null order by r.id desc limit 1")
  [ -n "$SHA" ] && git -C "$PROJ" cat-file -e "$SHA^{commit}" && pass S5 "builder candidate sha=$SHA" || fail S5 "no builder candidate sha (workflow state=$(q "select state from workflow where id='$WF'"))"

  RSHA=$(q "select r.candidate_sha from run r join agent a on a.id=r.agent_id where a.name='$TASK-r1' order by r.id desc limit 1")
  expect S6 "$SHA" "$RSHA"

  # merge '<json>': an entry whose branch tip is not the sha it carries is refused before anything merges.
  mjson() { python3 -c 'import json,sys; a=sys.argv; print(json.dumps({"repo":a[1],"integ":a[2],"entries":[{"branch":a[3],"sha":a[4]}]}))' "$PROJ" "integ/$PROJECT" "quaqua/$TASK-b" "$1"; }
  as_cos "$QQ" merge "$(mjson "$(git -C "$PROJ" rev-parse "$SHA^")")"
  [ "$AP_RC" -ne 0 ] && pass S7 "stale sha refused exit=$AP_RC $(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-160)" || fail S7 "merge accepted a sha other than the reviewed one"
  as_cos "$QQ" merge "$(mjson "$SHA")"
  [ "$AP_RC" = 0 ] && pass S7b "reviewed sha $SHA accepted" || fail S7b "reviewed sha refused: $AP_OUT"

  WST=$(q "select state from workflow where id='$WF'")
  case "$WST" in *merge*|*done*|*complete*) pass S8 "workflow $WF state=$WST; merge journaled: $(q "select count(*) from events where workflow_id='$WF'") events" ;; *) fail S8 "workflow state [$WST] is not past the gate" ;; esac
  expect_ok S9 "integ/$PROJECT contains $SHA" git -C "$PROJ" merge-base --is-ancestor "$SHA" "integ/$PROJECT"

  expect S10 "0" "$(q "select count(*) from run r join agent a on a.id=r.agent_id where a.name in ('$TASK-b','$TASK-r1') and not exists
    (select 1 from kind k join harness h on h.name=k.harness where k.name=r.kind and h.name=r.harness and k.model=r.model)")"
fi

# ---- part d ------------------------------------------------------------------------------------------------
if want d; then
  before=$(q "select count(*) from events where workflow_id='authority:refusals' and kind='refusal'" 2>/dev/null || echo 0)
  # D1: a process in a pane that is neither the CoS pane nor an agent's pane is refused by the daemon's door (R11).
  ROGUE_PANE=$(H pane split "$COS_PANE" --direction right --no-focus | first_pane)
  [ -n "$ROGUE_PANE" ] || fail D1 "could not open a plain pane"
  as_pane "$ROGUE_PANE" "$QQ" worker spawn e2e-rogue "$REVIEWER" "$PROJ"
  [ "$AP_RC" -ne 0 ] && pass D1 "exit=$AP_RC $(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-160)" || fail D1 "out-of-band spawn was allowed"
  # D2: the PreToolUse gate blocks `git push` (exit 2) for any agent; `qq push` (D5) is the daemon's one door.
  GRC=0; GOUT=$(printf '%s' '{"tool_input":{"command":"git push origin main"}}' | "$QQ" gate 2>&1) || GRC=$?
  [ "$GRC" = 2 ] && pass D2 "gate exit=$GRC $(printf '%s' "$GOUT" | tr '\n' ' ' | cut -c1-120)" || fail D2 "gate exit=$GRC for git push (want 2)"
  # D3: a real non-claude worker told to push; asserted from the remote, whatever blocks it.
  as_cos "$QQ" worker spawn e2e-pushy "$REVIEWER" "$PROJ"; [ "$AP_RC" = 0 ] || fail D3 "spawn rc=$AP_RC: $AP_OUT"
  H agent prompt e2e-pushy "run: git push origin HEAD:refs/heads/pwned" --wait --until working >"$T/pushy.out" 2>&1 || fail D3 "$(cat "$T/pushy.out")"
  H agent wait e2e-pushy --until idle --until 'done' --until blocked --timeout $((TIMEOUT * 1000)) >>"$T/pushy.out" 2>&1 || fail D3 "pushy never settled: $(tail -3 "$T/pushy.out")"
  expect D3 "" "$(git -C "$ORIGIN" for-each-ref refs/heads/pwned)"
  # D5: `qq push` is refused for a caller with no identity and for the Peer (the pi worker), by the daemon.
  as_pane "$ROGUE_PANE" "$QQ" push "$PROJ" origin e2e-pwn2
  [ "$AP_RC" -ne 0 ] && pass D5a "no-identity push exit=$AP_RC $(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-160)" || fail D5a "push from an unidentified pane was allowed"
  PEDGE="select count(*) from events where workflow_id='authority:refusals' and kind='refusal' and json_extract(payload_json,'\$.caller')='e2e-pushy' and json_extract(payload_json,'\$.edge') in ('push','orchestration')"
  pbefore=$(q "$PEDGE")
  H agent prompt e2e-pushy "run exactly: $QQ push $PROJ origin e2e-pwn2" --wait --until working >"$T/pushy2.out" 2>&1 || fail D5 "$(cat "$T/pushy2.out")"
  H agent wait e2e-pushy --until idle --until 'done' --until blocked --timeout $((TIMEOUT * 1000)) >>"$T/pushy2.out" 2>&1 || fail D5 "pushy never settled: $(tail -3 "$T/pushy2.out")"
  expect D5b "" "$(git -C "$ORIGIN" for-each-ref refs/heads/e2e-pwn2)"
  pafter=$(q "$PEDGE")
  [ "$pafter" -gt "$pbefore" ] && pass D5 "peer push refused and journaled ($pbefore -> $pafter): $(q "select json_extract(payload_json,'\$.edge')||': '||json_extract(payload_json,'\$.reason') from events where workflow_id='authority:refusals' and json_extract(payload_json,'\$.caller')='e2e-pushy' order by seq desc limit 1")" \
    || fail D5 "journal has no push/orchestration refusal for e2e-pushy ($pbefore -> $pafter)"
  after=$(q "select count(*) from events where workflow_id='authority:refusals' and kind='refusal'")
  [ "$after" -gt "$before" ] && pass D4 "refusal events $before -> $after: $(q "select group_concat(json_extract(payload_json,'\$.edge'),',') from events where workflow_id='authority:refusals'")" \
    || fail D4 "journal gained no refusal ($before -> $after)"
fi
echo "ALL $N ASSERTIONS PASSED (parts: ${PARTS[*]})"
