# Boundary Discipline

Place validation, type narrowing, and error handling at system boundaries. Trust internal code unconditionally. Business logic lives in pure functions. The shell is thin and mechanical.

## Why

Scattered validation is noisy, redundant, and gives a false sense of safety. Keeping logic out of framework wiring makes it testable without the framework.

## When

Wiring validation, error handling, or framework adapters.

## Rule

- **At boundaries** (CLI arguments, config files, external APIs, network protocols): validate, return errors, handle defensively.
- **Inside the system:** typed data, error propagation, no re-validation. Trust the types.
- **Across the boundary:** expose domain concepts, not the boundary's private representation. Keep general-purpose mechanism inside and special-purpose policy at the edge.

### Validation and error handling

- Validate config at parse time, at the boundary, not inside business logic.
- Parse raw data into domain types at the boundary.
- Do not re-export transport, storage, framework, or wire types through the public surface.
- No redundant nil checks deep in call chains when the boundary already validated.

### Code organization

- Business logic lives in pure functions with no framework dependencies.
- Parse functions are pure transforms from raw bytes to typed state.
- Prompt construction takes structured state in and returns a string.
- Scoring and assessment are pure transforms from state to results.

### The tests

- "Is this data crossing a system boundary right now?" If not, validation is redundant.
- "Can this be a pure function that the shell just calls?" If yes, extract it.
