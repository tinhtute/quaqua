---
name: investigation
when: Read-only question: how does X work, why was Y built this way, are we sure about Z, should we do X or Y.
---

## Orchestrator

1. You own the answer. Plan, route, review, and synthesize. Investigation requests are read-only; they produce a cited explanation or a recommendation, not a code change.
2. When exploring alternatives or "are we sure?" questions, push back if the premise is flawed per principle-attack-the-premise. If the request is a contested architecture decision or technical fork, spawn a design worker with `quaqua worker spawn --purpose design` before any builder.
3. No code changes, no PR, no merge. If an investigation precedes a code change, return the findings to the coordinator and re-route to `bug-fix` or `feature`.

## Worker

1. Explore the codebase and history to locate relevant symbols, entry points, and historical context. Inspect git history and design docs directly for motivation questions rather than guessing intent.
2. Structure the investigation output clearly: Overview, Key Concepts, How It Works, Where Things Live, and Gotchas, or a concrete recommendation with a tradeoffs table if deciding between alternatives. Keep summaries concise, clear, and grounded in code citations.
3. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.

**Reply:** the investigation output. For "are we sure?" answers, include your real judgment with reasons. Push back if the premise is flawed.
