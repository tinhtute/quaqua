#!/usr/bin/env bash
# T46 E2E: the switch signal (part `switch`) and clause (d) (part `d`) on the new binary; T58 adds the factory
# clauses (a), (b), (g) and (h) as parts `a`, `b`, `g` and `h`; T64c adds the outer-loop clauses (c), (e) and (f) as
# parts `c`, `e` and `f`.
# Spends REAL tokens on two models. Run only with anh iu's approval (T48); use --dry-run to read the plan.
# shellcheck disable=SC2015  # `A && pass || fail` is intended: fail exits, pass cannot fail
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: full_pipeline.sh [--dry-run] [--check] [-h|--help] [part...]      parts: switch | a | b | g | h | d | c | e | f (default: all)

  switch  one real task on the new binary: brief -> fanout to 2 harnesses -> cross-harness
          review bound to a candidate sha -> gate -> merge into the project integration branch;
          every kind/harness/model/effort is checked against qq.db.
  a       clause (a): a harness is added with `config set` only (no rebuild); `config export` round-trips.
  b       clause (b): 2 dependent tasks run in DAG order; an off-topology spawn and a Peer-to-Peer mail are
          refused and journaled; a worker answers a symbol query through the read-only code lookup.
  g       clause (g): `run list` and `run show` print events, transcript, tool calls, tokens, cost and
          candidate sha for every worker run of the project (run after b, or after any task.start worker).
  h       clause (h): the project's WORKSPACE_PROTOCOL.md sets a worker cap of N; spawn N+1 is refused.
  d       a non-claude worker's `git push`, an out-of-band spawn and a Peer's `quaqua push` are all refused.
  c       clause (c): `quaqua supervisor score --scorer $E2E_SCORER` scores the ended runs (run after switch or b); a
          loop-2 proposal on a rule or skill surface is high risk and `backlog list open` still lists it open.
  e       clause (e): a pipeline paused on a failing check; `source pull` files a loop-1 bug proposal; the CoS
          approves it (`backlog decide <id> approve --note --kind`) with a recorded verdict and it becomes a task.
  f       clause (f): an inbox/ideas/<name>.md file; `source pull` files a loop-3 feature proposal; it is high
          risk and stays open in `backlog list open`.
  --dry-run   print the planned steps; start no herdr, no daemon, no agent, spend nothing.
  --check     resolve every quaqua verb and flag this script calls against the binary (usage probes in a
              temp workspace); start no herdr, no daemon, no agent, spend nothing. Exit 1 on a miss.

env: QQ (binary, default target/debug/quaqua)  E2E_SCORER (part c: the scorer program `quaqua supervisor score
     --scorer` runs, prompt on stdin, JSON {"score":0|1|2,"quote":".."} on stdout; required when part c runs for real,
     not for --check or --dry-run)  E2E_PI_MODEL (model of the pi kind added with `config set`,
     required for a real run)  E2E_PI_EFFORT (default medium)  E2E_TIMEOUT (seconds per wait, default 900; 120 under E2E_STUB=1)
     E2E_BUILDER_KIND / E2E_REVIEWER_KIND (default sude / pie; must differ in model, the harness may match.
                     A reviewer kind already in the roster, e.g. suhuynh, is used as it is and needs no E2E_PI_MODEL
                     when only part switch runs: that is the claude-only run)
     E2E_LOOKUP_TOOL_RE (clause b: extended regex a code-lookup tool name must match in `run show`, default
                     'symbol', which matches find_symbol / find_referencing_symbols of the language-server MCP)
     E2E_STUB=1 (default off, test-only: runs the whole `switch` flow arm -> prompt -> report -> review -> gate ->
                     merge on STUB harnesses: an executable named after each kind's harness, first on PATH of the herdr
                     server, that reads one prompt, commits HELLO.txt (builder) or says `Verdict: clean` (reviewer) and
                     writes the report. No model, no tokens; E2E_PI_MODEL is not needed; part `d` is skipped.)
     E2E_STUB_NOLAND=1 (with E2E_STUB=1, test-only: the stub never reads a prompt, so none lands; the repro for a wait that
                     ends without a landed prompt)
     E2E_STOP_AFTER (C2 | C3 | C4 | S2b: end cleanly after that step; starts no agent, spends no tokens.
                     S2d: the pipeline places the builder (pane, worktree, agent row; starts ONE real builder-kind
                     agent, sends it NO prompt, starts no flow), asserts that, and ends; needs E2E_PI_MODEL)
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
    switch | a | b | g | h | d | c | e | f) PARTS+=("$a") ;;
    *) usage >&2; exit 2 ;;
  esac
done
case "${E2E_STOP_AFTER:-}" in "" | C2 | C3 | C4 | S2b | S2d) ;; *) echo "unknown E2E_STOP_AFTER '${E2E_STOP_AFTER}' (C2 | C3 | C4 | S2b | S2d)" >&2; exit 2 ;; esac
[ "${#PARTS[@]}" -gt 0 ] || PARTS=(switch a b g c e f h d)
STUB=${E2E_STUB:-0}
if [ "$STUB" = 1 ]; then PARTS=(switch); fi   # the stub flow never starts a real worker, so part d (real pi worker) is out
want() { local p; for p in "${PARTS[@]}"; do [ "$p" = "$1" ] && return 0; done; return 1; }

