# Worker spend source (hermes-cos-3 Q19 group 7, cost watchdog)

Read-only research, 2026-11-19, branch `quaqua/sude-spendsrc` off `2ecc892`. Transcripts were read for usage/model/cost fields only; no message content is quoted. No billing API was called.

**Verdict.** Recommended source: the `cost-state` line Claude Code itself appends to each session jsonl. It carries `totalCostUSD` plus per-model token and USD totals, so no price table is needed for finished Claude workers. Fall back to summed `message.usage` tokens, flagged as an estimate, only for the live gap inside a first long turn. The engine already has a `usage()` seam and a `run` schema; both are unwired. `--metrics` shows nothing because (a) no `run` row is ever created on the live CLI path, (b) nothing writes cost into one, and (c) the default view hides closed runs.

## 1. Why `--metrics` records 0 cost today

```
$ quaqua status --metrics --all | grep -c '^run '
0
$ sqlite3 -readonly ~/quaqua/runs/qq.db "select count(*), sum(ended_ms is null), sum(cost_usd is not null) from run"
0||
$ sqlite3 -readonly ~/quaqua/runs/qq.db "select count(*) from agent; select count(*) from workflow"
72
0
```

72 agents, 0 workflows, 0 runs. Chain of missing links:

| # | Gap | Where |
|---|-----|-------|
| 1 | `run` rows are opened only by `settle_run` → `run::open`, which belongs to the workflow path. The live `worker spawn` / `cleanup` CLI path never opens one (`workflow` table is empty). | `crates/qq-core/src/task/worker_flow.rs:469` (`settle_run`), `:487` (`run::open`) |
| 2 | A run is opened with `cost_state='unknown'` and closed without any usage: `close` updates only `ended_ms`, `outcome`, `candidate_sha`. Nothing ever sets `tokens_in`, `tokens_out`, `cost_usd`, or `cost_state='reported'`. | `crates/qq-core/src/run/mod.rs:53`, `:78-91` (header comment `:2`: "Cost is `unknown` until a driver reports one") |
| 3 | The writer seam exists but has no caller: `HarnessDriver::usage(session_file)` defaults to `Ok(None)`; only `pi` implements it; `claude` and `generic` do not. `grep -rn "usage(" crates/qq-core/src` finds no call outside the driver files. | `crates/qq-core/src/driver/mod.rs:136-137`, `driver/pi.rs:85-113` (parses `message.usage.cost.total`) |
| 4 | Even with cost recorded, `status --metrics` without `--all` lists only runs with `ended_ms IS NULL`, so a CoS reading it *after* cleanup (as `worker-timebox.md` instructs) sees nothing. | `crates/qq-core/src/status/mod.rs:52` (`WHERE (?1 OR r.ended_ms IS NULL)`); printed at `:91` (`cost=`; null prints `unknown`, correct) |

The Q19 text says "spend from `runs/telemetry.jsonl`". That file does not carry spend: `head -c 300 ~/quaqua/runs/telemetry.jsonl` shows `ts, task, builder, reviewer, review, rounds, status, minutes` only (719 lines).

## 2. Candidate sources

Probes (paths absolute, because `cd` and `ls` are shadowed by zoxide/eza in this shell):

```
$ herdr agent get sude-spendsrc        # → agent_session.value = 66b20fdf-… (session id), cwd, pane_id
# transcript = ~/.claude/projects/<cwd with "/" and "." → "-">/<agent_session>.jsonl
# ~/Lab/.wt/sude-spendsrc → -home-tinhpt-Lab--wt-sude-spendsrc
```

Shape of a finished worker's transcript (`sude-briefnone`, session `abfa7a63-…`, 178 lines): top-level record types include `assistant`, `user`, `api-request`, and **`cost-state`**. Each `assistant` record has `message.model` and `message.usage` with `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, `cache_creation.{ephemeral_5m,ephemeral_1h}_input_tokens`, `output_tokens_details.thinking_tokens`, `server_tool_use.*`. **No per-message USD.** The `cost-state` record has: `totalCostUSD`, `modelUsage{<model>:{inputTokens, outputTokens, thinkingTokens, cacheReadInputTokens, cacheCreationInputTokens, cacheCreationEphemeral{5m,1h}InputTokens, webSearchRequests, costUSD}}`, `hasUnknownModelCost`, `totalAPIDuration`, `totalDuration`, `startTime`.

Fleet scan over all 140 `sude-*` worktree transcripts:

```
transcripts 140 | with cost-state 137 | cost-state is the final line 132 | multiple cost-state lines 5 | hasUnknownModelCost=true 0
```

The 3 without: two sessions last written on 2026-11-19 (one is this session, 0 `cost-state` lines mid-turn) and one older run that ended without one. Where there are several, `totalCostUSD` grows with the same `startTime` (e.g. g3wait 0.693 → 1.159): it is cumulative across resumes, so **the last line wins**. It is appended at the end of a turn, not continuously.

