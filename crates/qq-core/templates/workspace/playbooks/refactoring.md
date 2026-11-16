---
name: refactoring
when: A behavior-preserving change to structure or shape (rename, extract, inline, dedupe, move).
---

## Orchestrator

1. You own the contract. The structure changes; the behavior does not. If cleanup reveals a missing feature or a bug, split it out and ship the structural change first against the pinned contract. A redesign is allowed, but name it and route to `feature`. Large or cross-cutting structural work belongs to `figure-it-out`.
2. Name the target shape and the structure the code is missing per principle-model-the-domain, principle-foundational-thinking, and principle-redesign-from-first-principles. Boring code stays when the shape is already clear and local. The reshape must delete branches or invalid states, not add indirection. If the target crosses a function boundary, spawn a design worker (`quaqua worker spawn --purpose design`) to explore the design before the move.
3. Review the proposed diff against principle-minimize-reader-load: confirm the reshape reduces reader load (fewer layers to trace, reduced mutable scope) without adding indirection or speculative abstraction. If the diff does not lower reader load somewhere, revert it.
4. Shape commits with principle-sequence-verifiable-units: a subtraction commit first, then the reshape, then follow-on cleanup, keeping each behavior-preserving slice green before the next.

## Worker

1. Pin the behavior contract first. Learn the contract over the affected subsystem, then write a characterization test, snapshot, or equivalence harness that captures current behavior before any structure moves. If the area has no coverage, write the pin before touching structure. Type check and lint are not a pin.
2. Subtract before you add per principle-subtract-before-you-add. Delete dead code, collapse one-caller wrappers, drop redundant validators, and remove orphan references before introducing the new shape. The smallest change that reaches the target shape ships per principle-laziness-protocol. A speculative cleanup that "might help" gets reverted.
3. Move in small behavior-preserving steps, each keeping the pin green. For API reshapes, migrate every caller and delete the old API in the same wave per principle-migrate-callers-then-delete-legacy-apis. No compatibility shims, no parallel old-and-new paths. Spot-check every rename against actual files. Renames silently miss usages in strings, prose, and back-references.
4. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works. Prove behavior is unchanged on the real artifact, not "it compiles". For larger reshapes, run an equivalence check: a script that diffs old-vs-new outputs, a recorded baseline replayed against the new code, or a smoke run on the matching surface.

**Reply:** the structure that changed, the pin you held it against, the equivalence proof, the reader-load delta, what shipped and what got reverted. No new behavior.