REPO_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
QQ=${QQ:-$REPO_ROOT/target/debug/quaqua}
# every pane call cds to the workspace copy first, so QQ must be absolute: resolve a relative path now, or refuse
case "$QQ" in
  /*) ;;
  */*) qq_dir=$(cd "$(dirname "$QQ")" 2>/dev/null && pwd) || { echo "QQ='$QQ' is relative and its folder does not exist; pass an absolute path, e.g. QQ=\$PWD/$QQ" >&2; exit 2; }
       QQ=$qq_dir/$(basename "$QQ") ;;
  *) qq_abs=$(command -v "$QQ") && case "$qq_abs" in /*) QQ=$qq_abs ;; *) qq_abs= ;; esac
     [ -n "${qq_abs:-}" ] || { echo "QQ='$QQ' is not an absolute path or a command on PATH; pass an absolute path, e.g. QQ=\$PWD/target/release/quaqua" >&2; exit 2; } ;;
esac
PI_MODEL=${E2E_PI_MODEL:-}
[ "$STUB" = 1 ] && [ -z "$PI_MODEL" ] && PI_MODEL=stub
PI_EFFORT=${E2E_PI_EFFORT:-medium}
if [ "$STUB" = 1 ]; then TIMEOUT=${E2E_TIMEOUT:-120}; else TIMEOUT=${E2E_TIMEOUT:-900}; fi   # the stub flow takes about a minute: a stuck wait is not worth 900 s
BUILDER=${E2E_BUILDER_KIND:-sude}
REVIEWER=${E2E_REVIEWER_KIND:-pie}
PROJECT=e2e
TASK=${E2E_TASK:-$BUILDER-e2e1}   # `worker spawn` names are <kind>-<task>, and the pipeline names its builder <task>-b, so the task id starts with the builder kind; the pipeline's task id (a string); its task row is made with `quaqua task create` in S2b
N=0

# ---- assertion helpers: one assertion per step, evidence on every pass, first failure exits --------
# a pipeline pause that says the prompt did not land is a failure, not an approval to resume: resuming only waits again
fail_if_not_landed() { case "$2" in *"did not land"*) fail "$1" "prompt did not land: $2" ;; esac; }
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
  C2 session        private herdr session (own XDG_CONFIG_HOME), \`quaqua daemon\` on the copy, \`quaqua ping\`;
                    a pane tagged grid_role=cos is the only door for CoS calls (identity = herdr pane)
  C3 seed           \`quaqua config import team.toml\`; roster has the claude builder kind
  C4 second harness \`quaqua config set kind.$REVIEWER '{harness:pi,model:\$E2E_PI_MODEL,...}'\` (live roster is claude-only); skipped when $REVIEWER is a roster reviewer kind and no E2E_PI_MODEL is set
part switch
  S1 sqlite roster  kind rows for $BUILDER and $REVIEWER in qq.db name two different models
  S2 fixture        git repo + local bare origin + integration branch integ/$PROJECT, brief file
  S2b task row      \`quaqua task create $PROJECT '<title>' --kind $BUILDER --repo <repo>\` writes a task row (project, title,
                    kind, state open); the same call again returns that row (created=false)
  S2c authority     SQLite holds the active cos agent row (made by the daemon on the first call from the cos pane)
                    and the cos -> peer spawn edge
  S2d placement     (E2E_STOP_AFTER=S2d) \`pipeline '<json>'\` with place_only=true from the cos pane (no 'pane' given): the
                    builder has an agent row, its own pane (not the cos pane) and worktree; no flow, no prompt sent
  S3 placement      the pipeline gave its builder and reviewer their own agent rows and panes (nothing is pre-spawned)
  S4 sqlite spawn   after the pipeline: its agent rows name two models, each equal to its kind row (config read from SQLite)
  S5 build          \`pipeline '<json>' --wait\` (plan=false) returns exit 0 (merged); exit 5 = paused for approval ->
                    \`pipeline --resume $TASK --wait\` and wait again (at most 3 pauses); builder reports a candidate sha
  S6 review         reviewer on another model reviews; its run row carries that same candidate sha
  S7 sha binding    \`merge '<json>'\` refuses a sha other than the reviewed one, accepts the reviewed sha
  S8 gate           the pipeline workflow finished and its journal reached the merge
  S9 merge          the integration branch contains the candidate sha
  S10 sqlite config no run of the task names a kind/harness/model/effort absent from qq.db
part d
  D1 spawn fence    \`worker spawn\` from a pane with no agent identity is refused
  D2 push fence     \`quaqua gate\` blocks \`git push\` (PreToolUse hook, exit 2)
  D3 worker push    a pi worker told to run \`git push\` leaves the bare origin without the ref
  D4 journal        qq.db journal holds a refusal event (authority:refusals) after D1
  D5 quaqua push fence  \`quaqua push\` from a pane with no agent identity is refused; the same \`quaqua push\` run by the pi
                    worker (a Peer) leaves the bare origin without the ref and journals a refusal on edge
                    push/orchestration naming that worker
part a (clause a)
  A1 config set    \`config set kind.e2e-extra '{harness:pi,...}'\` writes the kind row to qq.db; the roster names it
  A2 no rebuild    the binary's checksum is the one read at preflight
  A3 export        \`config export\` holds [kinds.e2e-extra]; \`config validate\` accepts the dump
  A4 round trip    \`config import\` of the dump, then \`config export\` again, is byte-identical; \`config diff\` of the dump
                    prints 'no differences'
part b (clause b)
  B1 DAG refusal   \`task create --parent\` makes a child; \`call task.start\` on the child while the parent is open is
                    refused (parents_unverified) and journaled on edge start
  B2 DAG order     the parent worker reaches verified, then the child starts and reaches verified; child run
                    started_ms >= parent run ended_ms
  B3 symbol query  \`run show\` of the child lists a lookup tool call (E2E_LOOKUP_TOOL_RE) that returned the symbol
  B4 off topology  a pi Peer told to \`worker spawn\` is refused: a refusal on edge spawn names that Peer
  B5 peer mail     the same Peer told to \`mail send\` to another Peer is refused: a refusal on edge mail, Peer-to-Peer
part g (clause g)
  G1 per run       for every row of \`run list --project\`, \`run show\` prints events, transcript, tool calls, tokens
                    (numbers or unknown), cost ($ or unknown) and candidate_sha
  G2 sha           at least one run carries a 40-hex candidate sha
part h (clause h, runs after d)
  H1 cap N         WORKSPACE_PROTOCOL.md committed on the repo's default branch sets worker_cap = active Peers + 1; a
                    spawn up to the cap is allowed
  H2 N+1 refused   the next \`worker spawn\` exits non-zero and creates no agent; a journal row is printed when present
part c (clause c)
  C5 score         \`quaqua supervisor score --scorer \$E2E_SCORER\` scores the ended runs: scored N, low >= 1, proposal ids
  C6 high rule/skill  \`quaqua backlog list open\` lists a loop-2 row, risk high, surface rule or skill, status open; no verdict
                    is recorded for it (it waits for the owner)
part e (clause e)
  E1 failing check a pipeline whose check fails (pipeline check=false: red on the base too) pauses with exit 5 and a journaled
                    pause_for_approval 'blocked 4: base_broken'
  E2 pull          \`quaqua source pull\` files a loop-1 bug proposal naming that workflow; \`backlog list open\` shows it, risk low
  E3 approve       \`quaqua backlog decide <id> approve --note T --kind $BUILDER\` as the CoS: output 'proposal <id> approved,
                    task <n>'; qq.db has a cos approve decision with the note, and an open task of kind $BUILDER from that proposal
part f (clause f)
  F1 idea file     inbox/ideas/<name>.md in the workspace, \`quaqua source pull\` files a loop-3 proposal for it
  F2 stays open    \`backlog list open\` shows that row with risk high; a second pull files nothing new and the row is still open
                    with no decision recorded
cleanup (trap)      stop daemon, stop the herdr session, trash the temp dir
PLAN
}

# ---- --check: every command and flag this script calls, resolved against the binary ---------------------
# Probes are usage errors (no daemon, no herdr, nothing started); the workspace is a throwaway temp dir.
# A verb is "found" when it is in the usage list; each flag/token must appear in that verb's list line or in
# the usage text its bad-argument probe prints.
check_cmds() {
  local tmp list line probe miss=0 verb tok spec TMO
  TMO=$(command -v timeout || command -v gtimeout || true)   # macOS has neither by default: the probes are quick usage errors
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
    "task|create|task create --kind --disposition --repo --parent"
    "config|-|export diff validate"
    "run|-|show list --project"
    "call|-|call"
    "push|-|push <repo> <remote> <branch>"
    "merge|-|merge"
    "supervisor|score --scorer|score --scorer"
    "source|-|pull"
    "backlog|decide|list decide --note --kind"
  )
  for spec in "${specs[@]}"; do
    IFS='|' read -r verb probe tok <<<"$spec"
    line=$(printf '%s\n' "$list" | grep -E "^  $verb( |$)" || true)
    if [ -z "$line" ]; then printf 'MISSING verb %-9s not in quaqua usage list\n' "$verb"; miss=1; continue; fi
    local out=""
    if [ "$probe" != "-" ] && [ "$verb" != "daemon" ]; then
      # shellcheck disable=SC2086  # probe words are split on purpose
      out=$(env -u HERDR_SOCKET_PATH -u HERDR_PANE_ID -u QQ_AGENT QUAQUA_HOME="$tmp" QUAQUA_ROOT="$tmp" ${TMO:+"$TMO" 10} "$QQ" $verb $probe 2>&1 || true)
    fi
    local text="$line"$'\n'"$out" bad=""
    if [ "$tok" != "-" ]; then
      for t in $tok; do printf '%s' "$text" | grep -qF -- "$t" || bad="$bad $t"; done
    fi
    if [ -n "$bad" ]; then printf 'MISSING %-9s token(s) not in usage:%s\n' "$verb $probe" "$bad"; miss=1
    else printf 'FOUND   %-9s %s\n' "$verb" "$([ "$tok" = "-" ] && echo "verb in usage list" || echo "tokens ok: $tok")"; fi
  done
  # verbs with a JSON argument: the shape must be refused as bad usage, not as unknown verb
  for spec in "mail send --to --kind --body-file --body" "grid reconcile --only --tab" "lead spawn --task"; do
    # shellcheck disable=SC2086  # spec words are split on purpose
    set -- $spec
    verb=$1; shift
    line=$(printf '%s\n' "$list" | grep -E "^  $verb( |$)" || true)
    [ -n "$line" ] || { printf 'MISSING verb %-9s not in quaqua usage list\n' "$verb"; miss=1; continue; }
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
if [ -z "$PI_MODEL" ] && ! { [ -n "${E2E_REVIEWER_KIND:-}" ] && [ "${PARTS[*]}" = switch ]; }; then
  echo "set E2E_PI_MODEL (model for the pi kind); a claude-only run needs E2E_REVIEWER_KIND=<roster kind> and part switch only" >&2; exit 1
fi
check_cmds >/dev/null || { echo "quaqua lacks a verb or flag this script calls; run with --check" >&2; exit 1; }
if want c; then   # part c scores with a model: refuse before anything starts when the scorer program is not named
  [ -n "${E2E_SCORER:-}" ] || { echo "part c needs E2E_SCORER: the scorer program for 'quaqua supervisor score --scorer' (prompt on stdin, JSON {\"score\":0|1|2,\"quote\":\"..\"} on stdout); set it or leave part c out" >&2; exit 1; }
  [ -x "$E2E_SCORER" ] || { echo "E2E_SCORER=$E2E_SCORER is not an executable file" >&2; exit 1; }
fi
QQ_SUM0=$(cksum <"$QQ")   # clause a: the binary must be the same file after `config set` (no rebuild)

# ---- C1 isolate -----------------------------------------------------------------------------------------
LIVE=$(cd ~/quaqua && pwd -P)
T=$(mktemp -d /tmp/qqe2e.XXXXXX)      # /tmp: unix socket paths are short
WS=$(cd "$T" && pwd -P)/ws
case "$WS/" in "$LIVE"/*) echo "refusing: workspace $WS is inside $LIVE" >&2; exit 1 ;; esac
SESSION="qqe2e-$$-$RANDOM"
export HERDR_SESSION="$SESSION"   # herdr server, daemon (its pane lookup) and every quaqua call share this session
export XDG_CONFIG_HOME="$T/cfg"
mkdir -p "$XDG_CONFIG_HOME" "$WS" "$T/calls"
unset HERDR_SOCKET_PATH HERDR_ENV HERDR_PANE_ID HERDR_WORKSPACE_ID HERDR_TAB_ID HERDR_AGENT QQ_AGENT
export QUAQUA_HOME="$WS" QUAQUA_ROOT="$WS"
H() { herdr --session "$SESSION" "$@"; }

DAEMON_PID=""; SERVER_PID=""; cleaned=0
cleanup() {
  [ "$cleaned" -eq 0 ] || return 0
  cleaned=1; set +e
  if [ -n "${E2E_DUMP:-}" ]; then   # debug: what each agent pane shows at exit
    for a in $(H agent list 2>/dev/null | grep -o '"name": *"[^"]*"' | cut -d'"' -f4); do
      echo "--- pane $a"; H agent read "$a" --source recent-unwrapped --lines 25 2>&1 | tail -25
    done
  fi
  # E2E_KEEP=<dir>: copy the run records (events db, reports, daemon/herdr logs) out before the temp dir is trashed.
  if [ -n "${E2E_KEEP:-}" ]; then
    mkdir -p "$E2E_KEEP" && cp -r "$WS/runs" "$E2E_KEEP/" 2>/dev/null; cp "$T"/*.log "$T"/cfg/herdr/sessions/*/herdr-server.log "$E2E_KEEP/" 2>/dev/null
    for p in $(H pane list 2>/dev/null | grep -o '"pane_id": *"[^"]*"' | cut -d'"' -f4); do
      { echo "--- pane $p"; H pane read "$p" --source visible --lines 40 2>&1 | tail -40; } >>"$E2E_KEEP/panes.txt"
    done
  fi
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
if [ "$STUB" = 1 ]; then
  # Stub harnesses: one executable per harness name. herdr starts the executable named by the kind, so a stub first
  # on PATH stands in for the real CLI. It spends nothing: it only commits a file or writes a fixed report.
  mkdir -p "$T/stubbin"
  cat >"$T/stubbin/stub" <<'STUBEOF'
