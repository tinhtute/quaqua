# Boundary Discipline

Place validation, type narrowing, and error handling at system boundaries. Trust internal code unconditionally. Business logic lives in pure functions. The shell is thin and mechanical.

## Why

Scattered validation is noisy, redundant, and gives a false sense of safety. Keeping logic out of framework wiring makes it testable without the framework.

## When

Wiring validation, error handling, or framework adapters.

## Rule

- **At boundaries** (CLI arguments, config files, external APIs, network protocols): validate at parse time, return errors, parse raw data into domain types.
- **Inside the system:** typed data, error propagation, no re-validation, no deep nil checks. Trust the types.
- **Across the boundary:** expose domain concepts, not transport, storage, framework, or wire types. Keep general mechanism inside and special-purpose policy at the edge.
- **Logic:** pure functions with no framework dependency; parse, prompt construction, and scoring are pure transforms.

## The tests

- "Is this data crossing a system boundary right now?" If not, validation is redundant.
- "Can this be a pure function that the shell just calls?" If yes, extract it.
