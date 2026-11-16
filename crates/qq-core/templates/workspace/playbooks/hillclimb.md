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

1. Ground the workload and architecture before fixing the metric. Identify realistic workload dimensions that move the result (data size, history, state, concurrency), and select a case that reproduces the complaint. If no case reproduces it, fix the repro instead of hillclimbing. Establish a single measurable metric, direction of improvement, and a checkable stop predicate that pairs a target with a floor on attempts so a lucky early win cannot end the run.
2. Build the measurement harness, prove its sensitivity on contrasting realistic workloads, and freeze it per principle-build-the-lever. Confirm the target case reproduces the symptom while easier cases separate as expected. If the harness cannot distinguish them, revise the workload or metric. Make the harness output error counts and units of work completed. Once frozen, one repeatable command emits the metric, sampled enough to clear the noise (median of N samples, not a single run). Record the baseline metric and ensure the regression gate (tests that must keep passing) is green before touching code.
3. Open a decision log (`decision.tsv` outside tracked source or gitignored): id, hypothesis, change, before, after, delta, tests, verdict (kept or reverted), note. Consult it before each attempt.
4. Ground each hypothesis in a concrete mechanism rather than speculative tweaks. Order hypotheses by the performance mantras:
   - Don't do it. Stop work whose result nothing uses rather than cheapening it.
   - Do it, but don't do it again.
   - Do it less.
   - Do it later.
   - Do it when they're not looking.
   - Do it concurrently.
   - Do it cheaper.
5. Loop, one hypothesis per iteration:
   - Make the minimal change for the hypothesis.
   - Measure before and after with the frozen harness, and verify regression tests pass.
   - Accept only when the metric improves beyond noise, behavior is preserved, and tests remain green; otherwise revert the change completely.
   - Create one commit per accepted fix, staging only modified files. Log the outcome row in `decision.tsv`.
   - Each iteration must verify its unit before the next begins per principle-sequence-verifiable-units.
6. Push past the first plateau. On a stall or several rejects in a row, pivot category, combine near-misses, re-read the source, or try something more radical before concluding the hill is climbed. Correctness and simplicity outrank the number. Stop when the target predicate is satisfied or when remaining ideas are marginal and not worth their cost. Don't relax the predicate to meet it, and don't quit while cheap untried hypotheses remain. If you are stuck, surface it instead of spinning.
7. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.

**Reply:** the metric and target, baseline to final with the percent delta, iterations run (kept vs reverted), each accepted fix on one line, the worktree-local `decision.tsv` path (so the Orchestrator can collect it before `quaqua worker cleanup` removes the worktree), and the best idea you would try next if pushed further.
