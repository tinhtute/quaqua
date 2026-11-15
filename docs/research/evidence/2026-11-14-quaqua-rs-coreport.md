Outcome:         COMPLETE
Snapshot:        43a60bd on quaqua/tieude-coreport, worktree ~/Lab/.wt/tieude-coreport (nothing written there; read-only)
Scope:           read: crates/quaqua/src/engine/{runner,workflow,activity,activities,journal,journal_sqlite,clock,testkit,guard,child,telemetry*}.rs, worker/prompt/workflow.rs, pipeline/mod.rs, taskforce/{workflow,driver}.rs, liveness.rs; plan R7/Q24/Q30/Q35/Q42; read-only sqlite on ~/quaqua/runs/quaqua.db. Written: this report only (+ a scratch loc script outside the repo).
Verification:    measured/cited/inferred marks inline. Ran `cargo test --locked -p quaqua test_two_sequential_pause_resolved` → 1 passed (runner.rs test: kill, then resume with externally pushed ApprovalResolved, no duplicate dispatch). Skipped: no new test written (read-only brief), no daemon prototype.
Unknown / risk:  see "Unknown / risk" section below. Line buckets are my judgment of what R7 touches; the numbers are exact, the bucket labels are inferred. DB is live, so counts drift by tens between queries.
Ownership:       nothing held; no files claimed.

# Verdict: PORT the engine core, REWRITE the worker workflow and the pipeline driver

The engine core (journal, runner replay, clock seam, mocks, determinism lint, activities minus 3 polls) ports with named changes. What gets rewritten is not engine/: it is `SingleWorkerWorkflow` (745 prod lines) and `run_pipeline` (2,174 lines in pipeline/mod.rs), plus the delete-list. "Keep (reshape)" for engine/ holds once polls are removed. The kept part is not only the journal store and the clock seam: runner replay and Workflow trait survive too. The plan's "daemon = one workstream" under-counts: the real rewrite is the child supervisor and worker state machine, not the engine.

## Overview
Today's design: the runner is a synchronous, single-shot step loop. A workflow step is pure; every effect is an Activity that the runner executes inline and journals as `activity_scheduled` + `activity_completed`; the process exits when nothing is pending (`Blocked`) and a later process resumes. The worker watchdog uses that machinery as a poll loop. The journal is 98.7% poll bookkeeping (measured, below). Nothing in the runner needs polls; the worker workflow chose them.

## Key facts (measured)
- Poll share. Command: `sqlite3 -readonly ~/quaqua/runs/quaqua.db` with kind/payload filters.
  - total events 126,907; `activity_scheduled|completed` whose id is `wake_*`/`obs_*`: 83,230 rows (13.45 MB of 15.86 MB); plus `continue` 42,028 rows (runner bookkeeping).
  - worker workflows: 120,234 events, non-poll 1,160 → 99.0% poll. Pipeline workflows: 6,673 events, 251 non-(scheduled|completed|continue).
  - 3 worker workflows hit exactly 10,000 steps = 30,000 events (`RunnerOptions.max_steps`, runner.rs:64-81): a poll loop that never ends is bounded only by the step limit.
- An event subscription already existed and was removed: journal rows `HerdrEvents::Subscribe{event_types:["pane.exited"]}` (id `sub_events_0`) are tombstoned; commit 8101e30 (2026-10-21) "Replace pane polling and the event subscriber with one blocking native agent wait per wake". That subscriber was an *activity* (pulled by the step loop), not an event pushed into the workflow. Cited: git show 8101e30; activity.rs:261-264 comment.

## Answers to the five questions

### (1) Does replay require every observation to be a journaled activity? No.
- Replay re-runs `step()` over events from the latest snapshot (runner.rs:562-640 builds `historical_events_to_deliver` from journal kinds; 641-790 replays). Cited. The invariant is: every *input to step()* is a journal row. The workflow trait says step must do no I/O (workflow.rs:1-6) and guard.rs lints for it (cited; `test_all_crate_workflows_pass_determinism_guard`, engine/tests.rs:319).
- Polls are journaled only because they are activities: `dispatch_activity` writes scheduled + completed around `activities.execute` (runner.rs:198-239). An event pushed from outside is also a journaled row today: `resume(..., incoming_events)` (runner.rs:423) appends `approval_resolved`/`timer_fired`/`child_*` at 968-1041 and queues them. Measured: `test_two_sequential_pause_resolved_cycles_kill_between_dispatches_nothing` passes (resume with pushed events after a kill, nothing re-dispatched).
- What does break: replaying an *old* poll journal under a *new* worker `step()` (it would not understand `wake_N`/`obs_N_*` ids; replay also reconciles by per-id counts, runner.rs:641-790). Inferred, and moot: Q35/R17 keeps the old journal read-only and cuts over with no worker running, so no old workflow is ever replayed by new code.

