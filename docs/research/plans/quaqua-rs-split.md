# quaqua-rs split — LOCK (2026-10-16)

Status: **P1 DONE 2026-10-16 · P2 DONE 2026-10-21 (M0–M7 PASS, PR #83 `47f4903`)** — this file is the record.

## Outcome (2026-10-21)
- P2 ran on 2026-10-21 with anh iu present: M0–M7 **PASS** (`memory/2026-10-21.md`, card `projects/quaqua-rs.md`). `~/quaqua` is a non-git consuming workspace (`.quaqua/template.lock`, no `.git`, no `crates/`); the engine is `~/Lab/quaqua` at `47f4903` (installed with `cargo install --path … --locked`); the herdr plugin is `herdr-quaqua` from `~/Lab/quaqua/plugins/herdr-quaqua`.
- Deviations from the 2026-10-20 handoff: `~/quaqua` HEAD was `6c8934c`, not `bb1f27f`; `~/Lab/quaqua` **did exist** (old bash-era checkout `3889cc8`, 39 local commits, 17 branches) and was moved to `~/Lab/quaqua-old-2026-10-12` before a fresh clone; the live herdr entry was the **dead** `herdr-grid` (missing path), not `herdr-quaqua`; the isolated plugin smoke needed a short socket dir.
- M5 conflicts (both resolved by taking the template version, anh iu): `rules/_all/fanout-agent-grid.md`, `rules/_all/worker-timebox.md`.
- Upstream sync `095edc7` (`quaqua/tieude-p2upsync`) became PR #83, **squash-merged as `47f4903`** — the branch SHA is not a `main` ancestor by design.
- Closed 2026-10-21 (anh iu): M1 bundle/tar/md5, `~/quaqua-pre-p2-2026-10-21.git`, `~/Lab/quaqua-old-2026-10-12`, `~/.config/herdr/plugins.json.pre-m4`, the `tieude-p2smoke`/`tieude-p2upsync` branches, all 213 local `quaqua/*` branches in `~/Lab/quaqua` and 96 remote `quaqua/*` (only `quaqua/truonglao-p2tmpl`, PR #74, kept).
- Open follow-up: `quaqua init --git-hooks <repo>` (Challenge 4) — being built separately (`tieude-githooks`); no longer part of this phase.

Diagrams: `quaqua-rs-split.tldr` — open in tldraw; pages: Overview (high-level), Engine P2 (v2.1.1), Workflow (high-level), Workflow (detail), SLP vs Quaqua.
Run prompt (M0–M7): `quaqua-rs-split.migrate-prompt.md`.

## Goal
The Rust engine lives in its own checkout `~/Lab/quaqua`. `~/quaqua` holds only the workspace and uses the installed `quaqua` binary. The workspace is the md files, memory, runs, rules, team.toml and the hooks.

## Decisions (anh iu, 2026-10-16)
- Repo (anh iu, 2026-10-19): one upstream `therealtinhtute/quaqua` — engine on branch `engine`, workspace on `main`. Locally two dirs: the codebase checkout `~/Lab/quaqua` (named `~/Lab/quaqua-rs` on other machines; per-machine path lives in `projects/INDEX.md`, never hardcoded) and the consuming workspace `~/quaqua`. *Branch roles superseded 2026-10-20 — see Phase 2.*
- History: keep it with `git subtree split` (blame and PR #42 stay traceable). *Superseded 2026-10-20: one repo, nothing is split.*
- Grid: port `grid-workers.sh` to Rust before the split
- Order: lock the plan and run to-plan for both phases first, then build

## Phase 1: decouple in place (no behaviour change, ships alone)
- Move every hardcoded workspace path into one `workspace_root()`. The default stays `~/quaqua`, and one override (env or team.toml) can change it:
  - `crates/quaqua/src/worker/spawn.rs:257` (rules)
  - `crates/quaqua/src/worker/cleanup.rs:12` (runs)
  - `crates/quaqua/src/config.rs:262` and `crates/quaqua/src/main.rs:183` (team.toml)
- Port `grid-workers.sh` to `quaqua grid`. The binary then stops searching for the script (`cleanup.rs:37-80`), and pipeline/spawn/cleanup call the Rust grid.
- Done when:
  - `rg 'join\("quaqua/' crates/` finds only the default in `workspace_root()`
  - no Rust code shells out to `grid-workers.sh`
  - cargo tests pass, and the grid tests and the targeted bash suites pass

## Phase 2: one repo, embedded workspace template
> **Decision (anh iu, 2026-10-20)**: one upstream `main` holds the Rust engine **and** a base workspace template. No `engine`/`workspace` branches: the fork-and-prune plan (`quaqua/truonglao-p2split`, a4ebcce) is superseded. The template is embedded in the binary, so `quaqua init <dir>` needs no clone. A consuming workspace is **not** a git repo. Code moves to `~/Lab/quaqua`; `~/quaqua` becomes a consuming workspace and keeps its private state.
> **Status (2026-10-20)**: B1–B3 are done on `main` — #75 `025b825` (B1) · #76 `c092ce9` (B2, with `8db0665` `1a5718c`) · #77 `96882af` (B3) · #78 `c2c3e4d` (config `get|validate`) · #80 `d2a3d12` (test `QUAQUA_HOME`) · #81 `662b5a0` (auditor root). They landed as six PRs instead of one `p2tmpl-integ`, so `main` already carries the template layout (M2's merge half is done) and `~/quaqua` stayed on its pre-template history, never pulling it (`6c8934c` at M0, not the `bb1f27f` assumed here). M0–M7 had **not** run at that point.
> **Status (2026-10-21)**: M0–M7 ran and all passed — `~/quaqua` was converted in place. See **Outcome** and the per-step notes below.
> **Decisions (anh iu, 2026-10-20)**: one-time M1 backup only, no periodic timer (Challenge 1 risk accepted) · engine checkout stays `~/Lab/quaqua` · M0–M7 run in a separate session with anh iu present, not tonight · every CoS edit to `AGENTS.md`/`rules/_all` is upstreamed into `templates/workspace` by a worker the same day.

### What goes where
| Path in the repo (after B1) | Role | Note |
|---|---|---|
| `templates/workspace/**` | **managed** template, embedded | mirrors the workspace root: `AGENTS.md`, `identity.md`, `SOUL.md`, `PEER.md`, `models.md`, `team.toml`, `rules/_all/**`, `.claude/settings.json`, `.claude/rules/**`, `.claude/skills/herdr-orchestration/**`, `scripts/hooks/herdr-gate.sh`, `scripts/hooks/omp-worker-state.ts`, `scripts/auditor/**` |
| `templates/seed/**` | **seed-once**, embedded | `memory-rules.md`, `projects-INDEX.md`, `projects-_template.md`, `USER.example.md`; copied only when the target is missing (`init.rs:195`, `:268`), never updated |
| `crates/**`, `Cargo.*`, `plugins/**`, `tests/**`, `.github/**`, `docs/**`, `LICENSE`, `README.md`, `SETUP-PROMPT.md` | engine-only | never copied into a workspace |
| `AGENTS.md` (new, root) | engine-only | ≤20-line contributor stub for workers in engine worktrees: the CoS doc is `templates/workspace/AGENTS.md`; worker rules arrive via `runs/<name>.rules.md` (`worker/spawn.rs:309`) |
| `.claude/settings.json` (root) | engine-only | same gate, path `"$CLAUDE_PROJECT_DIR"/templates/workspace/scripts/hooks/herdr-gate.sh` — engine worktrees keep the PreToolUse gate (the gap a4ebcce accepted) |
| `memory/`, `projects/`, `runs/`, `raw/`, `reports/`, `USER.md`, `rules/<slug>/`, `docs/plans/` | private, workspace-only | never in the template; `init` never writes them except the seeds |

- **One `team.toml`.** `templates/workspace/team.toml` is both the embedded default and the workspace's starting config. Re-point `include_str!("../../../team.toml")` (`config.rs:322`, `pipeline/mod.rs:4097` as `../../../../templates/workspace/team.toml`), `engine/activities.rs:1598-1603`, `tests/worker_unit.rs:975,1039`, `tests/config.rs:18`. Side win: a worker cwd in an engine worktree no longer finds a stray `./team.toml` first (`config.rs:315`).
- **Gitignore trap** (`templates/workspace/rules/_all/git-hygiene.md:1`): `memory/`, `projects/`, `runs/`, `raw/`, `reports/`, `USER.md` in `.gitignore:2-9` match at any depth. The template must hold none of those names — that is why seeds stay flat in `templates/seed/`. `rules/*/` and `docs/plans/` are anchored (middle slash), so `templates/workspace/rules/_all/` is trackable. Guard: `git check-ignore` over every embedded path (extend `test_init_workspace.sh` §0).
- The workspace gets no `.gitignore`: it is not a repo.

### Embedding + `quaqua init`
- **Before B2** (shipped as #76): `run()` resolved the root from the shim's `QUAQUA_SCRIPT_DIR/../..`, else the nearest cwd ancestor with `templates/`, and failed without it. The shim (`scripts/workspace/`) no longer exists in the tree. It then created the private dirs, copied seeds, wired `core.hooksPath` (git repos only) and tagged the CoS pane.
- **After** (B2, shipped):
  - `crates/quaqua/build.rs` walks `templates/{workspace,seed}` and generates `static ENTRIES: &[Entry { path, bytes: include_bytes!(..), mode, kind: Managed|Seed }]`, with `cargo:rerun-if-changed` on both dirs. No new crate dependency. Works for `cargo install --path` and `--git` (both build in a full checkout); `cargo publish` would not (crates.io is out of scope).
  - `quaqua init [<dir>] [--update] [--dry-run]`. `<dir>` defaults to `workspace_root()`. The `QUAQUA_SCRIPT_DIR` and cwd-`templates/` resolution is deleted; nothing is read from disk but the target.
  - Plain `init`: create the private dirs, write every missing managed + seed file, keep modes (`herdr-gate.sh`, `install-auditor.sh` stay 0755), write `<dir>/.quaqua/template.lock`. An existing file is never touched (today's idempotence).
  - `wire_hooks` stays a no-op on a non-git target (`init.rs:534`). `INSTALL_LINE` (`init.rs:27`) becomes `cargo install --path <checkout>/crates/quaqua --locked`.
  - Proof of "no clone needed": `test_init_workspace.sh` stops copying `templates/` into its sandbox and runs `quaqua init "$T/ws"` in an empty temp dir (`tests/test_init_workspace.sh:20,79-81`).

### `init --update` rule
`.quaqua/template.lock` (JSON): `{ "engine": "<CARGO_PKG_VERSION>+<git sha from build.rs>", "files": { "<path>": { "md5": "<hex>", "mode": 420 } } }`, hashed with the existing `hash::md5_hex` (`hash.rs:4`; change detection, not security). Per managed file, with `L` = lock hash, `W` = workspace hash, `T` = new template hash:

| Case | Action |
|---|---|
| file missing in workspace | write `T`, record |
| `W == T` | record only |
| `W == L`, `T != L` (untouched locally) | overwrite with `T`, record |
| `W != L` (changed locally), or no lock entry and `W != T` | **never overwrite**: print `diff -u <path> <new>`, save `T` to `.quaqua/pending/<path>`, keep `L` |
| path in lock, gone from template | never delete; list as `removed upstream` |
| seed file | never updated |

- Exit `0` = nothing pending; exit `3` = at least one conflict (summary line `N conflicts — see .quaqua/pending/`). The CoS decides per file: take it (`cp .quaqua/pending/<p> <p>` then re-run `--update`, which records `W == T`) or keep the local one and upstream it (below).
- `.quaqua/pending/` is outside `rules/`, so the rule loaders (`worker/spawn.rs:317-336`, `lead/spawn.rs:236-254`, `.md` only) never read a pending copy.
- `--dry-run` prints the table's decisions and writes nothing.
- Local edits drift until upstreamed: a CoS lesson promoted into `rules/_all/` is also landed in `templates/workspace/rules/_all/` by a worker on `~/Lab/quaqua` **the same day** (anh iu, 2026-10-20), so the next `--update` sees `W == T`. That commit is the history and the cross-machine sync the no-git workspace no longer has.

### How the binary finds the workspace at runtime
- `workspace_root()` (`workspace.rs:43`): `QUAQUA_HOME` → its `QUAQUA_ROOT` alias → `$HOME/quaqua`; `workspace_root_checked()` (`workspace.rs:53`) errors when `projects/INDEX.md` is missing. Runs/rules/team.toml/journal go through it (`worker/spawn.rs:309`, `config.rs:318`, `engine/journal_sqlite.rs:20`, `status.rs:497`, `engine/telemetry_otel.rs:138`).
- Repo-shaped resolution, **fixed in B3** (#77):
  - `find_quaqua_root()` (shim dir → cwd ancestors → **exe ancestors** → silent cwd) is gone; `lead/spawn.rs:115`, `lead/cleanup.rs:41`, `pipeline/mod.rs:87` and the auditor (`auditor.rs:305`) use the checked resolver. `QUAQUA_ROOT` stays an alias of `QUAQUA_HOME` for one release; a missing `projects/INDEX.md` is an error, not a cwd fallback.
  - `quaqua config get|validate` (`main.rs:153-187`) now resolves like every other command via `config::load_team_config` (`config.rs:311`); the duplicate `load_spawn_config` is gone (#78).
  - Auditor: `resolve_auditor_root` (`auditor.rs:305`, #81) follows `QUAQUA_HOME` > `QUAQUA_ROOT` > `$HOME/quaqua`. Prompt step 1 reads `git log` of each repo in `projects/INDEX.md` (`auditor.rs:23`).
- `templates/workspace/scripts/hooks/herdr-gate.sh:24-27` resolves `$QUAQUA_BIN` → `quaqua` on PATH: no repo assumption. `templates/workspace/scripts/auditor/install-auditor.sh:8` takes root from its own location (`dirname/../..`) = the workspace: fine.

### Builders (CoS drives; sequential, no Lead — one owner of `crates/quaqua/src/` at a time)
Each was meant as one pipeline (sude build, suhuynh review, one round) from `quaqua/p2tmpl-integ`. **B1–B3 shipped as separate PRs to `main` instead** (#75 #76 #77, plus #78 #80 #81), so the integ-branch rule below is history; `main` carries the template layout and `~/quaqua` never pulled it.

| # | Task | Owns | Targeted test |
|---|---|---|---|
| B1 ✅ | **done #75 `025b825`** · `template-move`: `git mv` the managed files into `templates/workspace/`, seeds into `templates/seed/`; new root `AGENTS.md` stub + root `.claude/settings.json`; re-point every `team.toml` read listed above; retarget `test_docs.sh` (read root `AGENTS.md`, `scripts/`, `rules/_all`), `test_config.sh`, `test_gate_shim.sh`, `test_herdr_gate.sh`, `test_install_auditor.sh` to `templates/workspace/` | `templates/**`, root md files, `.claude/**`, `rules/_all/**`, `scripts/**`, `team.toml`, the `include_str!`/`CARGO_MANIFEST_DIR` sites, `tests/*.sh` path lines | `cargo test --locked -p quaqua config` · `bash tests/test_docs.sh` · `bash tests/test_config.sh` · `git ls-files templates \| git check-ignore --stdin` → empty |
| B2 ✅ | **done #76 `c092ce9`** (+ `8db0665`, `1a5718c`) · `embed-init`: `build.rs` table, `init [<dir>] --update --dry-run`, `.quaqua/template.lock`, the update rule, delete `QUAQUA_SCRIPT_DIR`/cwd resolution | `crates/quaqua/build.rs`, `crates/quaqua/Cargo.toml`, `src/init.rs`, `src/main.rs` (`Init` arm only, `:74`, `:504`), `tests/init.rs`, `tests/test_init_workspace.sh` | `cargo test --locked -p quaqua --test init` (one case per table row + `--dry-run` writes nothing + exit 3) · `bash tests/test_init_workspace.sh` |
| B3 ✅ | **done #77 `96882af`** (+ #78 `c2c3e4d`, #80 `d2a3d12`) · `root-resolve`: fold `find_quaqua_root` into `workspace_root`, `config` default + dedupe, auditor root + prompt | `src/workspace.rs`, `src/config.rs` (load fn), `src/main.rs` (`:153-187`), `src/lead/{spawn,cleanup}.rs` (root lines), `src/pipeline/mod.rs:87`, `src/auditor.rs`, `tests/test_spawn_lead.sh`, `tests/test_cleanup_lead.sh` | `cargo test --locked -p quaqua workspace` · `cargo test --locked -p quaqua auditor` · `bash tests/test_spawn_lead.sh` |
| gate ✅ | **closed 2026-10-20**: the six PRs went to `main` one by one — no single `p2tmpl-integ` review ran; #81 `662b5a0` closed the auditor root gap B3 introduced (`QUAQUA_ROOT` precedence) | — | `main` at `14e7d96`; `47f4903` after #83 |

Done when: `rg -n 'QUAQUA_SCRIPT_DIR|find_quaqua_root' crates/quaqua/src` is empty; `cd "$(mktemp -d)" && quaqua init ws && test -f ws/AGENTS.md -a -x ws/scripts/hooks/herdr-gate.sh -a -f ws/.quaqua/template.lock` with the installed binary and no checkout on cwd; `bash tests/run.sh --slow` 0 fail. **Verified on `main` `47f4903` (2026-10-21)**: both `rg` guards are empty; the init check and the B1–B3 test suites passed during their PRs; the live binary completed the M0–M7 migration.

### `~/quaqua` migration — **done 2026-10-21** (M0–M7 all PASS; the deviations are in **Outcome**)
Gate smoke `G`, run from `~/quaqua` (same as a4ebcce):
```
g() { jq -nc --arg c "$1" '{tool_name:"Bash",tool_input:{command:$c}}' | bash ~/quaqua/scripts/hooks/herdr-gate.sh >/dev/null 2>&1; echo $?; }
[ "$(g 'herdr pane split --current --direction right')" = 2 ] && [ "$(g 'quaqua worker spawn x tieude ~/Lab/quaqua')" = 0 ]
```
Convert **in place**, never rename-and-swap: live panes (the CoS, herdr-restored panes) hold `~/quaqua` as their cwd inode, so a swapped dir would leave them writing relative paths into the old one while the binary writes to `$HOME/quaqua`.

- **M0 preconditions** (CoS) — **done 2026-10-21, PASS on the rerun**: no Lead alive (`herdr agent list` has no `*-lead`); `git -C ~/quaqua worktree list` shows only `~/quaqua` (every `~/.wt/*` of this repo cleaned — removing `.git` would orphan them); tracked tree clean or the dirty files listed (2026-10-20: ` M rules/_all/worker-timebox.md`, kept as a local edit → a conflict in M5); `git -C ~/quaqua branch --no-merged origin/main --list 'quaqua/*'` recorded in the card. First run failed on 3 live worktrees (`suhuynh-r5r1`, `tieude-auditdist` dirty, `tieude-w2perf`) + 2 prunable `/tmp` ones — cleaned, the `tieude-auditdist` WIP saved as `a9ba7ad` on its branch. HEAD was `6c8934c`; 167 unmerged `quaqua/*` branches recorded.
- **M1 backup** (CoS; **one-time, no timer** — anh iu 2026-10-20) — **done 2026-10-21**:
  - `git -C ~/quaqua bundle create ~/quaqua-pre-p2-<date>.bundle --all` + `git bundle verify` (every local branch, incl. unpushed `quaqua/*`).
  - `tar --zstd -cf ~/quaqua-pre-p2-<date>.tar.zst -C ~ --exclude=quaqua/target quaqua` + `tar -tf … | grep -c '^quaqua/memory/'` > 0.
  - Private-state manifest: `(cd ~/quaqua && find memory projects runs raw reports rules docs/plans USER.md -type f -print0 | sort -z | xargs -0 md5sum) > ~/quaqua-pre-p2-<date>.md5`.
  - Result: bundle verified (337 refs), tar 11 MB / 6 memory files, md5 1140 entries self-check OK. All three trashed 2026-10-21 after PR #83 (anh iu).
- **M2 merge + push** — **done 2026-10-20** via #75 #76 #77 #78 #80 #81 (no `p2tmpl-integ` PR was needed; `main` carries the template layout). `~/quaqua` did **not** pull `main`; its HEAD at M0 was `6c8934c`, not the `bb1f27f` assumed earlier.
- **M3 engine checkout + install** (CoS) — **done 2026-10-21**: `~/Lab/quaqua` **did exist** (an old bash-era checkout at `3889cc8`, 39 local commits, 17 branches), so it was moved to `~/Lab/quaqua-old-2026-10-12` (anh iu) before `git clone <upstream> ~/Lab/quaqua` (the path is per-machine in `projects/INDEX.md`). The fresh clone was `14e7d96`, `git -C ~/Lab/quaqua status --porcelain --ignored` empty, 214 `quaqua/*` refs imported via `git fetch ~/quaqua 'refs/heads/quaqua/*:refs/heads/quaqua/*'`, and `cargo install --path ~/Lab/quaqua/crates/quaqua --locked` passed. Verify: `cargo install --list` shows the `~/Lab/quaqua` path; `G` passes; full `cargo test --locked` skipped per anh iu.
- **M4 herdr plugin** (CoS) — **done 2026-10-21** (anh iu ran the live swap; the CoS's tool classifier blocked it): `~/.config/herdr/plugins.json` pointed at `~/quaqua/plugins/herdr-grid`, a **missing path** — the plugin was already dead (the 2026-10-20 "already `herdr-quaqua`" read was wrong). Re-registered with **`herdr plugin link ~/Lab/quaqua/plugins/herdr-quaqua`**; `herdr plugin list` is clean and `quaqua grid reconcile --tab quaqua-workers --dry-run` printed "no workers to arrange". The isolated smoke needed a **short socket dir**. Rollback copy `plugins.json.pre-m4` was trashed after closure.
- **M5 convert** — **done 2026-10-21**, anh iu said go (CoS ran; changes the live workspace):
  1. `quaqua init --update ~/quaqua --dry-run`: expect "record only" for every managed file except local edits (conflicts). Stop on anything else.
  2. `mv ~/quaqua/.git ~/quaqua-pre-p2-<date>.git` (kept, it is the fast rollback).
  3. `trash` the engine-only paths, from `git -C ~/quaqua-pre-p2-<date>.git --work-tree ~/quaqua ls-files` minus the managed/seed list: `crates/ Cargo.toml Cargo.lock plugins/ tests/ .github/ docs/research/ templates/ LICENSE README.md SETUP-PROMPT.md .gitignore target/`. Never `scripts/` or `.claude/` (the live hook path), never `docs/plans/` (private). Abort if the list is empty or contains a private path.
  4. `quaqua init --update ~/quaqua` → writes `.quaqua/template.lock`; resolve the printed conflicts.
  - Verify: `test ! -e ~/quaqua/.git -a ! -e ~/quaqua/crates`; private manifest unchanged (`md5sum -c ~/quaqua-pre-p2-<date>.md5` → all OK); `G` passes; `quaqua config get kinds.suhuynh.model` gives the same value from `~/quaqua` and from `/`.
  - Result: all four checks passed. Both conflicts — `rules/_all/fanout-agent-grid.md` and `rules/_all/worker-timebox.md` — took the template version (anh iu); the manifest showed 4 mismatches, all expected (the 2 resolved rules + the memory log and card edited after M1).
- **M6 smoke** (CoS) — **done 2026-10-21, PASS**: `quaqua worker spawn tieude-p2smoke tieude ~/Lab/quaqua` → a one-line brief through `quaqua worker prompt` wrote `~/quaqua/runs/tieude-p2smoke.md` (HEAD `14e7d96`, worktree `~/Lab/.wt/tieude-p2smoke`, re-run matched) → `quaqua worker cleanup tieude-p2smoke ~/Lab/quaqua` (cost `$0.0032/$0.05`); cleaned. Then a fresh Claude session in `~/quaqua`: `herdr pane split` was blocked — `CLAUDE_PROJECT_DIR` hooks work in a non-git dir.
- **M7 register** (CoS, workspace md) — **done 2026-10-21**: `projects/INDEX.md` quaqua-rs/arch/arch-worker/arch-pipe rows → `~/Lab/quaqua`; `AGENTS.md` protocol note for engine workers; `rules/_all/config-dynamic.md` (embedded default = `templates/workspace/team.toml`), `rules/_all/portable-paths.md` ("the repo syncs" → "the template syncs"). Upstreamed by worker `tieude-p2upsync` as `095edc7` (3 files, `cmp` equal to `~/quaqua`); pushed as PR #83 and **squash-merged as `47f4903`** — `095edc7` is not a `main` ancestor. `~/Lab/quaqua` fast-forwarded, binary reinstalled, `init --update --dry-run` 0 conflicts, `G` PASS.

**Rollback**
- Before M5: reinstall from `~/quaqua` (`cargo install --path ~/quaqua/crates/quaqua --locked`), re-point the plugin back.
- After M5: `mv ~/quaqua-pre-p2-<date>.git ~/quaqua/.git && git -C ~/quaqua checkout -- .` (restores every trashed tracked file at `M`), `trash ~/quaqua/.quaqua`, reinstall from `~/quaqua`, plugin back. Private state was never touched; if it was, `tar -xf` the M1 archive.
- After M2: `main` keeps the template layout; reverting it needs anh iu (push).
- **Closed 2026-10-21**: every rollback artifact above was trashed after PR #83 merged (anh iu); a rollback now needs new backups.

**Live herdr panes and hooks during the switch**
- The CoS Claude session: its hook command `"$CLAUDE_PROJECT_DIR"/scripts/hooks/herdr-gate.sh` resolves to the same file before and after M5 (step 3 never touches `scripts/`, the template copy is byte-identical). Settings content is unchanged, so no `/hooks` re-review.
- Binary missing mid-M3 reinstall → the shim's fail-closed fallback blocks only the guarded/outward commands (`templates/workspace/scripts/hooks/herdr-gate.sh:31-46`). Do M3 while no worker spawns.
- Workers of other projects (`~/.wt/*` of other repos) keep running: they reach the workspace only through `workspace_root()` absolute paths, which do not change.
- herdr session restore: panes restored with cwd `~/quaqua` still find the dir (same inode).

### Needs anh iu
- ~~Approve this plan and the B1 layout~~ — approved; B1–B3 shipped (#75 #76 #77).
- ~~M2: merge `p2tmpl-integ` → `main` and push~~ — done via the six PRs; `~/quaqua` must not pull `main`.
- ~~M5: go for the in-place conversion of `~/quaqua`~~ — **done 2026-10-21** with anh iu present (the `.git` was kept as `~/quaqua-pre-p2-2026-10-21.git` until cleanup).
- ~~When to delete the one-time M1 backup and `~/quaqua-pre-p2-<date>.git`~~ — deleted 2026-10-21 after PR #83 merged (anh iu).

### Challenge
1. **No-git workspace drops history and sync for CoS-edited workspace files.** Private state (`memory/`, `projects/`, …) was never in git (`.gitignore:2-9`), so it loses nothing. What loses history is the CoS's local edits to `AGENTS.md`/`rules/_all/` between upstreams, and `rules/_all/portable-paths.md` assumes "the repo syncs across machines". Mitigation in the plan: upstream every local edit to the template **the same day** (M7, update rule). Gap: a disk loss before upstream loses those edits and all private state, exactly as today. **Resolved 2026-10-20 (anh iu): risk accepted — one-time M1 backup only, no periodic `tar --zstd` timer.**
2. **`quaqua worker spawn … ~/quaqua` stops working** (a worktree needs git). Every engine or docs task targets `~/Lab/quaqua` after M5; the CoS's own md edits stay direct (AGENTS.md rule 4). M7 updates the docs that name the repo.
3. **herdr plugin registry points into `~/quaqua`** (`~/.config/herdr/plugins.json` → `~/quaqua/plugins/herdr-grid`, a missing path; the 2026-10-20 read claiming `herdr-quaqua` was wrong). It must be re-pointed (M4) or the grid hook silently disappears once `plugins/` is trashed. **Resolved 2026-10-21**: `herdr plugin link ~/Lab/quaqua/plugins/herdr-quaqua` — list clean, isolated smoke needed a short socket dir, live reconcile passed.
4. **Accepted:** the engine clone gets no portable-paths `pre-commit` (`init.rs:534` wires only the init target). Follow-up: a `quaqua init --git-hooks <repo>` flag, out of this phase — **still open 2026-10-21**, being built separately (`tieude-githooks`).

### P2 risks
| Step | Risk | Mitigation |
|---|---|---|
| B1 | a moved file still read from the old path (tests, `include_str!`) | `cargo test` + `run.sh --slow` at the gate; `rg -n '"\.\./\.\./team\.toml"\|\.\./\.\./\.\./team\.toml' crates` empty |
| B1–M2 | `~/quaqua` pulls the new `main` and loses its root `AGENTS.md` | integ not merged until M2; M2 note: `~/quaqua` never pulls |
| B2 | the embedded table misses a file or its exec bit | `init` sandbox test checks `-x` on both scripts and byte-equality against `templates/` |
| M3 | old clone has local state | `status --porcelain --ignored` empty first |
| M5 | trash list catches a private or live-hook path | list built from `ls-files` minus template, abort rule, md5 manifest check |
| M5 | a pane mid-task reads a file being replaced | M0: no Lead alive, no worktree of `~/quaqua` |
| later | `init --update` overwrites a CoS edit | only when `W == L`; every other case goes to `.quaqua/pending/` |

## Steps (to-plan 2026-10-16)
Driver: the Lead `quaqua-rs-lead` (`quaqua lead spawn quaqua-rs`) runs the pipeline in Peer worktrees from `quaqua/quaqua-rs-integ`. The CoS verifies, and anh iu approves every push and every merge to main.

### P1: decouple in place — **done 2026-10-16 on `main`** (`80d22f8` workspace_root, `f3432d9` grid in-process)
1. **workspace_root**
   - Builder: a Peer (tieude).
   - Change: add one resolver. It reads `QUAQUA_HOME` first, then `$HOME/quaqua`.
   - Route 4 call sites through it: `spawn.rs:257`, `cleanup.rs:12`, `config.rs:262`, `main.rs:183`.
   - Verify:
     - `rg -n 'join\("quaqua/' crates/quaqua/src` finds only the resolver
     - a new unit test covers the env override and the default
     - `cargo test --locked` passes
2. **grid → Rust**
   - Builder: a Peer (tieude).
   - Change: give `quaqua grid reconcile` the full behaviour of `grid-workers.sh`, including `--only`, `--tab` and `--dry-run`. Then point every caller of `find_grid_script`/`run_grid_hook` at it: spawn, cleanup and pipeline (`pipeline/mod.rs:667-1230`).
   - Verify:
     - `rg -n 'grid-workers.sh' crates/quaqua/src` finds nothing
     - the 25 grid fixtures pass the differential test
     - `bash tests/test_pipeline_grid.sh` and `tests/test_spawn_worker.sh` pass
     - one live spawn→cleanup run on herdr arranges the panes correctly
3. **shim**
   - Change: `scripts/worker/grid-workers.sh` becomes a thin `exec quaqua grid reconcile "$@"`.
   - Verify: `bash tests/run.sh --slow` gives 25 pass / 0 fail (3 skip allowed).
4. **gate P1**
   - Review: suhuynh reviews the integ branch.
   - Then: PR integ→main, anh iu approves the merge, reinstall the binary.
   - P1 is done here. The system works even if P2 never lands.

### P2: one repo, embedded template — **done 2026-10-21** (B1–B3 2026-10-20, M0–M7 2026-10-21)
Steps B1–B3 and M0–M7 live in Phase 2 above (rewritten 2026-10-20).

## Fragile assumption
Every workspace path the binary uses goes through `workspace_root()`. If one is missed, the binary silently reads or writes `~/quaqua`. The Phase 1 rg check is the guard.

## Out of scope
- New features
- Publishing to crates.io
- Changing CI beyond what `tieude-ci` adds

## Engine core decisions (anh iu, 2026-10-19 · raw/2026-10-19-quaqua-archresearch.md)
- Core: embedded event-sourced workflow engine (6 layers: runtime · journal · activities · patterns · CLI/OTel · testkit).
- Journal: SQLite (`rusqlite` bundled, WAL) replaces `runs/.{name}.*` dotfiles.
- Process: ephemeral CLI, no daemon.
- Patterns: Rust `Workflow` trait impls; declarative TOML deferred.
- Migration: strangler fig in `crates/quaqua` — taskforce → worker/prompt → pipeline, tests green each commit; before the repo split.

## Steps: engine core (to-plan 2026-10-19, 3 Leads × 8)
Drivers: `engine-lead`, `worker-lead`, `pipe-lead` — 4 pipelines each (sude build + suhuynh review, one round) = 24 task workers. Peers branch from `quaqua/arch-integ`. One owner per **file** (listed below); nobody else edits it. `engine-lead` owns `arch-integ` and runs W2. CoS re-runs every Verify; anh iu approves the W0 contract, every push and merge to main.
Critical path: W0 (~1h) → anh iu approves → W1 (12 pipelines, longest ~2h) → W2 (~2h). Target: same day.
> **Status (2026-10-20)**: W0–W2 **DONE** — W2 closed on `arch-integ` `74e7a5c`, merged to `main` as #62 `0c7c71a`; the CoS re-ran `cargo test --no-fail-fast` 375 pass / 0 fail at the W2 gate.

### W0: freeze the contract (`engine-lead`, one pipeline, blocks W1)
- `crates/quaqua/src/engine/` API only (types, traits, docs, `todo!()` bodies; no callers):
  - `workflow.rs`: `Workflow` trait `step(&self, state, ctx, event) -> StepDecision` (Continue · ScheduleActivity · ScheduleTimer · StartChild · PauseForApproval · Complete · Fail); `WorkflowContext` = scheduling + `now()` only.
  - `activity.rs`: `Activity` trait + typed request/result enums for herdr, herdr events, git, grid, report-file, cost.
  - `journal.rs`: `Journal` trait; SQLite schema `events(workflow_id, seq, ts_ms, kind, payload_json)` + `snapshots(workflow_id, seq, state_json)`; DB `<workspace_root>/runs/quaqua.db`.
  - `telemetry.rs`: `SpanSink` trait, OTel GenAI field list.
  - `testkit.rs` (implemented, small): `VirtualClock`, `MockActivities`, in-memory `Journal`, `run_to_completion`.
  - Child contract: `SingleWorkerInput {name, kind, repo, brief, budget_sec, cost_budget}` → `SingleWorkerOutput {exit: 0|2..6, report_path}`.
  - File-ownership table for W1 checked into `engine/OWNERS.md`; a test fails if a W1 file listed there is missing after W2.
- Verify: `cargo test --locked` green; toy workflow runs on the testkit; determinism-guard test fails on `std::(fs|process|net)` in any `*Workflow` impl. **Gate: anh iu reviews `engine/` API before W1.**

### W1: 12 pipelines in parallel
Every W1 brief names its acceptance tests up front; W1 Peers skip the plan gate (the W0 contract is the plan). Peers verify only their own module: `cargo test --locked --lib <module>::` (testkit, no herdr, no bash suites, no full `cargo test`). Integration and live checks belong to the test team in W2.
**engine-lead** (`src/engine/`)
- E1 `journal_sqlite.rs`: `rusqlite` (`bundled`, pinned `=`), WAL, `busy_timeout=5000`. Verify: 2 concurrent writers, crash between append and snapshot recovers.
- E2 `runner.rs`: load snapshot → replay → step → run activity → append → exit when blocked. Verify: kill mid-run → resume = same state; 7200 s timer <50 ms on `VirtualClock`.
- E3 `activities.rs` + `clock.rs`: real impls over `herdr.rs`/`git.rs`. Verify: unit tests with a `HERDR_BIN_PATH` stub.
- E4 `telemetry_otel.rs`: spans to `runs/traces.jsonl`. Verify: golden-file test of one span.

**worker-lead** (`src/worker/prompt/`, `src/herdr_events.rs`, `src/taskforce/`)
- K1 `worker/prompt/workflow.rs`: `SingleWorkerWorkflow` timers (budget, grace, autoext, stall 600 s) → exit codes 2–6. Verify: one testkit test per exit code.
- K2 `herdr_events.rs`: `events.subscribe` activity (`pane.agent_status_changed`, `pane.exited`). Verify: stub socket test.
- K3 `worker/prompt/cost.rs` + report activity: report mtime + cost80/100 as activity results; drop ANSI scraping in `observer.rs`. Verify: testkit tests for cost80/100 + report-ready.
- K4 `taskforce/mod.rs`: `TaskforceWorkflow`. Verify: quorum + one-fail testkit tests.

**pipe-lead** (`src/pipeline/`, `src/grid/`, `src/status.rs`, `tests/worker_unit.rs`)
- P1 `pipeline/mod.rs` + `pipeline/state.rs`: `PipelineWorkflow` (builder child → plan gate → reviewer child → fix), children mocked per the W0 contract. Verify: plan gate, review fail → fix, resume at phase.
- P2 `grid/` activity: grid repair driven by events, no sync call in phases/spawn. Verify: unit tests on layout decisions.
- P3 `status.rs` (#47 P2): `quaqua status` reads `runs/quaqua.db`; WAITER-LOST flag. Verify: fixture DB tests.
- P4 `tests/worker_unit.rs`: replace the 6 ad-hoc `HerdrOps` mocks with `MockActivities`. Verify: compiles; test team runs it in W2.

Contract changes during W1: only via `engine-lead`, landed on `arch-integ` first; every team rebases.

### W2: integrate (`engine-lead`) + one test team — **done 2026-10-20**
- Merge the 12 branches into `arch-integ` in batches; wire K/P workflows to E1–E3; delete the `.{name}.*` dotfile code in `state.rs`.
- Test team `tester` (1 worker, tieude, own worktree on `arch-integ`) runs the full suites once per merge batch and files failures back to the owning Lead: `cargo test --locked`, `test_taskforce_cap.sh`, `test_prompt_worker*.sh`, `test_pipeline*.sh`, `--test pipeline_prompt`, `--test worker_unit`, live herdr probe.
- Done when: `bash tests/run.sh --slow` all PASS; `rg -n '\.budget|\.grace|\.autoext' crates/quaqua/src` empty; live spawn → prompt → cleanup, live pipeline, kill → `--resume`.

### Risks
- Provider concurrency (12 omp + 12 opencode at once) is unverified; throttling stalls workers → CoS checks the first 10 min and drops to 2 Leads if more than 3 stall.
- W2 conflicts if W0 under-specifies; mitigated by the contract tests and file ownership.

### Then
Repo split (Phase 2 above) after W2 — **done 2026-10-21** (M0–M7); `quaqua-rs` runs on the new core.
