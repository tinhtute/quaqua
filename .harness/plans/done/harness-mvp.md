---
status: completed
lane: normal
---
# Harness MVP — quaqua end to end

## Goal
- outcome: quaqua ships work end to end through its CLI with a CoS that remembers like Hermes, fans out ≥4 workers, and enforces cost and liveness for every kind.
- success_signal: one recorded run where CoS takes a task from brief to merged PR via quaqua CLI only, with a ≥4 mixed-kind wave, the review fork firing live, and a cost cap or watchdog stop observed on an agy worker.
- actors: Principal, CoS, Leads, workers.
- authority: Principal 2026-11-11 + raw/2026-11-11-quaqua-cmval.md.
- requirements:
  - R1: the G1 review fork can only Read/Write/Edit under `memory/`; a fork attempt to touch any other path or tool is denied by the engine, not by the prompt | acceptance: `node --test "mods/cos-memory/tests/**/*.test.js"` (the engine denies every `$.model.fork` tool, index.d.ts:6686; there is no Rust fork_allowlist test) | source: Q4
  - R2: `quaqua` refuses at config load (`load_team_config`) any kind on a claude or pi harness with `on_cost_cap = "kill"`, naming the kind and harness; kill on opencode/omp/agy still loads | acceptance: `cargo test -p quaqua config_kill_refused` | source: Q10, Q7
  - R3: the cos-memory mod is loaded in the real CoS session and the review fork has fired there (`review.lastReviewAt` set in the plugin store); `mods/cos-memory/tsconfig.json` is either committed or deliberately left out, with `git status` clean for `mods/cos-memory` | acceptance: `bash mods/cos-memory/tests/install_smoke.sh` `git status --porcelain mods/cos-memory` (empty) | source: Q3, Q8
  - R4: a `quaqua ship` command pushes the worker branch and opens the PR after one Principal approval, and never merges | acceptance: `cargo test -p quaqua ship` | source: Q1, Q2
  - R5: one recorded run takes a task from brief to merged PR through the quaqua CLI only, with a ≥4 mixed-kind wave, the review fork firing live, and a cost cap or watchdog stop observed on an agy worker; the Principal does the final merge | acceptance: `gh pr view <pr> --json mergedAt` `quaqua status --metrics` (4+ kinds, agy stop line) `jq .review.lastReviewAt ~/.claude/plugins/store/cos-memory_*.json` | source: Q2, Q3, Q5, Q6, Q7
- non-goals:
  - NG1: GUI series #214–#224 (deferred by Principal).
  - NG2: agy usage parsing (Q6) and the agy provider-error classifier (Q9) are not built here; the agy stop in R5 may be the time-budget watchdog, which already covers agy (Q7).

