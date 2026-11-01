# Open questions — 2026-11-01

Two items that were left unfixed on purpose. Neither can be settled by editing a
document; each needs a check this note specifies. Read the item, run the check,
then fix the owning file (named per item) — do not re-derive the background.

Base: `main` @ `2b1b5ab`. Every code line number below is from that tree.

---

## Item 1 — 3-worker grid geometry, and whether a running pane may be moved

### The claim under dispute

Two managed templates that both ship into a workspace say opposite things.

| Side | `file:line` | Text |
|---|---|---|
| Skill reference | `templates/workspace/.claude/skills/herdr-orchestration/references/operations.md:28` | "Scripts arrange 1 worker beside its orchestrator (anchor 2/5, worker 3/5); 2–4 in the project's workers tab: 2 left\|right, **3 A\|B top + C below**, 4 a 2x2 grid." |
| Rule | `templates/workspace/rules/_all/fanout-agent-grid.md:6` | "… 2 = left\|right, **3 = left full + right stacked `A │ B/C`**, 4 = 2x2 `A B / D C` …" |
| Skill reference | `operations.md:30` | "**Never move a working agent**, during `--wait`, or without `--no-focus`." |
| Rule | `fanout-agent-grid.md:12` | "**Moving or swapping a pane while its agent runs is allowed** — the move keeps the agent process and session (isolated herdr smoke, issue #79); still avoid moving panes during `--wait`." |

### What the code does today

Geometry is decided in Rust, not delegated to herdr: herdr only ever executes
`pane split` / `pane move`; quaqua computes the target shape.

- `crates/quaqua/src/grid/plan.rs:165` — the doc comment on the target cells:
  "2 = `A │ B`, 3 = `A │ B/C` (left full, right stacked), 4 = `A B / D C`."
- `crates/quaqua/src/grid/plan.rs:179-183` — `cells(3)` returns exactly three rects:
  left half **full height**, then right half split into top and bottom.
- `crates/quaqua/src/grid/plan.rs:200-201` — the build order that produces it:
  `1 => Some((0, "right"))`, `2 => Some((1, "down"))`. Pane 1 splits right off
  pane 0; pane 2 splits down off pane 1. There is no step that puts two panes on
  top and one across the bottom anywhere in `placement()` / `build_order()`
  (`plan.rs:198-210`).
- `crates/quaqua/tests/grid.rs` has no 3-pane geometry case (it covers 1, 2, 5, 8, 9
  workers at `:82`, `:108`, `:140`, `:162`, `:190`); the shape is pinned by
  `plan.rs:900 test_four_sequential_spawns_give_equal_2x2`, whose `wanted =
  ["right", "down", "down"]` asserts the direction of every split and, after
  spawn 3, that the tree `matches(&tree, 3)` — i.e. equals `cells(3)`.

So the rule (`fanout-agent-grid.md:6`) is what the code builds; `operations.md:28`
describes a layout that no code path can produce. The commit that rewrote the rule
says so explicitly and did not touch the skill file: `3731bf0 fix(grid): build and
restore the issue #79 target shape` — message line "Rule: 3 = A | B/C,
moving/swapping a running pane is allowed", diffstat touches only
`rules/_all/fanout-agent-grid.md`.

The same line of `operations.md:28` is stale in a second way: "anchor 2/5, worker
3/5". `plan.rs:462-484` splits the lone worker off the owner pane with
`ratio: 0.4` and, per its own comment (`plan.rs:464-469`), treats "shares the
anchor's tab" as the acceptance test — "a herdr `pane split` lands 50/50, never
the old 2/5|3/5". A doc that tells an agent to look for a 2/5|3/5 border will
never see it.

Both files are **managed**, not seed-once: `crates/quaqua/src/init.rs:40-45`
lists only the four `SEED_TARGETS`, and `init.rs:423-431` maps a seed to a renamed
destination — anything else (including `.claude/skills/**`) is written by `quaqua
init` on every run. A hand-edited copy in a workspace is therefore overwritten or
sent to `.quaqua/pending/`; the fix belongs in the engine template.

### Moving a pane whose agent is running

The two sentences are not actually about the same thing, and neither is the whole
truth:

- herdr capability: a `pane move` keeps the agent process and its session. The
  rule cites that as an isolated smoke under issue #79; nothing in this tree
  re-runs it (see "What a live check must show").