### (2) Can the same Workflow trait be driven by pushed events? Yes; the runner changes in five places.
Trait stays; `WorkflowEvent` (workflow.rs, 5 variants) gets one: `Signal { source, name, payload }` (inferred; the 5 variants carry no external observation). Runner changes:
1. Journal + replay arms for the new kind: history index (runner.rs:562-640) and incoming append (968-1041): ~30 lines, inferred.
2. No wait primitive: the runner returns `Blocked` at quiescence (runner.rs ~1057-1130) and exits. A daemon needs a per-workflow loop `recv(event | next timer) → resume`. That loop is new, small, outside the runner.
3. Timers: `RealClock::advance_to_next_timer` sleeps the calling thread (clock.rs:87-110); `RunnerClock` (runner.rs:19-30, used at 1057-1058) exposes only blocking advance. It cannot share a thread with an event channel. Change: `next_deadline()` + `fire_due(now)`; trait ~50 lines, RealClock 109 lines replaced, VirtualClock/`SystemClock` unchanged.
4. Children: the runner only journals `child_started` (StartChild arms) and never runs the child. Today `run_pipeline` is the child supervisor: it runs `SingleWorkerWorkflow` via `run_until_blocked` (pipeline/mod.rs:1645-1700) and feeds `ChildCompleted` back (2130-2142). The daemon must own this. New code; this is the largest piece.
5. Per-event `resume` re-reads the full history (`load_events(workflow_id, 1)`, runner.rs:466-467) and rescans timers. Fine after the journal shrinks (avg worker workflow today = 283 events, mostly polls); a long-lived in-memory handle is an optimization, not a first-slice need (inferred).
Activities stay inline (runner.rs:213) on a per-workflow thread; only the blocking `AgentWait` was the problem, and it goes.
Testkit: the runner's own tests already drive pushed events with `VirtualClock` + `InMemoryJournal` + a mock registry (runner.rs:33, tests 1287+; passing test above). `run_to_completion` (testkit.rs:309) cannot: it takes no incoming events and writes non-JSON payloads (`format!("id={fired_id}")`, testkit.rs ~340) that the runner cannot replay, so it is a second, divergent mini-runner (289 lines) → replace it with a thin helper over the real runner.

### (3) Journal schema for state-change-only; are old journals readable?
- No schema change. `events(workflow_id, seq, ts_ms, kind TEXT, payload_json TEXT)` and `snapshots` (journal_sqlite.rs:244-259); `kind` is free text and replay ignores unknown kinds (`_ => {}`, runner.rs ~636). A new `signal` kind needs no migration. "Only state changes" is a producer rule: the daemon emits a Signal on a status transition, not per tick. Not a storage feature.
- `continue` rows (42,028, 33% of all rows) are runner bookkeeping counted at replay (`history_continue_count` runner.rs:570/635; `replayed_continue_count` 646/784-785). An event-driven step that returns Continue still writes one. Either keep (cheap) or drop with a replay-reconcile change; recommend keep for slice 1 (inferred).
- Single writer: `append_at_cursor` rebase, `JournalCursor.diverged`, `snapshot_if_contiguous` (runner.rs:274-383, ~110 lines) exist for two writers on one workflow id. With the daemon as sole writer they are dead weight, harmless; delete later (inferred).
- Old journals stay readable: `ActivityRequest/Result` use tolerant deserializers that map unknown tags/shapes to `Unknown` (activity.rs:261-526, added in 8101e30); state snapshots use `#[serde(default)]`/`alias` (worker/prompt/workflow.rs:97-130). Lookup queries (verdict, status) read rows by kind, unaffected. Replay of old workflows under new code: not supported, not needed (Q35).

