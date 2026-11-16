#!/usr/bin/env bash
# PreToolUse gate (rules/_all/fanout-agent-grid.md + AGENTS.md rule 3). Binary
# resolution: $QUAQUA_BIN, else `quaqua` on PATH; the binary owns the real
# tokenizer + policy (`quaqua gate`).
#
# The fence must not vanish when the binary is missing: Claude Code treats every
# exit code other than 2 as a non-blocking error, so a bare `quaqua gate` that
# dies 127 would let every pane/workspace spawn+move through. It must also not lock an agent out of the shell before
# it can build the binary, so the fallback blocks only the guarded commands and
# lets everything else through with a warning.
#
# The fallback is deliberately cheap (no tokenizer): it scans the raw hook JSON
# on stdin for `herdr … <guarded pair>` and for outward actions (`git … push`,
# `gh` merges). That is a conservative superset — it
# also blocks argument text such as `echo "herdr pane split"` — which errs
# toward blocking while the binary is unavailable. The binary is the precise
# path (it tokenizes the command).
# `h\e\rdr`-style escaping is the one known miss; install the binary for exact
# semantics.
set -uo pipefail

bin=${QUAQUA_BIN:-quaqua}
# No SessionStart mode: the old rule-drift baseline / re-read list and the CoS-pane auto-tag are dropped; the CoS decides an owner.
if command -v "$bin" >/dev/null 2>&1; then
  # The hook JSON stays on stdin; `quaqua gate` blocks (exit 2) on empty or unparseable input.
  exec "$bin" gate
fi

# The fallback guarded list mirrors `guarded_pair`, `check_git` and `check_gh` in
# src/authority/gate.rs, the one predicate; change them together.
guarded='(pane[[:space:]]+(split|move|close)|tab[[:space:]]+(create|close)|workspace[[:space:]]+(create|close)|agent[[:space:]]+start)'
outward='(git[^"]*[[:space:]]push([^a-z]|$)|gh[[:space:]]+pr[[:space:]]+merge|gh[^"]*[[:space:]]api[^"]*(/merges?([^a-z]|$)|mergePullRequest))'

stdin=$(cat)
hit=''
grep -Eq "herdr.*$guarded" <<<"$stdin" && hit='pane/tab/workspace spawn+move+close'
grep -Eq "$outward" <<<"$stdin" && hit="${hit:+$hit and }outward action"

if [[ -n $hit ]]; then
  echo "quaqua gate: '$bin' not found — fallback match on $hit (fail-closed)" >&2
  echo "blocked: '$bin' is missing and '$hit' cannot be cleared by the fallback — install quaqua or set QUAQUA_BIN, then retry — rules/_all/fanout-agent-grid.md, AGENTS.md rule 3" >&2
  exit 2
fi

echo "quaqua gate: '$bin' not found — install quaqua or set QUAQUA_BIN; only pane/tab/workspace spawn+move+close, push and merge are blocked meanwhile" >&2
exit 0
