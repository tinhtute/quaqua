#!/bin/bash
# Scorer for `quaqua supervisor score --scorer <this file>`: prompt on stdin, one JSON object
# {"score":0|1|2,"quote":".."} on stdout. Any failure: exit 1, one line on stderr, nothing on stdout.
# env: SCORER_CLAUDE_BIN (default claude)  SCORER_CLAUDE_MODEL (default haiku)  SCORER_CLAUDE_TIMEOUT (s, default 120)
set -u
die() { echo "scorer_claude: $*" >&2; exit 1; }

BIN=${SCORER_CLAUDE_BIN:-claude}
MODEL=${SCORER_CLAUDE_MODEL:-haiku}
command -v "$BIN" >/dev/null 2>&1 || die "claude CLI not found: $BIN"
command -v python3 >/dev/null 2>&1 || die "python3 not found"

TMP=$(mktemp -d "${TMPDIR:-/tmp}/scorer_claude.XXXXXX") || die "cannot make a temp dir"
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/prompt" || die "cannot read the prompt on stdin"
[ -s "$TMP/prompt" ] || die "empty prompt on stdin"

SYS='You score one run against one rubric. Answer with exactly one JSON object and nothing else: {"score": 0|1|2, "quote": "<short evidence quoted from the run>"}. No prose, no code fence.'

# Bare answer: no tools, no slash commands, no MCP, no user/project settings or CLAUDE.md (run from an empty dir).
# stdin of the CLI is the prompt; its stderr goes to a file so only one line of ours reaches stderr.
(cd "$TMP" && exec "$BIN" -p --model "$MODEL" --system-prompt "$SYS" --tools "" \
  --disable-slash-commands --strict-mcp-config --setting-sources "" --no-session-persistence \
  < "$TMP/prompt" > "$TMP/out" 2> "$TMP/err") &
PID=$!
( sleep "${SCORER_CLAUDE_TIMEOUT:-120}"; kill "$PID" 2>/dev/null ) >/dev/null 2>&1 &
WD=$!
wait "$PID"; RC=$?
kill "$WD" 2>/dev/null; wait "$WD" 2>/dev/null
if [ "$RC" -ne 0 ]; then
  MSG=$(head -c 300 "$TMP/err" | tr '\n' ' ')
  die "claude exited $RC: ${MSG:-no stderr}"
fi

python3 -I - "$TMP/out" <<'PY' > "$TMP/json" 2> "$TMP/perr" || die "$(tr '\n' ' ' < "$TMP/perr")"
import json, sys
text = open(sys.argv[1], encoding="utf-8").read()
dec, i, obj = json.JSONDecoder(), 0, None
while True:
    i = text.find("{", i)
    if i < 0:
        break
    try:
        obj, _ = dec.raw_decode(text, i)
        break
    except ValueError:
        i += 1
if not isinstance(obj, dict):
    sys.exit("answer is not a JSON object: " + text[:120].replace("\n", " "))
s, q = obj.get("score"), obj.get("quote")
if isinstance(s, bool) or not isinstance(s, int) or s not in (0, 1, 2):
    sys.exit("score out of range or missing: %r" % (s,))
if not isinstance(q, str):
    sys.exit("quote is missing or not a string")
print(json.dumps({"score": s, "quote": q}, ensure_ascii=False))
PY
cat "$TMP/json"
