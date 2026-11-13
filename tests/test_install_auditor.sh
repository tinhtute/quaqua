#!/usr/bin/env bash
# Sandbox installer test: no real user service manager, claude, or HOME touched.
# Requires python3 to parse the generated plist independently of the renderer.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
src="$(cd "$here/.." && pwd)/templates/workspace/scripts"
T=$(mktemp -d "${TMPDIR:-/tmp}/test-install-auditor.XXXXXX")
T=$(cd "$T" && pwd -P)
trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

root="$T/repo & <mac> \"quoted\" | back\\slash"
mkdir -p "$root/scripts/auditor" "$T/bin" "$T/claude & tools" "$T/home"
cp "$src/auditor/install-auditor.sh" "$root/scripts/auditor/"
cp -R "$here/../templates/workspace/scripts/auditor/systemd" "$root/scripts/auditor/"
if [[ -d $here/../templates/workspace/scripts/auditor/launchd ]]; then cp -R "$here/../templates/workspace/scripts/auditor/launchd" "$root/scripts/auditor/"; fi
modern_bash=""
for b in "$BASH" "$(command -v bash)" /opt/homebrew/bin/bash /opt/zerobrew/bin/bash /usr/local/bin/bash; do
  if [[ -x $b ]] && "$b" -c '(( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) ))' 2>/dev/null; then
    modern_bash="$b"
    break
  fi
done
[[ -n $modern_bash ]] || fail "no Bash >= 4.4 found on system for test harness"
ln -s "$modern_bash" "$T/bin/bash"  # proves the installer discovers, rather than hardcodes, Bash
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
printf 'FORBIDDEN launchctl %s\n' "$*" >> "$CALLS"
exit 99
EOF
cat > "$T/claude & tools/claude" <<'EOF'
#!/bin/sh
printf 'FORBIDDEN claude %s\n' "$*" >> "$CALLS"
exit 99
EOF
chmod +x "$T/bin/uname" "$T/bin/systemctl" "$T/bin/launchctl" "$T/claude & tools/claude"
cat > "$T/bin/quaqua" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$T/bin/quaqua"
run() {
  : > "$T/calls"
  rc=0
  TEST_OS=$1 CALLS="$T/calls" HOME="${TEST_HOME:-$T/home}" \
    PATH="$T/bin:$T/claude & tools:$PATH" \
    "$BASH" "$root/scripts/auditor/install-auditor.sh" > "$T/out" 2> "$T/err" || rc=$?
}

run Linux
[[ $rc -eq 0 ]] || fail "Linux: exit $rc: $(cat "$T/err")"
dst="$T/home/.config/systemd/user"
[[ -L $dst/quaqua-auditor.timer && $(readlink "$dst/quaqua-auditor.timer") == "$root/scripts/auditor/systemd/quaqua-auditor.timer" ]] \
  || fail "Linux: wrong timer symlink"
# The user manager starts the job with a bare PATH (no ~/.local/bin): the
# service must pin the install-time PATH so the harness CLI resolves.
service="$dst/quaqua-auditor.service"
[[ -f $service && ! -L $service ]] || fail "Linux: service must be a rendered file, not a symlink"
pinned=$(sed -n 's/^Environment="PATH=\(.*\)"$/\1/p' "$service")
[[ -n $pinned ]] || fail "Linux: service does not pin PATH: $(cat "$service")"
case ":$pinned:" in *":$T/claude & tools:"*) ;; *) fail "Linux: pinned PATH lacks the harness dir: $pinned" ;; esac
grep -qxF 'ExecStart=%h/.cargo/bin/quaqua auditor run' "$service" || fail "Linux: ExecStart changed: $(cat "$service")"
[[ $(cat "$T/calls") == 'systemctl --user daemon-reload' ]] \
  || fail "Linux: unexpected manager calls: $(cat "$T/calls")"
grep -q 'systemctl --user enable --now quaqua-auditor.timer' "$T/out" \
  || fail "Linux: no manual enable instruction"
# A pre-fix install left the service as a symlink into the checkout: the
# reinstall must replace the link, not write through it into the template.
ln -sfn "$root/scripts/auditor/systemd/quaqua-auditor.service" "$service"
run Linux
[[ $rc -eq 0 && -f $service && ! -L $service ]] || fail "Linux: reinstall left a symlinked service"
grep -q '^Environment=' "$root/scripts/auditor/systemd/quaqua-auditor.service.in" \
  && ! grep -q '^Environment="PATH=/' "$root/scripts/auditor/systemd/quaqua-auditor.service.in" \
  || fail "Linux: reinstall wrote through the symlink into the template"
echo 'ok: Linux -> rendered service pins PATH, timer symlink, daemon-reload, no enable/start'

run Darwin
[[ $rc -eq 0 ]] || fail "Darwin: exit $rc: $(cat "$T/err")"
plist="$T/home/Library/LaunchAgents/com.quaqua.auditor.plist"
[[ -f $plist ]] || fail 'Darwin: no LaunchAgents plist'
[[ -d $T/home/Library/Logs ]] || fail 'Darwin: log directory not created'
[[ ! -s $T/calls ]] || fail "Darwin: invoked a manager or claude: $(cat "$T/calls")"
python3 - "$plist" "$root" "$T/home" "$T/claude & tools" "$T/bin/bash" "$T/bin/quaqua" <<'PY'
import pathlib, plistlib, sys
plist, root, home, claude_dir, bash_bin, quaqua_bin = sys.argv[1:]
with open(plist, 'rb') as f:
    p = plistlib.load(f)
