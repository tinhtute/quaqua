<!-- audience: orchestrator -->
# Caller identity

- Infer caller identity from the harness (herdr pane get/agent list), never ambient env; fail closed — 2026-10-15, runs/tieude-orchtag2.md
- A pane's token set is evidence, not proof: a pane with no `grid_farm` is attributed by its `cwd`, so never infer its farm from the token alone — 2026-10-29, runs/tieude-farmdoc.md

## Pane tokens

Identity quaqua reads back is the pane's herdr token set; `quaqua identity heal` restores it after a herdr restart.

| Token | Written by | Meaning |
| --- | --- | --- |
| `grid_role` | `worker spawn` (`worker`), `lead spawn` / `lead tag` / `lead repair` (`orchestrator`), `cos tag` (`cos`) | pane class; cleanup, reconcile and pipeline move refuse a `cos` pane |
| `grid_owner` | `worker spawn` (`QUAQUA_OWNER`), `lead spawn` / `lead tag` (`<slug>-lead`), `cos tag` (`cos`) | owner stamped on each incident, so it decides which Lead sees it |
| `grid_group` | `worker spawn` (repo name) | project group |
| `grid_tab` | `worker spawn` | the `<repo>-workers` tab label the planner accepts |
| `grid_farm` | `worker spawn`, `lead spawn` / `lead tag` / `lead repair`, `identity heal` (Leads and workers); **not** `cos tag` | farm id of the workspace that spawned the pane |

## Farm id

- `farm_id` is the last component of the canonicalized workspace root, lowercased, every character outside `[a-z0-9_-]` replaced by `-` (`~/quaqua` → `quaqua`, `~/Work/TTDVKH` → `ttdvkh`); `farm_of_runs_dir(runs)` applies it to the directory holding `runs/`. It comes from the filesystem — never from `team.toml`, and never from herdr `workspace_id`, because one farm spans several herdr workspaces (one per SLP team).
- A pane without a `grid_farm` token falls back to its `cwd`: under the farm root → local; a `cwd` ancestor with `runs/<name>.owner` (a worker worktree, subdirectories included) → local; otherwise foreign. herdr `pane get` exits 0 even for a dead pane, so the answer comes from the `pane_not_found` body.
- `HERDR_PANE_ID` short-circuits the CoS's own-pane resolution; unset it when reproducing an identity or attribution bug — 2026-10-29, runs/tieude-farmdoc.md