- quaqua policy: the grid **refuses** to move a pane that is working.
  `crates/quaqua/src/grid/apply.rs:145-148`:
  `if pane_obj.agent_status.as_deref() == Some("working") { return Err(format!("pane {actual_pane} became busy; replan required")); }`
  — and the plan defers such panes before it ever gets there:
  `plan.rs:477` (and 644, 664, 686, 695) sets `deferred: is_status_deferred(...)`
  from `templates/workspace/team.toml:100` `deferred_statuses = ["working"]`.
  Pinned twice: `crates/quaqua/tests/grid.rs:534`
  (`test_apply_ops_rechecks_idle_and_fails_if_busy`) asserts the error string, and
  `tests/grid.rs:763` / `:902` assert the deferred-status config path.
- `Target` has no `Move` variant at all (`grid/types.rs:119-131`: `NewTab` |
  `Split`), so every reposition is park-then-split (`apply.rs:251-264`), and the
  park is the move the guard above covers.

Read together: `fanout-agent-grid.md:12` is true about herdr and is the reason a
repair never kills an agent's session; `operations.md:30` is true about what the
repair will *do* (it defers), but stated as an absolute rule it is wrong — the
deferral list is exactly `working`, so a `blocked` pane (an agent waiting on a
question) is **not** deferred and can be moved mid-answer. That corner is untested
and unnamed in both docs.

### What a live check must show

Isolated herdr only — temp `HOME` + `XDG_CONFIG_HOME`, `HERDR_SOCKET_PATH` unset,
never the live daemon (`AGENTS.md` '## Handoff', citing `runs/tieude-pubfix.md`,
2026-10-14):

1. Spawn 3 stub agents into one `<repo>-workers` tab, run
   `quaqua grid reconcile --tab <repo>-workers`, then
   `herdr pane list --json` → the three rects must be: one at half width and full
   height, two at half width and half height stacked in the right half. That is
   `cells(3)` (`plan.rs:179-183`) and settles the geometry question outright; a
   "two on top, one across the bottom" result would falsify the rule instead.
2. Re-run the same reconcile → no ops applied (converged), i.e. `--check` does not
   churn on a correct layout.
3. With all three agents mid-turn (`agent_status = working`), run reconcile again
   → expect `WATCHDOG`-free no-op: every op deferred, no `became busy` error.
4. Separately: `herdr pane move` one working agent, then `herdr agent list` /
   `pane get` → the same agent name must still report the same session. That is
   the only thing that re-proves the "#79 smoke" clause the rule cites.

### Verdict

- Geometry: `resolved by the code — operations.md:28 is stale on both the 3-pane
  shape and the 2/5|3/5 ratio; fix that line`.
- Move-while-working: `open — needs an isolated herdr smoke for the capability
  claim, plus a decision on whether operations.md:30 should say "deferred, not
  forbidden" and whether blocked panes should be deferred too`.

---

## Item 2 — `worker prompt --resume` exits 3 with a journal sequence conflict

### The claim under dispute

`quaqua worker prompt --resume <name>` exited **3** with
`journal error: journal sequence conflict for worker:tieude-cleanrepo3#1: expected 134, got 122`
while that worker was healthy and still working; its report landed afterwards.

`~/quaqua/runs/suhuynh-jlock.md` (2026-10-29) explains an earlier journal failure —
`database is locked` — as a stale pre-fix binary on `PATH`. That is a **different**
failure: lock contention is timing, a sequence conflict is arithmetic, and the
stale binary has nothing to do with it. `journal_sqlite.rs` is at `be33350` (the
busy-retry fix) and the conflict guard predates it by far — `git log -S
"SequenceConflict"` points at `0c7c71a arch: engine core rewrite W0–W2 (#62)`.

### Where the number comes from

- The error is rendered at `crates/quaqua/src/engine/journal.rs:48-50`.
- It is constructed at exactly one production site,
  `crates/quaqua/src/engine/journal_sqlite.rs:593-605`:

  ```rust
  let next_seq = current_max + 1;
  let assigned_seq = if event.seq == 0 { next_seq }
  else if event.seq != next_seq { ...SequenceConflict { expected_seq: next_seq, actual_seq: event.seq } }
  ```

  `current_max` is `SELECT COALESCE(MAX(seq), 0) FROM events WHERE workflow_id = ?1`
  (`journal_sqlite.rs:570-577`) read **inside** `BEGIN IMMEDIATE` — so the guard
  sees the truth. `expected 134` means the DB held 133; `got 122` means the
  caller asked to write 122. The journal was not corrupt; it refused a stale
  writer, correctly.
