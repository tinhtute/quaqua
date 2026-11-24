#!/usr/bin/env bash
# T39 manual probe: native pi / agy drivers on a real pane, isolated herdr session.
# Run by hand (spends a few cents of tokens per kind). Never run by CI or agents.
set -euo pipefail

usage() {
  cat <<'EOF'
usage: native_probe.sh [--kind pi|agy|both] [--timeout-ms N] [--startup-only] [--cwd DIR] [--temp-home] [--yes] [-h|--help]

  Per kind, in an isolated `herdr --session` (temp XDG config dir, removed on exit):
    1 start   `herdr agent start --kind K` reaches ready
    2 prompt  one short prompt is delivered and the agent goes working
    3 stop    stop sequences tried in order until the pane is idle again
    4 tokens  where the CLI reports tokens (slash command, screen line, session file)
  Prints one table per kind and writes all of them to a results file (path printed).

  --kind        pi | agy | both (default both)
  --timeout-ms  readiness / idle timeout per step (default 60000)
  --startup-only  run step 1 only (no prompt, no model call, no cost confirmation); on a
                failed start the table prints the pane's screen text as the blocker
  --cwd DIR     pane working directory (default: the temp dir). agy asks "trust this folder?" for a
                new dir and answering would write trust state under ~/.gemini, which this script
                never does; pass a dir agy already trusts to get past it
  --temp-home   also swap HOME for an empty dir (CLIs will be signed out; default keeps
                your real HOME so pi/agy find their credentials)
  --yes         skip the typed cost confirmation (do not use unless you mean it)

  Never touches the default herdr session. Needs herdr 0.9.5, python3, a signed-in pi/agy.
  XDG_CONFIG_HOME is isolated for herdr; mise's global config (which provides the pi shim)
  is pinned via MISE_GLOBAL_CONFIG_FILE to your real one so panes still resolve pi.
EOF
}

KINDS="pi agy"
TIMEOUT_MS=60000
TEMP_HOME=0
ASSUME_YES=0
STARTUP_ONLY=0
PANE_CWD=""
while [ $# -gt 0 ]; do
  case "$1" in
    --kind)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      case "$2" in
        pi) KINDS="pi" ;;
        agy) KINDS="agy" ;;
        both) KINDS="pi agy" ;;
        *) usage >&2; exit 2 ;;
      esac
      shift 2 ;;
    --timeout-ms)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      case "$2" in ''|*[!0-9]*) usage >&2; exit 2 ;; esac
      TIMEOUT_MS="$2"; shift 2 ;;
    --startup-only) STARTUP_ONLY=1; shift ;;
    --cwd)
      [ $# -ge 2 ] && [ -d "$2" ] || { usage >&2; exit 2; }
      PANE_CWD=$(cd "$2" && pwd); shift 2 ;;
    --temp-home) TEMP_HOME=1; shift ;;
    --yes) ASSUME_YES=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

for need in herdr python3; do
  command -v "$need" >/dev/null 2>&1 || { echo "missing: $need" >&2; exit 1; }
done
for k in $KINDS; do
  command -v "$k" >/dev/null 2>&1 || { echo "missing CLI on PATH: $k" >&2; exit 1; }
done

# ---- cost warning: nothing below this line starts a pane or an agent ----------------
if [ "$STARTUP_ONLY" -eq 1 ]; then
  echo "startup-only: starting $KINDS in an isolated herdr session; no prompt is sent."
else
  cat <<EOF
WARNING: this script starts REAL pi/agy sessions and sends each one a real prompt.
  kinds: $KINDS   -> about $(echo "$KINDS" | wc -w | tr -d ' ') model call(s) plus a few slash commands; costs tokens.
  It runs in its own herdr session (not your default one) and stops everything on exit.
EOF
fi
if [ "$STARTUP_ONLY" -eq 0 ] && [ "$ASSUME_YES" -ne 1 ]; then
  printf 'Type yes to continue: '
  read -r answer || answer=""
  [ "$answer" = "yes" ] || { echo "aborted (nothing started)"; exit 1; }
fi