### (4) Which Workflow impls survive
| impl | prod lines | fate | why / evidence |
|---|---|---|---|
| `SingleWorkerWorkflow` (worker) | 745 (+616 test) | **replaced** | every transition is a `wake_N` → `obs_N_{report,diff,session}` chain (workflow.rs:281-480); ~15 poll fields in state (poll_count, report_mtime_ms, session_*, current_diff_hash…, lines 84-245). Survives as ideas: deadline/grace/auto-extend/nudge arithmetic (`wait_interval`, `nudge_deadline_ms`, `evaluate`, ~185 lines at 560-745), exit-code table, report validation (watchdog.rs). Rewrite as event-driven reducer over `Signal`s + `TimerFired`. |
| `PipelineWorkflow` (impl only) | 870 (+3,526 test) | **ports with change** | 0 refs to AgentWait/DiffHash/CheckMtime/Session in 2175-3044 (measured grep); only schedules `ReportFile::Read` (4 sites). Already reacts to `ChildCompleted`/`PauseForApproval`. Change: child ids + who starts children. |
| `run_pipeline` + helpers | 2,174 | **replaced** | the child supervisor and dual-state driver (Q24); 3 poll refs; becomes daemon dispatch. |
| `TaskforceWorkflow` | 485 (+ driver, report) | **not ported** | R15; it has its own driver (taskforce/driver.rs:607, 751 call `run_until_blocked`). |
So 2 of 3 impls survive in some form, and only one of those (pipeline) survives close to as-is.

