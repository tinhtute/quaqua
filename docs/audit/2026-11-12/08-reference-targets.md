# Reference targets: measurable today, current value, what is missing

**Verdict: FAIL, 2/5.** Of 21 targets, 3 can be measured today with shipped commands, 11 can be measured in part, and 7 cannot be measured at all. Most current values are unknown, because real telemetry lives in the workspace, which was not available to this audit.

Audited at `41a5327`. `src/` means `crates/quaqua/src/`. `tpl/` means `templates/workspace/`.

## Scope

All bullets under "Reference targets" in the brief: Autonomy, Quality, Fanout and reliability, Cost and speed, Chief of Staff memory and learning, Harness engineering. Each is quoted in the first column below.

"Measurable today" means a shipped command or file produces the number without new code. "Current value" is given only where I could read it. The workspace files (`runs/telemetry.jsonl`, `runs/quaqua.db`, `memory/`, `reports/`) do not exist in this environment, so most values read "not available to this audit".

## Findings

### Autonomy

| Target | Measurable today | Current value | Missing to measure |
|---|---|---|---|
| "The owner's only touches per task are approvals: zero manual steps between brief and an approved merge." | no | Not zero. The project's own plan lists 6 human steps plus 4 unwired chores (`.harness/plans/active/harness-mvp.md:17`). | An approval event and a manual-step event per task. No task id exists to count against. |
| "At least 80% of tasks finish without the owner re-explaining intent." | partial | not available to this audit | Correction capture writes `memory/corrections.md` (`CHANGELOG.md:14`), but lines are not tied to a task and only vi/en phrase patterns count. Needs a task id on each correction and a task total. |
| "A long-running project survives a session restart or a machine reboot with no lost work." | no | Unknown. Design supports resume (`src/engine/runner.rs:1-5`, `src/pipeline/mod.rs:44`), but no recorded reboot test exists in the repo. | A recorded kill-and-resume eval. A lost waiter is detectable (`src/status.rs:58`), not counted. |

### Quality

| Target | Measurable today | Current value | Missing to measure |
|---|---|---|---|
| "100% of merges carry a passing verdict at the exact head commit." | yes | not available to this audit | `quaqua status --metrics` matches merge commits on `main` and integration branches against journaled verdicts (`src/status.rs:1099-1106`, tests `:4345`, `:4549`). Needs the live `runs/quaqua.db`. |
| "0 self-reviewed merges." | partial | Inside a pipeline the config forbids same-harness review (`src/pipeline/mod.rs:631-633`). Count across all merges is unknown. | Builder and reviewer identity on the verdict row joined to the merge. Merges of unreviewed work (auto-skip, `src/pipeline/gate.rs:198-205`; solo workers) are not distinguished from reviewed ones. |
| "Independent review catches a real defect in at least 20% of reviewed tasks; a reviewer below that for a sustained window is switched off or replaced." | partial | not available to this audit | `review_changed_outcome` per pipeline is recorded (`src/status.rs:1070`). No per-reviewer rate over a window, no check that the finding was a real defect, and no switch-off. |
| "An escaped defect (one found after merge) becomes a rule or a check within one cleanup cycle." | no | unknown | No escaped-defect record. The GitHub CI source records `ci_failed` only for `quaqua/*` branches with a thread (`src/source/github_ci.rs:1-7`), not for `main` after merge. |

### Fanout and reliability

| Target | Measurable today | Current value | Missing to measure |
|---|---|---|---|
| "A wave of 8 or more mixed-vendor workers runs clean with no manual layout or cleanup." | no | Largest recorded wave is 4 mixed-kind workers, with friction (`.harness/plans/active/harness-mvp.md:21`). | A wave record and a recorded 8-worker run. |
| "100% of stalls, timeouts, and budget overruns are detected by the engine, not by a human." | partial | Stall, idle, time budget and hard cap are detected for all five harnesses (`.harness/plans/active/harness-mvp.md:23`). Cost overrun is not detectable on agy (`src/worker/prompt/cost.rs:776-781`). | A count of human-detected incidents to compare against. Watchdog exits are in the journal (`src/status.rs:1097-1099`). |
| "Every failure is classified; unknown failures stay under 5%." | partial | not available to this audit | Exit codes 2–7 classify watchdog stops (`src/worker/prompt/mod.rs:56-69`), and provider errors have labels (`src/worker/prompt/retry.rs:125-142`). No `unknown` bucket is counted, and exit 1 mixes usage errors with refusals. |
| "At least 90% of interrupted workers recover through resume instead of a fresh respawn." | no | unknown | No `resumed` or `respawned` event. `status --metrics` counts only exit-6 resumes (`src/status.rs:4264`). |

### Cost and speed

