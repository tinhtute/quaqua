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
- Q1: Which steps from brief to merged PR still need a manual action today? | type: research | by: AFK | blocked_by: none | status: open
- Q2: Should the CLI open and merge PRs itself, or stop at push+PR with merge by the Principal? | type: grill | by: HITL | blocked_by: Q1 | status: open
- Q3: What session shape makes the G1 review fork fire live, and how is that recorded? | type: research | by: AFK | blocked_by: none | status: open
- Q4: Is a prompt-only tool restriction on the review fork acceptable, or must the engine enforce a tool allowlist? | type: grill | by: HITL | blocked_by: Q3 | status: open
- Q5: Does a 4-worker mixed-kind wave run clean today (grid, reports, cleanup)? | type: task | by: AFK | blocked_by: none | status: open
- Q6: How can quaqua measure usage for agy workers (log parsing, API, or a time-based cap)? | type: research | by: AFK | blocked_by: none | status: open
- Q7: Which harnesses lack stall/timeout detection in the watchdog? | type: research | by: AFK | blocked_by: none | status: open
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
- open_items: 8 map lines
- exact_next_action: wayfinder
