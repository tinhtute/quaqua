---
name: bug-fix
when: A reported defect to reproduce, root-cause, and fix with runtime evidence.
---

## Orchestrator

1. Frame the defect, reproduction conditions, and verify commands in the brief. Require commit staging where the failing reproduction lands before the fix in git history.
2. If two or more previous fixes in this class failed the same gate, stop patching and cite principle-attack-the-premise to test the underlying assumption first.
3. Keep the worker scoped to reproduction, root-cause isolation, and the minimal fix.
4. Review the failing-then-passing evidence on the target branch before approving or merging. Verify on the same surface; inconclusive or wrong-surface is not a pass. A green suite shows branch behavior, not bug absence.

## Worker

1. Reproduce the bug directly on the target surface with runtime evidence. Capture the failing observation before making changes. If two or more fixes sharing a premise have failed, stop and apply principle-attack-the-premise.
2. Binary-search and isolate the mechanism to find the root cause, citing principle-fix-root-causes. Trace the symptom back to the first point of failure rather than patching downstream effects.
3. Write an automated reproduction test asserting observable behavior rather than internal implementation details, citing principle-test-behavior-not-implementation. Stage commits so the failing reproduction lands before the fix in git history: the failing test first and the fix on top.
4. Implement the minimal fix addressing the root cause. Belt-and-suspenders that "might help" is a hypothesis, not a fix, and does not ship; when evidence refutes a hypothesis, revert what it motivated. The smallest change the evidence justifies ships, nothing more. Verify on the same surface so the original repro now passes. "Inconclusive" or wrong-surface is not a pass (flag it). Unit tests and targeted test suites show branch behavior, not bug absence.
5. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.
