---
status: active
lane: high-risk
---
# CoS as a Hermes-style agent

## Goal
- outcome: the live CoS runs with full Hermes parity. The engine injects persona, USER, memory and INDEX as a standing block at every session start, plus a computed situation section with memory fill. Memory writes are batch writes checked against the cap. Prohibitions are enforced in code at the gate. Block sources get a threat scan at load. Instruction files have a write gate. AGENTS.md no longer carries the session-start duties as prose.
- success_signal: in a fresh CoS session on the canonical machine, the api-request-blob shows the standing block and `## Now (computed)` (fill header on each capped section) in the system role. AGENTS.md has no prose session-start duties. Tests pass for: cos-memory batch write (over-cap returns current content), the gate returns `ask` on each approval-list item (deny on force push and pushes to main), load-time `[BLOCKED]` on a poisoned `memory/rules.md` line, a worker write to USER.md or memory/rules.md refused, and an approval gate on agent writes to AGENTS.md / SOUL.md / identity.md. Running `qq` with the daemon down leaves it up.
- actors: anh iu (Principal), the CoS, the quaqua engine (crates/quaqua live, crates/qq-core rewrite), the workspace template
- authority: anh iu 2026-11-18 ("Hermes parity đầy đủ"); PR #287 docs/research/hermes-cos-memory/ (Hermes facts are cited from worker reports, not re-verified); PR #286 f56e515
- requirements (all code in ~/Lab/quaqua, qq-core and mods/cos-memory; run from there):
  - R1: The SessionStart hook injects the standing block (persona, USER, memory/rules.md, INDEX) plus `## Now (computed)` with a fill header (`used/cap`) on each capped section, for every source except a resume whose transcript already holds the block; on resume the computed section stays frozen | acceptance: `tests/qq-test.sh session_start` green; a fresh live CoS session's api-request-blob shows both in the system role | source: Q5, Q6
  - R2: `## Now (computed)` replaces the prose session-start duties (unhandled audits, today's memory file, worker/inbox state), and those duties are deleted from AGENTS.md and its template | acceptance: `grep -nE 'session start|on wake' AGENTS.md` lists only injection statements, no duty to perform | source: Q6
  - R3: cos-memory writes are batch writes checked against the cap (rules 2200, USER 1375) on the final state; an over-cap batch writes nothing and returns the current content | acceptance: `node --test mods/cos-memory/tests/` and `tests/qq-test.sh caps` green, with an over-cap case | source: Q6, Q10
  - R4: Only the CoS writes USER.md and memory/rules.md, through cos-memory, with no per-write approval; each write passes the strict threat scan and a drift guard (an outside edit since load → refuse and keep a `.bak`); a worker, Lead or reviewer write is refused | acceptance: `tests/qq-test.sh memory_write_owner drift` green with a worker-claim case and an external-edit case | source: Q10
  - R5: Agent writes to AGENTS.md, SOUL.md and identity.md go through an approval gate (PreToolUse `ask`) | acceptance: `tests/qq-test.sh instruction_file_gate` green for each of the three files | source: Q10
  - R6: qq-core ports the Hermes threat-pattern library: cumulative scopes all ⊂ context ⊂ strict, invisible-unicode check on the raw text, NFKC before the regexes, 64 KiB scan cap; load uses context scope and replaces a hit with `[BLOCKED: …]` in the injected copy only (file untouched); cos-memory writes use strict; the `screen_line` secret scan stays | acceptance: `tests/qq-test.sh threat_scan` green, including a poisoned memory/rules.md line rendered as `[BLOCKED` with the file bytes unchanged; the same scan over the five live standing files has 0 hits | source: Q11
  - R7: `authority/gate.rs` returns `ask` for every approval-list action with a tool-visible pattern (branch delete local and remote, tag and release, outward send or publish, money spend, deploy, data delete, history rewrite) on top of the push, merge and pane gates; force push and pushes to main stay `deny` | acceptance: `tests/qq-test.sh gate` green with one case per item | source: Q9
  - R8: The cos-inbox digest (UserPromptSubmit and Stop) also reports worker state changes since the last digest (stalled, idle, done, exited), from an engine worker-state event source; silent when nothing changed; the standing block is never re-rendered mid-session | acceptance: `tests/qq-test.sh inbox` green with a state-change case and a no-change case (empty output) | source: Q7
  - R9: The `qq` launch command checks `quaqua ping` and starts the daemon in the background when it is down; no launchd unit, nothing at login | acceptance: `quaqua daemon stop; qq; quaqua ping` reports the daemon up | source: Q13
- non-goals:
  - NG1: V3 journal-as-recall (anh iu deferred it, brainstorm-2026-11-18.md:26)
  - NG2: the opencode CoS path (no hook, no `@` expansion); the CoS runs on Claude Code only (anh iu 2026-11-18)
  - NG3: the small Hermes copies (memory_add steering text, head/tail truncation marker); scope open in issue #289
  - NG4: a Claude Code mod `prompt.*` injection path (skipped by cc-plugin-sec-default); open in issue #290
  - NG5: the `init --update` lock-base bug (AGENTS.md re-flagged every run); fixed outside this map: `init --update --accept <path>` on local branch quaqua/sude-initlock (ddaad0a, red 417c428; CoS verified red→green and scratch repro); PR #291 merged (cf61565)
  - NG6: changes in crates/quaqua (removed by T49 9aaa291; Q8)

## Map
- Q1: Which machine and workspace is the canonical CoS, and should ~/quaqua on this machine be synced to origin/main? | type: grill | by: HITL | blocked_by: none | status: decided → this machine is canonical; synced to origin/main f56e515 (binary rebuilt, `init --update` run) | evidence: anh iu 2026-11-18
- Q2: On the machine where #286 was reported "live synced", which binary answers `memory hook session-start`? | type: task | by: HITL | blocked_by: none | status: out-of-scope → moot: this machine is canonical, and since T49 (9aaa291) qq-core itself builds `quaqua`
- Q3: How does the template's `quaqua memory hook session-start` reach the qq-core verb? | type: grill | by: HITL | blocked_by: none | status: decided → no fix needed: T49 (9aaa291) removed crates/quaqua and qq-core builds `quaqua`; the earlier "broken wiring" finding came from a stale local checkout | evidence: `cargo install --path crates/qq-core` on f56e515, then the hook printed a 7481-char standing block (2026-11-18)
- Q4: How are the three pending template conflicts (AGENTS.md, models.md, team.toml) resolved? | type: grill | by: HITL | blocked_by: none | status: decided → roster follows upstream (tieude dropped, sude default at medium, dormant block dropped); AGENTS.md = upstream + the local ZHARNESS block; CoS applied it once by anh iu's ok (daemon down, no worker) | evidence: anh iu 2026-11-18; 3-way merge on md5-matched bases 1d59275 / 4debd0b / 85b4c59
- Q5: Does the standing block inject on the `clear` source on the current Claude Code (this session began with /clear and got no block, but the binary was still 2.6.0 then)? | type: research | by: AFK | blocked_by: none | status: decided → yes on the engine side: `session_start` renders for every non-empty source except a resume whose transcript already has the block (qq-core memory/mod.rs:147-149), and the live SessionStart hook has no matcher (all sources); the live-session proof is part of the success_signal | evidence: synthetic `source: clear` on a66f1a0 → 7481-char block (2026-11-18)
- Q6: P7 shape | type: grill | by: HITL | blocked_by: none | status: decided → V2 computed situation + V1 memory parity as one slice; computed section frozen on resume; prose duties it replaces are deleted | evidence: anh iu 2026-11-18, PR #287 brainstorm-2026-11-18.md:26-28
- Q7: Should the frozen-at-start situation get a mid-session refresh (e.g. the cos-inbox.sh UserPromptSubmit digest), since a frozen snapshot cannot catch a worker that stalls mid-session (brainstorm-2026-11-18.md:33)? | type: grill | by: HITL | blocked_by: none | status: decided → yes, as a delta: the cos-inbox digest (UserPromptSubmit + Stop) also reports worker state changes since the last digest (stalled, idle, done, exited), silent when none; it goes in the user turn, so the standing block stays frozen and cached; the engine needs a worker-state event source for it | evidence: anh iu 2026-11-18
- Q8: Does each parity item land in crates/quaqua or only in qq-core? | type: grill | by: HITL | blocked_by: none | status: decided → qq-core only; crates/quaqua no longer exists on main | evidence: 9aaa291
- Q12: The new binary needs the farm daemon. How does it start, and what does it resume from runs/qq.db on first start? | type: research | by: AFK | blocked_by: none | status: decided → it starts by hand today (`nohup quaqua daemon`); the engine ships no launchd unit (only docs mention launchd); first start resumed nothing (all qq.db tables empty after init) and `stop` says workflows resume at the next start; the macOS start bug (2ea6390 flock on qq.db) was fixed by PR #288 | evidence: a66f1a0, live daemon 2026-11-18, `quaqua` usage text
- Q13: Should the farm daemon start on its own (a launchd unit with KeepAlive, like the old auditor job) or stay manual, given that spawn, inbox and config all need it? | type: grill | by: HITL | blocked_by: none | status: decided → it starts on its own, but only from the `qq` launch command (the ~/.zshalias function that opens herdr, then claude), never at login: `qq` checks `quaqua ping` and starts the daemon in the background if it is down. No launchd unit. | evidence: anh iu 2026-11-18
- Q9: Which E-lite items does herdr-gate.sh enforce beyond push, merge and pane close (branch delete, deploy, release/tag, spend, external send)? Each item needs a tool-visible pattern. | type: grill | by: HITL | blocked_by: none | status: decided → block every approval-list action in code before it runs: branch delete (local and remote), tag and release, outward send or publish, money spend, deploy, and destructive acts (data delete, force push, history rewrite), on top of the existing push, merge and pane gates. Each needs a tool-visible pattern in qq-core `authority/gate.rs`. CoS default, anh iu may override: a hit returns the PreToolUse `ask` (approval popup), matching AGENTS.md "ask anh iu before these"; force push and pushes to main stay a hard deny. | evidence: anh iu 2026-11-18
- Q10: Does the instruction-file write gate cover USER.md? Hermes lets the agent write USER, while quaqua has the CoS draft and anh iu approve (brainstorm-2026-11-18.md:27). | type: grill | by: HITL | blocked_by: none | status: decided → like Hermes: only the CoS writes USER.md and memory/rules.md, through the cos-memory tool, with no per-write approval. Writes get the strict threat scan, the cap (1375/2200) on the final state, and a drift guard (an outside edit since load → refuse and keep a .bak, as Hermes `_detect_external_drift`). Workers, Leads and reviewers can never write them; Hermes children run with `skip_memory=True` and cannot call memory. AGENTS.md, SOUL.md and identity.md stay approval-gated (Hermes treats user-authored instruction files the same way: `agent_config_mod`). | evidence: anh iu 2026-11-18; hermes-agent tools/memory_tool.py and tools/delegate_tool.py (main, read 2026-11-18)
- Q11: What does the load-time threat scan reuse: `caps.rs::screen_line` as is, or a stricter scope like Hermes `scope="strict"` (hermes-source.md §3)? | type: research | by: AFK | blocked_by: none | status: decided → `screen_line` alone falls short: it checks secrets and the line cap only (caps.rs:128-133), with no injection patterns. Port the Hermes library to qq-core: cumulative scopes all ⊂ context ⊂ strict, invisible-unicode check on the raw text, NFKC before the regexes, 64 KiB scan cap. Strict scope gates cos-memory writes. Context scope at load replaces a hit with `[BLOCKED: …]` in the injected copy only. Keep the `screen_line` secret scan. | evidence: github.com/NousResearch/hermes-agent tools/threat_patterns.py (main, read 2026-11-18); the 9 patterns most likely to misfire, run over the five live standing files: 0 hits

## Phases and Verification
- approach: not-planned

## Log
- 2026-11-18 audit on this machine (CoS, read-only):
  - The installed `~/.cargo/bin/quaqua` is 2.6.0, built 2026-11-15. It rejects `memory hook session-start` ("unrecognized subcommand").
  - The live `.claude/settings.json` SessionStart has no `memory hook session-start` line (the origin/main template has one).
  - The local ~/Lab/quaqua main is 8196406, while origin/main is f56e515.
  - This session's first api-request-blob has no "SessionStart hook additional context".
  - Live sizes: rules.md 2193/2200, INDEX 3596, USER 295 (filled), AGENTS.md 9652.
  - The audits from 11-14-19 to 11-18-19 are all unhandled, and memory/2026-11-18.md did not exist. This is the prose-duty gap P7 targets, reproduced here.
- 2026-11-18 software-factory-rewrite.md parked to `.monows/plans/parked/` (a new directory) by anh iu's decision, so this map holds the one active slot.
- 2026-11-18 sync (anh iu: "máy này là canonical, sync lên origin/main"):
  - ~/Lab/quaqua fast-forwarded 8196406 → f56e515 (147 commits).
  - `cargo install --path crates/qq-core --locked --force` replaced quaqua 2.6.0. The old binary is kept in the session scratchpad only.
  - Backup of the touched workspace files: .quaqua/backup-2026-11-18-sync/.
  - `quaqua init . --update` exit 3: 17 managed files overwritten; conflicts AGENTS.md (95 lines), models.md (28), team.toml (57) are in .quaqua/pending/.
  - Checks: settings.json now has the session-start hook; a synthetic startup event rendered a 7481-char block (persona, USER, rules, INDEX). herdr-gate.sh and cos-inbox.sh exit 0. The daemon is not running.

- 2026-11-18 wayfinder resume: Q5, Q11 and Q12 decided by research; Q13 split out of Q12; no HITL grill this session (this session already decided Q1, Q4 and Q6).

- 2026-11-18 anh iu decided Q9, Q10 and Q13 in one message. Q10 was checked against Hermes source first. CoS default on Q9: `ask` popup, with force push and pushes to main as hard deny.

- 2026-11-18 anh iu: drop the opencode path, fix the other faults, file what is unclear as issues. Fog cleared: opencode → NG2; small Hermes copies → #289 (NG3); mod prompt.* → #290 (NG4); init lock-base bug → worker sude-initlock (NG5); parked plan got a dated staleness note. Q7 stays open as the next grill.
- 2026-11-18 brainstorm lock: R1–R9 derived from the decided map; lane stays high-risk (security gates, public hook contract, dependent steps).
- 2026-11-18 wayfinder: anh iu decided Q7 (delta via the cos-inbox digest). This /clear session got the standing block as SessionStart additional context (persona, USER, rules, INDEX). Map fully decided → brainstorm lock.

## Validation

## Current State and Next Action
- active_phase: none
- lifecycle_status: planned
- blockers: none (PR #288 merged a66f1a0; live daemon runs the main build)
- open_items: none
- exact_next_action: to-plan (after anh iu approves the lock)
