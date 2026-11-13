#!/usr/bin/env bash
# Sandbox test for cleanup-lead.sh: the team workspace (label <slug>, not the
# caller's) is closed; a Lead in the caller's workspace or a foreign-labelled
# one gets only its pane closed; live Peers block the cleanup; dead Peer owner
# files do not; the INDEX.md owner cell returns to cos. herdr is stubbed on
# PATH and logs every call; the script runs against a temp repo root.
# Usage: tests/test_cleanup_lead.sh   (exit 0 = pass)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
src="$(cd "$here/.." && pwd)/scripts"
T=$(mktemp -d "${TMPDIR:-/tmp}/test-cleanup-lead.XXXXXX")
T=$(cd "$T" && pwd -P)
trap 'rm -rf "$T"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

root="$(cd "$here/.." && pwd)"
bin="${QUAQUA_BIN:-$root/target/debug/quaqua}"
(cd "$root" && cargo build --locked --quiet) || fail "cannot build quaqua"
[[ -x "$bin" ]] || fail "no quaqua binary at $bin"
export QUAQUA_BIN="$bin"

mkdir -p "$T/bin" "$T/projects" "$T/runs"
export QUAQUA_RUNS_DIR="$T/runs"
cat > "$T/INDEX.orig" <<'EOF'
| slug | repo | owner | status | one-line goal |
|------|------|-------|--------|---------------|
| baro | ~/Lab/baro | baro-lead | active | player |
| baro-web | ~/Lab/baro-web | baro-web-lead | active | site |
EOF

cat > "$T/bin/herdr" <<'STUB'
#!/usr/bin/env bash
echo "herdr $*" >> "$CALLS"
case "${1:-} ${2:-}" in
  "agent get")
    # $HERDR_ERR names fail with a transport error (stderr, rc 42);
    # $HERDR_ERRJSON names fail with a non-not_found JSON error on stderr;
    # $HERDR_GARBLE names 'succeed' with a non-JSON body (rc 0).
    case " ${HERDR_ERR:-} " in *" ${3:-} "*)
      echo 'temporary transport error' >&2; exit "${HERDR_ERR_RC:-42}" ;; esac
    case " ${HERDR_ERRJSON:-} " in *" ${3:-} "*)
      printf '{"error":{"code":"unavailable","message":"backend down"}}\n' >&2; exit 1 ;; esac
    case " ${HERDR_GARBLE:-} " in *" ${3:-} "*)
      echo '<html>502 Bad Gateway</html>'; exit 0 ;; esac
    if [[ " ${HERDR_LIVE:-} " == *" ${3:-} "* ]]; then
      printf '{"result":{"agent":{"name":"%s","pane_id":"%s","workspace_id":"%s"}}}\n' \
        "$3" "${LEAD_PANE:-w7:p1}" "${LEAD_PANE%%:*}"
    else
      echo '{"error":{"code":"agent_not_found"}}'; exit 1
    fi ;;
  "agent list")
    # Live names plus $HERDR_LISTED (listed although `agent get` says
    # agent_not_found); "cos" keeps the list non-empty.
    names="cos ${HERDR_LIVE:-} ${HERDR_LISTED:-}"
    printf '{"result":{"agents":['; sep=
    for n in $names; do printf '%s{"name":"%s"}' "$sep" "$n"; sep=,; done
    printf ']}}\n' ;;
  "workspace get") printf '{"result":{"workspace":{"workspace_id":"%s","label":"%s"}}}\n' "$3" "${WS_LABEL:-baro}" ;;
esac
exit 0
STUB
cat > "$T/bin/quaqua" <<'STUB'
#!/usr/bin/env bash
echo "FAIL: stale PATH quaqua invoked: $*" >&2
exit 99
STUB
chmod +x "$T/bin/herdr" "$T/bin/quaqua"
export HERDR_BIN_PATH="$T/bin/herdr"
export HERDR_SOCKET_PATH="$T/herdr.sock"
export PATH="$T/bin:$PATH"

