#!/usr/bin/env bash
# Sandbox installer test for the github-ci poll timer: a temp HOME, a temp
# checkout, and stubs on PATH, so no real user service manager, gh, or HOME is
# touched. Requires python3 to parse the generated plist independently of the
# renderer.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
src="$(cd "$here/.." && pwd)/templates/workspace/scripts"
T=$(mktemp -d "${TMPDIR:-/tmp}/test-install-github-ci-poll.XXXXXX")
T=$(cd "$T" && pwd -P)
trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

root="$T/repo"
mkdir -p "$root/scripts/github-ci-poll" "$T/bin" "$T/home"
cp -R "$src/github-ci-poll/." "$root/scripts/github-ci-poll/"
git -C "$root" init -q
git -C "$root" remote add origin 'git@github.com:acme/widget.git'

cat > "$T/bin/uname" <<'EOF'
#!/bin/sh
printf '%s\n' "$TEST_OS"
EOF
cat > "$T/bin/systemctl" <<'EOF'
#!/bin/sh
printf 'systemctl %s\n' "$*" >> "$CALLS"
EOF
cat > "$T/bin/launchctl" <<'EOF'
#!/bin/sh
printf 'launchctl %s\n' "$*" >> "$CALLS"
EOF
cat > "$T/bin/quaqua" <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$T/bin/gh" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$T/bin/uname" "$T/bin/systemctl" "$T/bin/launchctl" "$T/bin/quaqua" "$T/bin/gh"

# run <os> [installer args...] — HOME and PATH are the sandbox's, never the user's.
run() {
  local os=$1
  shift
  rc=0
  TEST_OS=$os HOME="$T/home" CALLS="$T/calls" \
    PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    bash "$root/scripts/github-ci-poll/install-github-ci-poll.sh" "$@" >"$T/out" 2>"$T/err" || rc=$?
}
calls() { cat "$T/calls" 2>/dev/null || true; }
reset() { rm -rf "$T/home" "$T/calls" "$T/out" "$T/err"; mkdir -p "$T/home"; }

# ── Linux: default repo from the checkout's origin, 5-minute timer ──────────
reset
run Linux
[[ $rc -eq 0 ]] || fail "Linux: exit $rc: $(cat "$T/err")"
service="$T/home/.config/systemd/user/quaqua-github-ci-poll.service"
timer="$T/home/.config/systemd/user/quaqua-github-ci-poll.timer"
[[ -f $service ]] || fail 'Linux: no user service'
[[ -L $timer ]] || fail 'Linux: no timer symlink'
[[ $(readlink -f "$timer") == "$root/scripts/github-ci-poll/systemd/quaqua-github-ci-poll.timer" ]] \
  || fail "Linux: timer points at $(readlink -f "$timer")"
grep -qxF "ExecStart=$T/bin/quaqua source github-ci poll --repo acme/widget" "$service" \
  || fail "Linux: service does not poll the origin repo: $(cat "$service")"
grep -qxF 'OnCalendar=*:0/5' "$timer" || fail 'Linux: timer is not on a 5-minute cadence'
grep -qxF 'Persistent=true' "$timer" || fail 'Linux: timer does not replay a missed run'
[[ $(calls) == 'systemctl --user daemon-reload' ]] \
  || fail "Linux: unexpected manager calls: $(calls)"
grep -qF 'systemctl --user enable --now quaqua-github-ci-poll.timer' "$T/out" \
  || fail 'Linux: no manual enable instruction'
echo 'ok: Linux -> rendered service names the origin repo, 5-minute timer, daemon-reload only'

