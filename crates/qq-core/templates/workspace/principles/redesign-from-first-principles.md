# Redesign from First Principles

When integrating a change, do not bolt it onto the existing design. Redesign as if the requirement had been a foundational assumption from the start.

## Why

Incremental patching around existing assumptions accumulates technical debt, increases code complexity, and locks in accidental architectural constraints.

## When

Integrating a new requirement into an existing design.

## Rule

- Read all affected files and understand the current design.
- Ask: "if we were writing this from scratch with this new requirement, what would we build?"
- Propagate the change through every reference: types, docs, examples, rationale sections.
- Think about the whole redesign, then deliver it incrementally.