# ---- isolation ------------------------------------------------------------------------
# /tmp, not $TMPDIR: unix socket paths are short (sun_path).
T=$(mktemp -d /tmp/hp.XXXXXX)
SESSION="t39probe-$$-$RANDOM"
RESULTS="$T/native_probe_results.txt"
KEEP="${TMPDIR:-/tmp}/native_probe_results.$(date +%Y%m%d-%H%M%S).txt"
REAL_HOME="$HOME"
# The XDG override hides mise's global config (pi is a mise shim), so pin it first.
if [ -z "${MISE_GLOBAL_CONFIG_FILE:-}" ]; then
  _mise_cfg="${XDG_CONFIG_HOME:-$HOME/.config}/mise/config.toml"
  [ -f "$_mise_cfg" ] && export MISE_GLOBAL_CONFIG_FILE="$_mise_cfg"
fi
export XDG_CONFIG_HOME="$T/cfg"
mkdir -p "$XDG_CONFIG_HOME"
if [ "$TEMP_HOME" -eq 1 ]; then
  mkdir -p "$T/home"
  export HOME="$T/home"
fi
unset HERDR_SOCKET_PATH HERDR_ENV HERDR_PANE_ID HERDR_WORKSPACE_ID HERDR_TAB_ID HERDR_AGENT

H() { herdr --session "$SESSION" "$@"; }

SERVER_PID=""
WS_IDS=""
cleaned=0
cleanup() {
  [ "$cleaned" -eq 0 ] || return 0
  cleaned=1
  set +e
  for ws in $WS_IDS; do H workspace close "$ws" >/dev/null 2>&1; done
  H server stop >/dev/null 2>&1
  if [ -n "$SERVER_PID" ]; then
    sleep 1
    kill -0 "$SERVER_PID" 2>/dev/null && kill "$SERVER_PID" 2>/dev/null
  fi
  [ -f "$RESULTS" ] && cp "$RESULTS" "$KEEP" 2>/dev/null
  # Only ever remove the dir we created, and only if it still looks like ours.
  case "$T" in
    /tmp/hp.??????)
      if command -v trash >/dev/null 2>&1; then trash "$T" 2>/dev/null || echo "left $T"
      else echo "left $T (no trash on PATH; remove by hand)"; fi ;;
  esac
  [ -f "$KEEP" ] && echo "results: $KEEP"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

# ---- helpers --------------------------------------------------------------------------
# json_find KEY: first value of KEY anywhere in the JSON on stdin ("" if absent).
json_find() {
  python3 -I -c '
import json, sys
key = sys.argv[1]
def find(o):
    if isinstance(o, dict):
        if key in o and not isinstance(o[key], (dict, list)):
            return o[key]
        for v in o.values():
            r = find(v)
            if r is not None:
                return r
    elif isinstance(o, list):
        for v in o:
            r = find(v)
            if r is not None:
                return r
try:
    r = find(json.load(sys.stdin))
except Exception:
    r = None
print("" if r is None else r)' "$1"
}

log() { printf '%s\n' "$*" | tee -a "$RESULTS"; }

# agent_status PANE -> idle|working|blocked|done|unknown|"" (empty = herdr call failed)
agent_status() { H agent get "$1" 2>/dev/null | json_find agent_status || true; }

screen() { H pane read "$1" --source recent-unwrapped --lines "${2:-60}" 2>&1 || true; }

# wait_status PANE TIMEOUT_MS STATUS... -> prints ok|timeout
wait_status() {
  local pane="$1" to="$2"; shift 2
  local args=() s
  for s in "$@"; do args+=(--until "$s"); done
  if H agent wait "$pane" "${args[@]}" --timeout "$to" >/dev/null 2>&1; then echo ok; else echo timeout; fi
}

start_server() {
  nohup herdr --session "$SESSION" server >"$T/server.log" 2>&1 &
  SERVER_PID=$!
    for _ in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    H status server 2>/dev/null | grep -q 'running' && return 0
  done
  return 1
}