| Source | Gives | Accuracy | Latency | Covers |
|---|---|---|---|---|
| **A. `cost-state` line in session jsonl** | `totalCostUSD`, per-model tokens + USD | Exact: it is Claude Code's own figure. Example below: sum of `modelUsage.*.costUSD` equals `totalCostUSD` to the last digit. | Updated at turn end; absent in a first turn that is still running. Reading = tail of a file, ms. | Claude workers (all current kinds: sude, ni, suhuynh, truonglao). Pi's own format differs (item C). |
| **B. Sum of `message.usage` × price table** | tokens always; USD only via a table | Approximate. Streaming duplicates the same request (this transcript: 26 assistant records → 15 unique `requestId`s), so dedupe by `requestId`, last record wins. Fixed multipliers (out 5×, cache-read 0.1×, 5m write 1.25×, 1h write 2×) applied to `modelUsage` over 269 Sonnet 5.5 runs give an implied input rate with p10 $1.49, median $1.67, p90 $1.91, max $3.01 per Mtok. A single table is therefore off by roughly ±15%, more at the tail. Free-fit of per-field rates is ill-conditioned (collinear fields), so the table cannot be calibrated from this data alone; it needs the vendor's published prices. | Live, per request line. | Claude; any harness whose log has token fields. |
| C. pi session jsonl `message.usage.cost.total` | USD per assistant message | Exact when present | Live | pi only. Code exists (`driver/pi.rs:85`), no caller. Pi is paused in claude-only mode (`models.md`); no pi transcript was inspected. |
| D. Statusline JSON (`~/.claude/statusline.sh`, `refreshInterval` 30 s) | The script reads `cost.total_duration_ms`, `cost.total_api_duration_ms`, `context_window`, `session_id`, `transcript_path` from stdin JSON | Not verified for a USD field: the existing script does not read one and I did not capture the payload. Only runs while a UI renders it; a script would have to persist per session. | 30 s | Claude only. Rejected: same number as A with an extra moving part. |
| E. Provider billing API | Org-level spend | Not attempted (constraint). Coarse, delayed, not per worker. | Hours | Rejected. |
| F. `herdr agent get` / `list` | Status, session id, pane. No tokens or cost. | – | – | Used only to map worker → session id. |
| G. generic / opencode / agy workers | Unknown | Not investigated: no such worker exists in claude-only mode; the generic driver declares "no cost source" (`driver/mod.rs:14`). | – | Report `unknown`, never `0` (already how `render` behaves). |

### Worked example (finished worker `sude-briefnone`, session `abfa7a63-6a14-4fc6-81de-f7ca51889de3`)

```
$ grep '"type":"cost-state"' ~/.claude/projects/-home-tinhpt-Lab--wt-sude-briefnone/abfa7a63-6a14-4fc6-81de-f7ca51889de3.jsonl | tail -1 | jq '{totalCostUSD, modelUsage}'
totalCostUSD 0.2990416
claude-sonnet-5-5: inputTokens 1066, outputTokens 6173, cacheRead 923949, cacheWrite1h 35647, costUSD 0.2988449
claude-haiku-5-5 : inputTokens 1867, outputTokens 20,   costUSD 0.0001967
```

Check: 0.2988449 + 0.0001967 = **0.2990416** = `totalCostUSD`. Source A answers `cost=$0.30` with no table.

Cross-check with source B on the Sonnet line, median implied rate $1.669/Mtok input and the multipliers above: equivalent input tokens = 1066 + 5×6173 + 0.1×923949 + 2×35647 = 195,620 → ≈ **$0.327**, +9% against the true $0.299 (inside the ±15% band). Summing deduped transcript `usage` (30 in, 6161 out, 802,809 cache-read, 35,583 cache-write) differs from `modelUsage` by up to 13% on cache-read, so B undercounts tokens relative to the harness's own totals (cause not isolated). B is an estimate; A is the number.

## 3. Recommendation

**One source: the session jsonl, read with the Claude driver's `usage()`.** Prefer the last `cost-state` line (exact). If a transcript has none yet (first long turn), return the deduped token sum with `cost_usd=None` so it stays `unknown` in `status`, and let the watchdog compare **tokens** to a configured token-equivalent, or show `~$` from a table clearly marked as an estimate. Do not build the 80/100/120% ladder on B alone.

Smallest engine change, in the order they run:

1. `crates/qq-core/src/driver/claude.rs`: implement `usage()`. Scan the file backwards for the last `"type":"cost-state"` line; map `totalCostUSD` → `Usage.cost_usd`, sum `modelUsage` tokens → `input/output/cache_read/cache_write`. If none, dedupe `assistant` records by `requestId`, sum tokens, `cost_usd=None`. Add a helper `claude_session_file(cwd, session_id)` (slug = cwd with `/` and `.` → `-`). Unit test with a fixture jsonl (fake lines, no real content).
2. `crates/qq-core/src/run/mod.rs`: add `record_usage(conn, workflow_id, &Usage)` setting `tokens_in/out`, `cost_usd`, and `cost_state='reported'` only when `cost_usd` is `Some` (the schema `CHECK` at `db/sql/0003_run_capture.sql:19` already enforces that pairing). Idempotent.
3. `crates/qq-core/src/task/cleanup.rs` (`worker`, before `release_pane` at ~`:340`): read `agent_session.value` via `herdr agent get <name>` while the pane still exists, call the driver's `usage()`, then `record_usage`. A failure to read leaves `unknown`; it must never block cleanup.
4. Make a `run` row exist on the live path: either call `run::open` from the CLI spawn path (`task/worker_flow.rs`, next to the `settle_run` logic at `:469`) or have cleanup open and close it. Without this, step 2 has no row to write (today's table is empty).
5. `crates/qq-core/src/status/mod.rs:52`: show closed runs under `--metrics` (drop the `ended_ms IS NULL` filter when `metrics` is set, or document `--all`), and update `templates/workspace/rules/_all/worker-timebox.md` to match.
6. Live polling (separate, after 1-5): in `task/prompt.rs` `wait` (`:785`), on the existing poll tick read `usage()` and write `runs/.<name>.spend` next to the other sidecars (`side()` at `:634`). The watchdog, `timers.cost_buffer` and `on_cost_cap` then read that sidecar; they stay out of this change.

Risks: `cost-state` is an undocumented transcript record and could change with a Claude Code release, so keep the B fallback and a "no cost-state → unknown" test; it also appears only at turn end, so a single long turn is blind until it finishes; session-file lookup breaks if the worktree path changes before cleanup reads it.
