#!/usr/bin/env bash
# Usage: install-github-ci-poll.sh [--repo <owner/name>] [--dry-run]
#
# Installs the 5-minute `quaqua source github-ci poll` timer:
#   Linux: renders the user service, symlinks the timer, reloads the user manager.
#   macOS: renders a launchd LaunchAgent plist in ~/Library/LaunchAgents.
# The unit carries the resolved `quaqua`, the repo, the install-time PATH and
# the workspace runs dir (`$QUAQUA_RUNS_DIR`, else `<checkout>/runs`), because
# both managers start it with a bare PATH and no shell profile.
# Nothing is enabled, started or bootstrapped: the installer prints the
# activation command for the user to run.
#
# --repo defaults to the origin remote of the checkout this script lives in
# (`<checkout>/scripts/github-ci-poll/`), so a clone of any fork schedules its
# own repository. --dry-run prints the rendered unit and writes nothing.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd -P)"

usage() {
  cat <<'EOF'
Usage: install-github-ci-poll.sh [--repo <owner/name>] [--dry-run]

  --repo <owner/name>  repository to poll (default: this checkout's origin remote)
  --dry-run            print the rendered unit, write nothing, call no manager
EOF
}

repo=''
dry_run=0
while (($#)); do
  case $1 in
    --repo)
      (($# >= 2)) || { echo 'error: --repo needs <owner/name>' >&2; exit 2; }
      repo=$2
      shift 2
      ;;
    --repo=*)
      repo=${1#--repo=}
      shift
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

# `owner/name` only: the unit interpolates it into an ExecStart word, so a
# path-shaped origin must be refused, not guessed at.
valid_repo() {
  [[ $1 =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || return 1
  local owner=${1%%/*} name=${1#*/}
  [[ $owner != . && $owner != .. && $name != . && $name != .. ]]
}

# `owner/name` from a clone URL: https://host/owner/name(.git),
# git@host:owner/name(.git), ssh://git@host/owner/name(.git). A local path is
# not a clone URL: it names no repository on a forge.
repo_from_url() {
  local url=$1
  url=${url%/}
  url=${url%.git}
  case $url in
    *://*) url=${url#*://}; url=${url#*/} ;;
    *@*:*) url=${url#*:} ;;
    *) return 1 ;;
  esac
  local name=${url##*/} owner=${url%/*}
  owner=${owner##*/}
  valid_repo "$owner/$name" || return 1
  printf '%s/%s\n' "$owner" "$name"
}

if [[ -z $repo ]]; then
  origin=$(git -C "$root" remote get-url origin 2>/dev/null) || origin=''
  [[ -n $origin ]] || {
    echo "error: no --repo given and $root has no git origin remote; pass --repo <owner/name>" >&2
    exit 1
  }
  repo=$(repo_from_url "$origin") || {
    echo "error: cannot derive <owner/name> from the origin remote ($origin); pass --repo <owner/name>" >&2
    exit 1
  }
fi
valid_repo "$repo" || {
  echo "error: --repo must be <owner>/<name>, got: $repo" >&2
  exit 2
}

quaqua_bin=$(command -v quaqua || true)
if [[ -z $quaqua_bin ]]; then
  echo 'error: quaqua must be on PATH before installing the github-ci poll timer' >&2
  exit 1
fi
quaqua_bin="$(cd "$(dirname "$quaqua_bin")" && pwd -P)/$(basename "$quaqua_bin")"

if ((!dry_run)); then
  # The timer is useless without gh, and a unit that fails every 5 minutes is
  # worse than a refusal at install time.
  if ! command -v gh >/dev/null 2>&1; then
    echo 'error: gh must be on PATH before installing the github-ci poll timer' >&2
    exit 1
  fi
fi

# Escape XML text first, then escape sed's replacement metacharacters.
xml_replacement() {
  printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' \
    -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g" \
    -e 's/[\\&|]/\\&/g'
}

# systemd units are plain text: only sed's replacement metacharacters matter.
plain_replacement() {
  printf '%s' "$1" | sed -e 's/[\\&|]/\\&/g'
}

# Both managers start the job with a bare PATH and no shell profile: pin the
# install-time PATH (so `gh` resolves) and the runs dir (so the poll records
# into this workspace's database, not `$HOME/quaqua`).
job_path="$(dirname "$quaqua_bin"):${PATH:-/usr/bin:/bin}:/usr/bin:/bin:/usr/sbin:/sbin"
runs_dir=${QUAQUA_RUNS_DIR:-$root/runs}

render() { # render <template> <quaqua> <repo> <path-env> <runs-dir> <escape-fn>
  sed -e "s|@QUAQUA@|$($6 "$2")|g" \
      -e "s|@REPO@|$($6 "$3")|g" \
      -e "s|@PATH@|$($6 "$4")|g" \
      -e "s|@RUNS_DIR@|$($6 "$5")|g" \
      -e "s|@HOME@|$($6 "$HOME")|g" \
      "$1"
}

# Write via a temp file and rename, so a crash never leaves a half-written unit
# in place and a reinstall with the same inputs is byte-identical.
tmp=''
trap '[[ -z $tmp ]] || rm -f "$tmp"' EXIT
install_rendered() { # install_rendered <destination>  (content on stdin)
  tmp=$(mktemp "$(dirname "$1")/.$(basename "$1").XXXXXX")
  cat >"$tmp"
  chmod 644 "$tmp"
  mv -f "$tmp" "$1"
  tmp=''
}

case $(uname -s) in
  Darwin)
    dst="$HOME/Library/LaunchAgents"
    plist="$dst/com.quaqua.github-ci-poll.plist"
    template="$root/scripts/github-ci-poll/launchd/com.quaqua.github-ci-poll.plist.in"
    if ((dry_run)); then
      render "$template" "$quaqua_bin" "$repo" "$job_path" "$runs_dir" xml_replacement
      exit 0
    fi
    mkdir -p "$dst" "$HOME/Library/Logs"
    render "$template" "$quaqua_bin" "$repo" "$job_path" "$runs_dir" xml_replacement \
      | install_rendered "$plist"
    # %q keeps the printed command safe for paths containing spaces.
    printf 'installed %s — activate manually with:\n' "$plist"
    printf 'launchctl bootstrap gui/%s %q\n' "$(id -u)" "$plist"
    ;;
  Linux)
    src="$root/scripts/github-ci-poll/systemd"
    dst="$HOME/.config/systemd/user"
    service="$dst/quaqua-github-ci-poll.service"
    timer="$dst/quaqua-github-ci-poll.timer"
    if ((dry_run)); then
      render "$src/quaqua-github-ci-poll.service.in" "$quaqua_bin" "$repo" "$job_path" "$runs_dir" plain_replacement
      exit 0
    fi
    mkdir -p "$dst"
    render "$src/quaqua-github-ci-poll.service.in" "$quaqua_bin" "$repo" "$job_path" "$runs_dir" plain_replacement \
      | install_rendered "$service"
    # The timer carries no per-install value: symlink it, like the auditor.
    ln -sfn "$src/quaqua-github-ci-poll.timer" "$timer"
    if [[ ! -e $timer ]]; then
      echo "error: timer symlink $timer does not resolve (target $src/quaqua-github-ci-poll.timer is missing)" >&2
      exit 1
    fi
    if command -v systemctl >/dev/null 2>&1; then
      systemctl --user daemon-reload
    else
      echo 'note: systemctl is not on PATH; reload the user manager before enabling the timer' >&2
    fi
    echo "unit quaqua-github-ci-poll.service: --repo $repo"
    echo "installed into $dst — enable with: systemctl --user enable --now quaqua-github-ci-poll.timer"
    ;;
  *) echo 'error: unsupported OS (expected Darwin or Linux)' >&2; exit 1 ;;
esac
