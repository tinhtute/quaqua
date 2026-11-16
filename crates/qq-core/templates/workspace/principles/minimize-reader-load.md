# Minimize Reader Load

Maintainability is the work a reader must do to understand code. Track two independent axes: layers to trace (indirections between question and answer) and state to hold (hidden or mutable context in the reader's head).

## Why

Working memory is finite. Excessive layers, single-caller wrappers, pass-through abstractions, and wide mutable state make code impossible to trace and maintain.

## When

Reviewing or shaping code that is hard to trace. Count layers between question and answer, and hidden state in the reader's head; collapse one-caller wrappers and shrink mutable scope.

## Rule

- **Collapse layers** that cost more than they save: wrappers with one caller, adapters with no second implementation, speculative indirection that was never needed. Inline them.
- **Make adjacent layers change the abstraction.** A layer that repeats the same methods and arguments adds reader load without compression. Collapse pass-through layers.
- **Demand interface compression.** A broad interface that hides little complexity makes readers learn both the surface and the implementation. Prefer boundaries that hide meaningful decisions.
- **Shrink state scope:** prefer pure functions (returns over mutations), locals over fields, fields over module state, and module state over globals. Derive instead of sync.
- **Name the invariant at the boundary,** not in every consumer, so the reader learns it once.
- Before adding a layer or a piece of state, ask: does it reduce reader load elsewhere by at least as much?
- **The test:** can a new reader answer "where does X come from?" and "what can change X?" in under 30 seconds? If not, cut layers or cut state.
