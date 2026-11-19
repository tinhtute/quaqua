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
# `gh` merges, and the U3 tripwires: alias push, `gh api` ref/contents writes,
# auto-merge, `query=@file`, pane input, `grid_role=` tokens, `sqlite3` on
# qq.db without -readonly, deleting main/master). That is a conservative superset — it
# also blocks argument text such as `echo "herdr pane split"` — which errs
# toward blocking while the binary is unavailable. The binary is the precise
# path (it tokenizes the command).
# `h\e\rdr`-style escaping is the one known miss; install the binary for exact
# semantics.
set -uo pipefail

bin=${QUAQUA_BIN:-quaqua}
if command -v "$bin" >/dev/null 2>&1; then
  # The hook JSON stays on stdin; `quaqua gate` blocks (exit 2) on empty or unparseable input.
  exec "$bin" gate
fi

# The fallback guarded list mirrors `guarded_pair`, `check_git`, `check_gh`,
# `check_sqlite` and `check_herdr` in src/authority/gate.rs, the one predicate;
# change them together.
guarded='(pane[[:space:]]+(split|move|close|send-keys|send-text|run)|tab[[:space:]]+(create|close)|workspace[[:space:]]+(create|close)|agent[[:space:]]+start)'
outward='(git[^"]*[[:space:]]push([^a-z]|$)|gh[[:space:]]+pr[[:space:]]+merge|gh.*[[:space:]]api.*(/merges?([^a-z]|$)|mergePullRequest|enablePullRequestAutoMerge|query=@)|git[^"]*[[:space:]]config.*[aA][lL][iI][aA][sS]\.[^[:space:]]*[[:space:]].*push|git.*[[:space:]]-c[[:space:]]*[\\"'"'"']*[aA][lL][iI][aA][sS]\.[^[:space:]]*push)'

# gh api ref/contents writes: a mutating method, or a field flag (gh then POSTs).
gh_write="(-X|--method)[=[:space:]'\"\\\\]*(PATCH|PUT|POST|DELETE)"
gh_field='[[:space:]](-f|-F|--field|--raw-field|--input)[[:space:]=]'
trunk_flag='([[:space:]]-[a-zA-Z]*[dD][a-zA-Z]*|--delete)'
trunk_pre='[[:space:]"'"'"'/]'
trunk_name="$trunk_pre(main|master)([^a-zA-Z0-9_-]|\$)"
# the delete flag may follow the name (`git branch main -D`)
trunk_del="git.*[[:space:]]branch.*($trunk_flag.*$trunk_name|$trunk_pre(main|master)([^a-zA-Z0-9_-].*)?$trunk_flag)"

stdin=$(cat)
hit=''
grep -Eq "herdr.*$guarded" <<<"$stdin" && hit='pane/tab/workspace spawn+move+close'
grep -Eq "$outward" <<<"$stdin" && hit="${hit:+$hit and }outward action"
if grep -Eq 'gh.*[[:space:]]api.*(git/refs|/contents/)' <<<"$stdin" &&
  { grep -Eiq "$gh_write" <<<"$stdin" || grep -Eq "$gh_field" <<<"$stdin"; }; then
  hit="${hit:+$hit and }outward action"
fi
grep -Eq 'herdr.*report-metadata.*grid_role=' <<<"$stdin" && hit="${hit:+$hit and }grid_role token"
# `.open` and `ATTACH` reopen a database read-write, past -readonly.
if grep -Eq 'sqlite3.*qq\.db' <<<"$stdin" &&
  { ! grep -Eq 'sqlite3.*--?readonly' <<<"$stdin" || grep -Eiq '\.open|attach' <<<"$stdin"; }; then
  hit="${hit:+$hit and }qq.db write"
fi
grep -Eq "$trunk_del" <<<"$stdin" && hit="${hit:+$hit and }main/master delete"

if [[ -n $hit ]]; then
  echo "quaqua gate: '$bin' not found — fallback match on $hit (fail-closed)" >&2
  echo "blocked: '$bin' is missing and '$hit' cannot be cleared by the fallback — install quaqua or set QUAQUA_BIN, then retry — rules/_all/fanout-agent-grid.md, AGENTS.md rule 3" >&2
  exit 2
fi

echo "quaqua gate: '$bin' not found — install quaqua or set QUAQUA_BIN; only pane/tab/workspace spawn+move+close+input, push, merge and the tripwires are blocked meanwhile" >&2
exit 0