run() {  # run <slug> [VAR=val...]; sets rc and $T/{out,err,calls}
  local slug=$1; shift
  : > "$T/calls"
  cp "$T/INDEX.orig" "$T/projects/INDEX.md"
  printf '# rules\n' > "$T/runs/baro-lead.rules.md"
  rc=0
  CALLS="$T/calls" PATH="$T/bin:$PATH" HERDR_BIN_PATH="$T/bin/herdr" HERDR_SOCKET_PATH="$T/herdr.sock" HERDR_PANE_ID=w9:p13 LEAD_PANE=w7:p1 QUAQUA_ROOT="$T" env "$@" \
    "$QUAQUA_BIN" lead cleanup "$slug" >"$T/out" 2>"$T/err" || rc=$?
}

# ── team workspace: closed, owner -> cos, rules dump removed ────────────────
printf 'baro-lead' > "$T/runs/sude-old.owner"   # dead Peer: not registered
run baro HERDR_LIVE=baro-lead
[[ $rc -eq 0 ]] || fail "team: exit $rc: $(cat "$T/err")"
grep -qx 'herdr workspace close w7' "$T/calls" || fail "team: workspace not closed: $(cat "$T/calls")"
! grep -q 'pane close' "$T/calls" || fail "team: pane closed instead of workspace"
grep -qE '^\| baro \| ~/Lab/baro \| cos \|' "$T/projects/INDEX.md" || fail "team: owner not reset"
grep -qE '^\| baro-web \| ~/Lab/baro-web \| baro-web-lead \|' "$T/projects/INDEX.md" \
  || fail "team: other row touched"
[[ ! -e $T/runs/baro-lead.rules.md ]] || fail "team: rules dump kept"
echo "ok: team workspace -> closed, owner reset to cos, dead Peer owner file ignored"

# ── Lead in the caller's workspace (old spawn): pane only, never the CoS ws ──
run baro HERDR_LIVE=baro-lead LEAD_PANE=w9:p1P
[[ $rc -eq 0 ]] || fail "caller ws: exit $rc: $(cat "$T/err")"
grep -qx 'herdr pane close w9:p1P' "$T/calls" || fail "caller ws: pane not closed: $(cat "$T/calls")"
! grep -q 'workspace close' "$T/calls" || fail "caller ws: closed the caller's workspace"
echo "ok: Lead in the caller's workspace -> only its pane closed"

# ── workspace not labelled <slug>: pane only ────────────────────────────────
run baro HERDR_LIVE=baro-lead WS_LABEL=other
[[ $rc -eq 0 ]] || fail "label: exit $rc: $(cat "$T/err")"
grep -qx 'herdr pane close w7:p1' "$T/calls" || fail "label: pane not closed"
! grep -q 'workspace close' "$T/calls" || fail "label: foreign workspace closed"
echo "ok: foreign-labelled workspace -> only the Lead pane closed"

# ── live Peer: refused before any close ─────────────────────────────────────
printf 'baro-lead' > "$T/runs/sude-live.owner"
printf 'cos' > "$T/runs/sude-cos.owner"
run baro "HERDR_LIVE=baro-lead sude-live sude-cos"
[[ $rc -eq 1 ]] || fail "live peer: exit $rc, want 1"
grep -q 'still owns live Peers: sude-live' "$T/err" || fail "live peer: unclear refusal: $(cat "$T/err")"
grep -q 'sude-cos' "$T/err" && fail "live peer: CoS worker counted as the Lead's Peer"
! grep -q 'close' "$T/calls" || fail "live peer: closed something despite refusal"
cmp -s "$T/projects/INDEX.md" "$T/INDEX.orig" || fail "live peer: INDEX touched"
echo "ok: live Peer -> refused, nothing closed, INDEX untouched"

