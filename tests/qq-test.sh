#!/usr/bin/env bash
# Run `cargo test --locked -p qq-core NAME` per name; fail when a name matches
# zero tests (plain cargo exits 0 on zero matches).
# Usage: tests/qq-test.sh NAME...
# QQ_TEST_CARGO_OUT=FILE: skip cargo and count the saved output (for self-checks).
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: tests/qq-test.sh NAME..." >&2
  exit 2
fi

count_passed() {
  awk '/^test result:/ { for (i = 1; i < NF; i++) if ($(i+1) ~ /^passed/) n += $i } END { print n + 0 }'
}

status=0
for name in "$@"; do
  if [ -n "${QQ_TEST_CARGO_OUT:-}" ]; then
    out="$(cat "$QQ_TEST_CARGO_OUT")"
  else
    out="$(cargo test --locked -p qq-core "$name" 2>&1)" || { printf '%s\n' "$out"; echo "FAIL: cargo test failed for '$name'" >&2; status=1; continue; }
  fi
  printf '%s\n' "$out"
  passed="$(printf '%s\n' "$out" | count_passed)"
  if [ "$passed" -eq 0 ]; then
    echo "FAIL: '$name' matched zero tests" >&2
    status=1
  fi
done
exit "$status"