## Map
- Q1: Which steps from brief to merged PR still need a manual action today? | type: research | by: AFK | blocked_by: none | status: decided → 6 steps still need a human (brief text, report acceptance, test rerun, push, PR open, merge to main) plus 4 unwired chores (worker cleanup, binary reinstall, branch delete, CI-failure watch); no `quaqua` command touches the remote | evidence: raw/2026-11-11-quaqua-q1shipflow.md @ e77b22c
- Q2: Should the CLI open and merge PRs itself, or stop at push+PR with merge by the Principal? | type: grill | by: HITL | blocked_by: Q1 | status: decided → stop at push+PR: a quaqua ship command pushes and opens the PR after one Principal approval; merge to main stays manual by the Principal | evidence: user 2026-11-11
- Q3: What session shape makes the G1 review fork fire live, and how is that recorded? | type: research | by: AFK | blocked_by: none | status: decided → one long-lived interactive (not `-p`) Claude Code session that loads the mod, does ≥10 non-meta user turns and (≥10 Stops or ≥15 tool calls) since the last review, has `stop_hook_active` false and an unchanged `memory/rules.md` + `memory/<today>.md` stamp, then stays open for the detached fork to finish; recorded today only as `review.lastReviewAt` + `stamp` in `~/.claude/plugins/store/cos-memory_<src>-<hash>.json` | evidence: raw/2026-11-11-quaqua-q3forklive.md @ e77b22c
- Q4: Is a prompt-only tool restriction on the review fork acceptable, or must the engine enforce a tool allowlist? | type: grill | by: HITL | blocked_by: Q3 | status: decided → the engine must enforce a tool allowlist on the G1 review fork (Read/Write/Edit scoped to memory/); prompt-only is not acceptable because the fork runs unattended; follow-up: implementation task | evidence: HITL grill 2026-11-12, CoS session
- Q5: Does a 4-worker mixed-kind wave run clean today (grid, reports, cleanup)? | type: task | by: AFK | blocked_by: none | status: decided → 4 mixed-kind workers (omp, claude, agy, claude) ran to COMPLETE; frictions were topology gate requiring --solo for 3rd+ worker and agy taking ~1h | evidence: raw/2026-11-11-quaqua-q*.md, memory 2026-11-11
- Q6: How can quaqua measure usage for agy workers (log parsing, API, or a time-based cap)? | type: research | by: AFK | blocked_by: none | status: decided → log parsing via ~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript.jsonl mapped by conversation_summaries.db; time-based cap remains the runaway backstop | evidence: raw/2026-11-11-quaqua-q6agyusage.md @ e77b22c
- Q7: Which harnesses lack stall/timeout detection in the watchdog? | type: research | by: AFK | blocked_by: none | status: decided → None: all 5 harnesses (claude, opencode, omp, pi, agy) get stall, idle, budget and hard-cap detection; the per-harness gaps are elsewhere — agy has no cost tracking and no provider-error classification (exit 7), and claude/pi have no stop keys, so on_cost_cap=kill cannot interrupt them | evidence: raw/2026-11-11-quaqua-q7watchdog.md @ e77b22c
- Q8: Why does the cos-memory Stop counter persist stops:0 in live -p sessions, and how is the mod loaded into the real CoS session? | type: research | by: AFK | blocked_by: Q3 | status: decided → cos-memory turn.complete trigger (2fdda74) counts live in interactive and -p; old stops:0 came from classic.Stop being skipped under cc-plugin-sec-default (inferred); follow-up: load the mod in the real CoS, and decide on the untracked mods/cos-memory/tsconfig.json | evidence: raw/2026-11-12-mvp-sude-q8live.md, raw/2026-11-12-mvp-sude-q8fix.md @ 2fdda74
- Q9: What error text does an agy pane print on a provider error, so retry.rs can classify it for exit 7? | type: research | by: AFK | blocked_by: Q7 | status: decided → agy error line is `⚠ <STATUS> (code NNN): msg` then `Error ID: <uuid>` (or generic "Agent execution terminated due to error."); retry.rs classifies by status: retry RESOURCE_EXHAUSTED/UNAVAILABLE/DEADLINE_EXCEEDED/INTERNAL/ABORTED; fatal PERMISSION_DENIED/UNAUTHENTICATED/INVALID_ARGUMENT/NOT_FOUND/FAILED_PRECONDITION/UNKNOWN; 429/5xx → exhausted=true; "AI credits balance is too low" → fatal billing | evidence: raw/2026-11-12-mvp-tieude-q9agyerr.md
- Q10: Should kinds on claude/pi be refused on_cost_cap="kill" at config load, since no interrupt is possible (stop.rs:78 refuses at run time)? | type: grill | by: HITL | blocked_by: Q7 | status: decided → config load must refuse on_cost_cap="kill" for kinds on claude/pi harnesses (no stop key), failing fast instead of the run-time refusal at stop.rs:78; follow-up: implementation task | evidence: HITL grill 2026-11-12, CoS session
- fog: when work spans days, should a long-lived quaqua-rs Lead own MVP execution instead of CoS?

