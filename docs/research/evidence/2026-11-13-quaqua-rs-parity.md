Outcome:         COMPLETE
Snapshot:        no writes to any repo; report only. Engine read at 43a60bd (main, worktree ~/Lab/quaqua)
Scope:           read-only inventory of crates/quaqua/tests/*.rs (39 files), #[cfg(test)] modules in
                 crates/quaqua/src (534 inline tests), tests/*.sh (24 suites, tests/run.sh runner)
Verification:    `git log --oneline -3` → 43a60bd Merge pull request #271 …; `rg -c '#\[(tokio::)?test\]'`
                 → 725 integration + 534 inline = 1259 tests; per-file test+line extraction script (output
                 below); harness/model-id classifier script → 163 harness-specific, 44 pin literal model ids;
                 `rg -n '#\[ignore'` → 1 ignored test; `rg -n 'cfg\(target_os'` → 8 platform-gated.
                 Deliberately skipped: `cargo test --locked -- --list` (not needed once the source scan gave
                 exact fn+line for all 1259), tests/run.sh, and the full suite (brief forbids; nothing here
                 depends on a run's result)
Unknown / risk: macOS pass/fail status of individual tests is NOT measured — I ran no tests. Platform risk is
                 inferred from cfg gates and from the one test whose mechanism is fd-level (inbox.rs:2518).
                 Per-row harness/model counts are classifier output (regex over test bodies), accurate to ±2.
Ownership:       released — no scope held

## Findings

### Overview

The engine carries **1259 Rust tests** (725 in `crates/quaqua/tests/`, 534 inline in `crates/quaqua/src/`)
plus **24 shell suites** (`tests/test_*.sh`, 6990 lines, driven by `tests/run.sh`). Coverage is
behaviour-first: the test names read as user-observable outcomes, and the biggest files pin whole
lifecycles (spawn → prompt → stop → cleanup, plan → build → review → fix → accept).

Measured totals (script over every `#[test]`/`#[tokio::test]` and its body):

| | count |
|---|---|
| Rust tests total | 1259 |
| … in `crates/quaqua/tests/` | 725 |
| … inline in `crates/quaqua/src/` | 534 |
| tests whose body names a harness (claude/pi/agy/opencode/omp) | 163 (13%) |
| tests whose body pins a literal model id | 44 (3.5%) |
| `#[ignore]` | 1 |
| `#[cfg(target_os = …)]`-gated | 8 |
| shell suites | 24 |

Literal model ids that appear in test bodies: `opus-5.5` (20), `gemini-3.8-flash` (20),
`claude-3-7-sonnet` (8), `claude-999` (4), `claude-opus-5-5` (3), plus `opencode-go/deepseek-v4.1-flash`
and `claude-opus-5-5`. All of them are **config fixtures** (a `team.toml` string a test feeds the
validator), not assertions about a real provider — measured: `rg -o` over `crates/quaqua/tests/*.rs`.

### Summary table

`harness` / `model` columns are the classifier's per-subsystem counts (regex over test bodies, ±2).
`tests` counts Rust tests only; shell suites are named in each section.

| subsystem | tests | behaviours pinned | harness-specific | pins model ids |
|---|---|---|---|---|
| worker spawn/prompt/stop/cleanup | 314 | 30 | ~65 | ~21 |
| grid / layout | 164 | 20 | ~18 | ~11 |
| bus / mail / inbox | 133 | 21 | ~1 | 0 |
| engine / journal / activities | 89 | 17 | ~11 | ~4 |
| pipeline | 64 | 20 | 0 | 0 |
| config | 60 | 14 | ~11 | ~2 |
| status | 54 | 20 | ~9 | ~1 |
| lead | 47 | 9 | ~12 | ~1 |
| init | 36 | 12 | ~7 | 0 |
| gate | 35 | 11 | ~3 | 0 |
| taskforce / council | 35 | 12 | ~6 | ~3 |
| memory | 34 | 12 | ~5 | 0 |
| doctor | 30 | 8 | ~3 | 0 |
| transcripts | 22 | 8 | 0 | 0 |
| auditor | 17 | 10 | 0 | 0 |
| usage / estimate | 16 | 10 | ~1 | 0 |
| ship / merge | 13 | 8 | ~1 | ~1 |
| source (github-ci) | 9 | 5 | 0 | 0 |
| other (identity, liveness, workspace, cos, live-db guard, state, rules, util, herdr, hash, playbook) | 87 | 22 | ~6 | 0 |
| **total** | **1259** | — | **~163** | **~44** |

### worker spawn / prompt / stop / cleanup — 314 tests

Files: `tests/worker_unit.rs` (108), `tests/prompt_detach.rs` (11), `tests/prompt_playbook.rs` (11),
`tests/prompt_playbook_check.rs` (6), `tests/watchdog.rs` (15), inline `src/worker/prompt/*` (121),
`src/worker/cleanup.rs` (16), `src/worker/spawn.rs` (3), `src/worker/leadgate.rs` (16), `src/launch.rs` (7).
Shell: `test_spawn_worker.sh`, `test_prompt_worker.sh`, `test_prompt_worker_watchdog.sh`,
`test_cleanup_worker.sh`, `test_omp_worker_state.sh`.

Spawn
1. Name must be `<kind>-<task>`, ascii `[a-z0-9-]`, ≤20 chars; a bad name exits 1 before git or herdr is
   called — `worker_unit.rs:29` [cited].
2. An unknown harness is rejected before the pane split; so is a missing or unsupported effort —
   `worker_unit.rs:671`, `:721`, `:770` [cited].
3. Early rejection records `spawn_failed` and writes nothing to the default runs dir —
   `worker_unit.rs:820`, `:840` [cited].
4. Launch args carry the exact model and effort per harness: opencode gets
   `{"model":{"providerID":…,"model":…,"variant":…}}` in `OPENCODE_CONFIG_CONTENT`, not a `--model` flag —
   `worker_unit.rs:1829` [cited].
5. pi launches isolated with exact model; agy carries rules context — `worker_unit.rs:1951`, `:1998` [cited].
6. Review mode is per-harness; claude review settings allow read-only evidence and deny writes —
   `worker_unit.rs:2069`, `:2133` [cited].
7. claude launch hands claude a written settings file, for regular and review workers —
   `worker_unit.rs:2253` [cited].
8. Launch fails closed with no model or no effort — `worker_unit.rs:2401` [cited].
9. Kind → harness fallback resolves; a renamed kind still passes cap/cleanup/cost; `ni-fast` ≠ `ni` —
   `worker_unit.rs:2545`, `:3072`, `:3177` [cited].
10. A herdr start error is reported with the herdr error code and message —
    `worker_unit.rs:2805`, `:2492`, `:2519` [cited].
11. `--approve` policy gates the spawn — `worker_unit.rs:3509` [cited].
12. The default grid tab comes from the owner pane, not a stale env var; a grid timeout fails the spawn —
    `worker_unit.rs:3581`, `:3690`, `:3761` [cited].
13. `--base` defaults to a fetched `origin` default branch, an explicit base ignores origin, a fetch
    failure warns and uses the local default, no origin uses HEAD — `worker_unit.rs:5805`, `:5833`,
    `:5853`, `:5868` [cited].
14. Worker pane env blanks credentials and preserves the state/error files — `src/worker/spawn.rs:980`,
    `:1003` [cited]; a missing omp hook warns — `:1027` [cited].
15. Worktree path/branch naming, base-ref validation and the prepare guards — `worker_unit.rs:4556`,
    `:4577`, `:4652` [cited].

Stop
16. Stop keys are per harness: opencode `esc esc`, omp `esc`, agy `esc`, and **claude/aider are refused**
    (no verified sequence: `esc esc` opens Claude Code's message selector) — `worker_unit.rs:3873` [cited].
17. An unmapped harness and a missing agent both fail closed and send nothing — `worker_unit.rs:3908`,
    `:3928` [cited]; an unreachable herdr is reported as io, not as missing-agent — `:3962` [cited].

Cleanup
18. Cleanup removes per-worker opencode state and cleans budget/grace/cost/metadata —
    `worker_unit.rs:3990`, `:4123` [cited].
19. Cleanup refuses a lead name, refuses a live agent with no worktree, and refuses a foreign repo while
    keeping metadata — `src/worker/cleanup.rs:1046`, `:1015`, `:950` [cited]; a gone worktree drops the
    registration — `:1076` [cited].
20. Cleanup metrics: happy path, all readers failing, second lifecycle excludes the first, a retry
    produces one row — `src/worker/cleanup.rs:755`, `:1118`, `:1163`, `:1255` [cited].

Prompt
21. Exit-code contract 0–8 is pinned case by case: 0 first wake with a ready report, 6 malformed report /
    unaccounted playbook step, 2 hard cap with a moving diff, 3 stall/idle/gone, 4 budget, 5 blocked —
    `src/worker/prompt/workflow.rs:880`–`:1102` [cited].
22. Grace (+25%, once) and auto-extend (bounded at +50%) on a progressing budget; the nudge is sent once —
    `src/worker/prompt/workflow.rs:1204`, `:1227`, `:1239` [cited].
23. The playbook block is injected with each cited leaf once, the pin is written only after the prompt
    lands, and a follow-up overwrites it — `prompt_playbook.rs:186`, `:630`, `:722` [cited].
24. A builder report that does not account for its pinned steps exits 6; a reviewer report ignores the pin —
    `prompt_playbook_check.rs:254`, `:290`, `:374`; `src/worker/prompt/playbook_check.rs:465` [cited].
25. The prompt core survives its coordinator exiting, Ctrl-C leaves the core running, and simultaneous
    resumes start exactly one core — `prompt_detach.rs:359`, `:484`, `:502` [cited].
26. Brief headers are validated (topology required, solo reason ≥10 chars, budget regex incl. non-ascii) —
    `worker_unit.rs:80`, `:96`, `:111`, `:120`; `src/worker/leadgate.rs:1038`, `:1062` [cited].
27. Cost watchdog warns at 80% and 100%, escalates once at budget+buffer, and a `kill` kind requests one
    stop — `src/worker/prompt/cost.rs:1332`–`:1485` [cited].
28. Provider-error classification is per harness (claude/omp/pi/agy/opencode tables, spent-ladder detection,
    error record wins over pane text) — `src/worker/prompt/retry.rs:537`–`:1056` [cited].
29. Verdict recording: reviewer overrides a self-report on the same SHA, a differing SHA is inconclusive,
    an unknown level is inconclusive, two resumes give one verdict per key — `src/worker/prompt/verdict.rs:564`
    –`:1167` [cited].
30. A reviewer that changes its worktree is void; a review with no baseline is accepted —
    `src/worker/prompt/readonly.rs:148`–`:183` [cited].

Shell twins: `test_prompt_worker.sh` pins the exact herdr prompt text, the path-only stdout contract, stale
report archiving, byte-stability, `--check-brief` budget resolution [cited: file header, lines 1–12];
`test_spawn_worker.sh` pins name validation, `--base`, same-name retry, claude `--review` allows/denies, the
claude workspace-trust dialog and the best-effort grid check [cited: header lines 1–12].

### grid / layout — 164 tests

Files: `tests/grid.rs` (56), `tests/layout.rs` (51), `tests/layout_user.rs` (17),
`tests/layout_callsites.rs` (8), `tests/gui_views.rs` (8), inline `src/grid/reconcile.rs` (19),
`src/grid/plan.rs` (3), `src/grid/lock.rs` (2).
Shell: `test_herdr_grid_plan.sh`, `test_herdr_grid_rust.sh`, `test_pipeline_grid.sh`,
`test_panemove_working_agent.sh`.

1. Layout formula: 0 workers → no ops; 1 → split beside the anchor; 2 → land in the grid tab, never the
   orchestrator tab; 5 → overflow to `workers_2`; 8 → two tabs of four; 9 → rejected with an error —
   `grid.rs:72`, `:82`, `:108`, `:140`, `:196`, `:282` [cited].
2. Reconcile converges immediately when the custom tab is already right; it honours deferred statuses —
   `grid.rs:434`, `:1020` [cited].
3. A deferred op propagates to the dependent splits; check converges when a dependent split follows a
   deferred anchor — `grid.rs:536`, `:1314` [cited].
4. Apply re-checks idle and fails if the pane became busy; check never moves a still-working pane —
   `grid.rs:629`; `src/grid/reconcile.rs:1641` [cited].
5. Repair renames with new pane ids and respects `--only`; plan and repair agree on a foreign workers tab —
   `grid.rs:589`, `:1565` [cited].
6. Hook locking: dead-pid reclaim, backoff after failure, early exit, shadow-mode file precedence —
   `grid.rs:730`, `:684`, `:752`, `:822` [cited].
7. Shrinking across chunks does not duplicate the overflow tab; a tab ending in `-2` does not lose chunk-0
   splits — `grid.rs:1732`, `:1889` [cited].
8. The grid never moves the CoS pane — `cos.rs:409` [cited].
9. All builtin layouts load; unknown fields are denied; a schema-v4 layout is refused with a migrate hint —
   `layout.rs:113`, `:134`, `:64` [cited].
10. `layout check` covers all layers and the mail rules; `layout show`/`why` print the decision —
    `layout.rs:195`, `:260`, `:298` [cited].
11. Shadow mode: a denied peer spawn proceeds and records `would_deny` (6 call sites: worker spawn, lead
    spawn, taskforce, worker prompt, lead prompt, cos lead spawn) — `layout.rs:1223`–`:1486` [cited].
12. Enforce mode: a denied peer spawn exits 1 and leaves no worktree or pane; a cos→lead spawn is allowed —
    `layout.rs:1868`, `:1940` [cited].
13. An unknown caller resolves to `identity-unknown`; a missing owner and a cycle have distinct rule ids —
    `layout.rs:360`, `:1696` [cited].
14. A refused spawn leaves no layout-role marker; a post-agent failure retains it —
    `layout_callsites.rs:600`, `:668`, `:748` [cited].
15. A worker spawn rejects a lead name even when the slug's lead role exists — `layout_callsites.rs:819` [cited].
16. Taskforce weight precedence across all tiers; a council counts n slots against the cap —
    `config_layouts.rs:648`, `:203` [cited].
17. User layouts: role-by-name chair spawns a council, depth cycles are denied, upward and unreachable roles
    denied, ambiguous role denied without a marker — `layout_user.rs:79`, `:148`, `:472`, `:529`, `:558` [cited].
18. `layout migrate` carries taskforce members, fails above five, supports dry-run unified diff and `.bak` —
    `layout_user.rs:877`, `:947`; `layout.rs:746`, `:770` [cited].
19. GUI views: columns+version pinned on a fresh DB, idempotent, read-only, never planted in a legacy DB —
    `gui_views.rs:50`, `:307`, `:327`, `:344` [cited].
20. `plan.jq` is **retired but kept as a parity oracle**: `test_herdr_grid_rust.sh` asserts differential
    equality between `quaqua grid plan` and `plan.jq` over every fixture — `tests/test_herdr_grid_plan.sh:1-6`,
    `tests/test_herdr_grid_rust.sh:1-5` [cited].

### bus / mail / inbox — 133 tests

Files: `tests/inbox.rs` (36), `tests/bus_migrate.rs` (24), `tests/mail.rs` (19), `tests/recall.rs` (17),
`tests/events_hook.rs` (14), `tests/subscription.rs` (9), `tests/farm_attribution.rs` (7),
inline `src/bus/mod.rs` (5), `src/bus/mail.rs` (2). Shell: `test_hermetic_db.sh`, `test_live_db_guard.sh`.

1. A retried send gives one row (client-id idempotence); an over-long body errors — `mail.rs:53`, `:166` [cited].
2. Inbox cursor reads are idempotent; the digest mixes mail and incidents and never redelivers —
   `inbox.rs:57`, `:165`, `:230` [cited].
3. The noise filter drops the consumer's own pane's blocked incidents; foreign blocked panes are dropped and
   real incidents kept — `inbox.rs:301`, `:803` [cited].
4. The high-water-mark cursor advances and deduplicates; interleaved mail+incidents never redeliver —
   `inbox.rs:408`, `:930` [cited].
5. The CoS inbox hook blocks once and skips when a stop hook is active; it exits 0 on a quaqua exit 2 —
   `inbox.rs:657`, `:759` [cited].
6. Foreign-farm rows never reach CoS or a lead — `inbox.rs:870` [cited].
7. Trigger digest: golden format, thread grouping newest-first, same-kind collapsing, overflow truncation —
   `inbox.rs:1053`, `:1112`, `:1200`, `:1298` [cited].
8. A peer cannot advance a lead's or CoS's cursor — `inbox.rs:1957` [cited].
9. A closed stdout (EBADF) must not advance the cursor — `inbox.rs:2518` [cited]; follow exits on EPIPE and
   does not reprint in non-advancing mode — `inbox.rs:2640`, `:2580` [cited].
10. Mail send is fail-closed on identity (no `from`, unresolvable) and refuses a pane from another
    workspace, another farm, or a dead caller; an untagged lead pane self-heals — `mail.rs:264`, `:288`,
    `:765`, `:805`, `:843`, `:665` [cited].
11. Subscriptions: 14-day open, lead dual subscriptions, a trigger extends monotonically, a duplicate does
    not, delivery skips expired ones — `subscription.rs:325`–`:602` [cited].
12. Worker/lead spawn+cleanup drive the thread lifecycle — `subscription.rs:645`, `:742` [cited].
13. Bus migrate: fresh DB creates the new tables, a legacy DB is refused without dropping, a live worker
    blocks it, dry-run leaves the DB untouched, a second run is a no-op — `bus_migrate.rs:194`, `:259`,
    `:331`, `:770`, `:736` [cited].
14. Migration preserves per-mail ack state, `msg_key`, project ref, and the cursor/ack id source —
    `bus_migrate.rs:1371`, `:1505`, `:1617` [cited].
15. Events hook: exactly one notification per incident key, blocked unique per episode, a hard timeout on a
    hanging notification, foreign panes dropped — `events_hook.rs:24`, `:182`, `:321`, `:378` [cited].
16. An idle agent with unread triggers is nudged once per batch; a busy agent and CoS are never nudged —
    `events_hook.rs:891`, `:967`, `:1000` [cited].
17. Recall prints the trigger thread, owner and five most recent triggers; a ref prints path + first 40 lines;
    absolute/parent/symlink escapes are not read; the DB is opened read-only — `recall.rs:67`, `:116`, `:160`,
    `:218`, `:517` [cited].
18. Under `QUAQUA_TEST=1` no default path may resolve to the live workspace DB (`FarmBus`, journal, cost
    query, opencode spend, raw) — `live_db_guard.rs:32`–`:193` [cited].
19. Farm attribution: a local exited pane is recorded, other herdr errors fail open, a worker cwd subdir is
    local, legacy null-owner incidents stay visible to CoS — `farm_attribution.rs:57`, `:74`, `:94`, `:116` [cited].

### engine / journal / activities — 89 tests

Files: inline `src/engine/activities.rs` (22), `src/engine/runner.rs` (21), `src/engine/tests.rs` (14),
`src/engine/journal_sqlite.rs` (12), `src/engine/clock.rs` (9), `src/engine/telemetry_otel.rs` (9),
`src/engine/activity.rs` (2).

1. A toy workflow runs deterministically twice; every crate workflow passes the determinism guard —
   `src/engine/tests.rs:92`, `:319` [cited].
2. Kill+resume preserves state and event sequence; resume after completion returns completed, not blocked —
   `src/engine/runner.rs:1504`, `:2485` [cited].
3. A mid-step kill on a rearmed timer resumes cleanly; a reused clock on resume does not duplicate a timer —
   `src/engine/runner.rs:1841`, `:2177` [cited].
4. Two lost side effects in one replay are dispatched in order; a rebase never moves the cursor backwards —
   `src/engine/runner.rs:2808`, `:3609` [cited].
5. A corrupt payload returns a serialization error; a workflow failure is recorded in the journal —
   `src/engine/runner.rs:2536`, `:2570` [cited].
6. Journal sequence stays monotonic under concurrent writers; open waits out a concurrent writer instead of
   failing locked; concurrent opens never fail — `src/engine/journal_sqlite.rs:979`, `:858`, `:906` [cited].
7. Crash recovery reopens and replays; the journal creates parent directories; the default path is pinned —
   `src/engine/journal_sqlite.rs:1040`, `:1148`, `:1167` [cited].
8. A virtual clock runs a 7200 s timer in under 50 ms; overlapping timers are all delivered; deadlock is
   detected when no events are pending — `src/engine/runner.rs:1384`; `src/engine/tests.rs:508`, `:557` [cited].
9. Activities via a herdr stub: prompt, send-keys, agent-wait, stop; a missing pane or unknown agent fails;
   `HERDR_BIN_PATH` resolution — `src/engine/activities.rs:978`–`:1189` [cited].
10. Report-file ops, git ops, git diff hash and the cost/budget activity — `src/engine/activities.rs:1189`,
    `:1268`, `:1425`, `:1511` [cited].
11. Every activity arm is live with no deferrals (meta-test) — `src/engine/activities.rs:1903` [cited].
12. Telemetry: golden span serialization, a minimal span omits `None`, atomic single-buffer newline, sink
    error on an unwritable path — `src/engine/telemetry_otel.rs:222`, `:251`, `:426`, `:366` [cited].

### pipeline — 64 tests

Files: inline `src/pipeline/mod.rs` (39), `tests/pipeline_prompt.rs` (12), `tests/pipeline_cancel.rs` (8),
`src/pipeline/gate.rs` (3), `src/pipeline/state.rs` (2). Shell: `test_pipeline.sh`, `test_pipeline_grid.sh`.

1. A clean run completes; the plan gate pauses for approval and proceeds on resume — `src/pipeline/mod.rs:3550`,
   `:3055` [cited].
2. A review verdict with findings drives a fix and a fresh review; rounds exhausted fails with exit 5 blocked —
   `src/pipeline/mod.rs:3147`, `:3626` [cited].
3. A severity below `Must fix` completes clean; a review with no verdict fails closed —
   `src/pipeline/mod.rs:3690`, `:3720` [cited].
4. A `CHALLENGE` in a build report stops as DISPUTED; a plan report with blockers fails as BLOCKED; a build
   report with `NO_CHANGES` skips review — `src/pipeline/mod.rs:3853`, `:3902`, `:3949` [cited].
5. A mid-pipeline resume does not replay completed phases — `src/pipeline/mod.rs:3338` [cited].
6. A review that changes its worktree is void; the review prompt carries the gated base SHA and is never
   empty — `src/pipeline/mod.rs:4822`, `:4898` [cited].
7. The driver lock recovers from a panicking holder — `src/pipeline/mod.rs:4357` [cited].
8. Cancel writes terminal events to the parent and non-terminal children, is idempotent, and refuses while a
   child worker pane is alive — `pipeline_cancel.rs:132`, `:208`, `:257` [cited].
9. A cancelled pipeline shows as a terminal phase in `status`; resuming it is refused —
   `pipeline_cancel.rs:304`, `:490` [cited].
10. Resume resends a prompt that never went out; an in-process prompt check keeps the state resumable; a
    watchdog exit keeps a dispatched prompt marked sent — `pipeline_prompt.rs:372`, `:422`, `:457` [cited].
11. A per-phase budget reset grants a fresh allowance; `--rounds` on resume raises the cap —
    `pipeline_prompt.rs:680`, `:584` [cited].
12. The builder prompt injects and pins the brief playbook; a gap is warned at acceptance; the plan prompt
    carries no worker steps and no pin — `pipeline_prompt.rs:724`, `:764`, `:813` [cited].
13. The grid hook uses the lead's default tab, an explicit `--grid-tab` wins, CoS uses the repo default —
    `pipeline_prompt.rs:491`, `:529`, `:557` [cited].
14. Build gate table and wiring: `test_decide_build_gate_table`, `test_build_gate_wire_and_pfile_line` —
    `src/pipeline/gate.rs:216`, `:400` [cited].

### gate — 35 tests

Files: `tests/gate.rs` (27), inline `src/gate/drift.rs` (5), `src/gate/*` (3). Shell: `test_gate_shim.sh`,
`test_herdr_gate.sh`.

1. Direct herdr calls are blocked, including equivalent spellings — `gate.rs:53`, `:71` [cited].
2. Scripts and read-only herdr calls are allowed; argument text, heredocs and help positions are allowed;
   help-shaped words outside a help position are blocked — `gate.rs:101`, `:124`, `:150`, `:182` [cited].
3. Outward pushes and `gh` writes ask; force-push and main are denied; read-only git/gh stays allowed —
   `gate.rs:223`, `:275`, `:319` [cited].
4. `gh pr merge` asks when the PR is WIP/cancelled or has unchecked items; a lookup failure asks, never
   allows; merging to main denies without a lookup — `gate.rs:399`, `:422`, `:434` [cited].
5. `pkill -f` and `pkill --full` are denied; `pkill` without `-f` and `pgrep` stay allowed — `gate.rs:450`,
   `:474` [cited].
6. Removed worker scripts are denied with the replacement hint — `gate.rs:488` [cited].
7. The binary emits PreToolUse JSON and follows the hook JSON contract; it warns on rule drift after session
   start — `gate.rs:609`, `:671`, `:711` [cited].
8. Session start tags the CoS pane, skips worker/lead panes, skips outside the workspace and inside a
   worktree, and exits 0 on a herdr error — `gate.rs:1000`, `:1071`, `:1136`, `:1194` [cited].
9. Drift: startup records and warns once, resume keeps the old baseline, compact rebaselines —
   `src/gate/drift.rs:169`, `:185`, `:194` [cited].
10. The shim fails **closed** (exit 2) when quaqua is missing, honours `$QUAQUA_BIN`, and
    `.claude/settings.json` routes through the shim — `tests/test_gate_shim.sh:1-7` [cited].

### lead — 47 tests

Files: `tests/lead_repair.rs` (28), `tests/lead_prompt.rs` (8), inline `src/lead/card.rs` (4),
`src/lead/prompt.rs` (4), `src/lead/spawn.rs` (3). Shell: `test_spawn_lead.sh`, `test_cleanup_lead.sh`.

1. A landed prompt clears the input then submits with a wait; a draft left in the box is non-zero; a rejected
   submit names the reason; a missing lead is non-zero with the spawn hint — `lead_prompt.rs:174`, `:216`,
   `:227`, `:238` [cited].
2. Prompt and spawn refuse when the card has a stale open item unless allowed — `lead_prompt.rs:262`, `:296`
   [cited].
3. Repair-lead stamps tokens, restores a missing owner file, refuses to own itself, rejects an unregistered
   slug and a CoS-owned project — `lead_repair.rs:364`, `:405`, `:499`, `:528`, `:549` [cited].
4. Identity heal: a CoS token is stamped only for a matching session+pane, fails closed on a mismatch, is
   idempotent, and dry-run previews without mutating — `lead_repair.rs:758`, `:822`, `:1219`, `:1503` [cited].
5. A foreign-farm CoS pane is never stamped — `lead_repair.rs:1763`, `:1796` [cited].
6. Lead spawn writes the caller owner, pins the playbook from the task, and delivers the kickoff when the
   lead reports done — `src/lead/spawn.rs:894`, `:945`, `:1145` [cited].
7. Card parsing: the NOW section, PR/SHA extraction, stale-item detection matching only the exact PR number —
   `src/lead/card.rs:272`, `:292`, `:300`, `:421` [cited].
8. The lead peer cap holds while CoS keeps the global cap; `lead_max_peers` overrides the default —
   `worker_unit.rs:1684`, `:1736` [cited].
9. The lead gate blocks a third worker on a multi-phase CoS project and records a solo override line in
   today's memory — `worker_unit.rs:5257`, `:5366` [cited].

### taskforce / council — 35 tests

Files: `tests/council.rs` (19), inline `src/taskforce/mod.rs` (12), `src/taskforce/report.rs` (4).

1. Quorum at 2 of 3 completes; 1 of 3 fails cleanly — `council.rs:167`, `:246` [cited].
2. Fail-fast kills live members, with the boundary tested one earlier and one later — `council.rs:308`,
   `:828`, `:1168` [cited].
3. Resume from a partial journal; a terminal replay spawns nothing — `council.rs:371`, `:466` [cited].
4. A merge failure is resumable; the error path cleans up spawned members; spawn rolls back on failure —
   `council.rs:543`, `:632`, `:882` [cited].
5. No majority yields dissent; a verdictless report fails with code 6; a member blocked (exit 5) maps to
   failed — `council.rs:685`, `:746`, `:790` [cited].
6. Taskforce markers and state are written; dual-lens behaviour is unchanged — `council.rs:928`, `:1003` [cited].
7. A taskforce member cannot review (rejected at spawn) — `council.rs:1056` [cited].
8. Council membership comes from `team.toml` `taskforce.council`, with a sane fallback when absent —
   `council.rs:1352`, `:1448` [cited].
9. Dual lens: quorum completes with both report paths; one failure drives the step with `child_failed` —
   `src/taskforce/mod.rs:388`, `:478` [cited].
10. Member verdict parsing (clean/findings/challenge) and council report majority+dissent rendering —
    `src/taskforce/report.rs:110`–`:136` [cited].

### config — 60 tests

Files: `tests/config.rs` (17), `tests/config_layouts.rs` (14), inline `src/config.rs` (29).

1. A repo `team.toml` validates; every kind carries a model and an effort — `config.rs:40`, `:47` [cited].
2. The rule-key table covers every source rule — `config.rs:82` [cited].
3. `get default-reviewer` is a one-per-line list; `get cap` applies a project override; the lead peer cap
   matches the SLP — `config.rs:163`, `:176`, `:204` [cited].
4. Unknown key or project fails clearly; invalid fixtures are rejected — `config.rs:284`, `:295`, `:316` [cited].
5. The opencode models table merges over defaults — `config.rs:412` [cited].
6. `on_cost_cap = "kill"` is refused at spawn unless the kind has a stop key and a fallback — `config.rs:458`,
   `:493` [cited].
7. Builder kinds default to every configured kind; `team.toml` wins and must name kinds —
   `src/config.rs:1167`, `:1174` [cited].
8. pi kinds validate against the pi thinking set, agy against the agy effort set, claude against its effort
   vocabulary — `src/config.rs:1201`, `:1215`, `:1449` [cited].
9. A cap over the limit is rejected; `lead_max_peers` must fit the global cap — `src/config.rs:1231`, `:1238`
   [cited].
10. Defaults: `spawn_policy = auto`, `on_cost_cap = warn`, `cost_buffer = 20%` (bounded), `mail_ack = 30m` —
    `src/config.rs:1339`, `:1348`, `:1373`, `:1387` [cited].
11. A fallback to a missing kind, to itself, or from a review kind to a non-review kind is rejected —
    `src/config.rs:1457`, `:1465`, `:1473` [cited].
12. A project layout override resolves; validation rejects a bad layout and an unknown project layout —
    `config_layouts.rs:155`, `:106`, `:373` [cited].

### init — 36 tests

Files: `tests/init.rs` (30), inline `src/init.rs` (1), `tests/compact_hook.rs` (5). Shell:
`test_init_workspace.sh`, `test_memory_compact.sh`.

1. Init writes missing files with modes and a lock, never clobbers an existing file, and defaults to the
   workspace root — `init.rs:153`, `:202`, `:227` [cited].
2. Init fails closed on an empty template dir — `init.rs:253` [cited].
3. `update` writes a missing managed file, records an already-current one, overwrites an untouched one, keeps
   a locally changed one and pends the new one, treats a pre-existing divergent file as a conflict, lists a
   file removed upstream, and never touches a seed — `init.rs:275`–`:448` [cited].
4. Dry-run writes nothing — `init.rs:466` [cited].
5. Init wires the embedded hook and migrates a legacy one; it resolves herdr through `HERDR_BIN_PATH` only —
   `init.rs:545`, `:642` [cited].
6. Git hooks: wires an existing repo only, refuses a foreign `hooksPath`, rejects a non-git dir —
   `init.rs:718`, `:818`, `:878` [cited].
7. The worker push guard installs pre-push only, refreshes an older revision, and rejects workers only —
   `init.rs:929`, `:1059`, `:1142` [cited].
8. A fresh init has the AGENTS coordinator lines and playbooks, no fixture, and the poteto skill —
   `init.rs:1191`, `:1276`, `:1326` [cited].
9. The pre-commit hook refuses added machine paths — `init.rs:1566` [cited].
10. The compact hook installs beside the herdr gate, is idempotent, and a dry-run writes nothing —
    `compact_hook.rs:113`, `:153`, `:186` [cited].
11. Shell: every embedded file lands byte-identical, engine-only paths never leak into a workspace, and a git
    target gets `core.hooksPath` wired to the embedded pre-commit — `tests/test_init_workspace.sh:1-14` [cited].

### doctor — 30 tests

Files: `tests/doctor.rs` (16), inline `src/doctor.rs` (14). Shell: `test_install_auditor.sh`,
`test_install_github_ci_poll.sh`.

1. A missing binary, a shell-function shadow, and a stale binary each exit non-zero — `doctor.rs:204`, `:215`,
   `:225` [cited].
2. A healthy setup exits zero; a missing checkout says the stale check was skipped — `doctor.rs:241`, `:251`
   [cited].
3. A workspace that is also an engine checkout warns and still exits 0; a split workspace is quiet —
   `doctor.rs:272`, `:292` [cited].
4. Auditor unit missing / inactive timer fails the run; a github-ci timer that is active is OK, inactive or
   dangling fails, never-installed is silent — `doctor.rs:311`, `:328`, `:346`, `:358`, `:377`, `:402` [cited].
5. `QUAQUA_TEST` does not disable the auditor check; on macOS doctor notes manual activation instead of
   failing — `doctor.rs:423`, `:442` [cited].
6. Workspace drift reports problems and a clean workspace passes — `doctor.rs:457` [cited].
7. A pi kind is warned about only when `pi` does not list its model — `src/doctor.rs:2018`, `:2064` [cited].
8. CLI: version and help are available; `worker prompt --help` lists the exit codes — `cli.rs:11`, `:258` [cited].

### status — 54 tests

Files: inline `src/status.rs` (46), `tests/gui_views.rs` (8).

1. Default hides terminal workflows and unresolved verdicts; `--all` shows everything; the hidden line
   appears only in sections that hid rows — `src/status.rs:2386`, `:2422`, `:2440` [cited].
2. An empty flag cell prints `OK`; an unknown arg exits 2 and `--help` exits 0; a missing or empty DB yields
   no workflows cleanly — `src/status.rs:2454`, `:2484`, `:2496`, `:2515` [cited].
3. `waiter-lost` is raised when the orchestrator is idle while a worker works, or when a report exists unread;
   a negative report-read with an idle owner does not raise it — `src/status.rs:2639`, `:2668`, `:2717` [cited].
4. A read marker overrides atime; a stale or unreadable read marker is treated as unread — `src/status.rs:2759`,
   `:2807`, `:2854` [cited].
5. A corrupted DB surfaces a DB error; a drifted schema surfaces a DB error; a herdr list error or invalid
   JSON surfaces a herdr error — `src/status.rs:2932`, `:2959`, `:2995`, `:3031` [cited].
6. Rows are flagged `done-not-cleaned`, `idle-unprompted`, `orphan` — `src/status.rs:3052` [cited].
7. An estimate appears once a group has the minimum samples and never for an idle/done worker —
   `src/status.rs:3147`, `:3242`, `:3310` [cited].
8. A verdict goes stale on a new commit; the strongest verdict wins per branch+sha; a self-report at HEAD is
   inconclusive — `src/status.rs:3571`, `:3775`, `:3872` [cited].
9. `--metrics` renders text and JSON round-trips; exit-6 resumes are counted; a cap truncation is noticed in
   both formats — `src/status.rs:4208`, `:4580`, `:5052` [cited].

### usage / estimate — 16 tests

1. Text report, JSON report, `--days` window, and a meter-unavailable case that still keeps the report —
   `usage.rs:107`, `:184`, `:250`, `:286` [cited].
2. Missing logs exit non-zero but still render; retry semantics; the token tripwire and its matching —
   `usage.rs:343`, `:380`, `:397` [cited].
3. `usage` and the watchdog share one spend source; a task cost spans the whole session across days and
   windows — `usage.rs:416`, `:500` [cited].
4. Estimate: fewer than the minimum samples returns none, five samples give a median and time-left, an even
   count, elapsed past the median floors at zero — `src/estimate.rs:204`–`:252` [cited].

### auditor — 17 tests + 1 shell suite

1. `auditor run` pins the report naming, the `claude -p` invocation (read-only tool whitelist, repo-root cwd,
   report on stdout, default model), the prompt contents, `AUDITOR_MODEL`/`AUDITOR_OUT` overrides, claude
   failure handling, and that nothing but the report lands in the repo tree — `tests/test_auditor.sh:1-7`
   [cited].
2. The prompt template carries all keywords and a 7-day lessons window — `src/auditor.rs:1416`, `:1429` [cited].
3. Rule expiry: expired / not-expired / malformed; unpromoted lessons at 6 vs 8 days, stopping at the verdict
   and treating a case-insensitive `none` — `src/auditor.rs:1462`, `:1525`, `:1616` [cited].
4. The casebook flags recurring failures at 1× vs 2× and cites them — `src/auditor.rs:1692` [cited].
5. Collecting `rules.md` files avoids a symlink loop — `src/auditor.rs:1755` [cited].
6. The lead inbox scan ignores another farm and another recipient — `src/auditor.rs:1771` [cited].
7. Old pipeline runs are counted not listed while fresh ones are flagged; old runs aggregate and name at most
   five — `src/auditor.rs:2067`, `:2232` [cited].
8. A recorded repo that is not a git repo is one line per repo — `src/auditor.rs:2150` [cited].

### memory — 34 tests

1. `memory check`: a clean workspace exits 0 silently, missing files are not violations, a `rules.md` over the
   cap names the file size and cap, exactly at the cap is clean, `USER.md` has its own cap, the cap counts
   chars not bytes — `memory_check.rs:62`, `:73`, `:80`, `:93`, `:103`, `:117` [cited].
2. A secret in either file is a violation; no env var raises a cap; a missing workspace dir is an error, not
   a pass — `memory_check.rs:128`, `:148`, `:168` [cited].
3. Hooks: a write that breaks the cap is denied with size and cap, a write at the cap is allowed, a secret is
   denied, an edit is denied only when the result breaks the cap, a multiedit is simulated as a whole —
   `memory_hooks.rs:134`, `:153`, `:169`, `:188`, `:218` [cited].
4. Writes outside the capped files are ignored; unparseable hook input fails open; a bash write that left the
   cap broken is flagged — `memory_hooks.rs:238`, `:269`, `:300` [cited].
5. Init installs both memory hooks idempotently and keeps hooks a workspace already declares —
   `memory_hooks.rs:381`, `:430` [cited].
6. Line truncation and secret shapes — `src/memory.rs:509`–`:562` [cited].

### transcripts — 22 tests

1. `transcripts index` builds an incremental index and reports rows, never writes the bus DB, and uses the
   default DB under the runs dir — `transcripts.rs:202`, `:256`, `:269` [cited].
2. Search prints one anchored line per hit, ranks the strongest match first, filters by project and since,
   exits 0 with "no matches", builds a missing index once, and rejects an empty query —
   `transcripts.rs:290`, `:334`, `:368`, `:419`, `:428`, `:501` [cited].
3. `tail` prints the last n user messages; an unknown session exits 1 with empty stdout —
   `transcripts.rs:452`, `:480` [cited].
4. Parsing: a string content is a user message, tool results/thinking/tool-use are not text, a user entry with
   a tool result is dropped whole, bookkeeping and bad lines are skipped, harness wrappers are not speech,
   sidechain entries are flagged not dropped — `src/transcripts/parse.rs:223`–`:316` [cited].

### ship / merge — 13 tests

1. Ship refuses without approval, on a non-COMPLETE report, and with zero commits ahead; it pushes then opens
   a PR and never merges; a rerun reuses the open PR — `ship.rs:76`, `:84`, `:101`, `:108`, `:126` [cited].
2. After cleanup, ship resolves the repo without `--repo`, preferring the spawn record then the newest
   telemetry row — `ship.rs:150`, `:187` [cited].
3. Merge queue: one branch at a time, undo on a failing check, halt on an integ-base failure, conflict
   rework, and the invariant that it never pushes — `tests/test_merge_queue.sh:1-8` [cited].
4. Merge warns on a HEAD with no verdict and leaves the exit code unchanged — `src/merge/mod.rs:580` [cited].

### source (github-ci) — 9 tests

1. A failed run on a known branch emits one trigger; success, unknown and foreign branches are ignored —
   `github_ci.rs:240`, `:321` [cited].
2. A repeat poll records each run once; a failed `gh` leaves the position unchanged; a missing `gh` exits 1
   with a clear error — `github_ci.rs:364`, `:399`, `:436` [cited].
3. A new trigger wakes each live subscriber once; a duplicate poll wakes nobody; CoS and a busy subscriber are
   never nudged; an expired subscription is skipped — `github_ci.rs:482`, `:545`, `:643`, `:697` [cited].

### other — 87 tests

- identity/liveness/workspace: token resolution cases, depth-walk cycle detection, caller env fallback —
  `src/identity.rs:833`, `:868`, `:1202` [cited]; alive/down/unknown liveness with a screen fallback and a
  stale hook state — `src/liveness.rs:389`, `:395`, `:473`, `:549` [cited]; `QUAQUA_HOME` wins over `HOME`,
  default `~/quaqua`, `checked_root` requires a projects index — `src/workspace.rs:305`, `:313`, `:326` [cited].
- cos: the token helper, the sidebar chip format, `cos tag` with explicit/env/current pane, an unresolvable
  pane fails, grid-apply and gate refuse to move/close the CoS pane, cleanup refuses to close it —
  `cos.rs:80`, `:104`, `:227`, `:309`, `:358`, `:409`, `:466`, `:740` [cited].
- playbook: `list` from fixture frontmatter, `show` prints only the orchestrator section, step ids parsed, an
  unknown principle fails, a fourth cited leaf fails — `playbook.rs:75`, `:140`, `:216`, `:263`, `:302` [cited];
  all 18 playbooks and 24 leaves load — `playbook.rs:397` [cited].
- util/hash/herdr: sha256 known vectors, ISO timestamp round-trip, `herdr agent wait` arg construction —
  `src/bus/mail.rs:1185`, `:1197`, `src/herdr.rs:546` [cited].

### Behaviours that are harness-specific (163 tests, ~13%)

The harness-specific surface is concentrated in five places:

| area | what is per-harness | evidence |
|---|---|---|
| launch argv/env | opencode `OPENCODE_CONFIG_CONTENT` JSON, pi isolation flags, agy rules context, claude settings file, omp state envs | `worker_unit.rs:1829`, `:1951`, `:1998`, `:2253`; `src/launch.rs:560`, `:674`, `:712` [cited] |
| stop keys | opencode `esc esc`, omp `esc`, agy `esc`, **claude refused** | `worker_unit.rs:3873` [cited] |
| provider-error retry | one classifier table per harness + a spent-ladder rule | `src/worker/prompt/retry.rs:537`–`:1056` [cited] |
| cost/spend reading | claude session JSONL, omp session dir, opencode sqlite, pi session dir | `src/worker/prompt/cost.rs:1645`, `:1663`, `:1709`, `:1772`, `:1925` [cited] |
| config validation | pi thinking set, agy effort set, claude effort vocabulary, opencode model variant | `src/config.rs:1201`, `:1215`, `:1449`, `:1481` [cited] |

Plus: `sude` → `omp` kind mapping (`worker_unit.rs:1781` [cited]), the omp worker-state extension
(`tests/test_omp_worker_state.sh`, asserts `idle <n>` / `working <n>` / `blocked <n> permission` [cited:
lines 65-73]), the omp hook-missing warning (`src/worker/spawn.rs:1027` [cited]) and the omp/claude/opencode
launch lines in `layout_callsites.rs` (7 model-id pins) [cited].

### Tests that pin implementation detail rather than behaviour (flag for the rewrite)

These assert an internal contract a rewrite could legitimately change; they should not gate parity:

- fake-driver / mock contracts: `grid.rs:480`, `:1934`, `:1997` (fake herdr driver, unified driver
  instantiation), `worker_unit.rs:553` (mock activities herdr contract), `grid.rs:575` (fake clock helper).
- lock internals: `src/grid/lock.rs:169`, `:186` (reentrancy, outermost guard release).
- serde/trait plumbing: `src/engine/activity.rs:532`, `:539` (Value round-trips), `src/engine/tests.rs:165`
  and `src/engine/journal_sqlite.rs:1123` (trait object safety).
- hashing/format primitives: `src/bus/mail.rs:1185`, `:1197`.
- internal state helpers: `src/worker/prompt/cost.rs:1518` (report-state mtime stability),
  `src/pipeline/mod.rs:4107`, `:4168` (child classification, mismatched child id), `src/state.rs:468`, `:484`.
- meta/repo-hygiene tests (not behaviour at all): `worker_unit.rs:5054` (no dangling legacy role reference),
  `playbook.rs:546` (forbidden-terms grep), `src/engine/activities.rs:1903` (every activity arm live),
  `:1931` (suite completes with 16 threads), `src/engine/tests.rs:319` (determinism guard over all
  workflows), `tests/test_docs.sh` (doc/bootstrap consistency).
- `src/engine/tests.rs:393`, `:433` (owners table/files) — the second is the only `#[ignore]`d test.

### Ignored, platform-gated and self-skipping (the brief's unknown)

**Ignored (1)** — `src/engine/tests.rs:432`, `#[ignore = "enforced in W2 after all files are created: cargo
test --locked -p quaqua -- --ignored test_w1_owners_files_exist"]` [cited]. It never runs in the default gate.

**Platform-gated (8)** — `doctor.rs:309`, `:326`, `:344`, `:356`, `:375`, `:400`, `:421` are
`#[cfg(target_os = "linux")]` (auditor unit + launchd/systemd timer cases) and `doctor.rs:440` is
`#[cfg(target_os = "macos")]` (manual-activation note). On macOS the seven Linux cases do not exist, so the
timer behaviour is **not** covered there beyond the macOS note — this matches the cross-platform rule (each
OS has its own case) [cited]. `src/bus/events.rs:88` also carries a Linux-only branch [cited].

**macOS failure risk (inferred, not measured)** — I ran no tests, so I cannot confirm any failure. The one
test whose mechanism is fd-level is `inbox.rs:2518` `test_closed_stdout_ebadf_does_not_advance_cursor`: it
runs `sh -c "<quaqua> inbox --for cos 1>&-"` and asserts a non-zero exit plus an unmoved cursor [cited at
`:2530-2570`]. It is plausible on both platforms via the EBADF path, but macOS ships bash 3.2 as `/bin/sh`,
so the `1>&-` spelling is the thing to re-check first. Treat this as the first test to run on a macOS host.

**Self-skipping shell suites (3)** — `test_herdr_grid_rust.sh` skips without `jq` (`:58`),
`test_panemove_working_agent.sh` skips without a herdr binary (`:32`) or `jq` (`:36`), and
`test_hermetic_db.sh` is skipped locally unless `CI=1` or `HERMETIC_REQUIRE_STRACE=1` (`run.sh:80`) [cited].

**Slow suites skipped by default (2)** — `test_prompt_worker.sh` and `test_pipeline.sh` run only when a file
they cover changed, or with `--slow` (`run.sh:33-46`, `:88-96`) [cited].

### Gotchas the rewrite must not lose

1. **`claude` stop is a deliberate refusal.** `stop_keys("claude") == None` because `esc esc` opens Claude
   Code's message selector (`worker_unit.rs:3873-3879`, with the Lead decision dated 2026-10-17 in the
   comment) [cited]. A rewrite that "fixes" this breaks a known-bad interaction.
2. **`plan.jq` is retired but is still the parity oracle.** `tests/test_herdr_grid_plan.sh:1-6` says the live
   planner is `quaqua grid plan` and `plan.jq` is kept only as an oracle; `test_herdr_grid_rust.sh` asserts
   differential equality against it [cited]. Dropping the file drops the only cross-check of the layout
   formula.
3. **`scripts/` no longer exists** — `ls scripts` → `No such file or directory` [measured]; managed scripts
   moved to `templates/workspace/scripts/` (commit 462855e `refactor(templates): move managed workspace files
   into templates/workspace`) [measured]. `tests/test_spawn_worker.sh:17` still computes
   `src="$here/../scripts"` but never uses `$src` (`rg '\$src'` → no matches) [measured]: dead code, not a
   failure. `tests/test_docs.sh:54-86` still resolves `scripts/*.sh` references [cited] — re-check it before
   deleting anything else under that name.
4. **The shell suites double-cover the Rust tests** for spawn, prompt, pipeline and grid; the shell-only
   subjects with no Rust twin are the plugin/hook/installer surfaces: `test_omp_worker_state.sh`,
   `test_memory_compact.sh`, `test_install_auditor.sh`, `test_install_github_ci_poll.sh`, `test_herdr_gate.sh`,
   `test_gate_shim.sh`, `test_docs.sh`, `test_merge_queue.sh`, `test_panemove_working_agent.sh` [inferred from
   the file list and headers].
5. **44 model-id pins are fixtures, not behaviour.** `opus-5.5` and `gemini-3.8-flash` appear 20× each, but
   as `team.toml` strings fed to the validator or to `resolve_launch` [measured: `rg -o`; cited:
   `worker_unit.rs:1206`, `:1215`]. They will rename; the behaviour to keep is "the exact configured model
   reaches the launch argv/env".
6. **The exit-code vocabulary is load-bearing across three layers** — `worker prompt` 0–8
   (`src/worker/prompt/workflow.rs:880`–`:1102`), `pipeline` 5/6 (`src/pipeline/mod.rs:3626`, `:3720`), and
   `status --metrics` counting exit-6 resumes (`src/status.rs:4580`) [cited]. A rewrite that renumbers any of
   them silently breaks `status` and the watchdog.

## Playbook

1: done — read the tree at 43a60bd, extracted every `#[test]`/`#[tokio::test]` with file:line (1259), read
   the 24 shell-suite headers, `git log` on the deleted `scripts/` dir, and the one `#[ignore]` / 8
   `cfg(target_os)` gates; no history question was answered by guessing.
2: done — structured as Overview → Summary table → per-subsystem behaviour lines → harness-specific →
   implementation-detail flags → ignored/platform → gotchas.
3: done — every Evidence line re-checked against its command output (counts, ignore, cfg, `scripts/` absence,
   `$src` unused); `Verified: review-clean 43a60bd`.

## Blockers
none

## Lessons
- A parity inventory should count "which tests are meta/impl-detail" as a first-class column: 20+ of 1259
  assert repo hygiene or mock contracts and would wrongly gate a rewrite.
- A retired artefact kept as a parity oracle (`plan.jq`) is invisible to `rg` on the source tree — grep the
  test headers for "oracle"/"retired" before proposing its deletion.
