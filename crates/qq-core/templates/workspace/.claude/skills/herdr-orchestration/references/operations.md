# Herdr operations

`AGENTS.md` and `team.toml` are authoritative. Prefer a `quaqua` subcommand over manual orchestration (`quaqua --help`). Check `HERDR_ENV=1`; never run bare `herdr`; `herdr --help` has the syntax.

## Command recipes

```bash
# Normally use quaqua worker spawn (own worktree, rollback on failure).
herdr pane split --current --direction right --cwd <owned-worktree> --no-focus
herdr agent start <name> --kind <kind> --pane <pane_id> -- <launch args from `team.toml`>
quaqua worker prompt <name> "<brief>"   # never raw herdr agent prompt: it carries the watchdog
herdr agent read <name> --source recent-unwrapped --lines 150
herdr agent wait <name> --until idle --timeout 60000
herdr agent list; herdr pane close <pane_id>
```

`prompt --wait` can return idle before the reply renders: read again and verify the report, not the lifecycle state. Read `recovery.md` before sending input after a failure; a blocked agent takes only `herdr agent send-keys <name> enter`.

Watchdog exit codes: `recovery.md`; resume a stalled worker with `quaqua worker prompt --resume <name>`. Accept a report only after rerunning its exact test, then clean up at once and close only panes you created. Never `herdr server stop`.

Before wide fan-out, check scopes and shared resources; never split writers that share files. Each writer has its own worktree; one shared-resource test lane at a time. UI checks follow the UI line of `.claude/rules/brief.md`.