### (5) Lines per bucket (script and output)
Command (`~/Lab/.wt/tieude-coreport`, run from `crates/quaqua/src`): `bash $SCRATCH/loc.sh` where each row is `part|file|from|to|bucket|kind` and the count is `sed -n "from,top" file | wc -l`. Scratch copy: /private/tmp/claude-501/-Users-tinhtute-Lab--wt-tieude-coreport/d2f2bcd6-9016-496d-8a76-51ce677ca6d8/scratchpad/loc.sh (output loc.tsv beside it). Totals: `awk -F'|' '$2 ~ /^engine\// && $6=="prod"{t[$5]+=$4;s+=$4} END{...}' loc.tsv`.
```
engine/ prod only:   unchanged 3343 (64%)  changed 755 (15%)  replaced 1106 (21%)   total 5204
  minus telemetry (R15 deletes, 578 lines): unchanged 3343 (72%) changed 755 (16%) replaced 528 (11%)  total 4626
engine/ prod+test:   unchanged 7424 (72%)  changed 1663 (16%) replaced 1266 (12%)  total 10353  (wc -l engine/*.rs = 10390; mod.rs 37 not counted)
workflows prod (worker+pipeline+taskforce): replaced 3404, changed 870, unchanged 0
all rows:            prod unchanged 3972 / changed 1625 / replaced 4510; test unchanged 4081 / changed 4434 / replaced 776
```
Test buckets follow the owner file (heuristic; pipeline's 3,526 test lines are not classified line by line).

## Part table
| part | file | lines | bucket | change | evidence |
|---|---|---|---|---|---|
| journal trait + records | engine/journal.rs | 133 | unchanged | none; `Journal` is object-safe, kind is a string | journal.rs:96-133 |
| sqlite helpers | engine/sqlite.rs | 75 | unchanged | none | busy-retry only |
| Journal impl (events, snapshots) | engine/journal_sqlite.rs:539-734 | 196 | unchanged | none (BEGIN IMMEDIATE, seq conflict) | journal_sqlite.rs:540-645 |
| schema + open | journal_sqlite.rs:235-300 | 66 | changed | new DB path (Q35); `JOURNAL_SCHEMA_SQL` is shared with bus::gui (deleted by Q28) | :244-259, `live_db_open_refused` :275 |
| verdict types + record/load | journal_sqlite.rs:1-234, 301-538 | 472 | unchanged (relocate) | verdict logic is not journal; moves to run-record home (R19) | :130-170, :301 |
| runner: clock trait | runner.rs:1-50 | 50 | changed | blocking `advance_to_next_timer` → `next_deadline`/`fire_due` | :19-30, :1057-1058 |
| runner: outcome/options/errors/data | runner.rs:51-195 | 145 | unchanged | +1 payload struct for Signal | :60-190 |
| runner: dispatch + cursor + snapshot | runner.rs:196-383 | 188 | unchanged | multi-writer rebase becomes optional | :198, :274-383 |
| runner: run/resume entry | runner.rs:384-455 | 72 | changed | daemon loop uses resume per event | :388, :423 |
| runner: history index + replay | runner.rs:456-967 | 512 | unchanged (+1 arm) | Signal arm; per-id count reconcile stays | :456-790 |
| runner: incoming + forward loop | runner.rs:968-1286 | 319 | changed | Signal append; timer block (1057-1100) off blocking clock | :968-1041, :1046+ |
| Workflow trait + ctx | engine/workflow.rs | 177 | changed | +`Signal` variant | workflow.rs ~93-110 |
| activity enums + tolerant serde | engine/activity.rs | 555 | unchanged | poll variants stay as tombstones for old rows | :261-526 |
| activities: non-poll (spawn, prompt, git, cost, grid, report r/w) | engine/activities.rs | 632 | unchanged | none | :182-351, 407-531, 564-591, 635-768 |
| activities: AgentWait, DiffHash, CheckMtime, Session | engine/activities.rs | 130 | replaced | become daemon watchers (herdr event sub, fs watch) | :352-406, 532-563, 592-634 |
| SystemClock | clock.rs:1-41 | 41 | unchanged | none | |
| RealClock (blocking sleep) + tests | clock.rs:42-310 | 269 | replaced | deadline-based timer for event loop | :87-110 |
| testkit mocks (VirtualClock, InMemoryJournal, MockActivities) | testkit.rs:1-274 | 274 | unchanged | drive the real runner already | runner.rs:33 |
| testkit `run_to_completion` | testkit.rs:275-563 | 289 | replaced | no incoming events; non-JSON journal payloads | :309, ~340 |
| determinism guard | engine/guard.rs | 114 | unchanged | keeps step pure | tests.rs:319 |
| child.rs | engine/child.rs | 71 | changed | SingleWorkerInput/Output follow new worker wf | |
| telemetry + telemetry_otel | engine/telemetry*.rs | 578 | replaced | not ported (R15); `JsonlSpanSink` has no callers outside engine | grep SpanSink |
| liveness.rs (outside engine/) | liveness.rs | 629 | unchanged | no engine dependency; daemon feeds it events instead of probes | liveness.rs:7 |

## Unknown / risk
- I did not prototype. The claim "resume-per-event is enough for slice 1" rests on the journal shrinking ~100x (measured poll share) and on one passing test (pause/approval resume); not measured at N concurrent workflows.
- Herdr's push surface is not checked here (Q37 spike owns it). If herdr cannot push `status` transitions, the daemon re-introduces polling inside itself (just not in the journal); R7's acceptance (`daemon_idle_worker_adds_no_poll_events`) is about journal rows, so it would pass either way. Worth an explicit decision.
- 8101e30 shows the team already tried an event subscriber and replaced it with a blocking wait; the daemon must not repeat "subscriber as activity".
- `continue` rows and the multi-writer rebase are kept in the first slice by choice, not necessity.
- Q24 says rewrite pipeline; the data says only the driver (2,174 lines) is a rewrite and the 870-line state machine can port. This is a challenge to the map line, not a decision.

## Smallest first slice that proves it (port, not rewrite)
Add `WorkflowEvent::Signal`, its journal kind and the two runner arms, then a `qq-core` test where a stub worker workflow (a ~40-line reducer: Signal{status}→state, TimerFired→deadline exit) runs on the ported runner with `VirtualClock`, receives 3 pushed Signals through `resume(..., incoming_events)`, is killed and resumed in between, and idles 2 virtual hours with a 7200s timer. Assert: journal rows = signals + timer + completion only (zero `wake_*`/`activity_*` rows), replay after kill yields the same state. That is exactly R7's `daemon_idle_worker_adds_no_poll_events`, needs the Signal variant and the clock `next_deadline` change (≈150 lines, inferred) and nothing from the worker rewrite. If it passes unchanged on runner replay, the port verdict is confirmed; if replay needs more than the named arms, it is a rewrite.

## Playbook
1: done
2: done
3: done

Verified: inconclusive 43a60bd

## Blockers
none

## Lessons
none