#!/usr/bin/env bash
# stub harness: one prompt in, one report out. Phase and report path come from the pipeline's own prompt text.
# As `claude` it boots like the real one: in a folder it has not been trusted in it shows the trust dialog until it
# reads Down+Enter, then a short boot window drops any typed input, and only then does it take a prompt. (The pi stub
# has no dialog and no boot window.)
if [ "$(basename "$0")" = claude ]; then
  trusted="${STUB_TRUST_DIR:?}/$(printf '%s' "$PWD" | cksum | cut -d' ' -f1)"
  if [ ! -e "$trusted" ]; then
    printf 'Quick safety check: Is this a project you created or one you trust?\n  1. Yes, I trust this folder\n  2. No, exit\n'
    while IFS= read -r line; do case "$line" in *$'\e[B'*) break ;; esac; done
    : >"$trusted"
    printf '\033[2J\033[H'
    # bash 3.2 (macOS /bin/bash) has no EPOCHREALTIME and only whole-second read -t: count whole seconds with SECONDS,
    # rounded up plus one so the window is never shorter than STUB_BOOT_MS
    end=$(( SECONDS + ${STUB_BOOT_MS:-1500} / 1000 + 2 ))
    while [ "$SECONDS" -lt "$end" ]; do IFS= read -r -t 1 _ || true; done   # typed during boot: dropped
  fi
fi
printf '? for shortcuts\n> '
[ -z "${STUB_NOLAND:-}" ] || sleep 3600   # E2E_STUB_NOLAND=1: draws the prompt, never reads one, so no prompt lands
prompt=""
while IFS= read -r line; do
  prompt="$prompt$line"$'\n'
  # herdr's claude detection reads a `✻ Working…` line in the bottom 12 lines as a live turn, so
  # draw it after the echoed prompt, not before (a long echo would push it out of the window)
  case "$prompt" in *"Write your report to "*)
    # claude reads `✻ Working…`; pi reads a `Working...` line (herdr's per-harness manifests)
    if [ "$(basename "$0")" = claude ]; then printf '\n✻ Working…\n'; else printf '\nWorking...\n'; fi
    sleep 3; break ;; esac
done
report=$(printf '%s' "$prompt" | grep -o 'report to [^ ]*\.md' | head -1 | sed 's/^report to //')
case "$prompt" in
  *"Pipeline phase: BUILD"*)
    printf 'hello\n' >HELLO.txt && git add HELLO.txt && git -c user.email=stub@example.invalid -c user.name=stub commit -qm "stub: add HELLO.txt"
    sha=$(git rev-parse HEAD)
    printf 'Outcome: COMPLETE\nVerified: unit-test-verified %s\n\n## Blockers\nnone\n\n## Lessons\nnone\n' "$sha" >"$report.tmp" ;;
  *) printf '## Findings\nnone\n\nVerdict: clean\n' >"$report.tmp" ;;
esac
mv "$report.tmp" "$report"
printf '\033[2J\033[H? for shortcuts\n> '   # turn over: back to the idle screen
sleep 3600
STUBEOF
  chmod +x "$T/stubbin/stub"
  for h in claude pi; do ln -sf stub "$T/stubbin/$h"; done
  export PATH="$T/stubbin:$PATH"
  export STUB_NOLAND=${E2E_STUB_NOLAND:-}
  mkdir -p "$T/stubtrust"; export STUB_TRUST_DIR="$T/stubtrust"
  # A pane shell that sources the user's rc puts the real CLIs back first on PATH; an empty ZDOTDIR keeps it as is.
  mkdir -p "$T/zdot"; echo "# empty: no user rc" >"$T/zdot/.zshrc"; export ZDOTDIR="$T/zdot"
fi
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
stop_after() { [ "${E2E_STOP_AFTER:-}" = "$1" ] || return 0; echo "STOP after $1 (E2E_STOP_AFTER): $N assertions passed, no prompt sent"; exit 0; }

