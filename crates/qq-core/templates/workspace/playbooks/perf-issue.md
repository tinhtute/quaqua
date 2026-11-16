---
name: perf-issue
when: A measured slowness to trace and improve against a baseline.
---

## Orchestrator

1. You own the measurement story: plan, review, and verify the numbers. Tie every fix to an observed measurement rather than reading source instead of measuring.
2. Demand baseline numbers, post-fix numbers, and deltas grounded per principle-explain-the-number. Ask "why not double?" and ensure the limiter is identified from profiles or system counters, not guesses. "Inconclusive" or wrong-surface is not a pass.
3. If the fix crosses a function boundary, spawn a design worker (`quaqua worker spawn --purpose design`) to explore the design before implementing. For sustained improvement against a metric across multiple hypotheses rather than a one-off fix, route to `hillclimb`.

## Worker

1. Capture a baseline trace on the target surface with runtime evidence. Vet the baseline and each later number per principle-explain-the-number, identifying the limiter bounding the result and ruling out measurement noise, skipped work, or cached runs.
2. Try the performance mantras in order, cheapest first:
   - Don't do it. Stop work whose result nothing uses rather than cheapening it.
   - Do it, but don't do it again.
   - Do it less.
   - Do it later.
   - Do it when they're not looking.
   - Do it concurrently.
   - Do it cheaper.
   When an earlier mantra meets the target, stop.
3. Plan and implement the minimal fix from the trace. Capture a post-fix trace on the same surface. Apply principle-sequence-verifiable-units, verifying each attempt before trying the next.
4. Parse and compare the artifacts (diff, metrics). "Inconclusive" or wrong-surface is not a pass; flag it. Return the baseline number, post-fix number, delta, and artifact path.
5. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.
