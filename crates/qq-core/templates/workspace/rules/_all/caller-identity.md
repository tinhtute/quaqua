<!-- audience: orchestrator -->
# Caller identity

- Infer caller identity from the harness (herdr pane get/agent list), never ambient env; fail closed — 2026-10-15, runs/tieude-orchtag2.md

## Pane tokens

Identity quaqua reads back is the pane's herdr token set; `quaqua identity heal` restores it after a herdr restart.

| Token | Written by | Meaning |
| --- | --- | --- |
| `grid_role` | `worker spawn` (`worker`), `lead spawn` (`orchestrator`) | pane class; cleanup, reconcile and pipeline move refuse a `cos` pane |
| `grid_owner` | `worker spawn` (`QUAQUA_OWNER`), `lead spawn` (`<slug>-lead`) | owner stamped on each incident, so it decides which Lead sees it |
| `grid_group` | `worker spawn` (repo name) | project group |
| `grid_tab` | `worker spawn` | the `<repo>-workers` tab label the planner accepts |
| `grid_farm` | `worker spawn`, `lead spawn`, `identity heal` | farm id of the workspace that spawned the pane |

## Farm id

- `farm_id` is the last component of the canonicalized workspace root, lowercased, every character outside `[a-z0-9_-]` replaced by `-` (`~/Work/Acme` → `acme`, `~/Work/Acme Web` → `acme-web`); `farm_of_runs_dir(runs)` applies it to the directory holding `runs/`. It comes from the filesystem — never from `team.toml`, and never from herdr `workspace_id`, because one farm spans several herdr workspaces (one per SLP team).
- A pane's `cwd` is never read to attribute it. Tokens only narrow (`Scope::admits`, `crates/qq-core/src/grid/mod.rs`): a pane with another `grid_owner` or `grid_farm`, or a role other than `worker`, is not ours; a pane without those tokens passes, because the spawn's DB record is what claims it.
- `HERDR_PANE_ID` short-circuits the CoS's own-pane resolution; unset it when reproducing an identity or attribution bug — 2026-10-29, runs/tieude-farmdoc.md
