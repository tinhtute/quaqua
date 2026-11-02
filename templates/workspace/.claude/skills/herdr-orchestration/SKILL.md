---
name: herdr-orchestration
description: Herdr command recipes and worker spawn, prompt, monitoring, recovery, and cleanup. Use before every delegation or herdr pane/agent/tab command; never native subagents.
---

# Herdr orchestration

Authority: `AGENTS.md`, `models.md`, and `rules/_all/`; references explain mechanics, not new roles.
Precondition: `test "${HERDR_ENV:-}" = 1`, else tell anh iu and stop. Never run bare `herdr` (it attaches the TUI). `herdr --help` / `herdr <group>` are the syntax authority.

## Flow
1. Pick kind from `models.md`. Task-worker cap per orchestrator is `[limits] max_workers_per_orchestrator` in `team.toml` (effective value: `quaqua config get limits.max_workers_per_orchestrator`); a pipeline counts as 2. Project Leads do not count toward that cap, and a Lead's own Peers cap at `[roles.lead] max_peers` (`quaqua config get roles.lead.max_peers`, default 4) — the same gate every spawn path checks. Query `herdr agent list`, never infer live names from memory.
2. `quaqua worker spawn [--base <ref>] <name> <kind> <repo>` → prints pane + worktree. It grids the pane itself — `quaqua grid reconcile` only to repair a layout (`rules/_all/fanout-agent-grid.md`). It injects worker rules (`rules/_all/` excluding orchestrator-only rules, plus `rules/<repo-basename>/`) into `runs/<name>.rules.md`; don't repeat those rules in the brief.
3. Use the five-part brief from `rules/_all/brief.md`; set `PROMPT_WORKER_BUDGET_SEC` to its time budget. Call `quaqua worker prompt <name> "<brief>"`, never raw `herdr agent prompt` (single authority: wraps `herdr agent prompt --wait`, watchdog, and atomic report delivery). A pipeline-size builder plans before code; its project Lead approves via `quaqua pipeline --resume` after the plan-ready pause.
4. Read the report and re-run its test yourself before reporting completion. Update the project card's `## Now` and today's `memory/YYYY-MM-DD.md` per `AGENTS.md`.
5. Cleanup immediately after the verified outcome, no extra approval: `quaqua worker cleanup <name> <repo>`. Branch kept. Only the project Lead accepts into its integration branch; main merge, push, and branch deletion need the Principal's approval.

Worker names: `<kind>-<task>`, ASCII `[a-z0-9-]`, at most 20 characters, never ending `-lead` (`quaqua worker spawn`). A failed startup may reuse its registered worktree; a foreign directory is refused. Lead names: `<slug>-lead`, Herdr's 32-character limit (`quaqua lead spawn <slug>`).

## References
- Read `references/operations.md` before manual pane/tab commands, layout recovery, plugin operations, or launcher/environment advice.
- Read `references/recovery.md` on watchdog exits 2–5, blocked/timeout/stalled/not-ready/unknown state, before any input.
- Read `references/judgment.md` for neutral assignments, ownership, risk-driven extra review, and acceptance evidence. Independent bounded-task handbacks use the one report contract in `rules/_all/brief.md` (`## Report format`); `PEER.md` holds the commit gates around it.