# Reinstalling converges: same bytes, one more reload, nothing else.
cp "$service" "$T/first.service"
run Linux
[[ $rc -eq 0 ]] || fail "Linux reinstall: exit $rc: $(cat "$T/err")"
cmp "$service" "$T/first.service" || fail 'Linux reinstall: service changed'
[[ $(calls) == 'systemctl --user daemon-reload
systemctl --user daemon-reload' ]] || fail "Linux reinstall: unexpected calls: $(calls)"
echo 'ok: Linux -> reinstall is byte-identical and only reloads the manager'

# ── Linux: --repo overrides the checkout's origin ───────────────────────────
reset
run Linux --repo other/project
[[ $rc -eq 0 ]] || fail "Linux --repo: exit $rc: $(cat "$T/err")"
grep -qxF "ExecStart=$T/bin/quaqua source github-ci poll --repo other/project" \
  "$T/home/.config/systemd/user/quaqua-github-ci-poll.service" \
  || fail "Linux --repo: service does not name the requested repo"
echo 'ok: Linux -> --repo wins over the origin remote'

# ── --dry-run: renders, writes nothing, calls no manager ────────────────────
reset
run Linux --dry-run
[[ $rc -eq 0 ]] || fail "dry-run: exit $rc: $(cat "$T/err")"
grep -qF -- '--repo acme/widget' "$T/out" || fail 'dry-run: rendered service has no --repo'
grep -qF "ExecStart=$T/bin/quaqua source github-ci poll" "$T/out" \
  || fail 'dry-run: rendered service has no ExecStart'
[[ ! -e $T/home/.config ]] || fail 'dry-run: wrote into HOME'
[[ ! -s $T/calls ]] || fail "dry-run: called a manager: $(calls)"
echo 'ok: --dry-run -> prints the unit, writes nothing, calls no manager'

# ── Darwin: LaunchAgent with the repo and a 300s interval ───────────────────
reset
run Darwin
[[ $rc -eq 0 ]] || fail "Darwin: exit $rc: $(cat "$T/err")"
plist="$T/home/Library/LaunchAgents/com.quaqua.github-ci-poll.plist"
[[ -f $plist ]] || fail 'Darwin: no LaunchAgents plist'
[[ -d $T/home/Library/Logs ]] || fail 'Darwin: log directory not created'
[[ ! -s $T/calls ]] || fail "Darwin: invoked a manager: $(calls)"
python3 - "$plist" "$T/bin/quaqua" "$T/bin" <<'PY' || fail 'Darwin: plist does not match the contract'
import plistlib, sys
plist, quaqua, bindir = sys.argv[1:4]
with open(plist, 'rb') as f:
    d = plistlib.load(f)
assert d['Label'] == 'com.quaqua.github-ci-poll', d['Label']
assert d['ProgramArguments'] == [quaqua, 'source', 'github-ci', 'poll', '--repo', 'acme/widget'], \
    d['ProgramArguments']
assert d['StartInterval'] == 300, d['StartInterval']
assert bindir in d['EnvironmentVariables']['PATH'].split(':'), d['EnvironmentVariables']['PATH']
assert d['StandardErrorPath'].endswith('/Library/Logs/quaqua-github-ci-poll.log'), d['StandardErrorPath']
PY
grep -qF 'launchctl bootstrap' "$T/out" || fail 'Darwin: no manual bootstrap instruction'
grep -qF "$plist" "$T/out" || fail 'Darwin: manual bootstrap names the installed plist'
cp "$plist" "$T/first.plist"
run Darwin
[[ $rc -eq 0 ]] || fail "Darwin reinstall: exit $rc: $(cat "$T/err")"
cmp "$plist" "$T/first.plist" || fail 'Darwin reinstall: plist changed'
[[ ! -s $T/calls ]] || fail 'Darwin reinstall: called a manager'
echo 'ok: Darwin -> plist carries --repo and StartInterval 300; bootstrap printed, never executed'

# ── Darwin --dry-run renders the same contract without writing ──────────────
reset
run Darwin --dry-run
[[ $rc -eq 0 ]] || fail "Darwin dry-run: exit $rc: $(cat "$T/err")"
grep -qF -- '<string>--repo</string>' "$T/out" || fail 'Darwin dry-run: no --repo argument'
grep -qF -- '<string>acme/widget</string>' "$T/out" || fail 'Darwin dry-run: no repo value'
grep -qF '<integer>300</integer>' "$T/out" || fail 'Darwin dry-run: no 300s interval'
[[ ! -e $T/home/Library ]] || fail 'Darwin dry-run: wrote into HOME'
echo 'ok: Darwin --dry-run -> prints the plist, writes nothing'

# ── the repo is derived from the origin URL, whatever its shape ─────────────
https_root="$T/https-repo"
mkdir -p "$https_root/scripts/github-ci-poll"
cp -R "$src/github-ci-poll/." "$https_root/scripts/github-ci-poll/"
git -C "$https_root" init -q
git -C "$https_root" remote add origin 'https://github.com/acme/widget.git'
reset
rc=0
TEST_OS=Linux HOME="$T/home" CALLS="$T/calls" \
  PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  bash "$https_root/scripts/github-ci-poll/install-github-ci-poll.sh" >"$T/out" 2>"$T/err" || rc=$?
[[ $rc -eq 0 ]] || fail "https origin: exit $rc: $(cat "$T/err")"
grep -qF -- '--repo acme/widget' "$T/home/.config/systemd/user/quaqua-github-ci-poll.service" \
  || fail 'https origin: repo not parsed from an https clone URL'
echo 'ok: https clone URL -> owner/name, no .git suffix'

# No origin and no --repo: refuse before writing anything, and say how.
no_origin="$T/no-origin"
mkdir -p "$no_origin/scripts/github-ci-poll"
cp -R "$src/github-ci-poll/." "$no_origin/scripts/github-ci-poll/"
reset
rc=0
TEST_OS=Linux HOME="$T/home" CALLS="$T/calls" \
  PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  bash "$no_origin/scripts/github-ci-poll/install-github-ci-poll.sh" >"$T/out" 2>"$T/err" || rc=$?
[[ $rc -eq 1 ]] || fail "no origin: exit $rc, want 1"
grep -q -- '--repo' "$T/err" || fail 'no origin: diagnostic does not name --repo'
[[ ! -e $T/home/.config && ! -s $T/calls ]] || fail 'no origin: installed anyway'
echo 'ok: no origin -> refuses and points at --repo'

# ── refusals: bad --repo, missing tools, unsupported OS ─────────────────────
reset
run Linux --repo noslash
[[ $rc -eq 2 ]] || fail "bad --repo: exit $rc, want 2"
grep -q 'must be <owner>/<name>' "$T/err" || fail 'bad --repo: no diagnostic'
[[ ! -e $T/home/.config ]] || fail 'bad --repo: installed anyway'

reset
run Linux --bogus
[[ $rc -eq 2 ]] || fail "unknown arg: exit $rc, want 2"

reset
# A PATH with coreutils but no gh: /usr/bin holds a real gh on many hosts, so
# the refusal is proven on a curated bin dir instead of a stripped PATH.
mkdir -p "$T/nogh-bin"
for t in dirname basename; do ln -sf "$(command -v "$t")" "$T/nogh-bin/$t"; done
cp "$T/bin/quaqua" "$T/nogh-bin/quaqua"
rc=0
TEST_OS=Linux HOME="$T/home" CALLS="$T/calls" PATH="$T/nogh-bin" \
  "$BASH" "$root/scripts/github-ci-poll/install-github-ci-poll.sh" --repo acme/widget \
  >"$T/out" 2>"$T/err" || rc=$?
[[ $rc -eq 1 ]] || fail "missing gh: exit $rc, want 1"
grep -q 'gh must be on PATH' "$T/err" || fail 'missing gh: no diagnostic'
[[ ! -e $T/home/.config ]] || fail 'missing gh: installed anyway'

reset
mkdir -p "$T/noquaqua-bin"
for t in dirname basename; do ln -sf "$(command -v "$t")" "$T/noquaqua-bin/$t"; done
rc=0
TEST_OS=Linux HOME="$T/home" CALLS="$T/calls" PATH="$T/noquaqua-bin" \
  "$BASH" "$root/scripts/github-ci-poll/install-github-ci-poll.sh" --repo acme/widget \
  >"$T/out" 2>"$T/err" || rc=$?
[[ $rc -eq 1 ]] || fail "missing quaqua: exit $rc, want 1"
grep -q 'quaqua must be on PATH' "$T/err" || fail 'missing quaqua: no diagnostic'
[[ ! -e $T/home/.config ]] || fail 'missing quaqua: installed anyway'

reset
run FreeBSD
[[ $rc -eq 1 ]] || fail "unsupported OS: exit $rc, want 1"
grep -qi 'unsupported' "$T/err" || fail 'unsupported OS: no diagnostic'
[[ ! -s $T/calls ]] || fail 'unsupported OS: called a manager'
echo 'ok: bad --repo, unknown arg, missing gh/quaqua, unsupported OS -> refuse before writing'

echo PASS
