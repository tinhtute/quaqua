#!/usr/bin/env bash
# Hermetic-workspace guard: the full test suite must never open the live
# workspace database at <passwd home>/quaqua/runs/quaqua.db*.
#
# The path is resolved the way the binary resolves it, not from $HOME alone:
# crates/quaqua/src/workspace.rs::system_user_home() reads the passwd entry for
# the current uid (getpwuid(getuid())->pw_dir) and uses $HOME only as a
# fallback. A tracker that trusted $HOME would report a false clean on any run
# with HOME overridden (CI isolation, a fixture, a sandbox).
#
# A decoy file is planted at the live DB path for the duration of the traced
# run: code that opens the live DB only when the file exists calls no open(2) on
# a missing path, so a trace from a host where the path is absent (CI) would
# never see it. An existing file is never overwritten - a real DB there is
# already the file this guard catches, and a decoy left by a crashed run is
# recognised by its marker and reused. The decoy and any directory this script
# created for it are removed on exit, so a repeat run finds nothing left to do.
#
# Usage:
#   tests/test_hermetic_db.sh                 trace `cargo test --locked`
#   tests/test_hermetic_db.sh --scan-only LOG replay a saved trace through the
#                                             same detector, no cargo run
#
# Exit: 0 - the suite never opened the live DB; on a host without strace the
#           guard prints one line, "skipped: strace unavailable", and exits 0
#       1 - one or more opens of the live DB (listed, aggregated), or a tracer
#           that produced no trace at all (a silent tracer must never pass)
#       N - the traced command's own status N when it fails without a hit
#
# Requires strace (Linux). HERMETIC_REQUIRE_STRACE=1 turns the missing-tracer
# skip into a failure; .github/workflows/test.yml sets it, so CI installs the
# tracer and runs the guard for real instead of silently skipping.
#
# Reproducing a detection (planted-test procedure, run in CI or any Linux box
# with strace):
#   1. cat >> crates/quaqua/tests/zz_planted_live_db.rs <<'RS'
#      use std::fs::OpenOptions;
#      #[test]
#      fn planted_open_of_live_workspace_db() {
#          let p = quaqua::workspace::live_workspace_db().expect("system user home");
#          OpenOptions::new().create(true).append(true).open(&p).expect("planted open");
#      }
#      RS
#   2. bash tests/test_hermetic_db.sh    # -> exit 1, lists the planted openat
#   3. rm crates/quaqua/tests/zz_planted_live_db.rs
#
# The detector can also be exercised without running the suite, by replaying a
# hand-built trace:
#   printf '%s\n' "1234  openat(AT_FDCWD, \"$HOME/quaqua/runs/quaqua.db\", O_RDWR) = 7" > trace.log
#   printf '%s\n' "1235  openat(AT_FDCWD, \"/tmp/other.db\", O_RDWR) = 8" >> trace.log
#   bash tests/test_hermetic_db.sh --scan-only trace.log   # -> exit 1, lists the first
set -uo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/.." && pwd)
require=${HERMETIC_REQUIRE_STRACE:-0}

usage() {
  printf 'usage: %s [--scan-only TRACE_LOG]\n' "${BASH_SOURCE[0]##*/}" >&2
}

# The passwd home of the current uid, or empty when the lookup is unavailable.
# getent reads the passwd database on Linux; tilde expansion on a literal name
# reads the same database through the shell, which covers hosts without getent.
passwd_home() {
  local home='' user=''
  if command -v getent >/dev/null 2>&1; then
    home=$(getent passwd "$(id -u)" 2>/dev/null | cut -d: -f6) || home=''
  fi
  if [[ -z $home ]]; then
    user=$(id -un 2>/dev/null) || user=''
    if [[ -n $user ]]; then
      home=$(eval "printf '%s' ~$(printf '%s' "$user" | tr -cd 'A-Za-z0-9._-')" 2>/dev/null) || home=''
    fi
  fi
  [[ $home == '~'* ]] && home=''
  printf '%s' "${home%/}"
}

# Prints every open of the live DB found in $1 as "<count>x <call>", and returns
# 1 when there is at least one. Over-matching is deliberate: `quaqua.db-journal`,
# `-wal` and `-shm` all sit under the same prefix, and a failed open (ENOENT,
# EACCES) is still an attempt to touch the live workspace.
scan_log() {
  local log=$1 total
  total=$(grep -cF -- "$live_db" "$log" 2>/dev/null) || total=0
  if [[ $total -eq 0 ]]; then
    return 0
  fi
  printf 'FAIL: %s open(s) of the live workspace database:\n' "$total"
  grep -F -- "$live_db" "$log" \
    | sed -E 's/^ *[0-9]+ +//; s/ = .*$//' \
    | sort | uniq -c | sort -rn \
    | sed -E 's/^ *([0-9]+) /\1x /'
  printf 'the suite must run against a temp workspace; see is_live_workspace_db in crates/quaqua/src/workspace.rs\n'
  return 1
}

# A failed run keeps its trace: the file is the evidence for the hit list, so
# it outlives the script and its path is printed.
keep_trace() {
  printf 'trace kept: %s\n' "$trace_log"
  workdir=''
}

# First line of a decoy file, so a run that crashed before its cleanup does not
# leave something indistinguishable from a real database.
decoy_marker='quaqua hermetic-db guard decoy (not a database)'

