# Experience First

When implementation convenience conflicts with user delight, choose delight.

## Why

A feature that is easy to build and awkward to use is a cost passed to every user. The maintainer and the caller are users too, and the next engineer inherits whatever shape ships.

## When

Product, UX, or feature-scope tradeoffs come up.

## Rule

- Every feature, control, and option must be justified.
- Ship less, ship better: a polished experience with three features beats a rough one with ten.
- Prototype before committing, because design decisions are cheaper in a throwaway prototype than in production code.
- Get the details right: transitions, alignment, spacing, feedback, error states.
- Tighten the core loop: every feature serves the central workflow or gets out of the way.

The user is whoever consumes the work. For a UI that is the end user. For a library or internal API it is the colleague who imports it. The engineer who maintains the code next is a user too. Weigh their experience the same way, and explain impact from their perspective.

Foundations should serve the experience. Foundational thinking (`principle-foundational-thinking`) governs the sequence of work. This principle governs the target.
