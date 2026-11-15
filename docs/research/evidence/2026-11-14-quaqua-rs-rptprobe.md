Outcome:         BLOCKED
Snapshot:        none (nothing written in the repo; branch quaqua/tieude-rptprobe2 @ 43a60bd, clean)
Scope:           read-only. Wrote only this report + throwaway /tmp/hp.* dir (probe server run dir). No repo files changed.
Verification:    see "Evidence" below. The behavioural question (agent list/get/wait/prompt on a report-agent pane) is NOT measured: the isolated server comes up headless with zero panes, and every verb that makes a pane is refused by the gate.
Unknown / risk:  all rows in the result table are UNMEASURED; the script below produces them. Assumptions in the script: `pane list` JSON contains a `pane_id` key; `workspace create` returns a root pane; `--wait`/`--until` flags as in help. Script is syntax-checked only (`bash -n`), never run.
Ownership:       released

## Evidence (measured)
- `herdr --version` -> `herdr 0.9.5`.
- Isolated server works headless: `HOME=$T XDG_CONFIG_HOME=$T/cfg herdr --session p31585 server` (HERDR_SOCKET_PATH unset) -> "herdr server running... api socket: /tmp/hp.ocI1/cfg/herdr/sessions/p31585/herdr.sock". `herdr --session ... status` -> `server: status: running`.
  - Gotcha: macOS TMPDIR (`/var/folders/...`) overflows sun_path: `Error: Custom { kind: InvalidInput, error: "local socket name length exceeds capacity of sun_path of sockaddr_un" }`. Use `mktemp -d /tmp/hp.XXXX`.
- Fresh server has no panes: `pane list` -> `{"id":"cli:pane:list","result":{"panes":[],"type":"pane_list"}}` (exit 0); `workspace list` -> `{"workspaces":[]}`.
- Refused verb: `herdr --session p31585 workspace create --label probe` -> gate hook: `blocked: spawn/move panes and workspaces only via quaqua worker spawn / quaqua lead spawn / quaqua worker cleanup / quaqua lead cleanup or `quaqua grid reconcile` — rules/_all/fanout-agent-grid.md`. Refused step: step 1 (create the only pane). `pane split`, `tab create`, `agent start` hit the same gate; I did not work around it (no raw socket call, no TUI attach).
- Server stopped: `server stop` -> exit 0; `status` -> `server: status: not running`; `pgrep -fl "herdr.*p31585"` -> none. (First stop attempt failed from my own zsh word-splitting, rc=127; the second succeeded.)
- From help/schema (measured text): `agent prompt` rejects with `agent_blocked` before sending input if the target is blocked; with `--wait` needs an observed working/blocked within 5000 ms else `agent_prompt_stalled`; `agent wait` default matches idle|done|blocked; `agent rename <TARGET> <NAME>|--clear`; `release-agent` takes `--source --agent [--seq]`; `PaneAgentState` = idle|working|blocked|unknown (no `done` from reports); AgentStatus adds `done`.

## Result table (command | accepted? | observed)
| command | accepted? | observed |
|---|---|---|
| agent list / get / wait / prompt on report-agent pane | UNMEASURED | run script |
| agent rename on that pane | UNMEASURED | run script |
| prompt text reaches stdin of `cat` | UNMEASURED | script checks `$T/stdin.log` |
| wait settles on working->idle | UNMEASURED | script step 4 |
| release-agent / stale --seq | UNMEASURED | script steps 6, 9, 10 |

## Fallback script (run by hand; isolated; prints every command, output, exit code; cleans up)
Save as rptprobe.sh, then `bash rptprobe.sh 2>&1 | tee rptprobe.out`. The script unsets HERDR_* and points HOME/XDG_CONFIG_HOME at a temp dir.

