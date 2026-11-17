# Migrate Callers Then Delete Legacy APIs

When introducing a new internal API while old callers still exist, migrate every caller and delete the old API in the same wave instead of preserving compatibility layers.

## Why

Compatibility shims and parallel old-and-new paths create ambiguity, double maintenance overhead, and inevitably linger as technical debt.

## When

Introducing a new internal API while old callers still exist: no external users depend on backward compatibility, and the project can absorb coordinated breaking changes as part of a simplification or refactor.

## Rule

- Migrate every caller in the same change wave.
- Delete the old API and its obsolete internal paths completely: no compatibility shims, no parallel old-and-new paths.
- Update tests to assert the new contract, and delete tests that only protect pre-refactor implementation details.