## Phases and Verification
- approach: five worker-sized phases; Q4 and Q10 are independent engine changes that run in parallel, then the mod goes live under the allowlist, then `quaqua ship`, then the recorded run. Rejected: prompt-only fork restriction (Q4) and run-time-only kill refusal (Q10). The four phases after `mvp-run` are candidates the recorded run produced, not part of R1–R5.
- phase_slug: `fork-allowlist`
  status: done (#249)
  - goal: R1 | depends_on: none
  - surfaces: crates/quaqua/src/ (engine-side tool allowlist for the review fork), mods/cos-memory/src/review.js, crates/quaqua/tests/ | avoided: worker/, merge/, grid/
  - phase check: `node --test "mods/cos-memory/tests/**/*.test.js"`
- phase_slug: `kill-refusal`
  status: done (#250)
  - goal: R2 | depends_on: none (parallel with `fork-allowlist`)
  - surfaces: crates/quaqua/src/config.rs, crates/quaqua/tests/config.rs | avoided: worker/stop.rs run-time refusal stays as a second guard
  - phase check: `cargo test -p quaqua config_kill_refused` `cargo test -p quaqua config`
- phase_slug: `cos-mod-live`
  status: done (#252; live fork: `cos-memory_quaqua-*.json` review.lastReviewAt=1794472950808, then 1794492448941 on this Mac 2026-11-12 21:07 +07, inflightAt null)
  - goal: R3 | depends_on: `fork-allowlist`
  - surfaces: mods/cos-memory/ (tsconfig.json decision: gitignored, `tsc -p` finds no inputs for a plain-.js mod), the real CoS plugin store | avoided: crates/
  - phase check: `bash mods/cos-memory/tests/install_smoke.sh` `git status --porcelain mods/cos-memory` (empty); live fork per Q3 session shape (≥10 user turns, ≥10 Stops or ≥15 tool calls, then stay open) and `jq .review.lastReviewAt ~/.claude/plugins/store/cos-memory_*.json`
- phase_slug: `ship`
  status: done (#251)
  - goal: R4 | depends_on: none
  - surfaces: crates/quaqua/src/ (new ship command, main.rs CLI), crates/quaqua/tests/ | avoided: merge to main (stays manual)
  - phase check: `cargo test -p quaqua ship`
- phase_slug: `mvp-run`
  status: done (#258 d51d2d9; 4+ kinds, agy-pilot/agy-wave exit 4 at 2m/2m, live fork lastReviewAt=1794492448941)
  - goal: R5 | depends_on: `fork-allowlist`, `kill-refusal`, `cos-mod-live`, `ship`
  - surfaces: runs/ report only, no code | avoided: any engine change (a gap found goes back as a new phase)
  - phase check: `gh pr view <pr> --json mergedAt` `quaqua status --metrics` `jq .review.lastReviewAt ~/.claude/plugins/store/cos-memory_*.json`
- phase_slug: `agy-progress`
  status: dropped 2026-11-13: no agy kind in team.toml, workspace runs claude-only (was: planned, candidate from `mvp-run`; raw/2026-11-12-quaqua-rs-agyprobe.md Q1)
  - goal: a working agy worker is not killed at its budget — agy reports no session id (`~/.gemini/config/hooks/herdr-agent-state.sh` absent, harness/mod.rs:79-82), so `session_seen` stays false, `last_progress_ms` stays 0 and the 25% grace never applies (workflow.rs budget block) | depends_on: none
  - surfaces: crates/quaqua/src/launch.rs (ship/install the hook), crates/quaqua/src/worker/prompt/{workflow,cost}.rs (`agy_session_mtime` fallback) | avoided: a pane-text progress source (spinners animate, it would mask real stalls)
  - phase check: a live agy worker past its budget with a moving conversation store gets grace, not exit 4
- phase_slug: `agy-nudge`
  status: dropped 2026-11-13: no agy kind in team.toml, workspace runs claude-only (was: planned, candidate from `mvp-run`; raw/2026-11-12-quaqua-rs-agyprobe.md Q2)
  - goal: the 80% nudge reaches an agy worker mid-turn — agy queues a prompt until the running turn ends, so the send path does `esc` (HerdrRequest::SendKeys, `Harness::Agy.stop_keys`) then the Prompt for agy only; if that aborts real work, agy kinds get no nudge and the brief's Time budget is the only stop | depends_on: none
  - surfaces: crates/quaqua/src/worker/prompt/workflow.rs (nudge send), crates/quaqua/src/harness/mod.rs | avoided: every other harness's send path
  - phase check: a live agy worker at 80% of budget reacts to the nudge before its turn ends
- phase_slug: `agy-effort`
  status: dropped 2026-11-13: no agy kind in team.toml, workspace runs claude-only (was: planned, candidate from `mvp-run`)
  - goal: `--effort` is not passed to a model that rejects it (agy logs `--effort is not supported for model "gemini-3.8-flash"`), and an agy metrics row carries dollars from the Q6 transcript parse instead of blank | depends_on: Q6
  - surfaces: crates/quaqua/src/launch.rs (effort per harness/model), crates/quaqua/src/worker/ (cost parse) | avoided: a cost cap for agy (NG2 keeps the time cap as the runaway backstop)
  - phase check: `quaqua status --metrics` shows a $ figure on an agy row; an agy launch log has no "effort is not supported"
- phase_slug: `merge-verdict`
  status: moved to quaqua-rs backlog 2026-11-13 (CoS tracks) (was: planned, candidate from `mvp-run`; G1 in docs/research/plans/pstack-integration.md § Pilot)
  - goal: a merge to main carries a verifier verdict at the head SHA — 19 of 19 merges in the mvp-run window had none, because no merge event is journaled; record the verdict at merge so `status --metrics` counts G1 from it | depends_on: none
  - surfaces: crates/quaqua/src/status.rs (G1 counting, :1102-1136), crates/quaqua/src/{verify,ship}.rs, crates/quaqua/src/merge/ | avoided: a merge gate (warning first, the count is the evidence)
  - phase check: `quaqua status --metrics` G1 numerator falls below 19/19 over the next merges

## Log
- 2026-11-11 — map opened from the Principal's destination; 8 lines, no decisions yet.
- 2026-11-12 — map decided (10 of 10); locked: R1–R5, five phases. Lane kept `normal` though there are several phases, because each is one reviewable unit; `fork-allowlist` mechanism is unknown (`$.model.fork` takes only `{ prompt, effort }`), the worker must verify it before building.
- 2026-11-12 — #249–#252 merged (fork-allowlist, kill-refusal, ship, cos-mod-live build); R5 started with this pilot, the first unit through `quaqua ship`.
- 2026-11-12 — `cos-mod-live` done: live review fork wrote lastReviewAt=1794472950808 in the installed store `~/.claude/plugins/store/cos-memory_quaqua-*.json` (inline copy still null); #253, #254 merged.
- 2026-11-12 — #255 merged (mvp wave 2, db985d5): watchdog exit-4 telemetry row, Budget lint, `ship` without --repo, pre-push marker, `cos-mod-live` marked done.
- 2026-11-12 — #256 merged (retro wave 3, 35b9edf): spawn bases on origin/<default>, `quaqua verify record`, watchdog blocked corroboration + Compact banner, clippy clean; 1217 tests pass.
- 2026-11-12 — #244 merged (73ee97e): docs/audit/2026-11-12 intent audit at 41a5327, 3/5; no requirement changed.
- 2026-11-12 — `mvp-run` done (R5): #258 merged d51d2d9 (brief → PR via `quaqua ship`, Principal squash-merge); 4+ mixed kinds (omp, claude, agy) with agy-pilot and agy-wave exit 4 at 2m/2m; live review fork fired on this Mac — `cos-memory_quaqua-4f9b87b55123.json` review.lastReviewAt=1794492448941 (21:07 +07) with `memory/rules.md` written at that instant; #259 adda975 fixed the mod test command to the glob form. Four follow-up candidates recorded from the run's gaps.
- 2026-11-13 — decision — closed: R1–R5 met (independent full review APPROVE_WITH_REQUESTS, phase `mvp-run`); agy-progress/agy-nudge/agy-effort dropped (no agy kind in team.toml, workspace runs claude-only); merge-verdict moved to quaqua-rs backlog (CoS tracks); absorb: none

## Validation
- 2026-11-12 CoS: R5 on main adda975 — PR #258 merged d51d2d9; `quaqua status --metrics` shows 4+ kinds and agy-pilot/agy-wave exit 4 at 2m/2m; live fork `jq -c .review ~/.claude/plugins/store/cos-memory_quaqua-4f9b87b55123.json` → `lastReviewAt:1794492448941` (2026-11-12 21:07:28 +07), `memory/rules.md` mtime 21:07 size 2402 matching the stamp. R2/R4 ride on main adda975 CI `Test` = success (workspace `cargo test`, mod tests, actionlint).
- 2026-11-12 R1: `node --test "mods/cos-memory/tests/**/*.test.js"` on adda975 → 38 pass, 0 fail (Node 24.21.0); the directory form fails on Node 22+, which is why #259 changed it.
- 2026-11-13 — phase `mvp-run` — verdict: APPROVE_WITH_REQUESTS — mode: full
  - R1 `node --test "mods/cos-memory/tests/**/*.test.js"` 38 pass 0 fail; R2 `cargo test -p quaqua config_kill_refused` + `--test config` 17 passed (refusal sits in validate/spawn/watchdog, not `load_team_config` — reword R2); R3 install_smoke all checks passed, `git status --porcelain mods/cos-memory` empty, live fork lastReviewAt 1794540430467 (2026-11-13 10:27 +07, Linux host); R4 `cargo test -p quaqua ship` 8 passed, never-merge asserted
  - R5 by history: PR #258 merged d51d2d9 2026-11-12T13:26Z; agy-pilot/agy-wave rows are Mac-only, host-local agy stop agy-watchtest exit 4 383s/300s; agy no longer configured
  - `QUAQUA_TEST=1 cargo test --locked --no-fail-fast` 1218 passed 1 failed (env-only identity test); without QUAQUA_TEST bus_migrate 2 fail on cargo 1.100 (hermeticity, new task); CI Test on bff639a success
  - judge: independent (runs/suhuynh-mvprev.md), claude/opus

## Current State and Next Action
- active_phase: none
- lifecycle_status: completed
- blockers: none
- open_items: none
- exact_next_action: none; plan closed 2026-11-13 (merge-verdict tracked in the quaqua-rs backlog by CoS)