- The stale number is the caller's, not the journal's: the runner reads the
  maximum once per process and keeps it in memory —
  `crates/quaqua/src/engine/runner.rs:358-359`
  `let all_events = journal.load_events(workflow_id, 1)?;`
  `let mut current_seq = all_events.iter().map(|e| e.seq).max().unwrap_or(0);`
  — then increments `current_seq` and passes it explicitly as `event.seq` on every
  append (`runner.rs:207-211`, `254-259`, `693-697`, …). One process can never
  diverge from the DB it is the only writer of; two can.
- `--resume` deliberately attaches to the **same** session id:
  `crates/quaqua/src/worker/prompt/mod.rs:994-1000`
  `let wf_id = format!("worker:{name}#{k_max}"); ... ExecAction::Resume { wf_id, budget }`,
  executed by `runner::resume` at `mod.rs:1123-1130`. And nothing stops a second
  watchdog from being alive: there is **no** pid file, lock or single-instance
  guard on `quaqua worker prompt` (`grep -rn "flock\|already running\|single.instance\|LockFile"`
  across `crates/quaqua/src/worker/` hits only a doc comment in `leadgate.rs:7`; the
  only `process::id` uses under `worker/prompt/` are test temp-dir names),
  and the liveness check is skipped for resume on purpose — `mod.rs:774-790`
  ("`--resume` keeps the watchdog's own gone/exit-3 handling"; `Liveness::Alive`
  is not consulted to refuse).
- The mismatch is then reported as a **stall**: every `RunnerError` maps to
  watchdog exit 3 — `mod.rs:1201-1209`, `return Err(watchdog_exit(3, &r))` — the
  same code as a real stall, an idle agent or a gone agent. `suhuynh-jlock.md:29`
  already flagged this conflation for the locked case; it applies to any runner
  error, including this one.

### Reconstruction from `~/quaqua/runs/quaqua.db` (real rows, read-only)

```sh
sqlite3 -readonly ~/quaqua/runs/quaqua.db \
  "select strftime('%H:%M:%S', ts_ms/1000, 'unixepoch'), min(seq), max(seq), count(*)
   from events where workflow_id='worker:tieude-cleanrepo3#1'
   group by ts_ms/1000 order by 1;"
```

| tick (UTC) | seq range | rows |
|---|---|---|
| 12:58:05 | 1–13 | 13 |
| 12:59:05 … 13:07:06 (9 ticks) | 14–121 | 12 each |
| 13:08:06 | 122–133 | 12 |
| 13:09:06 | 134–145 | 12 — **and** the instant the failing appender tried 122 |
| 13:09:29 | 146–160, ends `workflow_completed` | 15 |

`farm_events` for the same name:

```
568 | 2026-11-01 12:57:55 | spawned
569 | 2026-11-01 13:09:06 | stalled | {"reason":"runner error: journal error: journal sequence conflict for worker:tieude-cleanrepo3#1: expected 134, got 122"}
570 | 2026-11-01 13:09:29 | report_written
```

And `runs/.tieude-cleanrepo3.read` holds `1793538569` = `13:09:29Z`, written by the
**successful** watchdog when it returned the report (`mod.rs:1299-1300`); the worker
really was healthy, exactly as reported.

What this proves: at 13:09:06 one writer committed 12 events while another writer,
whose cursor stood at 121 — a value it must have read between 13:07:06 (when 121
was written) and 13:08:06 — tried to append 122. Two independent cursors existed on
one workflow id, so two writers existed. It also shows the loser never inserted a
row: the steady-state ticks carry 12 events each, none of them 24, so the loser
died on its first append attempt rather than interleaving. The guard worked; the
failure is a bookkeeping/exit-code defect, not a data one.

What it cannot prove post-hoc: **which** two invocations. There is no watchdog
registry, and `telemetry.jsonl` cannot answer it — the worker row is written by
`quaqua worker cleanup` (`crates/quaqua/src/worker/cleanup.rs:61`, `:132` counts
`worker:<name>#k` sessions), so the single `{"name":"tieude-cleanrepo3","prompts":1,
"exit":0,"wall_sec":719}` row says "one session", not "one process".

