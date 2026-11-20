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