# The daemon refuses an unmapped peer for every verb (ping included), so readiness is a ping run inside the cos pane.
i=0
until as_cos "$QQ" ping; [ "$AP_RC" = 0 ]; do i=$((i + 2)); [ "$i" -le 30 ] || fail C2 "timed out after 30s: quaqua ping in cos pane: $AP_OUT"; sleep 2; done
pass C2 "herdr session=$SESSION daemon pid=$DAEMON_PID ping ok in cos pane $COS_PANE"
if [ "$STUB" = 1 ]; then
  # The $0 guarantee: what a pane shell resolves for each harness must be the stub, checked before any prompt.
  for h in claude pi; do
    as_cos "${SHELL:-sh}" -ic "command -v $h"; [ "$AP_RC" = 0 ] || fail C2s "no $h in the cos pane shell: $AP_OUT"
    case "$(printf '%s' "$AP_OUT" | tail -1)" in "$T"/stubbin/*) pass C2s "pane shell resolves $h to the stub: $(printf '%s' "$AP_OUT" | tail -1)" ;; *) fail C2s "pane shell resolves $h to [$(printf '%s' "$AP_OUT" | tail -1)], not the stub: a real model would run" ;; esac
  done
fi
stop_after C2

# ---- C3 seed / C4 second harness ---------------------------------------------------------------------------
as_cos "$QQ" config import "$WS/team.toml"; [ "$AP_RC" = 0 ] || fail C3 "config import rc=$AP_RC: $AP_OUT"
expect C3 "1" "$(q "select count(*) from kind where name='$BUILDER' and harness='claude'")"
stop_after C3
PI0=$(q "select count(*) from kind where harness='pi'")   # clause (a): pi is absent from the roster before this one config set
if [ "$(q "select count(*) from kind where name='$REVIEWER' and can_review=1")" = 1 ] && [ -z "$PI_MODEL" ]; then
  pass C4 "reviewer kind $REVIEWER is in the roster ($(q "select harness||'|'||model from kind where name='$REVIEWER'")); no second harness added"
else
  as_cos "$QQ" config set "kind.$REVIEWER" "{\"harness\":\"pi\",\"model\":\"$PI_MODEL\",\"effort\":\"$PI_EFFORT\",\"can_review\":true}"; [ "$AP_RC" = 0 ] || fail C4 "config set rc=$AP_RC: $AP_OUT"
  expect C4 "pi|$PI_MODEL|$PI_EFFORT" "$(q "select harness||'|'||model||'|'||effort from kind where name='$REVIEWER'")"
fi
stop_after C4

# ---- project fixture (git repo with integration branch and a local bare origin) --------------------------
TREAL=$(cd "$T" && pwd -P)   # the engine canonicalizes repo paths; /tmp is a symlink on macOS
PROJ="$TREAL/proj"; ORIGIN="$TREAL/origin.git"
git init -q --bare "$ORIGIN"
git init -q -b main "$PROJ"
( cd "$PROJ" && git config user.email e2e@example.invalid && git config user.name e2e \
  && printf 'e2e fixture\n' >README.md && printf 'def e2e_answer_symbol():\n    return 42\n' >answers.py \
  && git add README.md answers.py && git commit -qm init \
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
  MB=$(q "select model from kind where name='$BUILDER'"); MR=$(q "select model from kind where name='$REVIEWER'")
  [ -n "$MB" ] && [ "$(printf %s "$MB" | tr 'A-Z' 'a-z')" != "$(printf %s "$MR" | tr 'A-Z' 'a-z')" ] && pass S1 "$BUILDER=$MB $REVIEWER=$MR (from qq.db)" || fail S1 "need two models, got [$MB] [$MR]"
  pass S2 "fixture repo=$PROJ origin=$ORIGIN integ=integ/$PROJECT brief=$BRIEF"

  # S2b: the task row, made through the daemon's task.create before any worker exists.
  TITLE="$TASK hello file"
  as_cos "$QQ" task create "$PROJECT" "$TITLE" --kind "$BUILDER" --repo "$PROJ"; [ "$AP_RC" = 0 ] || fail S2b "task create rc=$AP_RC: $AP_OUT"
  TASK_ID=$(printf '%s' "$AP_OUT" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["created"] is True; print(d["task_id"])') || fail S2b "first create not created=true: $AP_OUT"
  PROJECT_ID=$(q "select id from project where slug='$PROJECT'")
  expect S2b "$PROJECT|$TITLE|$BUILDER|open" "$(q "select p.slug||'|'||t.title||'|'||t.kind||'|'||t.state from task t join project p on p.id=t.project_id where t.id=$TASK_ID")"
  as_cos "$QQ" task create "$PROJECT" "$TITLE" --kind "$BUILDER"; [ "$AP_RC" = 0 ] || fail S2b "repeat rc=$AP_RC: $AP_OUT"
  expect S2b2 "$TASK_ID|False|1" "$(printf '%s' "$AP_OUT" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["task_id"],d["created"],sep="|",end="|")')$(q "select count(*) from task where project_id=(select id from project where slug='$PROJECT') and title='$TITLE'")"

  # S2c: the authority rows the first spawn needs, read from SQLite (no spawn): the daemon made the cos agent row on
  # the first call from the cos pane, the config import brought the cos -> peer spawn edge.
  expect S2c "1|1" "$(q "select count(*) from agent where name='cos' and role='cos' and ended_ms is null")|$(q "select count(*) from topology_edge where from_role='cos' and to_role='peer' and edge='spawn'")"
  stop_after S2b

  # The pipeline owns placement: it splits the builder (<task>-b) off the cos pane onto its own worktree and each
  # reviewer (<task>-r<round>) off the builder's pane, so nothing is spawned by hand here.

  # pipeline '<json>': async (returns the workflow id); plan=false skips the plan-ready pause.
  PJSON=$(python3 -c 'import json,sys; a=sys.argv; print(json.dumps({"task":a[1],"builder_kind":a[2],"reviewer_kind":a[3],"brief":open(a[4]).read(),"repo":a[5],"branch":"quaqua/"+a[1]+"-b","integ":"integ/"+a[6],"project_id":int(a[8]),"plan":False,"max_rounds":2,"timeout_ms":int(a[7])*1000}))' \
    "$TASK" "$BUILDER" "$REVIEWER" "$BRIEF" "$PROJ" "$PROJECT" "$TIMEOUT" "$PROJECT_ID")

  # S2d (E2E_STOP_AFTER=S2d): place_only makes the verb place the builder and return before the flow starts, so no
  # prompt is ever sent. It starts one real builder-kind agent (an idle harness at its input prompt).
  if [ "${E2E_STOP_AFTER:-}" = S2d ]; then
    PO=$(printf '%s' "$PJSON" | python3 -c 'import json,sys; d=json.load(sys.stdin); d["place_only"]=True; print(json.dumps(d))')
    as_cos "$QQ" pipeline "$PO"; [ "$AP_RC" = 0 ] || fail S2d "pipeline place_only rc=$AP_RC: $AP_OUT"
    BPANE=$(q "select pane_id from agent where name='$TASK-b' and ended_ms is null")
    [ -n "$BPANE" ] && [ "$BPANE" != "$COS_PANE" ] && pass S2d "builder $TASK-b has pane $BPANE (cos pane $COS_PANE)" || fail S2d "builder pane [$BPANE] vs cos pane [$COS_PANE]"
    expect S2d_wt "quaqua/$TASK-b" "$(git -C "$PROJ" worktree list --porcelain | awk '$1=="branch" && $2 ~ /-b$/ {sub("refs/heads/","",$2); print $2}')"
    expect S2d_noflow "0" "$(q "select count(*) from workflow where id='$WF'")"
    stop_after S2d
  fi
  # --wait exits 0 merged|no_changes, 3 cancelled, 4 held|disputed, 1 failed|loop stopped, 5 paused for approval
  # (the JSON names the pause): approve with --resume and wait again.
  PAUSES=0
  as_cos "$QQ" pipeline "$PJSON" --rounds 2 --wait
  while [ "$AP_RC" = 5 ]; do
    fail_if_not_landed S5 "$AP_OUT"
    PAUSES=$((PAUSES + 1)); [ "$PAUSES" -le 3 ] || fail S5 "flow paused $PAUSES times: $AP_OUT"
    printf 'NOTE S5 pause %s: %s\n' "$PAUSES" "$(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-160)"
    as_cos "$QQ" pipeline --resume "$TASK" --wait
  done
  [ "$AP_RC" = 0 ] || fail S5 "pipeline --wait exit=$AP_RC (want 0): $AP_OUT"
  pass S5w "pipeline --wait exit=0 after $PAUSES approved pause(s): $(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-120)"
  # exit 0 also covers no_changes (nothing built, nothing reviewed): only a merged run counts as the pass.
  expect S5s "merged" "$(q "select json_extract(payload_json,'\$.status') from events where workflow_id='$WF' and kind='workflow_completed'")"
  expect S3 "$BUILDER|$REVIEWER" "$(q "select (select kind from agent where name='$TASK-b')||'|'||(select kind from agent where name='$TASK-r1')")"
  # The pipeline drives its builder and reviewer itself (no `task.start`), so no `run` rows exist for them: the roster
  # check reads the agent rows (kind -> model) against the kind table instead.
  expect S4a "2" "$(q "select count(distinct lower(trim(k.model))) from agent a join kind k on k.name=a.kind where a.name in ('$TASK-b','$TASK-r1')")"
  SHA=$(git -C "$PROJ" rev-parse "quaqua/$TASK-b")
  [ -n "$SHA" ] && git -C "$PROJ" cat-file -e "$SHA^{commit}" && pass S5 "builder branch tip (candidate) sha=$SHA" || fail S5 "no builder commit (workflow state=$(q "select state from workflow where id='$WF'"))"
  expect S6 "1" "$(q "select count(*) from workflow where id='$WF' and state like '%complete%'")"

  # merge '<json>': an entry whose branch tip is not the sha it carries is refused before anything merges.
  mjson() { python3 -c 'import json,sys; a=sys.argv; print(json.dumps({"repo":a[1],"integ":a[2],"entries":[{"branch":a[3],"sha":a[4]}]}))' "$PROJ" "integ/$PROJECT" "quaqua/$TASK-b" "$1"; }
  as_cos "$QQ" merge "$(mjson "$(git -C "$PROJ" rev-parse "$SHA^")")"
  [ "$AP_RC" -ne 0 ] && pass S7 "stale sha refused exit=$AP_RC $(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-160)" || fail S7 "merge accepted a sha other than the reviewed one"
  as_cos "$QQ" merge "$(mjson "$SHA")"
  [ "$AP_RC" = 0 ] && pass S7b "reviewed sha $SHA accepted" || fail S7b "reviewed sha refused: $AP_OUT"

  WST=$(q "select state from workflow where id='$WF'")
  case "$WST" in *merge*|*done*|*complete*) pass S8 "workflow $WF state=$WST; merge journaled: $(q "select count(*) from events where workflow_id='$WF'") events" ;; *) fail S8 "workflow state [$WST] is not past the gate" ;; esac
  expect_ok S9 "integ/$PROJECT contains $SHA" git -C "$PROJ" merge-base --is-ancestor "$SHA" "integ/$PROJECT"

  expect S10 "0" "$(q "select count(*) from agent a where a.name in ('$TASK-b','$TASK-r1') and not exists (select 1 from kind k join harness h on h.name=k.harness where k.name=a.kind)")"
fi

# ---- clauses a, b, g (T58) ---------------------------------------------------------------------------------
ensure_project() {
  [ -n "${PROJECT_ID:-}" ] && return 0
  as_cos "$QQ" task create "$PROJECT" "e2e project anchor" --kind "$BUILDER" --repo "$PROJ"; [ "$AP_RC" = 0 ] || fail "$1" "task create rc=$AP_RC: $AP_OUT"
  PROJECT_ID=$(q "select id from project where slug='$PROJECT'")
  [ -n "$PROJECT_ID" ] || fail "$1" "no project row for $PROJECT"
}
# ask_peer STEP NAME TEXT: type TEXT into a worker pane and wait until it settles.
ask_peer() {
  H agent prompt "$2" "$3" >"$T/$2.out" 2>&1 || fail "$1" "$(cat "$T/$2.out")"
}
refusal_rose() { [ "$(refusals "$1" "$2")" -gt "$3" ]; }   # CALLER EDGE BEFORE
run_closed() { [ -n "$(q "select ended_ms from run where task_id=$1 and ended_ms is not null")" ]; }
refusals() { q "select count(*) from events where workflow_id='authority:refusals' and kind='refusal' and json_extract(payload_json,'\$.caller')='$1' and json_extract(payload_json,'\$.edge')='$2'"; }
last_reason() { q "select json_extract(payload_json,'\$.reason') from events where workflow_id='authority:refusals' and json_extract(payload_json,'\$.caller')='$1' and json_extract(payload_json,'\$.edge')='$2' order by seq desc limit 1"; }
start_json() { # NAME TASK_ID PROMPT REPORT
  python3 -c 'import json,sys; a=sys.argv; print(json.dumps({"name":a[1],"kind":a[2],"role":"peer","task_id":int(a[3]),"project_id":int(a[4]),"prompt":a[5],"report":a[6],"timeout_ms":int(a[7])*1000}))' "$1" "$BUILDER" "$2" "$PROJECT_ID" "$3" "$4" "$TIMEOUT"
}
probe_prompt() { # GOAL REPORT SHA
  printf 'Topology: solo - e2e DAG probe\nPlaybook: none, read-only unit\nContext: repo %s\nGoal: %s\nHard constraints: change no file, no commit, no push\nUnknowns: none\nEvidence: the report\n\nWrite your report to %s with exactly this shape, then stop:\nOutcome: COMPLETE\nVerified: unit-test-verified %s\n\n## Findings\n<what you found, one line>\n\n## Blockers\nnone\n\n## Lessons\nnone\n' "$PROJ" "$1" "$2" "$3"
}
task_verified() { [ "$(q "select state from task where id=$1")" = verified ]; }
task_id_of() { printf '%s' "$1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["task_id"])'; }

# ---- part a: clause (a), a harness added with config set only; export round-trips -----------------------------
if want a; then
  AKIND=e2e-extra
  [ "$PI0" = 0 ] && pass A0 "no pi kind before C4's config set; roster after: $(q "select group_concat(name||'='||harness) from kind")" || fail A0 "pi kinds before C4's config set: $PI0"
  as_cos "$QQ" config set "kind.$AKIND" "{\"harness\":\"pi\",\"model\":\"$PI_MODEL\",\"effort\":\"$PI_EFFORT\",\"can_review\":true}"; [ "$AP_RC" = 0 ] || fail A1 "config set rc=$AP_RC: $AP_OUT"
  expect A1 "pi|$PI_MODEL|$PI_EFFORT" "$(q "select harness||'|'||model||'|'||effort from kind where name='$AKIND'")"
  expect A2 "$QQ_SUM0" "$(cksum <"$QQ")"
  as_cos "$QQ" config export; [ "$AP_RC" = 0 ] || fail A3 "config export rc=$AP_RC: $AP_OUT"
  printf '%s\n' "$AP_OUT" >"$T/export1.toml"
  grep -qF "[kinds.$AKIND]" "$T/export1.toml" && pass A3 "export holds [kinds.$AKIND] ($(wc -l <"$T/export1.toml" | tr -d ' ') lines)" || fail A3 "export lacks [kinds.$AKIND]"
  as_cos "$QQ" config validate "$T/export1.toml"; [ "$AP_RC" = 0 ] && pass A3v "validate: $AP_OUT" || fail A3v "validate rc=$AP_RC: $AP_OUT"
  as_cos "$QQ" config import "$T/export1.toml"; [ "$AP_RC" = 0 ] || fail A4 "config import of the dump rc=$AP_RC: $AP_OUT"
  as_cos "$QQ" config export; [ "$AP_RC" = 0 ] || fail A4 "second config export rc=$AP_RC: $AP_OUT"
  printf '%s\n' "$AP_OUT" >"$T/export2.toml"
  expect A4i "$(cksum <"$T/export1.toml")" "$(cksum <"$T/export2.toml")"
  as_cos "$QQ" config diff "$T/export1.toml"; [ "$AP_RC" = 0 ] || fail A4 "config diff rc=$AP_RC: $AP_OUT"
  expect A4 "no differences" "$AP_OUT"
fi

# ---- part b: clause (b), DAG order, off-topology refusals, symbol query ----------------------------------------
if want b; then
  ensure_project B1
  PN="$BUILDER-dagp"; CN="$BUILDER-dagc"
  as_cos "$QQ" task create "$PROJECT" "dag parent" --kind "$BUILDER" --repo "$PROJ"; [ "$AP_RC" = 0 ] || fail B1 "task create parent rc=$AP_RC: $AP_OUT"
  TP=$(task_id_of "$AP_OUT")
  as_cos "$QQ" task create "$PROJECT" "dag child" --kind "$BUILDER" --repo "$PROJ" --parent "$TP"; [ "$AP_RC" = 0 ] || fail B1 "task create child rc=$AP_RC: $AP_OUT"
  TC=$(task_id_of "$AP_OUT")
  BSHA=$(git -C "$PROJ" rev-parse main)
  PREP="$WS/runs/e2e-dagp.md"; CREP="$WS/runs/e2e-dagc.md"
  PPROMPT=$(probe_prompt "report that the repo is readable (parent of a DAG probe)" "$PREP" "$BSHA")
  CPROMPT=$(probe_prompt "use the read-only code lookup tool (a language-server symbol search) to find where the symbol e2e_answer_symbol is defined; do not read the file with a shell command; put file:line in Findings" "$CREP" "$BSHA")
  as_cos "$QQ" call task.start "$(start_json "$CN" "$TC" "$CPROMPT" "$CREP")"
  [ "$AP_RC" -ne 0 ] && printf '%s' "$AP_OUT" | grep -q 'parents_unverified' \
    && pass B1 "child task $TC refused while parent $TP is open: $(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-160)" \
    || fail B1 "child start was not refused with parents_unverified (rc=$AP_RC): $AP_OUT"
  NSTART=$(q "select count(*) from events where workflow_id='authority:refusals' and kind='refusal' and json_extract(payload_json,'\$.edge')='start'")
  [ "$NSTART" -ge 1 ] && pass B1j "refusal on edge start journaled: $(q "select payload_json from events where workflow_id='authority:refusals' and json_extract(payload_json,'\$.edge')='start' order by seq desc limit 1" | cut -c1-200)" || fail B1j "no refusal on edge start in the journal"
  as_cos "$QQ" call task.start "$(start_json "$PN" "$TP" "$PPROMPT" "$PREP")"; [ "$AP_RC" = 0 ] || fail B2 "parent start rc=$AP_RC: $AP_OUT"
  wait_for B2 "$TIMEOUT" task_verified "$TP"
  wait_for B2 "$TIMEOUT" run_closed "$TP"
  as_cos "$QQ" call task.start "$(start_json "$CN" "$TC" "$CPROMPT" "$CREP")"; [ "$AP_RC" = 0 ] || fail B2 "child start after the parent verified rc=$AP_RC: $AP_OUT"
  wait_for B2 "$TIMEOUT" task_verified "$TC"
  PEND=$(q "select ended_ms from run where task_id=$TP"); CSTART=$(q "select started_ms from run where task_id=$TC")
  [ -n "$PEND" ] && [ -n "$CSTART" ] && [ "$CSTART" -ge "$PEND" ] \
    && pass B2 "parent $TP verified (run ended $PEND), then child $TC started (run started $CSTART)" \
    || fail B2 "child run did not start after the parent run ended (parent ended [$PEND], child started [$CSTART])"
  LRE=${E2E_LOOKUP_TOOL_RE:-symbol}
  SHOW=$("$QQ" run show "worker:$CN" 2>&1) || fail B3 "run show worker:$CN: $SHOW"
  # only the tool-calls section, only a line whose tool name matches, status ok, and the symbol inside its result
  HIT=$(printf '%s\n' "$SHOW" | sed -n '/^tool calls (/,$p' | grep -E "^  [^ ]*($LRE)[^ ]* ok args=.* result=.*e2e_answer_symbol" | head -1) || true
  [ -n "$HIT" ] && pass B3 "$(printf '%s' "$HIT" | cut -c1-200)" || fail B3 "no tool call matching /$LRE/ returned e2e_answer_symbol in run show worker:$CN"
  PA="$REVIEWER-pa"; PB="$REVIEWER-pb"; OFF="$REVIEWER-offtopo"
  as_cos "$QQ" worker spawn "$PA" "$REVIEWER" "$PROJ"; [ "$AP_RC" = 0 ] || fail B4 "spawn $PA rc=$AP_RC: $AP_OUT"
  as_cos "$QQ" worker spawn "$PB" "$REVIEWER" "$PROJ"; [ "$AP_RC" = 0 ] || fail B5 "spawn $PB rc=$AP_RC: $AP_OUT"
  sb=$(refusals "$PA" spawn)
  ask_peer B4 "$PA" "run exactly: $QQ worker spawn $OFF $REVIEWER $PROJ"
  wait_for B4 "$TIMEOUT" refusal_rose "$PA" spawn "$sb"
  sa=$(refusals "$PA" spawn)
  [ "$sa" -gt "$sb" ] && pass B4 "off-topology spawn refused and journaled ($sb -> $sa): $(last_reason "$PA" spawn)" || fail B4 "no spawn refusal journaled for $PA ($sb -> $sa)"
  expect B4n "0" "$(q "select count(*) from agent where name='$OFF'")"
  mb=$(refusals "$PA" mail)
  ask_peer B5 "$PA" "run exactly: $QQ mail send --to $PB --kind note --body hello"
  wait_for B5 "$TIMEOUT" refusal_rose "$PA" mail "$mb"
  ma=$(refusals "$PA" mail)
  [ "$ma" -gt "$mb" ] && pass B5 "Peer-to-Peer mail refused and journaled ($mb -> $ma): $(last_reason "$PA" mail)" || fail B5 "no mail refusal journaled for $PA ($mb -> $ma)"
fi

# ---- part g: clause (g), the CLI prints the whole record of every worker run -------------------------------------
if want g; then
  capture_done() { [ "$(q "select count(*) from run where ended_ms is null")" = 0 ]; }
  wait_for G1 120 capture_done
  RL=$("$QQ" run list --project "$PROJECT" 2>&1) || fail G1 "run list: $RL"
  [ -n "$RL" ] || fail G1 "run list --project $PROJECT printed no run (part b starts two task.start workers)"
  pass G1l "run list: $(printf '%s\n' "$RL" | wc -l | tr -d ' ') run(s)"
  SHA_RUNS=0
  while IFS= read -r line; do
    rid=$(printf '%s' "$line" | awk '{print $1}')
    S=$("$QQ" run show "$rid" 2>&1) || fail G1 "run show $rid: $S"
    for pat in '^events \([0-9]+\)' '^transcript \([0-9]+\)' '^tool calls \([0-9]+\)' '^tokens in=([0-9]+|unknown) out=([0-9]+|unknown)' '^cost (\$[0-9.]+|unknown)' '^candidate_sha ([0-9a-f]{40}|none)'; do
      printf '%s\n' "$S" | grep -Eq "$pat" || fail G1 "run $rid: run show has no line matching $pat"
    done
    sha=$(printf '%s\n' "$S" | sed -n 's/^candidate_sha //p'); [ "$sha" != none ] && SHA_RUNS=$((SHA_RUNS + 1))
    pass G1 "run $rid: $(printf '%s\n' "$S" | grep -E '^(events|transcript|tool calls|tokens|cost|candidate_sha)' | tr '\n' ';' | cut -c1-240)"
  done <<<"$RL"
  [ "$SHA_RUNS" -ge 1 ] && pass G2 "$SHA_RUNS run(s) carry a candidate sha" || fail G2 "no run carries a candidate sha"
fi

# ---- backlog helpers (clauses c, e, f) ------------------------------------------------------------------------
# bl_open STEP: runs `backlog list open` as the CoS (never inside $(...): as_pane counts its calls in this shell); the table
# is in AP_OUT, columns id loop kind risk surface status task, tab separated. row_of ID picks one row from AP_OUT.
bl_open() { as_cos "$QQ" backlog list open; [ "$AP_RC" = 0 ] || fail "$1" "backlog list open rc=$AP_RC: $AP_OUT"; }
row_of() { printf '%s\n' "$AP_OUT" | awk -F'\t' -v id="$1" '$1==id'; }
decisions_of() { q "select count(*) from decision where proposal_id=$1"; }

# ---- part c: clause (c), the Supervisor scores the runs and files a high-risk rule/skill proposal ----------------
if want c; then
  SOUT=$("$QQ" supervisor score --scorer "$E2E_SCORER" 2>&1) || fail C5 "supervisor score rc=$?: $SOUT"
  SLINE=$(printf '%s\n' "$SOUT" | grep -E '^scored [0-9]+ low [0-9]+ proposals \[' | head -1) || true
  [ -n "$SLINE" ] || fail C5 "no 'scored N low M proposals [..]' line: $SOUT"
  SCORED=$(printf '%s' "$SLINE" | awk '{print $2}'); LOW=$(printf '%s' "$SLINE" | awk '{print $4}')
  [ "$SCORED" -ge 1 ] || fail C5 "scored 0 runs: run part switch or b first (a worker run must have ended): $SOUT"
  [ "$LOW" -ge 1 ] || fail C5 "the scorer gave no score of 0, so no proposal was filed: $SOUT"
  pass C5 "$(printf '%s' "$SOUT" | tr '\n' ' ' | cut -c1-200)"
  SIDS=$(printf '%s' "$SLINE" | sed -E 's/^[^[]*\[([^]]*)\].*/\1/' | tr -d ',')
  as_cos "$QQ" backlog list open; [ "$AP_RC" = 0 ] || fail C6 "backlog list open rc=$AP_RC: $AP_OUT"
  CROW=""
  for sid in $SIDS; do
    CROW=$(printf '%s\n' "$AP_OUT" | awk -F'\t' -v id="$sid" '$1==id && $2==2 && $4=="high" && ($5=="rule" || $5=="skill") && $6=="open"' | head -1)
    [ -n "$CROW" ] && break
  done
  [ -n "$CROW" ] || fail C6 "no loop-2 high rule/skill open row among the filed proposals [$SIDS]: $(printf '%s' "$AP_OUT" | tr '\n' ';' | cut -c1-300)"
  CID=$(printf '%s' "$CROW" | cut -f1)
  expect C6d "0" "$(decisions_of "$CID")"
  pass C6 "backlog list open row [$(printf '%s' "$CROW" | tr '\t' ' ')] (id loop kind risk surface status task): loop 2, high, rule/skill, open, no verdict yet"