# ---- probe steps: each prints one status word ------------------------------------------
# Results are kept in per-kind variables so a failing step never aborts later reporting.
step_start() { # kind pane -> ready|not_ready|error
  local kind="$1" pane="$2" out
  if out=$(H agent start "probe-$kind" --kind "$kind" --pane "$pane" --timeout "$TIMEOUT_MS" 2>&1); then
    printf '%s\n' "$out" >"$T/$kind.start.out"
    local st; st=$(agent_status "$pane")
    case "$st" in idle|done) echo ready ;; *) echo "ready_but_status_${st:-none}" ;; esac
  else
    printf '%s\n' "$out" >"$T/$kind.start.out"
    # Grab the screen now, while the blocked pane is still open (visible + scrollback).
    { screen "$pane" 40; printf '\n--- visible ---\n'
      H pane read "$pane" --source visible 2>&1 || true; } >"$T/$kind.start.screen"
    case "$out" in *agent_not_ready*|*timeout*) echo not_ready ;; *) echo error ;; esac
  fi
}

# A prompt that keeps the agent busy long enough to stop it, at trivial token cost.
# shellcheck disable=SC2016  # backticks are literal prompt text
PROMPT='Run the shell command `sleep 90` and then reply with the single word done.'

step_prompt() { # pane -> working|not_working|rejected
  local pane="$1"
  H agent prompt "$pane" "$PROMPT" >"$T/prompt.out" 2>&1 || { echo rejected; return; }
  [ "$(wait_status "$pane" 20000 working blocked)" = ok ] || { echo not_working; return; }
  case "$(agent_status "$pane")" in working) echo working ;; blocked) echo blocked ;; *) echo not_working ;; esac
}

# Stop sequences, tried in order. Each is a list of herdr key names sent in one call.
# esc is the documented herdr example; ctrl+c is the second documented one; the doubled
# forms are candidates (pi and agy docs do not state a stop key: UNVERIFIED until run).
STOP_SEQS=("esc" "ctrl+c" "esc esc" "ctrl+c ctrl+c")

step_stop() { # kind pane -> prints idle|still_busy|not_attempted; winning sequence goes to $T/stop.worked
  local pane="$2" seq st
  echo none >"$T/stop.worked"
  [ "$(agent_status "$pane")" = working ] || { echo not_attempted; return; }
  for seq in "${STOP_SEQS[@]}"; do
    # shellcheck disable=SC2086  # word splitting of the key list is intended
    H agent send-keys "$pane" $seq >/dev/null 2>&1 || true
    if [ "$(wait_status "$pane" 15000 idle "done")" = ok ]; then
      st=$(agent_status "$pane")
      case "$st" in idle|done) echo "$seq" >"$T/stop.worked"; echo idle; return ;; esac
    fi
    # still working: try the next candidate
  done
  echo still_busy
}

step_tokens() { # kind pane -> sets TOKENS_WHERE, TOKENS_LINE
  local kind="$1" pane="$2" cmds cmd scr hit f
  TOKENS_WHERE="none found"; TOKENS_LINE=""
  # 1. what is already on screen after the turn
  scr=$(screen "$pane" 80)
  hit=$(printf '%s\n' "$scr" | grep -iE 'tokens?|[0-9]+(\.[0-9]+)?k? *(in|out)\b|context|cost|\$[0-9]' | tail -3 || true)
  if [ -n "$hit" ]; then TOKENS_WHERE="screen line (after turn)"; TOKENS_LINE=$(printf '%s' "$hit" | tr '\n' '|'); return; fi
  # 2. slash commands that may print usage (candidates; each only prints, spends nothing)
  case "$kind" in
    pi)  cmds="/session /stats /usage" ;;
    *)   cmds="/usage /cost /stats /context" ;;
  esac
  for cmd in $cmds; do
    H pane send-text "$pane" "$cmd" >/dev/null 2>&1 || true
    H pane send-keys "$pane" enter >/dev/null 2>&1 || true
    sleep 2
    scr=$(screen "$pane" 60)
    hit=$(printf '%s\n' "$scr" | grep -iE 'tokens?|input|output|context|cost' | tail -3 || true)
    H pane send-keys "$pane" esc >/dev/null 2>&1 || true
    if [ -n "$hit" ]; then TOKENS_WHERE="slash command $cmd"; TOKENS_LINE=$(printf '%s' "$hit" | tr '\n' '|'); return; fi
  done
  # 3. session transcript files written in the last 15 min (pi: ~/.pi/agent/sessions)
  f=$(find "$REAL_HOME/.pi/agent/sessions" "$REAL_HOME/.antigravity" "$REAL_HOME/.config/agy" \
        -type f -mmin -15 2>/dev/null | head -20 | xargs -r grep -lE '"(input|output|total)_?[Tt]okens"|"usage"' 2>/dev/null | head -1 || true)
  if [ -n "$f" ]; then TOKENS_WHERE="session file $f"; TOKENS_LINE="(usage fields present)"; fi
}

