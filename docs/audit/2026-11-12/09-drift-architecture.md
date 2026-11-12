# Drift and architecture: keep, change, cut, and where a redesign wins

**Verdict: PARTIAL, 3/5.** The core (event-sourced runner, SQLite journal and bus, typed reports, worktrees) is the right foundation. The engine has drifted toward terminal layout work and has no supervising process, no task entity and three unjoined telemetry stores.

Audited at `41a5327`. `src/` means `crates/quaqua/src/`. `tpl/` means `templates/workspace/`.

## Scope

- Brief step 4: "Find drift: features that do not serve the intent, and parts of the intent that nothing in the engine serves."
- "Engineered properly: a daemon, bus, database, or service is used whenever it is the right design."
- Intent: "If a daemon, an event bus, a database, or a persistent service is the right design, it belongs there. Every decision is judged on correctness, testability, observability, and how well it serves the lifecycle, not on what was decided before."

## Findings

### Drift: features that do not serve the intent

| Feature | Status | Evidence | Gap |
|---|---|---|---|
| Pane grid layout planner and reconciler | contradicted | `src/grid/` is 4720 lines (`cat crates/quaqua/src/grid/*.rs \| wc -l`), plus `tests/grid.rs` 1985 and `tests/layout.rs` 2697 lines, 22 JSON fixtures in `tests/fixtures/herdr-quaqua/`, and a herdr plugin (`plugins/herdr-quaqua/`). | The intent asks for "no manual layout", not for a layout engine. Arranging panes is for a human watching; a headless worker needs none. The grid also caps a tab at 4 workers (`tpl/AGENTS.md:85`) and adds a "mandatory grid-arrange pause" to the pipeline's exit 5 (`src/pipeline/mod.rs:71`). |
| herdr GUI specifications | partial | `CHANGELOG.md:73` (Shepherdr-style GUI panel specs); commits `fcdb4a0`, `cad5ea7`, `1ee6b8d` went to `main` without a PR. | Deferred by the owner already (`.harness/plans/active/harness-mvp.md:13`). They remain in the engine repo. |
| `quaqua usage` | partial | "OpenCode usage audit" (`src/usage.rs:1`, 1897 lines). | One vendor's logs. Overlaps with per-worker spend in `src/worker/prompt/cost.rs` and `status --metrics`. Its tests need an external `sqlite3` binary. |
| CoS persona in the managed template | partial | Persona and voice rules (`tpl/AGENTS.md:13`, `tpl/SOUL.md`, `tpl/identity.md`). | Harmless, but it is injected into the context the intent wants "kept small". It belongs in the seed files the owner edits, not in the managed harness. |
| Two layout generations and migration code | partial | `src/layout/legacy.rs`, `src/layout/migrate.rs` (488 lines), `quaqua config migrate` from schema 2 to 3 while the shipped template is still `schema_version = 2` (`tpl/team.toml:2`), `src/bus/migrate.rs` (953 lines). | For a single-owner system, carrying both generations is the "loyalty to earlier choices" the intent rejects. |
| Stale and conflicting docs | contradicted | `README.md:10` says "This repo is the live workspace: clone it to `~/quaqua`"; root `AGENTS.md:3` says "This repository is the **engine** ... not a workspace". `.harness/PROJECT.md:26` names `.harness/plans/active/superagent-bus.md` as the active plan; that file is under `.harness/plans/done/`. `README.md:118` shows install tag `v0.1.0` while the crate is 2.6.0 (`crates/quaqua/Cargo.toml`). | A coordinator that reads these gets wrong instructions. |

### Drift: parts of the intent nothing in the engine serves