fi

# ---- part e: clause (e), a failing check becomes a bug proposal; the CoS approves it; it becomes a task ------------
if want e; then
  ensure_project E1
  ET="$BUILDER-e2efail"; EWF="pipeline:$ET"
  EBRIEF="$T/brief-e.md"; sed 's/HELLO\.txt/CHECKFAIL.txt/g' "$BRIEF" >"$EBRIEF"
  # check = "false" fails after the merge and on the integration base too: the flow holds (merge code 4, base_broken)
  # instead of spending a fix round. The pipeline is left paused: that pause is the failure the source reads.
  EJSON=$(python3 -c 'import json,sys; a=sys.argv; print(json.dumps({"task":a[1],"builder_kind":a[2],"reviewer_kind":a[3],"brief":open(a[4]).read(),"repo":a[5],"branch":"quaqua/"+a[1]+"-b","integ":"integ/"+a[6],"project_id":int(a[8]),"plan":False,"max_rounds":2,"check":"false","timeout_ms":int(a[7])*1000}))' \
    "$ET" "$BUILDER" "$REVIEWER" "$EBRIEF" "$PROJ" "$PROJECT" "$TIMEOUT" "$PROJECT_ID")
  EPAUSE="select json_extract(payload_json,'\$.reason') from events where workflow_id='$EWF' and kind='pause_for_approval' order by seq desc limit 1"
  EP=0
  as_cos "$QQ" pipeline "$EJSON" --rounds 2 --wait
  while [ "$AP_RC" = 5 ] && ! printf '%s' "$(q "$EPAUSE")" | grep -q '^blocked 4: base_broken'; do
    fail_if_not_landed E1 "$AP_OUT"
    EP=$((EP + 1)); [ "$EP" -le 3 ] || fail E1 "flow paused $EP times without reaching the failing check: $AP_OUT"
    as_cos "$QQ" pipeline --resume "$ET" --wait
  done
  [ "$AP_RC" = 5 ] || fail E1 "pipeline --wait exit=$AP_RC (want 5, paused on the failing check): $AP_OUT"
  EREASON=$(q "$EPAUSE")
  case "$EREASON" in "blocked 4: base_broken"*) pass E1 "pipeline $ET paused, exit=5, journal: $(printf '%s' "$EREASON" | tr '\n' ' ' | cut -c1-160)" ;; *) fail E1 "last pause of $EWF is [$EREASON], not blocked 4: base_broken" ;; esac
  as_cos "$QQ" source pull; [ "$AP_RC" = 0 ] || fail E2 "source pull rc=$AP_RC: $AP_OUT"
  EID=$(q "select id from proposal where loop=1 and observation like '%$EWF%' order by id desc limit 1")
  [ -n "$EID" ] || fail E2 "no loop-1 proposal names workflow $EWF after source pull ($AP_OUT)"
  printf '%s' "$AP_OUT" | grep -Eq "^filed [1-9][0-9]* proposals:.*(^| )$EID( |$)" || fail E2 "source pull did not report proposal $EID as filed: $AP_OUT"
  EPULL=$AP_OUT
  bl_open E2; EROW=$(row_of "$EID")
  printf '%s' "$EROW" | awk -F'\t' '$2==1 && $4=="low" && $6=="open" {ok=1} END {exit !ok}' || fail E2 "row of proposal $EID in backlog list open is [$EROW], want loop 1, risk low, open"
  pass E2 "$EPULL; backlog list open row [$(printf '%s' "$EROW" | tr '\t' ' ')] (id loop kind risk surface status task)"
  ENOTE="e2e: fix the failing check"
  as_cos "$QQ" backlog decide "$EID" approve --note "$ENOTE" --kind "$BUILDER"; [ "$AP_RC" = 0 ] || fail E3 "backlog decide rc=$AP_RC: $AP_OUT"
  printf '%s' "$AP_OUT" | grep -Eq "^proposal $EID approved, task [0-9]+$" || fail E3 "unexpected decide output: $AP_OUT"
  ETASK=$(printf '%s' "$AP_OUT" | sed -E 's/.*task ([0-9]+)$/\1/')
  expect E3d "cos|approve|$ENOTE" "$(q "select decider_role||'|'||verdict||'|'||coalesce(note,'') from decision where proposal_id=$EID")"
  expect E3t "$BUILDER|open|$EID" "$(q "select kind||'|'||state||'|'||proposal_id from task where id=$ETASK")"
  as_cos "$QQ" backlog list approved; [ "$AP_RC" = 0 ] || fail E3 "backlog list approved rc=$AP_RC: $AP_OUT"
  printf '%s\n' "$AP_OUT" | awk -F'\t' -v id="$EID" -v t="$ETASK" '$1==id && $6=="approved" && $7==t {ok=1} END {exit !ok}' || fail E3 "backlog list approved has no row of proposal $EID with task $ETASK"
  pass E3 "$AP_OUT -> decision cos/approve/[$ENOTE]; task $ETASK kind $BUILDER open from proposal $EID"
