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
