---
name: session-pickup
when: Resuming or taking over a prior agent's in-flight work from the prior report, branch, and journal.
---

## Orchestrator

You own the resume point. Read the prior trail; do not redo completed work. The prior trail is authoritative input.

1. Locate the prior trail. Read the prior report (`runs/<name>.md` or `runs/<name>.resume.md`), recent worktree commits (`git log -n 5`, `git status`), and recent journal events (`quaqua status --all`). Summarize key decisions while keeping context minimal per principle-guard-the-context-window.
2. Reconstruct operational state: active worktree branch, what has already landed against base, open checklist items, and recorded decisions. Treat the prior record as authoritative rather than re-deriving it.
3. Diff done against pending. Identify the exact resume point. Do not re-run prior reproductions or redo completed work; resist the bias to verify from scratch when the artifact trail is present.
4. Route the remaining work to the matching playbook (`bug-fix`, `feature`, `multi-phase-plan`, `hillclimb`, etc.) and pick the verdict: continue the execution, ship a finished recommendation, ratify or override a prior conclusion, or postmortem a failed run. The pickup playbook ends here; the routed playbook owns the rest.
5. Verify inherited claims against the goal on the real artifact per principle-prove-it-works. A passing prior self-report is evidence, not final proof.

**Reply:** where the prior agent stopped, what you inherited vs redid (ideally nothing redone), the resume point, and the outcome.

## Worker