fi

# ---- part f: clause (f), an idea file becomes a high-risk feature proposal that stays open ---------------------------
if want f; then
  mkdir -p "$WS/inbox/ideas"
  FNAME="e2e-idea-$$.md"
  printf '# E2E idea %s\n\nLet the farm print a one-line status summary.\n' "$$" >"$WS/inbox/ideas/$FNAME"
  as_cos "$QQ" source pull; [ "$AP_RC" = 0 ] || fail F1 "source pull rc=$AP_RC: $AP_OUT"
  FID=$(q "select id from proposal where loop=3 and evidence='inbox/ideas/$FNAME'")
  [ -n "$FID" ] || fail F1 "no loop-3 proposal for inbox/ideas/$FNAME after source pull ($AP_OUT)"
  printf '%s' "$AP_OUT" | grep -Eq "^filed [1-9][0-9]* proposals:.*(^| )$FID( |$)" || fail F1 "source pull did not report proposal $FID as filed: $AP_OUT"
  pass F1 "$AP_OUT (proposal $FID is the idea file inbox/ideas/$FNAME)"
  as_cos "$QQ" source pull; [ "$AP_RC" = 0 ] || fail F2 "second source pull rc=$AP_RC: $AP_OUT"
  expect F2n "1" "$(q "select count(*) from proposal where loop=3 and evidence='inbox/ideas/$FNAME'")"
  bl_open F2; FROW=$(row_of "$FID")
  printf '%s' "$FROW" | awk -F'\t' '$2==3 && $4=="high" && $6=="open" {ok=1} END {exit !ok}' || fail F2 "row of proposal $FID in backlog list open is [$FROW], want loop 3, risk high, open"
  expect F2d "0" "$(decisions_of "$FID")"
  pass F2 "backlog list open row [$(printf '%s' "$FROW" | tr '\t' ' ')] (id loop kind risk surface status task): loop 3, high, still open after a second pull, no verdict"