assert p['Label'] == 'com.quaqua.auditor', p
assert p['ProgramArguments'] == [quaqua_bin, 'auditor', 'run'], p
assert pathlib.Path(p['ProgramArguments'][0]).is_absolute(), p
assert p['StartCalendarInterval'] == [{'Hour': 7, 'Minute': 0}, {'Hour': 19, 'Minute': 0}], p
paths = p['EnvironmentVariables']['PATH'].split(':')
assert str(pathlib.Path(quaqua_bin).parent) in paths and claude_dir in paths, paths
assert '/usr/bin' in paths and '/bin' in paths, paths
for k in ['StandardOutPath', 'StandardErrorPath']:
    assert p[k] == home + '/Library/Logs/quaqua-auditor.log', p
    assert pathlib.Path(p[k]).is_absolute(), p
assert not p.get('RunAtLoad') and not p.get('KeepAlive'), p
print('ok: Darwin -> valid XML, absolute pinned argv, PATH, 07:00/19:00, external logs')
PY
grep -qF 'launchctl bootstrap' "$T/out" || fail 'Darwin: no manual bootstrap instruction'
grep -qF "$plist" "$T/out" || fail 'Darwin: manual bootstrap names the installed plist'
cp "$plist" "$T/first.plist"
run Darwin
cmp "$plist" "$T/first.plist" || fail 'Darwin: reinstall changed plist'
[[ ! -s $T/calls ]] || fail 'Darwin: reinstall called a manager'
echo 'ok: Darwin -> idempotent file-only install; bootstrap printed, never executed'

run FreeBSD
[[ $rc -eq 1 ]] || fail "unsupported OS: exit $rc, want 1"
grep -qi 'unsupported' "$T/err" || fail 'unsupported OS: no diagnostic'
[[ ! -s $T/calls ]] || fail 'unsupported OS: called a manager'
echo 'ok: unsupported OS -> clear refusal without manager calls'

# Refuse an old Bash before installing anything; the test harness still uses
# its own modern $BASH. This stand-in models a failing >=4.4 version probe.
rm "$T/bin/bash"
cat > "$T/bin/bash" <<'EOF'
#!/bin/sh
exit 1
EOF
chmod +x "$T/bin/bash"
TEST_HOME="$T/old-home" run Darwin
[[ $rc -eq 1 ]] || fail "old Bash: exit $rc, want 1"
grep -q 'Bash >= 4.4' "$T/err" || fail 'old Bash: no version diagnostic'
[[ ! -d $T/old-home && ! -s $T/calls ]] || fail 'old Bash: installed or activated anyway'
echo 'ok: Darwin -> refuses Bash below 4.4 before writing or calling a manager'

if [[ $(/bin/bash -c 'printf "%s" "$BASH_VERSION"') == 3.* ]]; then
  rm "$T/bin/bash"
  ln -s /bin/bash "$T/bin/bash"
  TEST_HOME="$T/system-bash-home" run Darwin
  [[ $rc -eq 1 ]] || fail "system Bash 3.2: exit $rc, want 1"
  grep -q 'Bash >= 4.4' "$T/err" || fail 'system Bash 3.2: no version diagnostic'
  [[ ! -d $T/system-bash-home && ! -s $T/calls ]] || fail 'system Bash 3.2: installed anyway'
  echo 'ok: Darwin -> real macOS system Bash 3.2 refused'
fi

# No claude on the install PATH: checked before any destination is created.
rm "$T/bin/bash" "$T/claude & tools/claude"
ln -s "$modern_bash" "$T/bin/bash"
# Only essential native tools are visible; no user's claude can leak in.
rc=0
TEST_OS=Darwin CALLS="$T/calls" HOME="$T/missing-claude-home" \
  PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  "$BASH" "$root/scripts/auditor/install-auditor.sh" > "$T/out" 2> "$T/err" || rc=$?
[[ $rc -eq 1 ]] || fail "missing claude: exit $rc, want 1"
grep -q 'claude must be on PATH' "$T/err" || fail 'missing claude: no diagnostic'
[[ ! -d $T/missing-claude-home ]] || fail 'missing claude: installed anyway'
echo 'ok: Darwin -> requires claude on install PATH before writing files'
# No quaqua on the install PATH: checked before any destination is created.
rm "$T/bin/quaqua"
cat > "$T/claude & tools/claude" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$T/claude & tools/claude"
rc=0
TEST_OS=Darwin CALLS="$T/calls" HOME="$T/missing-quaqua-home" \
  PATH="$T/bin:$T/claude & tools:/usr/bin:/bin:/usr/sbin:/sbin" \
  "$BASH" "$root/scripts/auditor/install-auditor.sh" > "$T/out" 2> "$T/err" || rc=$?
[[ $rc -eq 1 ]] || fail "missing quaqua: exit $rc, want 1"
grep -q 'quaqua must be on PATH' "$T/err" || fail 'missing quaqua: no diagnostic'
[[ ! -d $T/missing-quaqua-home ]] || fail 'missing quaqua: installed anyway'
echo 'ok: Darwin -> requires quaqua on install PATH before writing files'
echo PASS
