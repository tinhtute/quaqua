#!/usr/bin/env bash
# Usage: install-auditor.sh
#   Linux: symlink systemd units and reload the user manager (no enable/start).
#   macOS: render a launchd plist in ~/Library/LaunchAgents (no manager calls).
#   Prints the platform's activation command for the user to run manually.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd -P)"
case $(uname -s) in
  Darwin)
    bash_bin=$(command -v bash) \
      || { echo 'error: bash must be on PATH before installing the auditor' >&2; exit 1; }
    if [[ ! -x $bash_bin ]] || ! "$bash_bin" -c \
      '(( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) ))'; then
      echo 'error: auditor requires Bash >= 4.4 on PATH (macOS /bin/bash 3.2 is too old)' >&2
      exit 1
    fi
    bash_bin="$(cd "$(dirname "$bash_bin")" && pwd -P)/$(basename "$bash_bin")"
    claude=$(command -v claude || true)
    if [[ -z $claude ]]; then
      claude=$(command -v opencode || true)
    fi
    if [[ -z $claude ]]; then
      echo 'error: claude must be on PATH before installing the auditor' >&2
      exit 1
    fi
    claude="$(cd "$(dirname "$claude")" && pwd -P)/$(basename "$claude")"
    quaqua_bin=$(command -v quaqua || true)
    if [[ -z $quaqua_bin ]]; then
      echo 'error: quaqua must be on PATH before installing the auditor' >&2
      exit 1
    fi
    quaqua_bin="$(cd "$(dirname "$quaqua_bin")" && pwd -P)/$(basename "$quaqua_bin")"
    # launchd does not source shell profiles. Keep the install-time toolchain
    # (e.g. node/mise) as well as Bash and the directory containing claude.
    job_path="$(dirname "$quaqua_bin"):$(dirname "$bash_bin"):$(dirname "$claude"):${PATH:-/usr/bin:/bin}:/usr/bin:/bin:/usr/sbin:/sbin"
    dst="$HOME/Library/LaunchAgents"
    plist="$dst/com.quaqua.auditor.plist"
    mkdir -p "$dst" "$HOME/Library/Logs"
    # Escape XML text first, then escape sed's replacement metacharacters.
    # This preserves paths with spaces, &, quotes, and the chosen delimiter.
    xml_replacement() {
      printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' \
        -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g" \
        -e 's/[\\&|]/\\&/g'
    }
    tmp=$(mktemp "$dst/.com.quaqua.auditor.XXXXXX")
    trap 'rm -f "$tmp"' EXIT
    sed -e "s|@ROOT@|$(xml_replacement "$root")|g" \
        -e "s|@HOME@|$(xml_replacement "$HOME")|g" \
        -e "s|@BASH@|$(xml_replacement "$bash_bin")|g" \
        -e "s|@QUAQUA@|$(xml_replacement "$quaqua_bin")|g" \
        -e "s|@PATH@|$(xml_replacement "$job_path")|g" \
      "$root/scripts/auditor/launchd/com.quaqua.auditor.plist.in" > "$tmp"
    chmod 644 "$tmp"
    mv -f "$tmp" "$plist"
    # %q also keeps the printed command safe for paths containing spaces.
    printf 'installed %s — activate manually with:\n' "$plist"
    printf 'launchctl bootstrap gui/%s %q\n' "$(id -u)" "$plist"
    ;;
  Linux)
    src="$root/scripts/auditor/systemd"
    dst="$HOME/.config/systemd/user"
    mkdir -p "$dst"
    for unit in quaqua-auditor.service quaqua-auditor.timer; do
      ln -sfn "$src/$unit" "$dst/$unit"
    done
    systemctl --user daemon-reload
    for unit in quaqua-auditor.service quaqua-auditor.timer; do
      if [[ ! -e "$dst/$unit" ]]; then
        echo "error: unit symlink $dst/$unit does not resolve (target $src/$unit is missing)" >&2
        exit 1
      fi
      resolved="$(readlink -f "$dst/$unit" 2>/dev/null || readlink "$dst/$unit")"
      echo "unit $unit: resolved -> $resolved"
    done
    if command -v systemctl >/dev/null 2>&1; then
      systemctl --user list-unit-files 'quaqua-auditor*' || true
    fi
    echo "installed into $dst — enable with: systemctl --user enable --now quaqua-auditor.timer"
    ;;
  *) echo 'error: unsupported OS (expected Darwin or Linux)' >&2; exit 1 ;;
esac
