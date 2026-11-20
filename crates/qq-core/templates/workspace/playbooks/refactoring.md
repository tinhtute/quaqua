---
name: refactoring
when: A behavior-preserving change to structure or shape (rename, extract, inline, dedupe, move).
---

## Orchestrator

1. You own the contract. The structure changes; the behavior does not. If cleanup reveals a missing feature or a bug, split it out and ship the structural change first against the pinned contract. A redesign is allowed, but name it and route to `feature`. Large or cross-cutting structural work belongs to `figure-it-out`.
2. Name the target shape and the structure the code is missing per principle-model-the-domain, principle-foundational-thinking, and principle-redesign-from-first-principles. Boring code stays when the shape is already clear and local. The reshape must delete branches or invalid states, not add indirection.
3. Review the proposed diff against principle-minimize-reader-load: confirm the reshape reduces reader load (fewer layers to trace, reduced mutable scope) without adding indirection or speculative abstraction. If the diff does not lower reader load somewhere, revert it.
4. Shape commits with principle-sequence-verifiable-units: a subtraction commit first, then the reshape, then follow-on cleanup, keeping each behavior-preserving slice green before the next.

## Worker
