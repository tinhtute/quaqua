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
stdin=$(cat)
tool=$(grep -o '"tool_name"[[:space:]]*:[[:space:]]*"[^"]*"' <<<"$stdin" | head -1 | sed 's/.*:[[:space:]]*"\(.*\)"$/\1/')
why="not found"
if command -v "$bin" >/dev/null 2>&1; then
  # A binary from before the Write/Edit gate answers every non-Bash tool with the
  # pane-spawn block, which would lock a worker out of every file. Ask it about a
  # Write to a path no policy protects: a binary that knows the tool allows it
  # (exit 0), an older one blocks (exit 2). That is a behaviour probe, not a
  # parse of its message. An old binary falls through to the text rules below.
  stale=''
  case $tool in
  Write | Edit | MultiEdit | NotebookEdit)
    "$bin" gate >/dev/null 2>&1 <<<'{"tool_name":"Write","tool_input":{"file_path":"/dev/null/qq-gate-probe"},"cwd":"/"}' ||
      stale=1
    ;;
  esac
  if [[ -z $stale ]]; then
    # The hook JSON stays on stdin; `quaqua gate` blocks (exit 2) on empty or unparseable input.
    exec "$bin" gate <<<"$stdin"
  fi
  why="predates the Write/Edit gate"
fi

# The fallback guarded list mirrors `guarded_pair`, `check_git`, `check_gh`,
# `check_sqlite` and `check_herdr` in src/authority/gate.rs, the one predicate;
# change them together.
guarded='(pane[[:space:]]+(split|move|close|send-keys|send-text|run)|tab[[:space:]]+(create|close)|workspace[[:space:]]+(create|close)|agent[[:space:]]+start)'
outward='(git[^"]*[[:space:]]push([^a-z]|$)|gh[^"|;&]*[[:space:]]pr[[:space:]]+merge|gh.*[[:space:]]api.*(/merges?([^a-z]|$)|mergePullRequest|enablePullRequestAutoMerge|query=@)|git[^"]*[[:space:]]config.*[aA][lL][iI][aA][sS]\.[^[:space:]]*[[:space:]].*push|git.*[[:space:]]-c[[:space:]]*[\\"'"'"']*[aA][lL][iI][aA][sS]\.[^[:space:]]*push)'

# T3 approval list: branch delete, trunk rename/force-move, tag, release, history rewrite.
listed='(git[^"|;&]*[[:space:]]tag[[:space:]]+(-[asdfmu]|--(delete|annotate|sign|force)|--(sort|format)(=[^[:space:]]+|[[:space:]]+[^[:space:]]+)[[:space:]]+[^-[:space:]"\\]|[^-[:space:]"\\])|git[^"|;&]*[[:space:]]branch[^"|;&]*[[:space:]](-[a-zA-Z]*[dD][a-zA-Z]*|--delete)([[:space:]]|$|[\\"])|gh[^"|;&]*[[:space:]]release[[:space:]]+create|git[[:space:]]+(filter-branch|filter-repo)|git[^"|;&]*[[:space:]]update-ref[[:space:]].*refs/(remotes|tags|heads/(main|master))|git[^"|;&]*[[:space:]]branch[^"|;&]*[[:space:]](-[a-zA-Z]*[mMfcC][a-zA-Z]*|--move|--copy|--force)[[:space:]].*(main|master))'
mcp_outward=(
  '^mcp__plugin_tinyfish_tinyfish__(run_deep_research(_async)?|run_web_automation(_async)?|create_dataset|populate_dataset|top_up_wallet)$'
  '^mcp__claude_ai_Claude_Data__share_(connector|live_queries)$'
  '^mcp__claude_ai_(Asana|Atlassian|Box|Canva|Gmail|Google_Calendar|Google_Docs|Google_Drive|Google_Sheets|Google_Slides|Higgsfield|HubSpot|Intercom|Linear|Notion|Slack|monday_com)__.*(send|post|reply|publish|share|invite|comment|message|upload|generate)'
)

# gh api ref/contents writes: a mutating method, or a field flag (gh then POSTs).
gh_write="(-X|--method)[=[:space:]'\"\\\\]*(PATCH|PUT|POST|DELETE)"
gh_field='[[:space:]](-f|-F|--field|--raw-field|--input)[[:space:]=]'
trunk_flag='([[:space:]]-[a-zA-Z]*[dD][a-zA-Z]*|--delete)'
trunk_pre='[[:space:]"'"'"'/]'
trunk_name="$trunk_pre(main|master)([^a-zA-Z0-9_-]|\$)"
# the delete flag may follow the name (`git branch main -D`)
trunk_del="git.*[[:space:]]branch.*($trunk_flag.*$trunk_name|$trunk_pre(main|master)([^a-zA-Z0-9_;|&-][^;|&]*)?$trunk_flag)"

