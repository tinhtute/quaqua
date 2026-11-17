---
name: worktree-cleanup
when: Reclaiming local disk: audit git worktrees, classify by merge state, uncommitted work, and age, then remove through quaqua worker cleanup.
---

## Orchestrator

You own the deletion. Removal happens only through `quaqua worker cleanup <name> <repo>`, never a hand-run `git worktree remove` and never `rm -rf`. This is the one playbook that deletes user state with no code review to catch a slip, so the gates below are the review.

1. Spawn one audit worker for the snapshot and the classification. The audit is read-only; it deletes nothing.
2. Frame the question and the base. Record `df -h /` before anything else. The audit reads paths from `git worktree list`, never hand-typed, because a hand-typed sibling path misses one. The merge base is the project integration branch `quaqua/<slug>-integ`.
3. The bucket is advice, not permission. The live agent is the real artifact per principle-prove-it-works. Cross-check every candidate against `quaqua status` and `herdr agent list`, and read `runs/<name>.owner` where it exists. A live agent wins over every heuristic, including a `safe` bucket.
4. Pause on irreversible loss. `wip:N` means N tracked uncommitted edits and `scratch:N` N untracked files, and both are held back rather than removed: `quaqua worker cleanup` runs `git worktree remove` without `--force`, so such a worktree refuses to go, and clearing it by hand would be the force or recursive delete this playbook bans. Show the diff and name the files, then let the owner decide whether to commit the work or clear it; that decision is theirs, and the removal runs again once the worktree is clean. Clean, merged, and not in use proceeds; `wip`, `scratch`, and in-use pause.
5. Remove the confirmed set one at a time through `quaqua worker cleanup <name> <repo>`. It closes the worker's pane, removes the worktree, keeps branch `quaqua/<name>`, and deletes the worker's metadata files such as `runs/<name>.owner` and `runs/<name>.grid`; the report stays. It runs `git worktree remove` without `--force`, so a worktree with uncommitted or untracked files refuses to be removed; that refusal is the gate, not an error to work around. Never force the removal, and never clear the files yourself: step 4's decision by the owner is the only route to a clean worktree.
6. A worktree quaqua did not create is not named in `runs/<name>.owner` and does not sit at `.wt/<name>`, so `quaqua worker cleanup` cannot address it by name. Report it for the human to remove. Do not delete it by hand, and do not delete the branch: a branch delete needs anh iu's approval.
7. Confirm with `df -h /` and a fresh `git worktree list`. Review the audit's reasons and report. Nothing is removed on an unconfirmed path.

**Reply:** `df -h /` before and after with space reclaimed, the worktrees pruned with the `quaqua worker cleanup` name and path for each, and a one-line reason for every worktree held back (in use by which agent, uncommitted edits with the diff size, untracked files named, or not quaqua-managed).

## Worker

1. Snapshot before classifying. Record `df -h /` and `git worktree list --porcelain`. Build the audit as a script that reads those paths, never as hand-typed arguments, and prove it by rerunning it on one known worktree per principle-build-the-lever. Cite principle-encode-lessons-in-structure: the classification lives in the script, not in a paragraph that drifts.
2. Classify each worktree by size, age, merge state, uncommitted work, and owner. Size comes from `du -sh <path>`; age from `git -C <path> log -1 --format=%ct`; merge state from `git -C <path> merge-base --is-ancestor HEAD quaqua/<slug>-integ`; uncommitted work from `git -C <path> status --porcelain` with `wip:N` tracked and `scratch:N` untracked; owner from `quaqua status`, `herdr agent list`, and `runs/<name>.owner` where present. Emit one bucket per path: `safe`, `wip`, `scratch`, `in-use`, or `not-quaqua`.
3. Cross-check every in-use candidate against the live agent list and report each match with the agent name and worktree path. A live agent wins over every heuristic. Keep large outputs off the coordinator thread per principle-guard-the-context-window: summarize per worktree instead of inlining full status output.
4. Report the proposed set with the `quaqua worker cleanup` name for each path, the space each would reclaim, and the held-back list with one reason each. Delete nothing; removal is the Orchestrator's action.
5. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.

**Reply:** the audit command, the bucket table, the proposed removals with their `quaqua worker cleanup` name, the held-back list with a reason each, and the `df -h /` before value.
