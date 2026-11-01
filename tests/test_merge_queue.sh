#!/usr/bin/env bash
# Sandbox test for quaqua merge (piggery refinery merge queue):
# 1. Merge queue: one branch at a time, notes integ commit, runs check, records new commit on pass.
# 2. Undo on failing check: if check fails, undos via reset --hard to noted commit, halts queue.
# 3. Check fails on integ base: halts queue with ask CoS message.
# 4. Conflict: merge --abort and returns rework to Peer.
# 5. Invariant: never pushes, never rewrites history beyond undoing last merge.
# Usage: tests/test_merge_queue.sh   (exit 0 = pass)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
bin="${QUAQUA_BIN:-$repo/target/debug/quaqua}"
if [[ ! -x "$bin" ]]; then
  if [[ -f "$repo/crates/quaqua/Cargo.toml" ]]; then
    cargo build --locked --manifest-path "$repo/crates/quaqua/Cargo.toml"
    bin="$repo/target/debug/quaqua"
  elif command -v quaqua >/dev/null 2>&1; then
    bin="$(command -v quaqua)"
  fi
fi
[[ -x "$bin" ]] || { echo "error: binary not found at $bin" >&2; exit 1; }

T=$(mktemp -d "${TMPDIR:-/tmp}/test-merge-queue.XXXXXX")
T=$(cd "$T" && pwd -P)
trap 'rm -rf "$T"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

# Setup sandbox project repo and team.toml
mkdir -p "$T/repo" "$T/projects" "$T/runs"
cp "$repo/templates/workspace/team.toml" "$T/team.toml"
export QUAQUA_ROOT="$T"
export QUAQUA_TEAM_TOML="$T/team.toml"
export HOME="$T"

cat > "$T/projects/INDEX.md" <<EOF
# Projects
| slug | repo | owner | status | one-line goal |
|---|---|---|---|---|
| myapp | $T/repo | cos | active | test merge queue |
EOF
echo "# myapp" > "$T/projects/myapp.md"

git -C "$T/repo" init -q
git -C "$T/repo" config user.email t@t
git -C "$T/repo" config user.name t
git -C "$T/repo" commit -qm "init" --allow-empty
base_commit=$(git -C "$T/repo" rev-parse HEAD)

# Create integration branch quaqua/myapp-integ
git -C "$T/repo" branch "quaqua/myapp-integ" "$base_commit"
git -C "$T/repo" checkout -q "quaqua/myapp-integ"

# Create two feature branches: branch-a (introduces a failing test) and branch-b (valid)
git -C "$T/repo" checkout -qb "feature-a" "$base_commit"
echo "FAIL" > "$T/repo/test_result.txt"
git -C "$T/repo" add test_result.txt
git -C "$T/repo" -c user.email=t@t -c user.name=t commit -qm "add feature a with failing test"
commit_a=$(git -C "$T/repo" rev-parse HEAD)

git -C "$T/repo" checkout -qb "feature-b" "$base_commit"
echo "PASS" > "$T/repo/feature_b.txt"
git -C "$T/repo" add feature_b.txt
git -C "$T/repo" -c user.email=t@t -c user.name=t commit -qm "add feature b"
commit_b=$(git -C "$T/repo" rev-parse HEAD)

git -C "$T/repo" checkout -q "quaqua/myapp-integ"

# Check script: fails if test_result.txt contains "FAIL"
cat > "$T/check.sh" << 'EOF'
#!/usr/bin/env bash
if [[ -f test_result.txt ]] && grep -q "FAIL" test_result.txt; then
  exit 1
fi
exit 0
EOF
chmod +x "$T/check.sh"

# ── 1. Undo on a failing check: feature-a fails check -> reset --hard, feature-b not merged ─
rc=0
"$bin" merge --check "$T/check.sh" --repo "$T/repo" myapp feature-a feature-b >"$T/out" 2>"$T/err" || rc=$?
[[ $rc -eq 2 ]] || fail "failing check: exit $rc, want 2 (rework to Peer)"
grep -q "with branch 'feature-a' merged into .* returning rework to Peer" "$T/err" \
  || fail "failing check: wrong error message: $(cat "$T/err")"

