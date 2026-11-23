---
name: herdr-orchestration
description: Herdr command recipes and worker spawn, prompt, monitoring, recovery, and cleanup. Use before every delegation or herdr pane/agent/tab command; never native subagents.
---

# Herdr orchestration

Authority: `AGENTS.md`, `.claude/rules/brief.md` and `team.toml`; references explain mechanics, not new roles.
Precondition: `test "${HERDR_ENV:-}" = 1`, else tell anh iu and stop. Never run bare `herdr` (it attaches the TUI). `herdr --help` / `herdr <group>` are the syntax authority.

## Flow
1. Pick kind from `quaqua roster`. Caps: `limits.max_workers_per_orchestrator` (a pipeline counts as 2) and `roles.lead.max_peers`; read them with `quaqua config get <key>`. Kinds, harnesses, models, efforts and role topology live in the config database, not in this file: change one only on anh iu's word with `quaqua config set <key> <value>`, dump it with `quaqua config export`, and see engine default changes with `quaqua config diff` (it never overwrites). Do not change worker kind, model or effort ad hoc. Query `herdr agent list` for live names, never memory. In agent shells run `~/.cargo/bin/quaqua`; the shell's `quaqua` function may launch Claude.
2. `quaqua worker spawn [--base <ref>] <name> <kind> <repo>` → prints pane + worktree. For engine work `<repo>` is the engine checkout from `projects/INDEX.md` (e.g. `~/Lab/quaqua`); `~/quaqua` is a workspace, not a git repo. It grids the pane itself — `quaqua grid reconcile` only to repair a layout. One project per workers tab; never 2+ workers in the CoS tab.
3. Use the brief shape in `.claude/rules/brief.md`; set `PROMPT_WORKER_BUDGET_SEC` to its time budget. Call `quaqua worker prompt <name> "<brief>"`, never raw `herdr agent prompt` (single authority: wraps `herdr agent prompt --wait`, watchdog, and atomic report delivery). A pipeline-size builder plans before code; its project Lead approves via `quaqua pipeline --resume` after the plan-ready pause.
4. Trust `runs/<name>.md`, not terminal output. Read the report and re-run its test yourself before reporting completion; `quaqua run show <name>` prints the run's events, transcript, tool calls, tokens, cost (`unknown` when the harness reports none) and candidate sha, and `quaqua run list --project <slug>` lists a project's runs. Open project cards as needed; read dated memory and `runs/` only for detail. Update the project card's `## Now` and today's `memory/YYYY-MM-DD.md` per `AGENTS.md`.
5. Cleanup immediately after the verified outcome, no extra approval: `quaqua worker cleanup <name> <repo>`. Branch kept. A solo worker branch is pushed by a fenced Lead or the owner, never by the CoS or the worker. Write `cost=$<y>/$<x>` (spend from `quaqua status --metrics`) on the cleanup line in today's memory.

Worker names: `<kind>-<task>`, ASCII `[a-z0-9-]`, at most 20 characters, never ending `-lead` (`quaqua worker spawn`). A failed startup may reuse its registered worktree; a foreign directory is refused. Lead names: `<slug>-lead`, Herdr's 32-character limit (`quaqua lead spawn <slug>`).

## References
- Read `references/operations.md` before manual pane/tab commands, layout recovery, plugin operations, or launcher/environment advice.
- Read `references/recovery.md` on watchdog exits 2–5, blocked/timeout/stalled/not-ready/unknown state, before any input.
- Read `references/judgment.md` for roles, Lead rules, budgets, retries, waves, the Peer brief opener, extra Reviewers, blind design lanes, and the acceptance checklist.
