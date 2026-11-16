# Subtract Before You Add

When evolving a system, remove complexity first, then build. Default to subtraction.

## Why

Adding new code on top of dead weight, redundant validators, or obsolete abstractions compounds technical debt and increases reader load. Simplifying the base first makes the addition smaller and safer.

## When

Sequencing an addition, refactor, or rewrite. Remove dead code, redundant validators, and stub references first, then build on the simpler base.

## Rule

- Sequence removal before construction.
- Cut before you polish (get to the minimum before investing in quality).
- Design for observed usage, not speculative edge cases.
- Avoid speculative validators, parsers, or guards beyond what the spec demands.
- When a reference has no novel content, delete it rather than leaving a stub.
- Leave the design slightly simpler and more capable behind the same or smaller surface than you found it.
