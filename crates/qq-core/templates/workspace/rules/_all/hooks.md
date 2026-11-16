# Hooks

- A fail-closed hook fallback must still parse the call it guards: block the guarded set, allow+warn the rest (block-all = lockout) — 2026-10-16, runs/tieude-gatefix.md
- A converged-check and the repair it guards must share one acceptance predicate, or the check times out on layouts the repair accepts — 2026-10-16, runs/tieude-gridfix.md