| Target | Measurable today | Current value | Missing to measure |
|---|---|---|---|
| "Every task has a cost and time budget, and no task overruns its hard cap." | partial | Time: the 7200 s hard cap is enforced with exit 2 (`src/worker/prompt/mod.rs:60`). Cost: no hard cap by default (`src/config.rs:29-36`). | Budgets are per worker run, not per task. Telemetry has `usd` and `usd_budget`, `wall_sec` and `budget_sec` per worker (`src/worker/cleanup.rs:26-30`), so overruns per run are computable; agy rows have no dollars. |
| "Cost per task and lead time per task are tracked, and both trend down month over month." | partial | not available to this audit | Per-worker and per-pipeline rows exist (`src/worker/cleanup.rs:13-34`, `src/status.rs:1060-1073`). No task id joins them, no intake timestamp, no monthly rollup. `--metrics` takes `--since` only (`src/status.rs:1010-1023`). |
| "Swapping a worker's model or vendor takes one config change and zero code changes." | yes | True among the 5 supported harnesses: change `[kinds.<k>]` in `tpl/team.toml:54-85`; validated by `quaqua config validate` (`src/config.rs:794-850`). False for a new harness: `src/harness/mod.rs:9-24` is a closed enum, and commit `0bc6066` added agy with code. | Nothing for the supported set. |

### Chief of Staff memory and learning

| Target | Measurable today | Current value | Missing to measure |
|---|---|---|---|
| "The Chief of Staff answers 'what did we decide about X' from memory or recall without the owner repeating it." | no | unknown | A recall eval set (question, expected anchor) run against `quaqua transcripts search` (`src/transcripts/mod.rs:8-12`). |
| "The background review fork runs on every qualifying session and proposes at least one useful memory or rule update per week." | no | Live firing not yet demonstrated; loading and counter behaviour are open questions (`.harness/plans/active/harness-mvp.md:19`, `:24`). | A journaled `review_fork` event with applied-operation count. The mod keeps counters in its own store only (`mods/cos-memory/README.md:147-150`). |
| "The same root cause never repeats 3 times without a rule." | partial | not available to this audit | The auditor flags 2 or more in 14 days (`src/auditor.rs:1077-1120`). It depends on hand-written `[case]` lines with matching wording. |
| "Memory stays within its caps with zero secrets stored." | yes | not available to this audit | `quaqua memory check [--workspace DIR]` exits 1 with one line per violation (`src/memory.rs:7-8`). Covers only `memory/rules.md` and `USER.md`. |

### Harness engineering

| Target | Measurable today | Current value | Missing to measure |
|---|---|---|---|
| "Every lifecycle stage has at least one machine check; prose-only rules shrink over time." | partial | By my count 8 of 11 stages have some machine check; intake, release and lesson promotion have none (`02-adlc.md`, stage map). 9 recurring coordinator rules are prose-only (`07-rules-self-improvement.md`). | A maintained stage-to-check registry and a prose-rule count per release. |
| "Every harness change ships only with green evals plus one recorded end-to-end run." | partial | CI requires `cargo test --locked` and actionlint (`.github/workflows/test.yml:1-12`). My run: 1183 passed, 7 failed for a missing `sqlite3` CLI. No recorded end-to-end run exists. 6 first-parent commits on `main` carry no PR reference. | An end-to-end eval in CI; shell and Node suites in CI. |
| "Any run can be fully traced: who did what, on which commit, at what cost, and with which verdict." | partial | not available to this audit | Verdict rows have the SHA (`src/worker/prompt/verdict.rs:328`). Telemetry rows have cost but no SHA or verdict (`src/worker/cleanup.rs:13-34`). No shared run id. `JsonlSpanSink` is unused. |

### Count

| | Count | Targets |
|---|---|---|
| Measurable today | 3 | merges with a verdict at head, model or vendor swap, memory caps and secrets |
| Partly measurable | 11 | re-explain rate, self-reviewed merges, review hit rate, engine detection of stalls and overruns, failure classification, budgets and hard caps, cost and lead time, root-cause repeats, stage checks, eval gating, tracing |
| Not measurable | 7 | owner touches, restart survival, escaped defects, 8-worker wave, resume rate, recall answers, review fork cadence |

## Refs

- Metrics implementation: `src/status.rs:1010-1106`, `:1212`, `:1620`, `:1660`.
- Telemetry row shapes: `src/worker/cleanup.rs:13-34`, `src/status.rs:1060-1073`.
- `quaqua status --metrics` could not be run for real values: there is no workspace or `runs/` directory here.
- Test totals: see Refs in `01-harness-engineering.md`.

## Fixes

1. Introduce a task id that every spawn, pipeline, verdict, merge, approval, correction and telemetry row carries. Eight of the targets become queries the day this lands.
2. Journal the missing events: `merge`, `approval`, `manual_step`, `resumed`, `respawned`, `review_fork`, `escaped_defect`, `wave_opened`, `wave_closed`.
3. Add `quaqua status --slo`: one line per reference target with value, window and pass or fail, reading only the journal.
4. Record three evals and run them on a schedule: reboot mid-pipeline, 8-worker mixed wave, and a 20-question recall set.
5. Poll CI on `main` after merge and open an `escaped_defect` trigger that stays open until a rule or check references it.
