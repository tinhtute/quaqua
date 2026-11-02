#!/usr/bin/env bash
# Test runner: executes every tests/test_*.sh sequentially, prints a
# PASS/FAIL count, and exits non-zero if any test failed.
# Requires Bash >= 4.4 (macOS system Bash 3.2 is insufficient), git, jq,
# Perl, python3, ripgrep, and a Rust toolchain (cargo/rustc — the crate's tests
# run below; a cold cargo registry cache needs network on the first run). Each
# test builds its own sandbox (temp HOME, stubs on PATH); no network, real
# agents, or services are touched by the sandboxed tests.
# The two slow suites (test_prompt_worker.sh, test_pipeline.sh — minutes each)
# are skipped unless the change touches a script they cover (slow_cover below),
# or they are forced: RUN_SLOW_TESTS=1 tests/run.sh  /  tests/run.sh --slow.
# "Touches" = the merge-base with origin/main diffed against the working tree
# (committed + uncommitted + untracked); with no origin/main, everything runs.
# Usage: tests/run.sh [--slow]   (exit 0 = all pass)
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
T=$(mktemp -d "${TMPDIR:-/tmp}/quaqua-tests.XXXXXX")
trap 'rm -rf "$T"' EXIT

# Scrub leaking parent shell env vars and set temp QUAQUA_RUNS_DIR and QUAQUA_TEST=1 once
unset QUAQUA_OWNER QUAQUA_HOME QUAQUA_ROOT HERDR_PANE_ID
export QUAQUA_TEST=1
export QUAQUA_RUNS_DIR="$T/runs"
mkdir -p "$T/runs"
pass=0 fail=0 failed=() skipped=()

# The files each slow suite exercises, space-separated: the suite runs only
# when one of them changed. A test's own file counts (editing a slow test must
# run it). test_prompt_worker.sh drives `quaqua worker prompt`, including its
# in-process pre-prompt grid check; test_pipeline.sh drives `quaqua pipeline`
# plus the worker steps it runs in-process.
slow_cover() {
  case "$1" in
    test_prompt_worker.sh)
      printf '%s' 'tests/test_prompt_worker.sh crates/quaqua/src/worker/prompt/' ;;
    test_pipeline.sh)
      printf '%s' 'tests/test_pipeline.sh crates/quaqua/src/pipeline/' ;;
  esac
}

force_slow=0 run_all=0 touched=''
[[ ${RUN_SLOW_TESTS:-} == 1 || ${1:-} == --slow ]] && force_slow=1
if (( ! force_slow )); then
  base=$(git -C "$here/.." merge-base HEAD origin/main 2>/dev/null) || base=''
  if [[ -n $base ]]; then
    touched=$( { git -C "$here/.." diff --name-only "$base"
                 git -C "$here/.." ls-files --others --exclude-standard; } 2>/dev/null ) || touched=''
  else
    echo "note: no origin/main merge-base — running every test, slow ones included" >&2
    run_all=1
  fi
fi

# Rust crate gate: the `quaqua` binary takes over from the shell engine, so its
# crate tests are part of this runner. Booked under cargo_test so the failure
# tail below picks up "$T/cargo_test.log" like any other case. --locked keeps the
# committed Cargo.lock authoritative: a manifest/lock drift fails here instead of
# being silently repaired by cargo.
if (cd "$here/.." && cargo test --locked) >"$T/cargo_test.log" 2>&1; then
  printf 'PASS %s\n' cargo_test
  pass=$((pass + 1))
else
  printf 'FAIL %s\n' cargo_test
  fail=$((fail + 1))
  failed+=(cargo_test)
fi

# Ensure quaqua binary is compiled and on PATH for shims
(cd "$here/.." && cargo build --locked --quiet)
export PATH="$here/../target/debug:$PATH"
export QUAQUA_BIN="${QUAQUA_BIN:-$here/../target/debug/quaqua}"

for t in "$here"/test_*.sh; do
  [[ -e $t ]] || { echo "no tests found in $here" >&2; exit 1; }
  name=$(basename "$t")
  cover=$(slow_cover "$name")
  if [[ -n $cover && $force_slow -eq 0 && $run_all -eq 0 ]]; then
    hit=''
    for f in $cover; do
      if grep -qF -- "$f" <<<"$touched"; then hit=$f; break; fi
    done
    if [[ -z $hit ]]; then
      printf 'SKIP %s (slow suite; nothing touched under: %s)\n' "$name" "$cover"
      skipped+=("$name")
      continue
    fi
    printf 'RUN  %s (slow suite; touched: %s)\n' "$name" "$hit"
  fi
  if bash "$t" >"$T/$name.log" 2>&1; then
    # A suite that skips itself (its subject is missing on this host, or was
    # replaced by the Rust binary) exits 0 but ran nothing: report it as SKIP,
    # never as a silent pass (finding 2, suhuynh-r6r1). Its cases must be mapped
    # in the test file. A suite declares the skip on its first line, either
    # "SKIP ..." or "skipped: <reason>".
    if grep -qE '^(SKIP|skipped:)' "$T/$name.log"; then
      reason=$(head -n1 "$T/$name.log")
      printf 'SKIP %s (%s)\n' "$name" "${reason#skipped: }"
      skipped+=("$name")
    else
      printf 'PASS %s\n' "$name"
      pass=$((pass + 1))
    fi
  else
    printf 'FAIL %s\n' "$name"
    fail=$((fail + 1))
    failed+=("$name")
  fi
done

# failing tests: surface their output (the tail is where the assertion died)
if ((${#failed[@]})); then
  for name in "${failed[@]}"; do
    printf '\n--- %s ---\n' "$name"
    tail -n 30 "$T/$name.log"
  done
fi

if ((${#skipped[@]})); then
  printf '\n%s passed, %s failed, %s skipped (of %s; slow suites run with --slow;\nself-skipping suites name their Rust replacements in the test file)\n' \
    "$pass" "$fail" "${#skipped[@]}" "$((pass + fail + ${#skipped[@]}))"
else
  printf '\n%s passed, %s failed (of %s)\n' "$pass" "$fail" "$((pass + fail))"
fi
(( fail == 0 ))