probe_kind() {
  local kind="$1" ws_json ws pane s_start s_prompt s_stop
  local stop_attempts
  stop_attempts=$(printf '"%s" ' "${STOP_SEQS[@]}")
  ws_json=$(H workspace create --cwd "${PANE_CWD:-$T}" --label "probe-$kind" --no-focus 2>&1) || {
    log "[$kind] workspace create failed: $ws_json"; return 0; }
  ws=$(printf '%s' "$ws_json" | json_find workspace_id)
  WS_IDS="$WS_IDS $ws"
  pane=$(printf '%s' "$ws_json" | python3 -I -c '
import json, sys
try:
    print(json.load(sys.stdin)["result"]["root_pane"]["pane_id"])
except Exception:
    print("")')
  [ -n "$pane" ] || { log "[$kind] no root pane in: $ws_json"; return 0; }

  s_start=$(step_start "$kind" "$pane")
  s_prompt="skipped"; s_stop="skipped"; STOP_WORKED="n/a"; TOKENS_WHERE="n/a"; TOKENS_LINE=""
  if [ "$STARTUP_ONLY" -eq 1 ]; then
    s_prompt="not run (startup-only)"; s_stop="not run (startup-only)"
  else case "$s_start" in
    ready*)
      s_prompt=$(step_prompt "$pane")
      s_stop=$(step_stop "$kind" "$pane")
      STOP_WORKED=$(cat "$T/stop.worked" 2>/dev/null || echo none)
      # let the turn settle, then look for tokens (only meaningful once idle)
      if [ "$s_stop" = idle ] || [ "$(agent_status "$pane")" = idle ]; then step_tokens "$kind" "$pane"
      else TOKENS_WHERE="not checked (pane not idle)"; fi ;;
  esac; fi

  {
    echo
    echo "=== $kind  ($(date -u +%FT%TZ), herdr $(herdr --version 2>&1 | awk '{print $2}'))"
    printf '| %-22s | %-32s |\n' step result
    printf '| %-22s | %-32s |\n' "----------------------" "--------------------------------"
    printf '| %-22s | %-32s |\n' "agent start -> ready" "$s_start"
    printf '| %-22s | %-32s |\n' "prompt delivered" "$s_prompt"
    printf '| %-22s | %-32s |\n' "stop -> idle" "$s_stop"
    printf '| %-22s | %-32s |\n' "stop sequence that worked" "$STOP_WORKED"
    printf '| %-22s | %-32s |\n' "tokens reported at" "$TOKENS_WHERE"
    echo "stop sequences tried in order: $stop_attempts"
    [ -n "$TOKENS_LINE" ] && echo "tokens evidence: $TOKENS_LINE"
    echo "agent start output: $(head -c 300 "$T/$kind.start.out" 2>/dev/null | tr '\n' ' ')"
    if [ -s "$T/$kind.start.screen" ]; then
      echo "BLOCKER screen at failed start (scrollback, then visible):"
      sed 's/^/  | /' "$T/$kind.start.screen"
    fi
    echo "final screen tail:"
    screen "$pane" 12 | sed 's/^/  | /'
  } | tee -a "$RESULTS"
  # Per-kind teardown now, so a later kind never shares a pane; the trap is the backstop.
  H workspace close "$ws" >/dev/null 2>&1 || true
}

# ---- run ------------------------------------------------------------------------------
: >"$RESULTS"
log "T39 native probe: kinds=[$KINDS] session=$SESSION timeout_ms=$TIMEOUT_MS temp_home=$TEMP_HOME"
start_server || { log "isolated herdr server did not come up; see $T/server.log"; cat "$T/server.log" >&2 || true; exit 1; }
for kind in $KINDS; do probe_kind "$kind"; done
log
log "done. Paste the results file (path below) back to the Lead."
