---
name: herdr-orchestration
description: Herdr command recipes and worker spawn, prompt, monitoring, recovery, and cleanup. Use before every delegation or herdr pane/agent/tab command; never native subagents.
---

# Herdr orchestration

Authority: `AGENTS.md`, `team.toml`, and `rules/_all/`; references explain mechanics, not new roles.
Precondition: `test "${HERDR_ENV:-}" = 1`, else tell anh iu and stop. Never run bare `herdr` (it attaches the TUI). `herdr --help` / `herdr <group>` are the syntax authority.

## Flow
1. Pick kind from `quaqua roster`. Caps: `limits.max_workers_per_orchestrator` (a pipeline counts as 2) and `roles.lead.max_peers`; read them with `quaqua config get <key>`. Query `herdr agent list` for live names, never memory. In agent shells run `~/.cargo/bin/quaqua`; the shell's `quaqua` function may launch Claude.
2. `quaqua worker spawn [--base <ref>] <name> <kind> <repo>` → prints pane + worktree. For engine work `<repo>` is the engine checkout from `projects/INDEX.md` (e.g. `~/Lab/quaqua`); `~/quaqua` is a workspace, not a git repo. It grids the pane itself — `quaqua grid reconcile` only to repair a layout (`rules/_all/fanout-agent-grid.md`).
3. Use the five-part brief from `rules/_all/brief.md`; set `PROMPT_WORKER_BUDGET_SEC` to its time budget. Call `quaqua worker prompt <name> "<brief>"`, never raw `herdr agent prompt` (single authority: wraps `herdr agent prompt --wait`, watchdog, and atomic report delivery). A pipeline-size builder plans before code; its project Lead approves via `quaqua pipeline --resume` after the plan-ready pause.
4. Trust `runs/<name>.md`, not terminal output. Read the report and re-run its test yourself before reporting completion. Update the project card's `## Now` and today's `memory/YYYY-MM-DD.md` per `AGENTS.md`.
5. Cleanup immediately after the verified outcome, no extra approval: `quaqua worker cleanup <name> <repo>`. Branch kept.

Worker names: `<kind>-<task>`, ASCII `[a-z0-9-]`, at most 20 characters, never ending `-lead` (`quaqua worker spawn`). A failed startup may reuse its registered worktree; a foreign directory is refused. Lead names: `<slug>-lead`, Herdr's 32-character limit (`quaqua lead spawn <slug>`).

## References
- Read `references/operations.md` before manual pane/tab commands, layout recovery, plugin operations, or launcher/environment advice.
- Read `references/recovery.md` on watchdog exits 2–5, blocked/timeout/stalled/not-ready/unknown state, before any input.
- Read `references/judgment.md` for the Peer brief opener, when an extra Reviewer is worth it, blind design lanes, and the acceptance checklist. Independent bounded-task handbacks use the one report contract in `rules/_all/brief.md` (`## Report format`).
