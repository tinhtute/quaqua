---
name: hillclimb
when: Sustained, scientific improvement of one metric against a target: loop hypotheses with before/after measurement, a decision log, and one commit per accepted win.
---

## Orchestrator

1. You own the metric and experimental integrity. Supervise, review, and delegate attempts. Hillclimb is for sustained, iterative improvement of one measurable metric against a target across multiple hypotheses; a one-off fix is `perf-issue` or `bug-fix`.
2. Core discipline: one change, one measurement, keep or revert. Never stack untested changes, and never claim a win from code inspection.
3. Protect context across iterations per principle-guard-the-context-window: review diffs and summaries rather than inlining large traces in the main thread.
4. When exploring multiple independent hypotheses concurrently, fan out to parallel workers in disjoint worktrees per principle-separate-before-serializing-shared-state, merging only accepted wins sequentially.
5. Correctness and simplicity outrank the number: revert a win that breaks behavior, and keep a simplification that holds the number per principle-laziness-protocol.
6. Before running `quaqua worker cleanup <name>`, collect the worker's worktree-local `decision.tsv` log from the path reported in the worker's Reply so the decision trail is preserved.

## Worker
