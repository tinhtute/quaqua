---
status: active
lane: normal
---
# Harness MVP — quaqua end to end

## Goal
- outcome: quaqua ships work end to end through its CLI with a CoS that remembers like Hermes, fans out ≥4 workers, and enforces cost and liveness for every kind.
- success_signal: one recorded run where CoS takes a task from brief to merged PR via quaqua CLI only, with a ≥4 mixed-kind wave, the review fork firing live, and a cost cap or watchdog stop observed on an agy worker.
- actors: Principal, CoS, Leads, workers.
- authority: Principal 2026-11-11 + raw/2026-11-11-quaqua-cmval.md.
- requirements: none until the map is decided
- non-goals:
  - NG1: GUI series #214–#224 (deferred by Principal).

## Map
- Q1: Which steps from brief to merged PR still need a manual action today? | type: research | by: AFK | blocked_by: none | status: decided → 6 steps still need a human (brief text, report acceptance, test rerun, push, PR open, merge to main) plus 4 unwired chores (worker cleanup, binary reinstall, branch delete, CI-failure watch); no `quaqua` command touches the remote | evidence: raw/2026-11-11-quaqua-q1shipflow.md @ e77b22c
- Q2: Should the CLI open and merge PRs itself, or stop at push+PR with merge by the Principal? | type: grill | by: HITL | blocked_by: Q1 | status: decided → stop at push+PR: a quaqua ship command pushes and opens the PR after one Principal approval; merge to main stays manual by the Principal | evidence: user 2026-11-11
- Q3: What session shape makes the G1 review fork fire live, and how is that recorded? | type: research | by: AFK | blocked_by: none | status: decided → one long-lived interactive (not `-p`) Claude Code session that loads the mod, does ≥10 non-meta user turns and (≥10 Stops or ≥15 tool calls) since the last review, has `stop_hook_active` false and an unchanged `memory/rules.md` + `memory/<today>.md` stamp, then stays open for the detached fork to finish; recorded today only as `review.lastReviewAt` + `stamp` in `~/.claude/plugins/store/cos-memory_<src>-<hash>.json` | evidence: raw/2026-11-11-quaqua-q3forklive.md @ e77b22c
- Q4: Is a prompt-only tool restriction on the review fork acceptable, or must the engine enforce a tool allowlist? | type: grill | by: HITL | blocked_by: Q3 | status: open
- Q5: Does a 4-worker mixed-kind wave run clean today (grid, reports, cleanup)? | type: task | by: AFK | blocked_by: none | status: decided → 4 mixed-kind workers (omp, claude, agy, claude) ran to COMPLETE; frictions were topology gate requiring --solo for 3rd+ worker and agy taking ~1h | evidence: raw/2026-11-11-quaqua-q*.md, memory 2026-11-11
- Q6: How can quaqua measure usage for agy workers (log parsing, API, or a time-based cap)? | type: research | by: AFK | blocked_by: none | status: decided → log parsing via ~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript.jsonl mapped by conversation_summaries.db; time-based cap remains the runaway backstop | evidence: raw/2026-11-11-quaqua-q6agyusage.md @ e77b22c
- Q7: Which harnesses lack stall/timeout detection in the watchdog? | type: research | by: AFK | blocked_by: none | status: decided → None: all 5 harnesses (claude, opencode, omp, pi, agy) get stall, idle, budget and hard-cap detection; the per-harness gaps are elsewhere — agy has no cost tracking and no provider-error classification (exit 7), and claude/pi have no stop keys, so on_cost_cap=kill cannot interrupt them | evidence: raw/2026-11-11-quaqua-q7watchdog.md @ e77b22c
- Q8: Why does the cos-memory Stop counter persist stops:0 in live -p sessions, and how is the mod loaded into the real CoS session? | type: research | by: AFK | blocked_by: Q3 | status: open
- Q9: What error text does an agy pane print on a provider error, so retry.rs can classify it for exit 7? | type: research | by: AFK | blocked_by: Q7 | status: open
- Q10: Should kinds on claude/pi be refused on_cost_cap="kill" at config load, since no interrupt is possible (stop.rs:78 refuses at run time)? | type: grill | by: HITL | blocked_by: Q7 | status: open
- fog: when work spans days, should a long-lived quaqua-rs Lead own MVP execution instead of CoS?

## Phases and Verification
- approach: not-planned

## Log
- 2026-11-11 — map opened from the Principal's destination; 8 lines, no decisions yet.

## Validation

## Current State and Next Action
- active_phase: none
- lifecycle_status: planned
- blockers: none
- open_items: 4 open of 10 map lines (Q4, Q8, Q9, Q10)
- exact_next_action: wayfinder