# Verify integ HEAD was restored to base_commit
curr_integ=$(git -C "$T/repo" rev-parse HEAD)
[[ "$curr_integ" == "$base_commit" ]] || fail "integ not reset to base_commit ($curr_integ != $base_commit)"
[[ ! -f "$T/repo/test_result.txt" ]] || fail "test_result.txt survived reset"
[[ ! -f "$T/repo/feature_b.txt" ]] || fail "feature_b was merged despite queue halt"
echo "ok: merge-queue undo on a failing check -> integ restored to base, queue halted"

# ── 2. Check fails on integ base without the branch -> ask CoS and hold queue ─────────────
cat > "$T/check_always_fail.sh" << 'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$T/check_always_fail.sh"

rc=0
"$bin" merge --check "$T/check_always_fail.sh" --repo "$T/repo" myapp feature-b >"$T/out" 2>"$T/err" || rc=$?
[[ $rc -eq 3 ]] || fail "integ base fail: exit $rc, want 3 (ask CoS and hold queue)"
grep -q "on integ base without 'feature-b' — ask CoS and hold queue" "$T/err" \
  || fail "integ base fail: wrong error message: $(cat "$T/err")"
curr_integ=$(git -C "$T/repo" rev-parse HEAD)
[[ "$curr_integ" == "$base_commit" ]] || fail "integ not reset after base failure"
echo "ok: check fails on integ base without the branch -> ask CoS and hold queue"

# ── 3. Merge conflict -> merge --abort and returns rework to Peer ──────────────────────────
# Create conflicting branch
git -C "$T/repo" checkout -qb "feature-conflict" "$base_commit"
echo "conflict" > "$T/repo/conflict.txt"
git -C "$T/repo" add conflict.txt
git -C "$T/repo" -c user.email=t@t -c user.name=t commit -qm "conflict branch"

git -C "$T/repo" checkout -q "quaqua/myapp-integ"
echo "base" > "$T/repo/conflict.txt"
git -C "$T/repo" add conflict.txt
git -C "$T/repo" -c user.email=t@t -c user.name=t commit -qm "base commit with conflict.txt"
integ_conflict_base=$(git -C "$T/repo" rev-parse HEAD)

rc=0
"$bin" merge --check "true" --repo "$T/repo" myapp feature-conflict >"$T/out" 2>"$T/err" || rc=$?
[[ $rc -eq 4 ]] || fail "conflict: exit $rc, want 4 (rework to Peer, distinct from operational errors)"
grep -q "merge conflict merging 'feature-conflict' into 'quaqua/myapp-integ' — returning rework to Peer" "$T/err" \
  || fail "conflict: wrong error message: $(cat "$T/err")"
curr_integ=$(git -C "$T/repo" rev-parse HEAD)
[[ "$curr_integ" == "$integ_conflict_base" ]] || fail "integ not clean after conflict abort"
echo "ok: merge conflict -> merge --abort (exit 4, distinct from operational 1, check 2, base 3)"

# ── 4. Primary checkout never moved & works when integ is checked out in another worktree ───
# Switch primary checkout to a different branch (e.g. master/main)
git -C "$T/repo" checkout -q -b "developer-working-branch" "$base_commit"

# Create an external worktree on integ
mkdir -p "$T/other-wt"
git -C "$T/repo" worktree add "$T/other-wt" "quaqua/myapp-integ"

rc=0
"$bin" merge --check "true" --repo "$T/repo" myapp feature-b >"$T/out" 2>"$T/err" || rc=$?
[[ $rc -eq 0 ]] || fail "happy with integ checked out elsewhere: exit $rc, want 0: $(cat "$T/err")"
grep -q "merged feature-b into quaqua/myapp-integ" "$T/out" || fail "happy: no merged line: $(cat "$T/out")"

# Verify primary checkout branch was NEVER moved from developer-working-branch
primary_branch=$(git -C "$T/repo" rev-parse --abbrev-ref HEAD)
[[ "$primary_branch" == "developer-working-branch" ]] || fail "primary checkout moved: $primary_branch != developer-working-branch"
echo "ok: primary checkout never moved ($primary_branch) & merge works when integ is checked out in another worktree"

# Verify commit was made on integ in the other worktree
[[ -f "$T/other-wt/feature_b.txt" ]] || fail "feature_b.txt not present on integ worktree"
git -C "$T/repo" worktree remove "$T/other-wt"

echo "PASS"
