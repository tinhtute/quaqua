---
name: feature
when: New or changed behavior, built from a named data shape.
---

## Orchestrator

1. Define the desired behavior, interfaces, and data contracts in the brief before implementation begins.
2. Establish the throughput checkpoint across four dimensions: blocking first steps (gates run before fan-out), independent workstreams (disjoint files/layers parallelize; shared writes serialize), shared mutable state (separate before serializing; serialize only for real invariants), and smallest safe decomposition. A dimension that genuinely does not apply keeps its item with `n/a: <reason>` rather than being dropped.
3. Code-coupled work (one feature, one migration) goes to a single owner with the checkpoint inline; parent-level fan-out is reserved for slices that produce independent artifacts. Rewrite the checkpoint at phase boundaries.
4. Require tests that validate external behavior and boundary invariants.
5. Re-run verify checks on the matching surface before integrating or merging. "Inconclusive" or wrong-surface is not a pass (flag it); a green suite shows branch behavior, not bug absence.

## Worker