| Intent | Status | Evidence | Gap |
|---|---|---|---|
| Goal to merged change "on its own" | missing | No command spans intake to merge; 6 human steps remain (`.harness/plans/active/harness-mvp.md:17`). | A task entity and a driver. See `02-adlc.md`. |
| Intake | missing | `grep -rni intake crates/quaqua/src` finds nothing. | See `02-adlc.md`. |
| Wave accounting | missing | `grep -rn "wave\|absorbed" crates/quaqua/src` finds nothing. | See `04-fanout.md`. |
| Self-written skills | missing | No code writes under `skills/`. | See `05-cos-memory.md`. |
| Telemetry-driven switch-off | missing | `review_changed_outcome` is recorded, never acted on (`src/status.rs:1070`). | See `07-rules-self-improvement.md`. |
| Engine-side verification rerun | missing | Rerun is a prose instruction (`tpl/AGENTS.md:78`). | See `01-harness-engineering.md`. |
| Hard cost cap | partial | Default `warn` (`src/config.rs:29-36`). | See `04-fanout.md`. |
| Non-Claude Chief of Staff | missing | CoS hooks, router skill and memory mod are Claude Code specific (`tpl/.claude/settings.json`, `mods/cos-memory/README.md:38-60`). | A dormant plan exists (`tpl/models.md:29-35`); it would lose the gate, inbox hooks, memory hooks and review fork. |

### Architecture today

| Aspect | Status | Evidence | Gap |
|---|---|---|---|
| Durable workflow core | met | Event-sourced runner with replay, snapshots and a determinism guard (`src/engine/runner.rs:1-5`, `src/engine/guard.rs:1-2`). SQLite WAL journal (`src/engine/journal_sqlite.rs:1-4`). | None. This is the part to build on. |
| Process model | partial | A single CLI binary; every verb shells out to `herdr` and `git` (`.harness/PROJECT.md:21`). Each worker's watchdog is the `quaqua worker prompt` process, backgrounded by the coordinator (`tpl/AGENTS.md:62`). Scheduled work runs from systemd or launchd timers (`README.md:7`, `:128-131`). | Timers, retries and cost checks exist only while some shell keeps a foreground process alive. A coordinator crash, compaction or reboot stops enforcement until a resume. `WAITER-LOST` is a named state (`src/status.rs:58`). |
| Bus | partial | Triggers, threads, subscriptions and cursors in `runs/quaqua.db` (`src/bus/mod.rs:1-4`). herdr events enter through a hook process per event (`src/bus/events.rs:1393`, `:1627`). Delivery to an agent is a nudge typed into its pane (`src/bus/events.rs:1315`). | No consumer runs continuously. Wake depends on a pane being idle and on hook delivery. |
| Stores | partial | `runs/quaqua.db` (journal, bus, verdicts), `runs/telemetry.jsonl`, `runs/traces.jsonl` (unused sink), `runs/transcripts.db`, plus per-worker dotfiles: `.budget`, `.grace`, `.autoext`, `.playbook`, `.error`, `.state`, `.repo`, `.owner` (`tpl/rules/_all/worker-timebox.md:3-6`, `src/worker/prompt/retry.rs:53-60`, `tpl/rules/_all/lead-gate.md:21`). | Worker state is spread over a dozen files and two formats with no shared key. Project state is parsed out of a Markdown table (`src/pipeline/mod.rs:89-116`). |
| Terminal dependency | partial | Workers are herdr panes; agy liveness is screen detection (`CHANGELOG.md:18`); prompts are typed into a pane with a typed-line limit (`CHANGELOG.md:43`). | Conflicts with "nothing lives only in a terminal". Every harness here has a headless mode the auditor already uses (`CHANGELOG.md:19`). |

## Keep, change, cut

**Keep**
- The event-sourced runner, journal and determinism guard (`src/engine/`).
- SQLite as the one database. A single owner on one machine does not need a network service or an external broker.
- Typed reports, the `Outcome` enum, verdict levels and SHA binding.
- Worktree-per-worker isolation and credential scrubbing.
- The role and layout gate, the Lead gate and the merge queue.
- The memory caps and secret gate, transcript index and compaction snapshot.
- Playbooks and principle leaves as versioned data.
- The auditor's deterministic pre-checks.
- The hermetic test guard in CI.

