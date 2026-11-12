# Fanout: scale, limits, liveness, recovery, events

**Verdict: PARTIAL, 3/5.** Caps, time limits, liveness and provider-error retry are engine-enforced. Cost limits are advisory by default, wave accounting is prose, and waiting is a 60-second poll.

Audited at `41a5327`. `src/` means `crates/quaqua/src/`. `tpl/` means `templates/workspace/`.

## Scope

- "Many agents from different vendors and harnesses run in parallel, with clear limits per coordinator and per Lead."
- "Every worker has liveness, time, and cost limits; stalls, timeouts, and provider errors are detected and classified."
- "Failed work resumes before it respawns, and retries depend on how the work failed."
- "Every wave is accounted for: each worker has arrived, respawned, or had its scope absorbed, so nothing disappears silently."
- "Workers wake and report through events, not polling."

## Findings

| Requirement | Status | Evidence | Gap |
|---|---|---|---|
| Mixed-vendor parallel agents with limits per coordinator and per Lead | partial | Five harnesses (`src/harness/mod.rs:9-24`). 8 slots per CoS, 4 Peers per Lead, pipeline and dual-lens weigh 2 (`tpl/team.toml:4-7`, `:28`). Cap check with a clear refusal message (`src/cap.rs:75-92`), called for pipelines and taskforces (`src/pipeline/mod.rs:673`, `src/taskforce/mod.rs:341`) and through the layout gate for spawns (`src/layout/gate.rs:560-561`). Per-project override (`tpl/team.toml:102-103`). | The largest recorded wave is 4 mixed-kind workers, with friction: "topology gate requiring --solo for 3rd+ worker and agy taking ~1h" (`.harness/plans/active/harness-mvp.md:21`). An 8-worker wave is not recorded. The grid holds 4 workers per tab (`tpl/AGENTS.md:85`). |
| Liveness, time and cost limits; stalls, timeouts and provider errors detected and classified | partial | Time budget, stall, idle and hard cap map to exits 2–4 (`src/worker/prompt/mod.rs:60-63`). Timers are config (`tpl/team.toml:9-19`). Liveness distinguishes `Unknown` from `Down` and needs two agreeing checks (`src/liveness.rs:1-6`). Provider errors are classified by HTTP status and pane text, with capped backoff and exit 7 (`src/worker/prompt/retry.rs:1-7`, `:125-142`). All five harnesses get stall, idle, budget and hard-cap detection (`.harness/plans/active/harness-mvp.md:23`). | Cost cap defaults to `warn` (`src/config.rs:29-36`). At 100% plus buffer the engine prints `ESCALATE` and notifies; it does not stop the worker (`tpl/rules/_all/worker-timebox.md:4`, `src/worker/prompt/cost.rs:1088`). agy has no spend source (`src/worker/prompt/cost.rs:776-781`). Claude and pi cannot be interrupted (`src/harness/mod.rs:68`). agy provider-error text is not yet classified (`.harness/plans/active/harness-mvp.md:25`). The budget auto-extends up to +50% on diff progress (`tpl/rules/_all/worker-timebox.md:6`), so the time budget is soft below the 7200 s cap. |
| Resume before respawn; retries depend on failure mode | partial | `quaqua worker prompt --resume` continues the same session and allowance (`src/worker/prompt/mod.rs:46-48`, `:240`). Transient provider errors resume automatically up to 4 times (`src/worker/prompt/retry.rs:6-7`). `quaqua pipeline --resume` continues the same phase (`src/pipeline/mod.rs:44`, `:58-60`). Agent-start failure takes a one-hop fallback kind (`src/config.rs:250-252`). | Only provider errors have a coded policy. "cap/OOM gets smaller scope; network drops retry as-is; tool errors use another model" is prose (`tpl/AGENTS.md:66`). After exits 2–5 a coordinator must inspect and resume by hand (`tpl/rules/_all/worker-timebox.md:9`). Nothing counts resume versus fresh respawn (`grep -rn respawn src` finds only usage-audit and a test comment). |
| Every wave accounted for | missing | The rule exists (`tpl/AGENTS.md:67`). | No wave object, no accounting command, no check. `grep -rn "wave\|absorbed" crates/quaqua/src --include=*.rs` returns nothing. `quaqua status` flags a lost waiter (`src/status.rs:58`, `:669`), which catches one way work disappears. |
| Wake and report through events, not polling | partial | herdr pane events feed the bus through `quaqua events hook` (`src/bus/events.rs:1393`, `:1525`, `:1599`). A new trigger nudges an idle consumer (`src/bus/events.rs:1315`). The CoS inbox is delivered by `UserPromptSubmit` and `Stop` hooks (`tpl/.claude/settings.json:42-55`). The runner uses timers behind a clock seam (`src/engine/runner.rs:510-540`). | The watchdog re-observes on a poll, default 60 s (`src/worker/prompt/mod.rs:895-898`). The coordinator waits on a backgrounded process and runs `quaqua status` on wake (`tpl/AGENTS.md:62`). The GitHub CI source is a 5-minute timer poll (`README.md:185-187`). `quaqua inbox --follow` polls every 2 seconds (`quaqua inbox --help`). |

## Refs

- Exit code table: `src/worker/prompt/mod.rs:56-69`.
- Watchdog knobs: `tpl/team.toml:9-19`, `tpl/rules/_all/worker-timebox.md:3-9`.
- 4-worker wave result: `.harness/plans/active/harness-mvp.md:21`. I did not see the run report it cites, because the workspace `raw/` directory is not in this repo.
- Tests covering this area passed in my run: `watchdog`, `pipeline_cancel`, `council`, `lead_repair`, `events_hook`, `subscription` (part of the 1183 passes reported in `01-harness-engineering.md`).

## Fixes

1. Hard cost cap for every kind: see fix 1 in `01-harness-engineering.md`. Until agy spend is readable, cap agy by time-priced estimate.
2. Add a wave record: `quaqua wave open <id>` lists expected workers, each spawn joins it, and `quaqua wave close` refuses until every member is `arrived`, `respawned` (with a reason) or `absorbed` (with the absorbing worker).
3. Move the watchdogs into one supervisor process that subscribes to herdr events and owns all timers, so a coordinator crash or reboot loses no waiter and waiting stops being a per-worker poll. Details in `09-drift-architecture.md`.
4. Encode the retry policy table (failure class → action → max attempts) in `team.toml` and have the supervisor apply it, journaling each attempt with its class.
5. Journal `resumed` and `respawned` events so the resume rate is a query.
6. Raise the grid past 4 per tab or drop the layout dependency for headless waves, then record an 8-worker mixed wave as an eval.