# ── no Lead / bad slug ──────────────────────────────────────────────────────
# A Lead with no agent and no trace (INDEX owner is cos, no runs/ files) is a typo.
sed 's/| baro-lead |/| cos |/' "$T/INDEX.orig" > "$T/INDEX.cos"
find "$T/runs" -name 'baro-lead*' -delete
: > "$T/calls"; cp "$T/INDEX.cos" "$T/projects/INDEX.md"; rc=0
CALLS="$T/calls" HERDR_PANE_ID=w9:p13 QUAQUA_ROOT="$T" "$QUAQUA_BIN" lead cleanup baro >"$T/out" 2>"$T/err" || rc=$?
[[ $rc -eq 1 ]] && grep -q "no live Lead 'baro-lead'" "$T/err" || fail "no lead: $(cat "$T/err")"
cmp -s "$T/projects/INDEX.md" "$T/INDEX.cos" || fail "no lead: INDEX touched"

# ── dead Lead orphan: no agent, INDEX still names it -> cleaned, owner -> cos ─
run baro
[[ $rc -eq 0 ]] || fail "orphan: exit $rc: $(cat "$T/err")"
grep -qE '^\| baro \| ~/Lab/baro \| cos \|' "$T/projects/INDEX.md" || fail "orphan: owner not reset"
grep -qE '^\| baro-web \| ~/Lab/baro-web \| baro-web-lead \|' "$T/projects/INDEX.md" || fail "orphan: other row touched"
[[ ! -e $T/runs/baro-lead.rules.md ]] || fail "orphan: rules dump kept"
! grep -q 'close' "$T/calls" || fail "orphan: closed something: $(cat "$T/calls")"
echo "ok: dead Lead orphan -> cleaned, INDEX owner back to cos, nothing closed"

# ── dead Lead that still owns a live Peer: refused ──────────────────────────
printf 'baro-lead' > "$T/runs/sude-live.owner"
run baro HERDR_LIVE=sude-live
[[ $rc -eq 1 ]] && grep -q 'still owns live Peers: sude-live' "$T/err" || fail "orphan+peer: $(cat "$T/err")"
cmp -s "$T/projects/INDEX.md" "$T/INDEX.orig" || fail "orphan+peer: INDEX touched"
echo "ok: dead Lead with a live Peer -> refused"

run Bad_slug
[[ $rc -eq 1 ]] && grep -q '^# Usage:' "$T/err" || fail "bad slug: $(cat "$T/err")"
[[ ! -s $T/calls ]] || fail "bad slug: herdr called"
echo "ok: missing Lead and bad slug -> refused"

run baro HERDR_LISTED=baro-lead
[[ $rc -eq 1 ]] && grep -q "cannot determine the state of Lead 'baro-lead'" "$T/err" \
  || fail "lead listed: $(cat "$T/err")"
! grep -q 'close' "$T/calls" || fail "lead listed: closed something"
echo "ok: Lead agent_not_found but still in agent list -> abort"

# ── Lead lookup failure is not "no Lead": abort, never a blind cleanup ──────
run baro HERDR_ERR=baro-lead
[[ $rc -eq 1 ]] || fail "lead err: exit $rc, want 1"
grep -q "cannot determine the state of Lead 'baro-lead'" "$T/err" \
  || fail "lead err: unclear abort: $(cat "$T/err")"
! grep -q 'close' "$T/calls" || fail "lead err: closed something: $(cat "$T/calls")"
cmp -s "$T/projects/INDEX.md" "$T/INDEX.orig" || fail "lead err: INDEX touched"
echo "ok: Lead lookup transport error -> abort (not 'no live Lead'), nothing touched"

# ── Peer liveness unknown -> abort: never close a workspace or rewrite INDEX ─
# A dead Peer is only a positively identified agent_not_found (covered above);
# every other `herdr agent get` failure leaves liveness unknown (F1).
printf 'baro-lead' > "$T/runs/sude-live.owner"
run baro HERDR_LIVE=baro-lead HERDR_ERR=sude-live
[[ $rc -eq 1 ]] || fail "peer err: exit $rc, want 1"
grep -q "cannot determine whether Peer 'sude-live' is still live" "$T/err" \
  || fail "peer err: unclear abort: $(cat "$T/err")"
