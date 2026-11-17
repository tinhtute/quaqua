---
name: visual-parity
when: Pixel-exact UI equivalence: matching two implementations or migrating a styling system.
---

## Orchestrator

1. You own pixel-exact equivalence. The baseline is the spec: you do not touch it. Equivalence is verified by automated image diff, not by eye.
2. Anti-shortcut clauses, stated and held: no harness modifications, no baseline tampering, and no component restructuring to make a diff pass. If the baseline looks wrong, stop and ask or escalate; never edit the baseline without approval.
3. Headless only: all visual verification must run headless per rules/_all/no-gui-driving.md. Never drive the real display (no launching GUI apps, no moving windows, no synthetic input, no screenshots on the live display session). An isolated/headless display (e.g. Xvfb or headless browser) is permitted only when explicitly authorized in the brief.
4. When parallelizing across components, parallelize across worktrees, allocating separate worktrees with one owner per component per principle-separate-before-serializing-shared-state. Shared primitives migrate first as a blocking phase before dependent components.
5. Review each component's image diff evidence before accepting or merging via `quaqua merge`.

## Worker

1. Establish the baseline first before any migration: a visual regression harness that screenshots the current component across its states, plus the target when matching two implementations. No baseline, no parity claim: this is a blocking prerequisite, not a follow-up.
2. Anti-shortcut clauses, stated and held:
   - No harness modifications.
   - No baseline tampering.
   - No component restructuring to make a diff pass.
   - If the baseline looks wrong, stop and ask; do not edit it.
3. Migrate one component at a time within your assigned worktree per principle-separate-before-serializing-shared-state. Shared primitives migrate first as a blocking phase.
4. Verify each component against its baseline via automated image diff on the matching surface run headless only per rules/_all/no-gui-driving.md. Never drive the real display. A nonzero diff is a failure. Investigate the pixel delta and iterate per component until the diff is zero.
5. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.

**Reply:** components migrated, the diff result for each, the baseline harness location, what's left.
