#!/usr/bin/env bash
# tests/test_panemove_working_agent.sh — does moving or swapping a pane keep the
# agent process and its session when the agent is *working*? (issue #192)
#
# Opt-in: skipped unless a herdr binary is available. Every herdr call goes to a
# private daemon on a throwaway socket ($HERDR_SOCKET_PATH under $TMPDIR), so the
# live session and other workers' panes are never touched. The script refuses to
# run if its socket already has a workspace.
#
# Leg A (always, no model, no cost): a long-lived process plus a reported agent
# identity. Checks pane id, terminal id, agent label/status and the process pid
# before and after a cross-tab `pane move` and an in-tab `pane swap`, and that the
# process kept writing output across both.
#
# Leg B (PANEMOVE_LIVE_AGENT=1, needs `omp`): a real agent, prompted into a
# `working` state, is moved and swapped; the run proves the same omp pid keeps the
# same session file, that the session keeps writing, and that the tool call
# started before the move completes afterwards (prints MOVED_OK).
#
# Usage: tests/test_panemove_working_agent.sh
#        PANEMOVE_LIVE_AGENT=1 tests/test_panemove_working_agent.sh
#        HERDR_BIN=/path/to/herdr tests/test_panemove_working_agent.sh
set -euo pipefail

# Resolve the binary and the real HOME *before* the sandbox overrides HOME: leg B
# hands the real HOME back to the agent it starts, or the harness opens a login
# wizard instead of its prompt.
real_home="$HOME"
real_xdg="${XDG_CONFIG_HOME:-$HOME/.config}"
HB="${HERDR_BIN:-$HOME/.local/bin/herdr}"
if [[ ! -x $HB ]]; then
  echo "SKIP test_panemove_working_agent.sh: no herdr binary at $HB (set HERDR_BIN)"
  exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "SKIP test_panemove_working_agent.sh: jq not found"
  exit 0
fi

T=$(mktemp -d "${TMPDIR:-/tmp}/panemove.XXXXXX")
T=$(cd "$T" && pwd -P)
server_pid=''
workspaces=()