# Bash writes to the live instruction and memory files (the binary resolves the
# workspace root; the fallback matches the basename, a superset), and a grant
# verb sent through a raw socket client or inline interpreter code.
live_names='((AGENTS|SOUL|identity|USER)\.md|memory/rules\.md)([^a-zA-Z0-9_.-]|$)'
live_redirect=">[[:space:]]*[^[:space:]\";&|]*$live_names"
live_tools="(^|[^a-zA-Z0-9_.-])(tee|mv|cp|dd|install|truncate|rm|unlink|shred|ln|rsync|python[0-9.]*|node|ruby|perl|php)[[:space:]][^\"|;&]*$live_names"
live_sed="(^|[^a-zA-Z0-9_.-])sed[[:space:]][^\"|;&]*(-[a-zA-Z]*i|--in-place)[^\"|;&]*$live_names"
grant_door='(^|[^a-zA-Z0-9_.-])(socat|nc|ncat|netcat|curl|python[0-9.]*|node|ruby|perl|php|deno|bun|lua)[[:space:]]'

hit=''
case $tool in
mcp__*)
  # The same three regexes as MCP_OUTWARD in src/authority/gate.rs.
  for re in "${mcp_outward[@]}"; do
    [[ $tool =~ $re ]] && hit='MCP tool that sends, publishes or spends'
  done
  ;;
Write | Edit | MultiEdit | NotebookEdit)
  # Only the live instruction files; the content is never scanned. The binary
  # tells the live workspace copy from a template; the fallback cannot.
  grep -Eq '"(file|notebook)_path"[[:space:]]*:[[:space:]]*"(([^"]*/)?(AGENTS|SOUL|identity|USER)\.md|([^"]*/)?memory/rules\.md)"' <<<"$stdin" &&
    hit='write to an instruction file'
  ;;
*)
  grep -Eq "herdr.*$guarded" <<<"$stdin" && hit='pane/tab/workspace spawn+move+close'
  grep -Eq "$outward" <<<"$stdin" && hit="${hit:+$hit and }outward action"
  grep -Eq "$listed" <<<"$stdin" && hit="${hit:+$hit and }approval-list action"
  if grep -Eq 'gh.*[[:space:]]api.*(git/refs|/contents/)' <<<"$stdin" &&
    { grep -Eiq "$gh_write" <<<"$stdin" || grep -Eq "$gh_field" <<<"$stdin"; }; then
    hit="${hit:+$hit and }outward action"
  fi
  grep -Eq 'herdr.*report-metadata.*grid_role=' <<<"$stdin" && hit="${hit:+$hit and }grid_role token"
  # `.open` and `ATTACH` reopen a database read-write, past -readonly.
  if grep -Eq 'sqlite3.*qq\.db' <<<"$stdin" &&
    { ! grep -Eq 'sqlite3.*--?readonly' <<<"$stdin" || grep -Eiq '\.open|\battach\b' <<<"$stdin"; }; then
    hit="${hit:+$hit and }qq.db write"
  fi
  # Any write to or naming of the daemon's qq.db: a removal/copy/edit tool or an
  # interpreter that names it, or a redirect onto it.
  if grep -Eq '(rm|mv|cp|tee|sed|truncate|dd|install|unlink|shred|python[0-9.]*|node|ruby|perl|php)[[:space:]][^"|;&]*qq\.db|>[[:space:]]*[^[:space:]";&|]*qq\.db' <<<"$stdin"; then
    hit="${hit:+$hit and }qq.db write"
  fi
  if grep -Eq "$live_redirect|$live_tools|$live_sed" <<<"$stdin"; then
    hit="${hit:+$hit and }instruction or memory file write"
  fi
  if grep -Eq 'grant\.(full|tier|lane)' <<<"$stdin" && grep -Eq "$grant_door" <<<"$stdin"; then
    hit="${hit:+$hit and }grant verb"
  fi
  grep -Eq "$trunk_del" <<<"$stdin" && hit="${hit:+$hit and }main/master delete"
  ;;
esac

if [[ -n $hit ]]; then
  echo "quaqua gate: '$bin' $why — fallback match on $hit (fail-closed)" >&2
  echo "blocked: '$bin' $why and '$hit' cannot be cleared by the fallback — install or rebuild quaqua or set QUAQUA_BIN, then retry — rules/_all/fanout-agent-grid.md, AGENTS.md rule 3" >&2
  exit 2
fi

echo "quaqua gate: '$bin' $why — install or rebuild quaqua or set QUAQUA_BIN; only pane/tab/workspace spawn+move+close+input, push, merge, the approval list, outward MCP tools and instruction-file writes are blocked meanwhile" >&2
exit 0