### What a live check must show

Deterministic, no live herdr needed for the first half — the existing harness
already builds one: `crates/quaqua/src/worker/prompt/mod.rs:1972-2024`
(`test_worker_prompt_runner_error_gives_non_zero_watchdog_exit`) drives
`prompt_worker(name, None, true, …)` against a stub herdr and a pre-populated
`quaqua.db`, and pins `Err((3, String::new()))`. Extending it:

1. Pre-populate `worker:<name>#1` with N events, start resume A, and — from a second
   `SqliteJournal` handle, between A's read and A's first append — append 12 events.
   A must fail with `SequenceConflict { expected_seq: N+13, actual_seq: N+1 }` and
   the watchdog must return `Err((3, ...))`. That reproduces this incident exactly
   (same 12-event tick, same off-by-one shape) with a stub and no live grid.
2. Then the decision the check exists to inform — pick one and only one:
   - refuse the attach: a per-name guard (`runs/.<name>.watchdog` pid + aliveness,
     or an advisory lock) so `--resume` on a name whose watchdog is already alive
     exits with a distinct code *before* touching the journal; or
   - tolerate it: let the runner re-base `current_seq` from the DB on
     `SequenceConflict` (or append with `seq: 0`, which the journal already
     assigns, `journal_sqlite.rs:594`).
3. Whatever is chosen, stop reporting it as `3`: a runner error is not a stall, and
   the CoS reflex (`--resume` on exit 3) re-enters the same race.

A live repro would be two `quaqua worker prompt --resume <name>` processes with
`PROMPT_WORKER_POLL_SEC=1` against one stub worker; expected observation is exit 3
on the later process plus one `stalled` farm_event whose reason starts
`runner error:`, and **no** gap in the surviving session's seq chain. Distinguishing
the two candidate causes needs nothing more: a lock/contention bug would surface as
`StorageError("database is locked")`, never as `expected/got`.

### Verdict

`open — needs one repro under the stub-herdr harness and a decision between a
single-instance guard and re-basing the cursor, plus a non-3 exit code for runner
errors.`

---

## Cross-cutting gotchas

- **Exit 3 is overloaded.** Stall, idle, gone, `workflow blocked`, `workflow
  failed`, and every runner error all return 3 (`mod.rs:1189`, `:1199`, `:1208`).
  Any "on exit 3, read the pane then `--resume`" loop re-enters a runner-error
  loop, as here.
- **`blocked` is not deferred.** Only `working` is (`team.toml:100`); a pane whose
  agent waits on a question can be relocated by a repair.
- **Both Item 1 files are managed templates.** Fix them in the engine, not in a
  workspace copy — a workspace edit is overwritten by the next `quaqua init`.
- **`quaqua worker cleanup` is the only writer of the worker telemetry row**
  (`cleanup.rs:61`), so telemetry can never count how many watchdogs touched a
  session.

## Checks run while writing this note

```
cargo test --locked -p quaqua --lib grid::plan::tests::test_four_sequential_spawns_give_equal_2x2
   test grid::plan::tests::test_four_sequential_spawns_give_equal_2x2 ... ok
   test result: ok. 1 passed; 0 failed; 438 filtered out
cargo test --locked -p quaqua --lib engine::journal_sqlite::tests::test_sqlite_journal_sequence_conflict
   test engine::journal_sqlite::tests::test_sqlite_journal_sequence_conflict ... ok
   test result: ok. 1 passed; 0 failed; 438 filtered out
```

The first proves the code builds the `A │ B/C` shape (the tree is asserted against
`cells(3)`); the second proves the guard rejects a non-consecutive explicit seq. The
incident rows above were read with `sqlite3` SELECTs only; the first few reads ran
without `-readonly` (no rows changed — `quaqua.db` and `quaqua.db-shm` mtimes
unchanged, only `quaqua.db-wal` advanced on close). Use `-readonly` next time.

## What this note did not run

- No live herdr grid, no `worker spawn`, no live `--resume`, no restart of
  anything; no writes under `runs/`.
- No full `cargo test`; only the two filters above.
- The "#79 move-keeps-the-session" smoke was not re-run (needs isolated herdr; see
  Item 1 step 4). Its status today is a cited claim, not a verified one.
