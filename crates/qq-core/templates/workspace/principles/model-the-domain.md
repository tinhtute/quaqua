# Model the Domain

Encode the real domain in a data structure instead of scattering it across conditionals. A structure that matches the domain makes invalid states unrepresentable and deletes branches. Choosing it at write time is cheap. Recovering it later reads as a refactor and gets deferred.

## Why

Scattered conditionals and booleans duplicate assumptions across files, invite desynchronization, and hide invalid states. A cohesive domain model makes illegal states unrepresentable and collapses branching.

## When

Writing stateful logic, or when code branches a lot or repeats a shape assumption across files.

## Rule

- Reach for a state machine instead of scattered booleans, phases, or lifecycle checks.
- Reach for a typed model instead of loose parameters or repeated shape assumptions.
- Reach for a map, registry, lookup table, or discriminated union instead of branching spread across files.
- Reach for a reducer or command/event model instead of ad hoc state mutations.
- Organize modules around one body of domain knowledge instead of a sequence such as load, validate, transform, and save. Execution order is not ownership.
- Maintain a small module boundary that gathers repeated behavior, ownership, or invariants.
- Reach for a queue, cache, index, graph, tree, or normalized collection where the access pattern calls for it.
- When none fits, work out what the code must never allow and how the data gets read, then find the structure that encodes exactly that.
- Do not force an abstraction. Prefer boring code if the current shape is already clear, local, and unlikely to grow. Be skeptical of an abstraction that adds indirection without removing branches, duplicated rules, invalid states, or lifecycle risk.
- Watch for warning signs: a new feature that grows an existing if/else chain by one more branch, a second boolean that must stay in sync with the first, or phase-named modules that repeat the same domain rules across steps.