fi

# ---- part d ------------------------------------------------------------------------------------------------
if want d; then
  before=$(q "select count(*) from events where workflow_id='authority:refusals' and kind='refusal'" 2>/dev/null || echo 0)
  # D1: a process in a pane that is neither the CoS pane nor an agent's pane is refused by the daemon's door (R11).
  PUSHY="$REVIEWER-pushy"   # worker spawn names are <kind>-<task>
  ROGUE_PANE=$(H pane split "$COS_PANE" --direction right --no-focus | first_pane)
  [ -n "$ROGUE_PANE" ] || fail D1 "could not open a plain pane"
  as_pane "$ROGUE_PANE" "$QQ" worker spawn e2e-rogue "$REVIEWER" "$PROJ"
  [ "$AP_RC" -ne 0 ] && pass D1 "exit=$AP_RC $(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-160)" || fail D1 "out-of-band spawn was allowed"
  # D2: the PreToolUse gate blocks `git push` (exit 2) for any agent; `quaqua push` (D5) is the daemon's one door.
  GRC=0; GOUT=$(printf '%s' '{"tool_input":{"command":"git push origin main"}}' | "$QQ" gate 2>&1) || GRC=$?
  [ "$GRC" = 2 ] && pass D2 "gate exit=$GRC $(printf '%s' "$GOUT" | tr '\n' ' ' | cut -c1-120)" || fail D2 "gate exit=$GRC for git push (want 2)"
  # D3: a real non-claude worker told to push; asserted from the remote, whatever blocks it.
  as_cos "$QQ" worker spawn "$PUSHY" "$REVIEWER" "$PROJ"; [ "$AP_RC" = 0 ] || fail D3 "spawn rc=$AP_RC: $AP_OUT"
  H agent prompt "$PUSHY" "run: git push origin HEAD:refs/heads/pwned" --wait --until working >"$T/pushy.out" 2>&1 || fail D3 "$(cat "$T/pushy.out")"
  H agent wait "$PUSHY" --until idle --until 'done' --until blocked --timeout $((TIMEOUT * 1000)) >>"$T/pushy.out" 2>&1 || fail D3 "pushy never settled: $(tail -3 "$T/pushy.out")"
  expect D3 "" "$(git -C "$ORIGIN" for-each-ref refs/heads/pwned)"
  # D5: `quaqua push` is refused for a caller with no identity and for the Peer (the pi worker), by the daemon.
  as_pane "$ROGUE_PANE" "$QQ" push "$PROJ" origin e2e-pwn2
  [ "$AP_RC" -ne 0 ] && pass D5a "no-identity push exit=$AP_RC $(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-160)" || fail D5a "push from an unidentified pane was allowed"
  PEDGE="select count(*) from events where workflow_id='authority:refusals' and kind='refusal' and json_extract(payload_json,'\$.caller')='$PUSHY' and json_extract(payload_json,'\$.edge') in ('push','orchestration')"
  pbefore=$(q "$PEDGE")
  H agent prompt "$PUSHY" "run exactly: $QQ push $PROJ origin e2e-pwn2" --wait --until working >"$T/pushy2.out" 2>&1 || fail D5 "$(cat "$T/pushy2.out")"
  H agent wait "$PUSHY" --until idle --until 'done' --until blocked --timeout $((TIMEOUT * 1000)) >>"$T/pushy2.out" 2>&1 || fail D5 "pushy never settled: $(tail -3 "$T/pushy2.out")"
  expect D5b "" "$(git -C "$ORIGIN" for-each-ref refs/heads/e2e-pwn2)"
  pafter=$(q "$PEDGE")
  [ "$pafter" -gt "$pbefore" ] && pass D5 "peer push refused and journaled ($pbefore -> $pafter): $(q "select json_extract(payload_json,'\$.edge')||': '||json_extract(payload_json,'\$.reason') from events where workflow_id='authority:refusals' and json_extract(payload_json,'\$.caller')='$PUSHY' order by seq desc limit 1")" \
    || fail D5 "journal has no push/orchestration refusal for $PUSHY ($pbefore -> $pafter)"
  after=$(q "select count(*) from events where workflow_id='authority:refusals' and kind='refusal'")
  [ "$after" -gt "$before" ] && pass D4 "refusal events $before -> $after: $(q "select group_concat(json_extract(payload_json,'\$.edge'),',') from events where workflow_id='authority:refusals'")" \
    || fail D4 "journal gained no refusal ($before -> $after)"
