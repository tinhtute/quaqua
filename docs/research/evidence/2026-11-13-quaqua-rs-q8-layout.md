Outcome:         COMPLETE
Snapshot:        read-only, no writes to the worktree; sha 43a60bd31a0944f47c2d70988d2b779ebcbcc245 (main), worktree /Users/tinhtute/Lab/.wt/tieude-layoutread
Scope:           crates/quaqua/src/layout/{mod,builtin,model,legacy,snapshot,migrate,gate}.rs, config.rs, cap.rs, worker/spawn.rs, lead/spawn.rs, taskforce/mod.rs, pipeline/mod.rs, main.rs, templates/layouts/*.toml, templates/workspace/team.toml, ~/quaqua/team.toml, ~/quaqua/runs/gate.layout.toml
Verification:    cargo build --locked -p quaqua --bin quaqua → "Finished dev profile in 13.99s"; then 6 sandboxed runs of target/debug/quaqua with QUAQUA_HOME/QUAQUA_RUNS_DIR/QUAQUA_TEAM_TOML pointed at mktemp -d (real output quoted below). No tests run (none needed: every claim is a code citation or a command output).
Unknown / risk: whether the gate's frozen cap or cap::check_owner_cap's live cap wins in production depends on which path a spawn takes; both are cited but the live-herdr cap layer was not exercised (no herdr server started, per constraints)
Ownership:       scope released

## Playbook
1: done: read layout/*.rs, config.rs, cap.rs, all spawn paths; git log on main 43a60bd
2: done: Overview / Key Concepts / How It Works / Where Things Live / Gotchas + effect-timing table
3: done: every Evidence line re-run in a mktemp sandbox; Verified line below

# Q8 — Where is the farm layout defined, and when is each part read?

## Overview

The "farm layout" is the topology (who may spawn/prompt whom, depth, per-role child caps, naming, council members). It has **four** storage layers and they do **not** all read the same thing at the same time:

| Layer | Where | Read at | Editable by anh iu? |
|---|---|---|---|
| Built-in layouts (5) | `templates/layouts/*.toml`, `include_str!`'d into the binary | every `resolve_layout` call, from the binary | **no** — needs a rebuild (PR #271 shape) |
| `[layout] use` / `[layouts.<name>]` | workspace `team.toml`, **schema v3 only** | every `resolve_layout` call, from disk | yes, but the workspace file is v2 today |
| v2 synthesis | `[limits]` + `[roles.*]` in `team.toml` | every `resolve_layout` call | yes, today's active path |
| Frozen project snapshot | `runs/<slug>.layout.toml` | every gate/worker-spawn/taskforce call **for callers under `<slug>-lead`** | only by `quaqua layout refreeze <slug>` |

The headline: **today (v2 workspace) the layout is live and re-read on every spawn; the moment a project Lead exists, that project's topology freezes at Lead-spawn time and only `refreeze` unfreezes it.** Caps are the exception — they are read live even inside a frozen project.

## Key Concepts

- **`resolve_layout(cfg)`** (`layout/mod.rs:32-78`) — the single live resolver. If `schema_version >= 3` and `[layout] use` is set: built-in name → load built-in (with `slp` patched from live `[limits].depth`, `[limits].max_workers_per_orchestrator`, `[roles.lead].max_peers`), else `[layouts.<name>]` → clone + validate. Otherwise → `Layout::from_legacy(cfg)`.
- **`Layout::from_legacy`** (`layout/legacy.rs:15-101`) — v2 synthesis. Copies `[roles.*]` `can_spawn`/`can_prompt`/`reports_to` verbatim, `limits.depth` → `depth`, `roles.lead.max_peers` → `lead.max_children`, `limits.max_workers_per_orchestrator` → `cos.max_children`, `naming = slug-lead` for lead, synthesizes a `taskforce` role.
- **`resolve_effective_layout(caller, runs_dir, cfg)`** (`layout/snapshot.rs:160-196`) — the one entry point the gate uses. Walks the caller's `.owner` chain; if it reaches `<slug>-lead` **and** `runs/<slug>.layout.toml` exists → returns the frozen layout (`LayoutOrigin::Snapshot`); otherwise → live `resolve_layout`.
- **Snapshot** (`layout/snapshot.rs:15-24`) — `{ sha256, source = "team.toml@<mtime>", layout }`, `deny_unknown_fields`, written atomically.

## How It Works — who reads what, when

1. **Lead spawn** (`lead/spawn.rs:373-393`): gate runs on the **live** layout (the caller is `cos`, no snapshot yet), then `write_layout_snapshot(runs_dir, slug, cfg)` freezes `resolve_layout(cfg)`. Freeze happens **once per `lead spawn`**, after the gate allows, before the herdr workspace is created.
2. **Worker spawn** (`worker/spawn.rs:618-621`): `resolve_effective_layout(&owner, …)` — if `owner` is `<slug>-lead`, naming comes from the **frozen** layout; `gate(...)` (`worker/spawn.rs:655`) then re-resolves the same way.
3. **Gate** (`layout/gate.rs:1068-1104`): for `verb == Prompt` it prefers the target's snapshot, else the owner's; for `Spawn` it uses the caller's own chain. Snapshot read failure → warns and falls back to live.
4. **Taskforce spawn** (`taskforce/mod.rs:231`): same `resolve_effective_layout(&owner, …)`.
5. **Pipeline** (`pipeline/mod.rs:724-726`): never resolves a layout for topology; it calls `cap::count_owner_slots` + `cap::check_owner_cap` with the **live** cfg, and spawns workers through `worker::spawn` (which gates on the snapshot).
6. **`layout show` / `layout why`** (`layout/mod.rs:169-200`, `214-236`): read-only; `--project` resolves via the snapshot and prints a `frozen sha ≠ live sha` drift warning (`layout/mod.rs:183-196`).

## Where Things Live

- Built-ins: `templates/layouts/{slp,lead-peer,advisor,dual-lens,council}.toml`; `layout/builtin.rs:9-19` `include_str!`s them; `BUILTIN_NAMES` at `builtin.rs:3`. **Not** seeded into a workspace (`init.rs` has no `layouts` reference; `~/quaqua/layouts` does not exist).
- Schema gate: `config.rs:669-685` — `schema_version < 3` + any `[layout]`/`[layouts.*]`/`[projects.<slug>].layout` → hard error.
- Project override: `config.rs:527-549` `resolve_project_layout` — `[projects.<slug>].layout` wins over global `[layout]`, v3 only.
- Cap cross-check: `layout/gate.rs:397-400` (`max_children` in `1..=limits.max_workers_per_orchestrator`), `gate.rs:800-841` (cap layer), `cap.rs:56-70` `owner_cap`.
- Migration: `layout/migrate.rs:26-70` — v2→v3, inserts `[layout] use = "slp"` when `Layout::from_legacy(cfg).edge_set() == slp.edge_set()`, else moves `[roles.*]` to `[layouts.custom.roles.*]` translating `max_peers`→`max_children`. CLI: `main.rs:412-419` (`--write`; default is a dry-run diff), `main.rs:733`.
- Read surface: `config.rs:1002-1040` — `config get layout.use` works; `config get layouts` → `unknown key 'layouts'`.

## Effect timing per knob

| Knob | Edit takes effect… | Evidence |
|---|---|---|
| `templates/layouts/*.toml` (built-in body) | **only after a rebuild** (engine PR) | `builtin.rs:13-19` `include_str!` |
| `[layout] use` (v3) | next spawn of any kind — no rebuild, no refreeze for CoS-owned work; **frozen projects keep the old topology until refreeze** | measured: `layout why demo-lead spawn peer` → `ALLOWED (rule_id: slp.lead.can_spawn.peer)` while live config was `flat` where lead cannot spawn peer |
| `[layouts.<name>]` body (v3) | same as above; validated on every load (`validate_layout_with_config`) | measured: custom `max_children = 3` shown as `max_children: 3` |
| `[roles.*] can_spawn/can_prompt/reports_to` at **v2** | next spawn (they *are* the layout) | `legacy.rs:26-52` |
| `[roles.*] can_spawn/can_prompt` at **v3 with `[layout] use = "slp"`** | **never** — silently ignored, `config validate` still says `ok` | measured: set `[roles.cos] can_spawn = ["peer"]` at v3 → `layout show` still `can_spawn: lead, peer, taskforce` |
| `[limits].depth` | next spawn; also live-overrides the `slp` built-in at v3 | measured: v3 `depth = 3` → `Depth: 3`; `legacy.rs:99` |
| `[limits].max_workers_per_orchestrator` | next spawn, **even inside a frozen project** (cap layer reads live cfg) | `cap.rs:56-70`, `gate.rs:1123`; measured: `8 → 5` → `cos max_children: 5` |
| `[roles.lead].max_peers` | next spawn for `pipeline`/`taskforce` (`check_owner_cap`, live cfg); **only after refreeze** for worker spawns under a project Lead (gate cap reads the frozen `lead.max_children`) | `cap.rs:63-68`, `gate.rs:800-812`; measured: v3 `max_peers 4→2` → `lead max_children: 2` in live resolution |
| `[projects.<slug>].layout` | next `lead spawn` for that slug (it is what gets frozen); live for CoS-owned work | `config.rs:527-549`, `lead/spawn.rs:392` |
| `runs/<slug>.layout.toml` | edited by hand → effective immediately for that project; `layout refreeze <slug>` re-derives it from live `team.toml` | measured: freeze at depth 1, live → 2, `show --project demo` still `Depth: 1` + drift warning; after `refreeze` → `Depth: 2` |
| `schema_version 2 → 3` | **only via `quaqua config migrate --write`**; without it `[layout]` is a hard error | measured: v2 + `[layout] use` → `error: schema_version 2 does not support [layout] (requires schema_version = 3)` |

**Mid-run readers.** A running Lead is an LLM in a pane: it re-reads nothing on its own (its prompt names `AGENTS.md`/`models.md`, `lead/spawn.rs:523`), but every `quaqua` call it makes re-reads `team.toml` from disk (`config::load_team_config`) — so `[limits]`/`[roles.lead].max_peers`/weights changes bite mid-run, while **topology changes do not** (its project snapshot is frozen). A running pipeline likewise: live caps (`pipeline/mod.rs:724`), frozen topology for the workers it spawns.

## Gotchas

1. **v3 silently ignores `[roles.*]` edges.** `migrate` leaves `[roles.cos]`/`[roles.lead]` in the file (only the header changed in the diff), and at v3 those tables stop defining topology — but `config validate` passes. An edit there looks applied and is not. (measured)
2. **`config migrate` fails on a v2 file that already has `[layout]`**: `error: migrated TOML failed parse check: … duplicate key` at the `[layout]` line. So "add `[layout] use` then migrate" is not a path. (measured)
3. **Freeze is per `lead spawn`, never automatic afterwards.** Nothing re-reads `team.toml` into an existing snapshot; only `quaqua layout refreeze <slug>` does (`layout/snapshot.rs:123-131`). `layout show --project` only *warns* about drift.
4. **The snapshot is real and stale in this workspace.** `~/quaqua/runs/gate.layout.toml` carries `source = "team.toml@1792829960"` (2026-10-24) while `~/quaqua/team.toml` is dated 2026-11-13, and the snapshot's `lead.can_prompt = ["peer"]` no longer matches the live `["lead", "peer"]`. (measured)
5. **Two caps, two sources.** The gate's cap layer reads `role.max_children` from the (possibly frozen) layout; `cap::check_owner_cap` reads live `[roles.lead].max_peers` / `[limits].max_workers_per_orchestrator`. Whichever is stricter wins, and they can disagree after a `team.toml` edit. (inferred from `gate.rs:800-812` + `cap.rs:56-70`; the live-herdr cap layer was not exercised)
6. **`count_owner_slots` resolves the layout with `resolve_project_layout(cfg, None)`** (`cap.rs:159`) — weights are computed from the *global* live layout, never a project snapshot.
7. **Custom layouts must be self-contained**: `[layouts.custom.roles.lead] can_spawn = ["peer"]` fails with `layout 'custom' role 'lead' can_spawn references unknown role 'peer'` until a `peer` role is defined, and every role must be reachable from `cos` (`role 'peer' is unreachable from root 'cos'`). (measured)
8. **A custom layout's `max_children` is authoritative but bounded**: `max_children = 99` with `limits.max_workers_per_orchestrator = 8` → `error: layout 'custom' role 'cos' max_children 99 must be in 1..=8`. (measured)

## Implications for one user-owned toml

Facts only; the design call is anh iu's.

1. The built-in layout bodies are the only layout fact that still needs a rebuild. Everything else (topology edges, depth, per-role caps, naming, members/quorum) is already expressible in `team.toml` at v3 — the PR #271 shape is not required for layout changes, only for changing a *built-in's* content.
2. The workspace file is v2 today, so the v3 surface is dead code for anh iu right now; `config migrate --write` is a one-way door that also changes which `[roles.*]` keys still mean anything (gotcha 1).
3. Topology and caps already have different refresh semantics: caps live, topology frozen per project. Any "one toml" answer has to say which of the two it promises.
4. `runs/<slug>.layout.toml` is a second copy of the layout in a user-writable location, with a `sha256` + `team.toml@mtime` provenance tag and a manual `refreeze` verb. It is the only thing standing between "edit team.toml" and "the next worker spawn obeys it" for an in-flight project.
5. `config get` exposes `layout.use` but not `[layouts.<name>]` contents — a GUI read contract (#214) would need new keys.
6. No workspace-level `layouts/` directory exists and `init` never seeds one; `templates/layouts/*` are engine-only files.

Verified: review-clean 43a60bd31a0944f47c2d70988d2b779ebcbcc245

## Blockers
none

## Lessons
- A config knob that is validated but not read (v3 `[roles.*]` edges) passes `config validate`; only a resolve-and-print command (`layout show`) proves a knob is live.
- Freeze-on-spawn plus a manual `refreeze` verb is a second source of truth; any "single source" claim must name the unfreeze trigger.
