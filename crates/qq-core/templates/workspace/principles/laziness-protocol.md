# Laziness Protocol

Bias toward deletion and the smallest change that solves the problem.

## Why

Unnecessary layers, speculative abstractions, and signal threading add ongoing maintenance costs without adding capability. Minimal diffs are easier to review, verify, and maintain.

## When

Refactoring, evaluating diff size, or tempted to add abstractions, layers, or signal threading.

## Rule

- **Consolidate decisions.** Do not repeat the same choice in several places. Put it behind one source of truth and pass the result as a simple flag.
- **Minimize the diff.** Make the smallest change that solves the problem. Fewer lines beat boilerplate.
