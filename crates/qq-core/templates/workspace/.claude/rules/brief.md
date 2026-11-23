# Worker briefs

Run `quaqua worker prompt --check-brief "<brief>"` before sending; it stops at the first error and names it. Header lines start at column 1, in this order:

```
Topology: <solo|lead — why>
Playbook: <name from `quaqua playbook list`>
Context: <files / PR pointers; an upstream unit's report pasted in full>
Goal: <what must be true at the end>
Hard constraints: <owned and forbidden paths, limits, the targeted test command in backticks>
Current design (open to challenge): <the approach as it stands>
Unknowns: <what is not known yet + how to verify it>
Evidence: <the exact verify command and the output to return>
Time budget: <N> min
Cost budget: $<x>
```

- Pipeline briefs may add `Must fix: <critical|major|minor|trivial>` and `Review rounds: <N>`.
- A brief never fixes the cause or the solution: `Current design` is a hypothesis, the worker delivers the goal.
- "Parity" or "like X" scopes every command of X unless the Principal excludes some.
- Worktree-relative paths only. No mention of AI.
- Acceptance conditions go in the review or fix brief, never into a plan file after plan-ready.
- Literal text containing `${` (e.g. `${QQ`): search with fixed-string and single quotes, `grep -nF '${QQ' f` or `rg -nF '${QQ' f`. Double quotes raise a harness permission prompt; the single-quoted regex form finds nothing (`${` is a quantifier; `rg` errors).
- UI work: the worker never drives the real display (GUI apps, synthetic input, screenshots, `osascript`, `open`, `screencapture`); it verifies by build, tests and lint unless the brief authorises a headless display.