! grep -q 'close' "$T/calls" || fail "peer err: closed something: $(cat "$T/calls")"
cmp -s "$T/projects/INDEX.md" "$T/INDEX.orig" || fail "peer err: INDEX touched"
echo "ok: Peer transport error -> abort, nothing closed, INDEX untouched"

run baro HERDR_LIVE=baro-lead HERDR_ERRJSON=sude-live
[[ $rc -eq 1 ]] || fail "peer errjson: exit $rc, want 1"
! grep -q 'close' "$T/calls" || fail "peer errjson: closed something: $(cat "$T/calls")"
cmp -s "$T/projects/INDEX.md" "$T/INDEX.orig" || fail "peer errjson: INDEX touched"
echo "ok: Peer non-not_found JSON error -> abort"

run baro HERDR_LIVE=baro-lead HERDR_GARBLE=sude-live
[[ $rc -eq 1 ]] || fail "peer garble: exit $rc, want 1"
! grep -q 'close' "$T/calls" || fail "peer garble: closed something: $(cat "$T/calls")"
cmp -s "$T/projects/INDEX.md" "$T/INDEX.orig" || fail "peer garble: INDEX touched"
echo "ok: Peer get rc 0 with a non-JSON body -> abort"

run baro HERDR_LIVE=baro-lead HERDR_LISTED=sude-live
[[ $rc -eq 1 ]] || fail "peer listed: exit $rc, want 1"
grep -q "cannot determine whether Peer 'sude-live' is still live" "$T/err" \
  || fail "peer listed: unclear abort: $(cat "$T/err")"
! grep -q 'close' "$T/calls" || fail "peer listed: closed something: $(cat "$T/calls")"
echo "ok: Peer agent_not_found but still in agent list -> abort (absence != broken)"
rm -f "$T/runs/sude-live.owner"

# ── concurrent cleanups on different INDEX rows must not lose updates ────────
# Both cleanups run concurrently on different rows. With the shared mkdir lock
# (projects/INDEX.md.lock.d), writes serialize without data loss or file corruption.
: > "$T/calls"
cp "$T/INDEX.orig" "$T/projects/INDEX.md"
ce=(CALLS="$T/calls" PATH="$T/bin:$PATH" HERDR_BIN_PATH="$T/bin/herdr" HERDR_SOCKET_PATH="$T/herdr.sock" HERDR_PANE_ID=w9:p13 LEAD_PANE=w7:p1 QUAQUA_ROOT="$T"
    HERDR_LIVE="baro-lead baro-web-lead")
env "${ce[@]}" "$QUAQUA_BIN" lead cleanup baro >"$T/out1" 2>"$T/err1" & p1=$!
env "${ce[@]}" "$QUAQUA_BIN" lead cleanup baro-web >"$T/out2" 2>"$T/err2" & p2=$!
rc1=0 rc2=0; wait $p1 || rc1=$?; wait $p2 || rc2=$?
[[ $rc1 -eq 0 && $rc2 -eq 0 ]] || fail "concurrent: exits $rc1/$rc2: $(cat "$T/err1" "$T/err2")"
grep -qE '^\| baro \| ~/Lab/baro \| cos \|' "$T/projects/INDEX.md" \
  || fail "concurrent: baro update lost: $(cat "$T/projects/INDEX.md")"
grep -qE '^\| baro-web \| ~/Lab/baro-web \| cos \|' "$T/projects/INDEX.md" \
  || fail "concurrent: baro-web update lost: $(cat "$T/projects/INDEX.md")"
find "$T/projects" -name 'INDEX.md.*' | grep -q . \
  && fail "concurrent: leftover tmp/lock files: $(find "$T/projects" -name 'INDEX.md.*')"
echo "ok: concurrent cleanups on different rows -> both updates kept, no torn file"

echo "PASS"
