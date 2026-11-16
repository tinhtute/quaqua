# Exhaust the Design Space

Build two to three competing prototypes and compare them side by side before committing to a novel interaction, architectural fork, or feel-driven design decision.

## Why

Choosing the first plausible design anchors the solution on an unexamined local optimum. Building competing alternatives cheaply explores the full tradeoff space, exposes hidden constraints, and grounds the final decision in observable evidence rather than debate.

## When

Facing a novel user interaction or architectural decision with no precedent in the codebase, or product design decisions where user experience depends on feel rather than pure logic.

## Rule

- **Design it more than once.** Require at least two structurally distinct candidates before synthesis, even when the first looks sufficient. Whole-shape alternatives, not point fixes inside one shape.
- **Build behind one switcher.** When comparing alternatives, place them behind a single switcher (flags, buttons, or inputs), with each variant clearly labeled.
- **Keep the exploration throwaway and cheap.** Build in an isolated scratch directory without production abstractions, frameworks, or test suites.
- **Compare candidates side by side on the matching surface.** The observation (rendered output, interaction feel, or recorded timing) is the evaluation.
- **Present alternatives, evidence, tradeoffs, and a recommendation.** Settle the decision before starting the production build.