cleanup() {
  for w in "${workspaces[@]}"; do
    "$HB" workspace close "$w" >/dev/null 2>&1 || true
  done
  # Stop only our own daemon: the socket lives under $T. `herdr server stop` is
  # otherwise forbidden (it would take the live session down).
  if [[ $HERDR_SOCKET_PATH == "$T"/* ]]; then
    "$HB" server stop >/dev/null 2>&1 || true
  fi
  if [[ -n $server_pid ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$T"
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
h() { "$HB" "$@"; }

# ── isolation: a private daemon, a private socket, a private state dir ───────
export HERDR_SOCKET_PATH="$T/herdr.sock"
export XDG_CONFIG_HOME="$T/config"
export HOME="$T/home"
unset HERDR_PANE_ID HERDR_WORKSPACE_ID HERDR_TAB_ID HERDR_AGENT HERDR_ENV \
      HERDR_SESSION GRID_TAB GRID_OWNER QUAQUA_OWNER QUAQUA_ROOT QUAQUA_RUNS_DIR 2>/dev/null || true
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$T/cwd"

if command -v setsid >/dev/null 2>&1; then
  setsid "$HB" server >"$T/server.log" 2>&1 </dev/null &
else
  "$HB" server >"$T/server.log" 2>&1 </dev/null &
fi
server_pid=$!
ready=0
for _ in $(seq 1 60); do
  if h status server 2>/dev/null | grep -q '^status: running'; then ready=1; break; fi
  sleep 0.25
done
[[ $ready == 1 ]] || { cat "$T/server.log" >&2; fail "isolated herdr server did not start"; }
[[ $(h workspace list | jq -r '.result.workspaces | length') == 0 ]] \
  || fail "refusing to run: $HERDR_SOCKET_PATH already has workspaces (not isolated)"
echo "ok: isolated herdr $(h status server | awk '/^version:/{print $2}') on $HERDR_SOCKET_PATH"

# ── 3-worker geometry is A │ B/C, as crates/quaqua/src/grid/plan.rs cells(3) ─
geo=$(h workspace create --label panemove-geo --cwd "$T/cwd" --no-focus)
ws=$(jq -r '.result.workspace.workspace_id' <<<"$geo")
workspaces+=("$ws")
p1=$(jq -r '.result.root_pane.pane_id' <<<"$geo")
p2=$(h pane split --pane "$p1" --direction right --cwd "$T/cwd" --no-focus | jq -r '.result.pane.pane_id')
p3=$(h pane split --pane "$p2" --direction down --cwd "$T/cwd" --no-focus | jq -r '.result.pane.pane_id')
layout=$(h pane layout)
area_w=$(jq -r '.result.layout.area.width' <<<"$layout")
area_h=$(jq -r '.result.layout.area.height' <<<"$layout")
rect() { jq -c --arg p "$1" '.result.layout.panes[] | select(.pane_id==$p) | .rect' <<<"$layout"; }
left=$(rect "$p1"); top=$(rect "$p2"); bottom=$(rect "$p3")
[[ $(jq -r '.x,.y,.width,.height' <<<"$left" | paste -sd, -) == "0,0,$((area_w / 2)),$area_h" ]] \
  || fail "3-worker left column expected full height: $left"
[[ $(jq -r '.x,.y,.width,.height' <<<"$top" | paste -sd, -) == "$((area_w / 2)),0,$((area_w - area_w / 2)),$((area_h / 2))" ]] \
  || fail "3-worker top-right cell wrong: $top"
[[ $(jq -r '.x,.y,.width,.height' <<<"$bottom" | paste -sd, -) == "$((area_w / 2)),$((area_h / 2)),$((area_w - area_w / 2)),$((area_h - area_h / 2))" ]] \
  || fail "3-worker bottom-right cell wrong: $bottom"
echo "ok: 3-worker geometry is A │ B/C (left full, right stacked): $left $top $bottom"

# ── leg A: a long-lived process with a reported agent identity ──────────────
cat > "$T/dummy-agent.sh" <<'EOF'
#!/bin/sh
echo "AGENT_START pid=$$"
n=0
while :; do n=$((n+1)); echo "HB $n pid=$$"; sleep 1; done
EOF
chmod +x "$T/dummy-agent.sh"

h pane run "$p2" /bin/sh "$T/dummy-agent.sh" >/dev/null
pid=''
for _ in $(seq 1 40); do
  pid=$(h pane read "$p2" --lines 50 | sed -n 's/.*AGENT_START pid=\([0-9]*\).*/\1/p' | tail -1)
  [[ -n $pid ]] && break
  sleep 0.25
done
[[ -n $pid ]] || fail "dummy agent never printed AGENT_START in $p2"
h pane report-agent "$p2" --source panemove --agent panemove-dummy --state working >/dev/null
h pane report-agent-session "$p2" --source panemove --agent panemove-dummy \
  --seq 1 --agent-session-id sess-panemove >/dev/null

hb_counter() { h pane read "$1" --lines 200 | sed -n 's/^HB \([0-9]*\) .*/\1/p' | tail -1; }
state() {  # pane id -> pane_id,tab_id,terminal_id,agent,agent_status
  jq -c '.result.pane | {pane_id,tab_id,terminal_id,agent,agent_status}' <<<"$(h pane get "$1")"
}
proc_pids() { h pane process-info --pane "$1" | jq -c '[.result.process_info.foreground_processes[] | .pid]'; }

before=$(state "$p2")
before_pids=$(proc_pids "$p2")
before_hb=$(hb_counter "$p2")
echo "leg A before: $before pids=$before_pids hb=$before_hb"

# cross-tab move of the working pane (the shape the grid uses)
scratch=$(h tab create --workspace "$ws" --label scratch --cwd "$T/cwd" --no-focus)
p4=$(jq -r '.result.root_pane.pane_id' <<<"$scratch")
move=$(h pane move "$p2" --tab "$(h pane get "$p4" | jq -r '.result.pane.tab_id')" \
  --split right --target-pane "$p4" --no-focus)
[[ $(jq -r '.result.move_result.changed' <<<"$move") == true ]] \
  || fail "move did not change anything: $move"
after=$(state "$p2")
after_pids=$(proc_pids "$p2")
sleep 2
after_hb=$(hb_counter "$p2")
echo "leg A after move: $after pids=$after_pids hb=$after_hb"

same_field() {  # name, before json, after json
  local b a
  b=$(jq -r --arg k "$1" '.[$k]' <<<"$2")
  a=$(jq -r --arg k "$1" '.[$k]' <<<"$3")
  [[ $b == "$a" && $b != null ]] || fail "move changed $1: $b -> $a"
}
for f in pane_id terminal_id agent agent_status; do same_field "$f" "$before" "$after"; done
[[ $(jq -r --argjson p "$pid" 'index($p) != null' <<<"$after_pids") == true ]] \
  || fail "dummy agent pid $pid is gone after the move (pids=$after_pids)"
[[ ${after_hb:-0} -gt ${before_hb:-0} ]] \
  || fail "dummy agent stopped printing across the move (hb $before_hb -> $after_hb)"
echo "ok: working pane kept pane/terminal/agent/status/pid and kept writing across a cross-tab move"

# in-tab swap of the same working pane
h pane swap --source-pane "$p2" --target-pane "$p4" >/dev/null
swapped=$(state "$p2")
swapped_pids=$(proc_pids "$p2")
sleep 2
swapped_hb=$(hb_counter "$p2")
for f in pane_id terminal_id agent agent_status; do same_field "$f" "$before" "$swapped"; done
[[ $(jq -r --argjson p "$pid" 'index($p) != null' <<<"$swapped_pids") == true ]] \
  || fail "dummy agent pid $pid is gone after the swap (pids=$swapped_pids)"
[[ ${swapped_hb:-0} -gt ${after_hb:-0} ]] \
  || fail "dummy agent stopped printing across the swap (hb $after_hb -> $swapped_hb)"
echo "ok: working pane kept pane/terminal/agent/status/pid and kept writing across an in-tab swap"
same_tab=$(h pane move "$p2" --tab "$(h pane get "$p2" | jq -r '.result.pane.tab_id')" \
  --split right --target-pane "$p4" --no-focus | jq -r '.result.move_result | "changed=\(.changed) reason=\(.reason)"')
echo "note: an in-tab reposition is a swap; a same-tab 'pane move' reports $same_tab"

# ── leg B: a real agent, working, moved and swapped ─────────────────────────
if [[ ${PANEMOVE_LIVE_AGENT:-0} != 1 ]]; then
  echo "note: leg B (real agent) skipped; set PANEMOVE_LIVE_AGENT=1 to run it"
elif ! command -v omp >/dev/null 2>&1; then
  echo "note: leg B skipped; omp not on PATH"
else
  live=$(h workspace create --label panemove-live --cwd "$T/cwd" \
    --env HOME="$real_home" --env XDG_CONFIG_HOME="$real_xdg" --no-focus)
  lws=$(jq -r '.result.workspace.workspace_id' <<<"$live")
  workspaces+=("$lws")
  lp=$(jq -r '.result.root_pane.pane_id' <<<"$live")
  h agent start panemove-live --kind omp --pane "$lp" --timeout 120000 >/dev/null \
    || fail "could not start omp in an isolated pane"
  h agent prompt panemove-live 'Run exactly this bash command and nothing else, no explanation: bash -c "sleep 45; echo MOVED_OK"' >/dev/null
  status=''
  for _ in $(seq 1 60); do
    status=$(h agent get panemove-live | jq -r '.result.agent.agent_status')
    [[ $status == working ]] && break
    sleep 1
  done
  [[ $status == working ]] || fail "omp never reached working (status=$status)"

  lstate() {
    h pane get "$lp" | jq -c '.result.pane | {pane_id,tab_id,terminal_id,agent,agent_status,
      agent_session:(.agent_session.value // null), agent_session_kind:(.agent_session.kind // null)}'
  }
  lbefore=$(lstate)
  for _ in $(seq 1 40); do
    [[ $(jq -r '.agent_session' <<<"$lbefore") != null ]] && break
    sleep 0.5
    lbefore=$(lstate)
  done
  lpid=$(h pane process-info --pane "$lp" | jq -r '.result.process_info.foreground_processes[] | select(.name=="omp") | .pid' | head -1)
  lfile=$(jq -r '.agent_session' <<<"$lbefore")
  [[ -n $lpid && $lfile != null ]] || fail "live agent has no omp pid/session file to track: $lbefore (pid=$lpid)"
  for _ in $(seq 1 40); do  # omp reports the path before the first write lands
    [[ -s $lfile ]] && break
    sleep 0.5
  done
  [[ -s $lfile ]] || fail "session file $lfile never appeared"
  lsize=$(wc -c <"$lfile")
  echo "leg B before: $lbefore pid=$lpid session_bytes=$lsize"

  lscratch=$(h tab create --workspace "$lws" --label scratch --cwd "$T/cwd" --no-focus)
  lroot=$(jq -r '.result.root_pane.pane_id' <<<"$lscratch")
  h pane move "$lp" --tab "$(h pane get "$lroot" | jq -r '.result.pane.tab_id')" \
    --split right --target-pane "$lroot" --no-focus | jq -r '.result.move_result | "note: cross-tab move of a working agent: changed=\(.changed) reason=\(.reason)"'
  h pane swap --source-pane "$lp" --target-pane "$lroot" >/dev/null

  lafter=$(lstate)
  lpid_after=$(h pane process-info --pane "$lp" | jq -r '.result.process_info.foreground_processes[] | select(.name=="omp") | .pid' | head -1)
  for f in pane_id terminal_id agent agent_session; do same_field "$f" "$lbefore" "$lafter"; done
  [[ $lpid_after == "$lpid" ]] || fail "omp pid changed across move+swap: $lpid -> $lpid_after"

  final=''
  for _ in $(seq 1 120); do
    final=$(h agent get panemove-live | jq -r '.result.agent.agent_status')
    [[ $final == idle || $final == "done" ]] && break
    sleep 1
  done
  [[ $final == idle || $final == "done" ]] || fail "moved agent never finished its turn (status=$final)"
  h pane read "$lp" --lines 300 >"$T/live-pane.txt"
  grep -q MOVED_OK "$T/live-pane.txt" \
    || fail "the tool call started before the move never completed after it"
  [[ $(wc -c <"$lfile") -gt $lsize ]] || fail "session file $lfile did not grow across the move"
  echo "ok: working agent kept pane/terminal/name/session file/pid across move+swap and finished its tool call ($final)"
fi

echo "PASS: all test_panemove_working_agent.sh cases passed"