**Change**
- Add a long-lived `quaqua supervisor` (systemd user service or launchd agent) that owns every worker's timers, cost checks, retries and liveness, subscribes to herdr's event stream, and drives task state. CLI verbs become thin clients that write a request and read state.
- Add `tasks`, `waves`, `approvals` and `merges` tables. Move the per-worker dotfiles and `telemetry.jsonl` rows into tables keyed by task id and run id.
- Read projects from a table or a TOML file, not from a Markdown table.
- Put harness specifics behind an adapter described in config, with a headless transport as the default and a herdr pane as an optional view.
- Make the router a deterministic command with an LLM tie-break.
- Give the CoS a harness-neutral core: gate, inbox and memory as engine commands that any harness's hook mechanism can call, with the Claude Code mod as one binding.

**Cut**
- `src/grid/` as a required part of the spawn and pipeline path. Keep it only as an optional viewer plugin.
- The GUI specs from this repo.
- `src/layout/legacy.rs`, the v2 layout path and `quaqua bus migrate` once the live workspace is on the current schema.
- `quaqua usage`, once per-harness spend is in the journal.
- The unused `JsonlSpanSink`, or wire it; do not carry both.
- The persona files from the managed template.

## Where a daemon, bus, database or other redesign beats today's design

| Concern | Today | Better design | Why it wins |
|---|---|---|---|
| Limits and liveness | One foreground poll loop per worker, 60 s (`src/worker/prompt/mod.rs:895-898`) | Supervisor daemon with one timer wheel and herdr event subscription | Enforcement no longer depends on a coordinator's shell. Reboot recovery is the daemon reading the journal at start. Detection latency drops from a poll interval to an event. Testable with the existing clock seam and testkit. |
| Lifecycle | Separate verbs chained by an LLM from prose | A `Task` workflow on the existing runner with stages as states | The eleven stages become a state machine with one id. A skipped stage is an illegal transition, not a forgotten instruction. |
| Wake and report | Hook process per event, nudge typed into a pane | Daemon consumes events and delivers to subscribers; agents block on `quaqua wait <task>` over a local socket | Removes polling and the idle-pane dependency. |
| Observability | Three stores, no shared key | Single journal with run id and task id on every row; views for metrics | Every reference target becomes a query. |
| Cost | Per-harness log scraping inside the watchdog loop | Spend collector in the daemon, with a kill path per harness including pane kill | A hard cap is possible for every kind. |
| Scheduled work | systemd timers for auditor and CI poll | Jobs inside the daemon | One install, one health check, missed runs replayed from the journal. |
| Database | SQLite | Stay on SQLite | WAL with a single writer daemon removes the busy-retry handling the CLI needs today (`CHANGELOG.md:33`). A server database adds operations work for no gain at this scale. |
| External bus | none | Do not add one | One machine, one owner. The SQLite bus plus a local socket covers it. |

## Refs

- Line counts: `find crates -name '*.rs' \| xargs wc -l` (114,250 total); `src/grid/*.rs` 4720.
- Unreviewed commits: `git log --first-parent --oneline main \| grep -v "Merge pull request\|(#[0-9]*)"` lists `fe188ba`, `966d965`, `160c3ed`, `1ee6b8d`, `cad5ea7`, `fcdb4a0`.
- The daemon recommendation is my engineering judgment from the evidence above, not a measured comparison. INFERENCE: I did not benchmark detection latency or failure rates of the current design.
- An earlier plan rejected "any memory daemon" for CoS memory (`.harness/plans/done/cos-memory-loop.md:28`). That non-goal covers memory only and, per the brief, earlier decisions are not binding.

## Fixes

1. Build the supervisor daemon around the existing runner and move the watchdog into it. This is the largest single step toward "limits apply to every worker" and "survives a restart".
2. Add the task table and `Task` workflow, then re-point `pipeline`, `merge` and a new `ship` at it.
3. Collapse telemetry into the journal with task and run ids.
4. Make herdr optional: headless transport first, grid as a viewer.
5. Fix the three stale docs (`README.md:10`, `.harness/PROJECT.md:26`, `README.md:118`).
6. Delete the cut list once the above lands.
