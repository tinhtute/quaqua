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
# the enumerated `gh` write subcommands). That is a conservative superset — it
# also blocks argument text such as `echo "herdr pane split"` — which errs
# toward blocking while the binary is unavailable. The binary is the precise
# path (its `git push` reads the hook cwd to resolve a refspec-less push; here
# every push is blocked, which is the fail-closed side of `ask`).
# `h\e\rdr`-style escaping is the one known miss; install the binary for exact
# semantics.
set -uo pipefail

bin=${QUAQUA_BIN:-quaqua}
# SessionStart has nothing to fence.
[[ ${1:-} == --session-start ]] && exit 0
if command -v "$bin" >/dev/null 2>&1; then
  # The hook JSON stays on stdin; `quaqua gate` blocks (exit 2) on empty or unparseable input.
  exec "$bin" gate
fi

# pane split | pane move | tab create | workspace create | workspace close |
# agent start — kept in step with authority/gate.rs.
guarded='(pane[[:space:]]+(split|move)|tab[[:space:]]+create|workspace[[:space:]]+(create|close)|agent[[:space:]]+start)'
# outward actions — kept in step with authority/gate.rs:
# any `git … push` (force/main checks need the tokenizer) and the enumerated
# gh writes. Read-only forms (git status, gh pr view, gh release list, …) fall
# through to the warning like any other command.
outward='(git[^"]*[[:space:]]push([^a-z]|$)|gh[[:space:]]+(pr[[:space:]]+(create|edit|merge|close|comment|ready)|issue[[:space:]]+(create|edit|close|comment)|release[[:space:]]+(create|edit|delete|upload)|repo[[:space:]]+(create|delete|fork|rename|edit|archive|unarchive|sync|set-default|transfer)))'

stdin=$(cat)
# pkill -f / --full: self-matches the calling shell and kills the session
pkill_f='(^|[[:space:]/;&|\"'\''])pkill[[:space:]]+.*(-[a-zA-Z0-9]*f|--full)'
if grep -Eq "$pkill_f" <<<"$stdin"; then
  echo "denied: pkill -f/--full self-matches the calling shell; use \`pgrep -f\` then kill the PID" >&2
  printf '{"decision":"deny","permissionDecisionReason":"pkill -f/--full is denied: self-matches the calling shell; use `pgrep -f` then kill the PID"}\n'
  exit 0
fi

hit=''
grep -Eq "herdr.*$guarded" <<<"$stdin" && hit='pane/workspace spawn+move'
grep -Eq "$outward" <<<"$stdin" && hit="${hit:+$hit and }outward action"

if [[ -n $hit ]]; then
  echo "quaqua gate: '$bin' not found — fallback match on $hit (fail-closed)" >&2
  echo "blocked: '$bin' is missing and '$hit' cannot be cleared by the fallback — install quaqua or set QUAQUA_BIN, then retry — rules/_all/fanout-agent-grid.md, AGENTS.md rule 3" >&2
  exit 2
fi

echo "quaqua gate: '$bin' not found — install quaqua or set QUAQUA_BIN; only pane/workspace spawn+move and outward actions are blocked meanwhile" >&2
exit 0