fi
# ---- part h: clause (h), WORKSPACE_PROTOCOL.md sets a worker cap of N; spawn N+1 is refused ---------------------
# Runs after d so no later spawn of this script meets the cap. N = Peers already active in the project + 1.
if want h; then
  ensure_project H1
  ACTIVE=$(q "select count(*) from agent a join project p on p.id=a.project_id where p.slug='$PROJECT' and a.role='peer' and a.ended_ms is null")
  CAPN=$((ACTIVE + 1))
  [ "$CAPN" -le 8 ] || fail H1 "cap would be $CAPN: above the engine ceiling 8 ($ACTIVE Peers already active)"
  FENCE=$(printf '\140\140\140')
  printf '# Workspace protocol\n\n%stoml\nworker_cap = %s\n%s\n' "$FENCE" "$CAPN" "$FENCE" >"$PROJ/WORKSPACE_PROTOCOL.md"
  git -C "$PROJ" add WORKSPACE_PROTOCOL.md && git -C "$PROJ" commit -qm "e2e: worker cap $CAPN" || fail H1 "could not commit the protocol on the default branch"
  as_cos "$QQ" worker spawn "$REVIEWER-capok" "$REVIEWER" "$PROJ"; [ "$AP_RC" = 0 ] || fail H1 "spawn up to the cap ($CAPN) was refused: $AP_OUT"
  expect H1 "$CAPN" "$(q "select count(*) from agent a join project p on p.id=a.project_id where p.slug='$PROJECT' and a.role='peer' and a.ended_ms is null")"
  as_cos "$QQ" worker spawn "$REVIEWER-capx" "$REVIEWER" "$PROJ"
  [ "$AP_RC" -ne 0 ] || fail H2 "spawn $((CAPN + 1)) was allowed with worker_cap = $CAPN"
  WHY=$(q "select json_extract(payload_json,'\$.reason') from events where workflow_id='authority:refusals' and json_extract(payload_json,'\$.reason') like 'worker cap%' order by seq desc limit 1")
  pass H2 "spawn $((CAPN + 1)) refused at worker_cap = $CAPN: exit=$AP_RC out=$(printf '%s' "$AP_OUT" | tr '\n' ' ' | cut -c1-160) journal=[${WHY:-none}]"
  expect H2n "0" "$(q "select count(*) from agent where name='$REVIEWER-capx'")"
fi

echo "ALL $N ASSERTIONS PASSED (parts: ${PARTS[*]})"