```bash
#!/usr/bin/env bash
# Isolated probe: herdr 0.9.5 report-agent on a plain-process pane. Throwaway.
set -uo pipefail
T=$(mktemp -d /tmp/hp.XXXX); S=p$RANDOM
export HOME=$T XDG_CONFIG_HOME=$T/cfg; unset HERDR_SOCKET_PATH HERDR_ENV HERDR_PANE_ID
H="herdr --session $S"
cleanup(){ $H server stop >/dev/null 2>&1; sleep 1; command -v trash >/dev/null && trash "$T" 2>/dev/null || echo "left $T"; }
trap cleanup EXIT
run(){ echo; echo "\$ $*"; "$@" 2>&1; echo "[exit $?]"; }
nohup herdr --session $S server >"$T/server.log" 2>&1 &
sleep 3; herdr --version
run $H workspace create --cwd "$T" --label probe --focus
P=$($H pane list 2>/dev/null | python3 -I -c 'import json,sys
def f(o):
    if isinstance(o,dict):
        if "pane_id" in o: return o["pane_id"]
        for v in o.values():
            r=f(v)
            if r: return r
    if isinstance(o,list):
        for v in o:
            r=f(v)
            if r: return r
print(f(json.load(sys.stdin)) or "")')
echo "PANE=$P"; [ -z "$P" ] && { echo "no pane"; exit 1; }
SRC=probe-src
run $H pane run "$P" "cat > $T/stdin.log"          # plain process, stdin captured
sleep 1
echo "### 0 baseline (no report)"; run $H agent list; run $H agent get "$P"
echo "### 1 report working seq1 (from outside the pane)"
run $H pane report-agent "$P" --source $SRC --agent fakeharness --state working --seq 1
run $H agent list; run $H agent get "$P"; run $H pane get "$P"
echo "### 2 rename (attach a name)"
run $H agent rename "$P" probe-a; run $H agent get probe-a; run $H agent list
echo "### 3 wait while working (expect timeout)"
run $H agent wait probe-a --until idle --timeout 3000
echo "### 4 wait settles on working->idle?"
( sleep 2; $H pane report-agent "$P" --source $SRC --agent fakeharness --state idle --seq 2 >"$T/r2.out" 2>&1; echo "bg report exit $?" >>"$T/r2.out" ) &
run $H agent wait probe-a --until idle --timeout 8000; cat "$T/r2.out"
run $H agent get probe-a
echo "### 5 prompt (text -> stdin?)"
run $H agent prompt probe-a "hello-from-prompt"
sleep 1; echo "stdin.log:"; cat "$T/stdin.log"; $H pane read "$P" 2>&1 | tail -5
run $H agent prompt probe-a "hello-wait" --wait --timeout 8000
echo "### 6 stale seq"
run $H pane report-agent "$P" --source $SRC --agent fakeharness --state blocked --seq 1
run $H agent get probe-a
run $H pane report-agent "$P" --source $SRC --agent fakeharness --state blocked --seq 3
run $H agent get probe-a
echo "### 7 prompt while blocked / wait --until blocked"
run $H agent prompt probe-a "x"; run $H agent wait probe-a --until blocked --timeout 2000
echo "### 8 report from inside the pane (HERDR_ENV path)"
run $H pane send-text "$P" $'\003'; run $H pane run "$P" 'echo ENV=$HERDR_ENV PANE=$HERDR_PANE_ID BIN=$HERDR_BIN_PATH'
sleep 1; $H pane read "$P" 2>&1 | tail -4
echo "### 9 release-agent"
run $H pane release-agent "$P" --source $SRC --agent fakeharness --seq 99
run $H agent list; run $H agent get probe-a; run $H agent get "$P"
echo "### 10 release with stale seq, re-report"
run $H pane report-agent "$P" --source $SRC --agent fakeharness --state idle --seq 100
run $H pane release-agent "$P" --source $SRC --agent fakeharness --seq 1
run $H agent get "$P"
```

Prototype note: throwaway; nothing here is production.

## Playbook
1: done
2: skip: direction set by brief
3: done: script above (scratch, not run)
4: skip: single probe, no alternatives
5: skip: blocked by gate; only server start/stop and the refusal were observed
6: done: this report
7: done: Verified: inconclusive 43a60bd

## Blockers
Cannot create a pane without a gate-refused verb (workspace create). Need anh iu to run the script above, or lift the gate for this worktree.

## Lessons
Headless `herdr server` starts with zero panes, so any pane-level probe needs a workspace-create verb; macOS $TMPDIR is too long for the herdr socket path (use /tmp/hp.XXXX).
