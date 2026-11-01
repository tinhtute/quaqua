# Herdr operations

`AGENTS.md`, `models.md`, and `rules/_all/` are authoritative. Prefer the `quaqua` subcommand over manual orchestration (`quaqua --help` lists them). Check `HERDR_ENV=1`; never run bare `herdr`. Use `herdr --help` / `herdr <group>` for current syntax.

## Command recipes

```bash
# Normally use quaqua worker spawn (own worktree, rules injection, rollback on failure).
herdr pane split --current --direction right --cwd <owned-worktree> --no-focus
# Read .result.pane.pane_id; never reuse a pane rooted in someone else's checkout.
herdr agent start <name> --kind <kind> --pane <pane_id> -- <launch args from models.md>

# Always use quaqua worker prompt (never raw herdr agent prompt) for the watchdog and file-only stdout contract.
quaqua worker prompt <name> "<brief>"
herdr agent get <name>
herdr agent read <name> --source recent-unwrapped --lines 150
herdr agent read <name> --source visible --lines 80
herdr agent wait <name> --until blocked --timeout 900000
herdr agent wait <name> --until idle --timeout 60000
herdr agent list
herdr pane close <pane_id>
```

`prompt --wait` may return idle before the reply renders: wait/read again and verify the report, not merely the lifecycle state. Long output not visible → ask once for a file and its path. Read `recovery.md` before sending any input after a failure. A pending provider/stream error may require `pane send-text` followed by `pane send-keys ... Enter`; never treat this as permission to blindly repeat a task.

## Panes, tabs, and layout

- `quaqua grid reconcile` (run by `quaqua worker spawn` and `quaqua worker cleanup`) arranges 1 worker beside its orchestrator (anchor 2/5, worker 3/5); 2–4 in the project's workers tab: 2 left|right, 3 A|B top + C below, 4 a 2x2 grid. The project's tab must not mix another orchestrator's workers. This replaces ad-hoc alternating splits.
- Use `GRID_TAB=<project>-workers` for spawn/cleanup or `quaqua grid reconcile --tab <project>-workers --only <names>` for a scoped group. The pipeline scopes every grid call to its own builder/reviewer; never sweep foreign workers.
- Never move a working agent, during `--wait`, or without `--no-focus`. When moving a pane across workspaces, IDs can change: use the agent name or `.result.move_result.pane.pane_id`, not a remembered old ID.
- Manual recovery can create a tab with `herdr tab create --workspace "$HERDR_WORKSPACE_ID" --cwd <repo> --label <project>-workers` (no `--focus`); read `.result.root_pane`. A new project's workspace can use `herdr workspace create`; read IDs from JSON. Use `herdr tab close` only after that tab's owned workers are closed.
- Plain `git worktree` + panes is the default. `herdr worktree create` creates linked workspaces (and may open a review sidebar); use it only when the task explicitly asks.
- Close only panes you created. Never `herdr server stop`. Query live state instead of persisting pane/worktree inventories.

## Monitoring and lifecycle

- Prefer the `quaqua worker prompt` watchdog: cap, budget, stall, idle, blocked, and one soft nudge are specified in `rules/_all/worker-timebox.md`. Run long `--wait` clients in the background when the orchestrator must continue doing other work; don't replace the watchdog with repeated polling or resubmission.
- A long-running worker's report is not accepted until the current artifact and its exact test have been verified. On a stalled/blocked UI, look first; make at most one safe recovery input, then wait via `quaqua worker prompt --resume <name>`. Destructive/outward choices go to the Principal.
- Check provider quota before assigning a long task. Flag a nearly exhausted 5-hour/weekly allowance. Startup update prompts are skipped; inspect trust dialogs and answer only within the task's authority.
- Tell the Principal briefly who was spawned/closed and why, and check in at phase ends. At session end, query `herdr agent list` and hand off or clean up every worker owned by the session. Useful context is not permission to keep a completed task worker indefinitely.
- Report read + exact test rerun green → update the card/memory per `AGENTS.md`, then cleanup immediately. Preserve immutable evidence by SHA; the report can be archived under `raw/` when the task calls for it. Durable lessons belong in `rules/` via CoS, not an automatic memory overwrite.

## Fan-out and verification

Before wide fan-out, assess independent scopes, phases, and shared resources; offer a one-worker alternative when the split or cost is uncertain. Never split writers sharing files, tasks under about 5 minutes, or units whose order matters. Each writer has its own worktree and pane cwd; one shared-resource test lane at a time. A read-only quality split may separate lint/types/build, authorized e2e, and a diff review. Follow the cap of 4 per orchestrator and the mandatory grid; Peers never create their own topology.

UI verification follows `rules/_all/no-gui-driving.md`: code-only by default, owner does the visual pass. Only an explicitly authorized isolated/headless test may use a browser. Prefer the `agent-browser` CLI for authorized manual browser repro; don't invent ad-hoc Playwright rigs or run a repo's e2e suite without authorization. For an authorized server repro use build + start, not `next dev`, and stop the server when done.

## Launcher and plugins

For launcher/environment advice, inspect the owner's existing launcher, `~/.zshrc`, and sourced alias/function files read-only before proposing a change. Never recommend an alias that shadows an existing `quaqua` function or infer launcher behavior from one agent's environment.

The `persiyanov.reviewr` plugin exposes a diff sidebar via `herdr plugin action invoke` (`toggle`/`open`/`close`); verify the installed version and current arguments before invoking it. Community plugins can be found under GitHub topic `herdr-plugin`; review the manifest/scripts and pin `--ref` before an authorized install. Historical integration/plugin inventories are not live state: query Herdr instead of assuming an old installed list still applies.
