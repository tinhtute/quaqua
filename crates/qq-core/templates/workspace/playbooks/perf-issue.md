---
name: perf-issue
when: A measured slowness to trace and improve against a baseline.
---

## Orchestrator

1. You own the measurement story: plan, review, and verify the numbers. Tie every fix to an observed measurement rather than reading source instead of measuring.
2. Demand baseline numbers, post-fix numbers, and deltas grounded per principle-explain-the-number. Ask "why not double?" and ensure the limiter is identified from profiles or system counters, not guesses. "Inconclusive" or wrong-surface is not a pass.
3. For sustained improvement against a metric across multiple hypotheses rather than a one-off fix, route to `hillclimb`.

## Worker