# Plants the decoy at $live_db and returns 1 when it cannot. Directories the
# planting needed are created one level at a time and remembered, so cleanup can
# remove exactly what this run made.
plant_decoy() {
  local dir=$live_db missing=() d tmp
  if [[ -e $live_db ]]; then
    if head -c 128 -- "$live_db" 2>/dev/null | grep -qF -- "$decoy_marker"; then
      decoy=$live_db
      printf 'note: reusing the decoy an earlier run left at %s\n' "$live_db"
    else
      printf 'note: a file already exists at %s; no decoy planted\n' "$live_db"
    fi
    return 0
  fi
  dir=${dir%/*}
  while [[ ! -d $dir ]]; do
    missing=("$dir" "${missing[@]}")
    dir=${dir%/*}
  done
  for d in "${missing[@]}"; do
    if ! mkdir -- "$d" 2>/dev/null; then
      printf 'error: cannot create %s for the decoy\n' "$d" >&2
      return 1
    fi
    created_dirs+=("$d")
  done
  tmp="$live_db.hermetic-decoy.$$"
  if ! printf '%s\n' "$decoy_marker" >"$tmp" || ! mv -f -- "$tmp" "$live_db"; then
    printf 'error: cannot write the decoy %s\n' "$live_db" >&2
    rm -f -- "$tmp"
    return 1
  fi
  decoy=$live_db
  printf 'planted decoy: %s (removed at exit)\n' "$live_db"
}

# Removes the decoy and the directories this run created for it. Anything else
# found in those directories is left in place and reported: it was not written
# by this guard, so it is evidence, not litter.
# shellcheck disable=SC2329  # reached through on_exit below
cleanup_decoy() {
  local d i
  if [[ -n $decoy ]]; then
    rm -f -- "$decoy" "$decoy".hermetic-decoy.* 2>/dev/null
    decoy=''
  fi
  for ((i = ${#created_dirs[@]} - 1; i >= 0; i--)); do
    d=${created_dirs[i]}
    if ! rmdir -- "$d" 2>/dev/null && [[ -d $d ]]; then
      printf 'note: %s is not empty; left in place\n' "$d" >&2
    fi
  done
  created_dirs=()
}

# shellcheck disable=SC2329  # installed as the EXIT trap
on_exit() {
  cleanup_decoy
  [[ -n $workdir ]] && rm -rf -- "$workdir"
  return 0
}

main() {
  local arg=${1:-}
  case $arg in
    --scan-only)
      if [[ $# -ne 2 || ! -f ${2:-} ]]; then
        usage
        return 1
      fi
      if scan_log "$2"; then
        printf 'ok: no open of %s in %s\n' "$live_db" "$2"
        return 0
      fi
      return 1
      ;;
    '') ;;
    *)
      usage
      return 1
      ;;
  esac

  if ! command -v strace >/dev/null 2>&1; then
    if [[ $require == 1 ]]; then
      printf 'error: strace is required (HERMETIC_REQUIRE_STRACE=1) but not on PATH\n' >&2
      return 1
    fi
    printf 'skipped: strace unavailable\n'
    return 0
  fi

  local status trace='open,openat'
  # workdir is script-scope so the EXIT trap can still see it after main returns.
  workdir=$(mktemp -d "${TMPDIR:-/tmp}/quaqua-hermetic.XXXXXX") || return 1
  trace_log=$workdir/strace.log

  # openat2 only when this strace knows the syscall, so an older tracer is a
  # narrower trace rather than an error.
  if strace -qq -e trace=openat2 true >/dev/null 2>&1; then
    trace='open,openat,openat2'
  fi

  printf 'tracing: cargo test --locked (live db: %s)\n' "$live_db"
  plant_decoy || return 1
  # -s 4096: strace truncates strings to 32 bytes by default, which would cut
  # every live path into ".../quaqua" and hide the hit. -f: follow each test
  # process; -qq: drop attach/exit noise; -o: keep stdout/stderr of the suite
  # intact, the live trace goes to its own file.
  ( cd "$repo" && strace -f -qq -s 4096 -e "trace=$trace" -o "$trace_log" cargo test --locked )
  status=$?

  if [[ ! -s $trace_log ]]; then
    printf 'error: strace wrote no trace to %s; the guard did not run\n' "$trace_log" >&2
    keep_trace
    return 1
  fi
  if scan_log "$trace_log"; then
    if [[ $status -ne 0 ]]; then
      printf 'note: cargo test also failed under strace (status %s)\n' "$status" >&2
      return "$status"
    fi
    printf 'ok: no open of %s during cargo test --locked\n' "$live_db"
    return 0
  fi
  if [[ $status -ne 0 ]]; then
    printf 'note: cargo test also failed under strace (status %s)\n' "$status" >&2
  fi
  keep_trace
  return 1
}

live_home=$(passwd_home)
if [[ -z $live_home && -n ${HOME:-} ]]; then
  live_home=${HOME%/}
fi
if [[ -z $live_home ]]; then
  printf 'error: cannot resolve the system user home (passwd and HOME both empty)\n' >&2
  exit 1
fi
live_db="$live_home/quaqua/runs/quaqua.db"
workdir=''
trace_log=''
decoy=''
created_dirs=()
# A crashed or interrupted run leaves nothing behind: the trace and the decoy
# live only for the duration of this script.
trap 'on_exit' EXIT

main "$@"
exit $?
