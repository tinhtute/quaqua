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

1. Clarify the core domain model, state transitions, and boundary interfaces before writing business logic.
2. Design mutating operations and lifecycle steps to converge to the correct state across retries and restarts, citing principle-make-operations-idempotent.
3. Write tests that assert what users and callers observe through the contract, citing principle-test-behavior-not-implementation.
4. Implement the feature code. Verify on the matching surface; "inconclusive" or wrong-surface is not a pass (flag it). A green suite shows branch behavior, not absence of bugs. Confirm new and existing targeted test suites pass cleanly.
5. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.
