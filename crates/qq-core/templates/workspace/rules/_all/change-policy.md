# Change Policy

## 5 Questions Before Fixing a Mechanism
1. Outcome needed: what exact user or system outcome must be true?
2. What it served: why was the existing mechanism built that way?
3. Constraints: which constraints are real requirements vs implementation artifacts?
4. Keeping wrong behaviour? Are tests/docs pinning accidental bugs or obsolete behavior?
5. Resolution: patch, replace, or remove? Prefer removal over needless abstraction.

## Tests
- One per invariant or acceptance criterion.
- Bug fix starts with a reproduction that fails on old code.
- No tests for wording, format, or layout.
- Extend existing test cases before adding new test files.
- Report test lines added and removed in run summaries.

## Docs
- One home per fact; delete stale text immediately when cutover lands.
- Scope: project repos; quaqua `runs/`, `raw/`, and handoffs are evidence and exempt.
