---
status: active
lane: normal
---
# CoS persistent memory and learning loop

## Goal
- outcome: the CoS session persists what matters and learns from its runs, Hermes-style, with deterministic scripts/hooks first and LLM only where judgement is irreducible; this lock delivers slice G5 (hard memory caps + secret gate), later slices land via refine.
- success_signal: an over-cap or secret-bearing write to memory/rules.md or USER.md is blocked (Write/Edit) or flagged in the same turn (Bash) in a `quaqua init` workspace.
- actors: anh iu, CoS (Quả Quả), Auditor.
- authority: anh iu 2026-11-11 (wayfinder Q4, Q5; "lock rồi spawn worker G5")
- requirements:
  - R1: `quaqua memory check [--workspace DIR]` counts Unicode chars (not bytes) of memory/rules.md (cap 2200) and USER.md (cap 1375), and scans both for secrets (API keys, tokens, private keys); exit 0 clean, non-zero listing each violation | acceptance: `cargo test -p quaqua memory_check` | source: Q3 G5, Q4
  - R2: `quaqua init` installs hooks: PreToolUse Write|Edit|MultiEdit on those files simulates the post-edit content and denies over-cap/secret writes; PostToolBatch runs `quaqua memory check` and on violation returns additionalContext naming the file, size and cap (catches Bash/sed writes) | acceptance: `cargo test -p quaqua memory_hooks` | source: Q4, advisor 2026-11-11
  - R3: caps are constants in one place, never raised by config | acceptance: `cargo test -p quaqua memory_check` | source: AGENTS.md Memory
- non-goals:
  - NG1: G1 nudge, G3 transcript recall, G4 compact steer, correction capture, the CoS mod (`$.tool.register`, `$.model.fork`) — later phases via refine
  - NG2: changing worker settings or caps of rules/ files

## Map
- Q1: How do Hermes Agent's memory tiers, skill self-creation, session search and subagent delegation work? | type: research | by: AFK | blocked_by: none | status: decided → 4 layers: MEMORY.md 2200 + USER.md 1375 frozen into system prompt at session start, written only via a memory tool that errors over cap (agent consolidates); procedural skills in ~/.hermes/skills (progressive disclosure, created after ≥5 tool calls / error recovery / user correction, patched in use); SQLite+FTS5 session archive searched on demand with LLM-summarized hits; optional Honcho user model. Loop driver = periodic nudge: a background review fork (shares prompt cache) every N turns decides what to persist; pre-compaction pass extracts memory before summarizing, lineage kept in SQLite; subagents = isolated delegation tool | evidence: hermes-agent.nousresearch.com/docs/user-guide/features/memory, mranand.substack.com/p/inside-hermes-agent-how-a-self-improving (2026-11-11)
- Q2: How do other open-source agents (Letta/MemGPT, mem0, OpenHands/Claude Code memory, Zep/Graphiti, others) handle persistent memory, learning and orchestration? | type: research | by: AFK | blocked_by: none | status: decided → Letta: agent edits its own resident memory blocks + archival paging (same shape as Hermes); Mem0: automatic extraction to vector+graph, lossy, per-user; Zep/Graphiti: temporal facts with validity windows (fact succession, no contradiction); Hindsight: banks of facts/experiences/observations for learning from past runs. Fit for quaqua (one CoS, files+SQLite, no vector infra): Hermes/Letta agent-curated files + FTS search; Zep's validity idea maps to project `## Now` | evidence: coworker.ai/blog/mem0-vs-zep-vs-letta (2026-11-11)
- Q3: What does quaqua already do for CoS memory, context, learning and orchestration, and where are the gaps vs Q1/Q2? | type: research | by: AFK | blocked_by: none | status: decided → Has: rules.md 2200 / USER.md 1375 caps (Hermes numbers, convention only), dated memory log, project `## Now`, immutable raw/, lessons promoted by hand at cleanup, hourly Auditor (expired rules, recurring cases), bus triggers + `quaqua recall`, compact hook = re-read list, workers via herdr (Agent tool denied). Gaps: G1 no periodic nudge / background review; G2 no skill/rule auto-draft from runs; G3 no search over past CoS transcripts (~/.claude/projects/*.jsonl); G4 no pre-compaction memory flush; G5 caps not enforced by a tool | evidence: ~/quaqua/.claude/settings.json hooks, memory/rules.md, crates/quaqua/src/auditor.rs (2026-11-11)
- Q4: Which destination and first slice does anh iu pick (memory, context, learning, orchestration)? | type: grill | by: HITL | blocked_by: Q1, Q2, Q3 | status: decided → port all Hermes/pi-hermes-memory mechanisms (G1–G5) into Claude Code hooks + quaqua engine; deterministic script first, LLM only where judgement is irreducible (poteto/pstack) | evidence: user 2026-11-11
- Q5: Should the CoS get a sub-session (Hermes-style review fork or a read-only herdr helper) for background review, given the Agent tool is denied? | type: grill | by: HITL | blocked_by: Q4 | status: decided → Claude Code mod: `$.model.fork(effort:low)` review fork + `$.tool.register` capped memory tools; hooks/scripts for counting, gating, compact steer, recall | evidence: user 2026-11-11

## Phases and Verification
- approach: Rust subcommand + init-installed command hooks (deterministic); rejected: prompt/agent hooks (LLM) and Write-only gate (Bash bypass)
- phase_slug: `main`
  status: planned
  - goal: R1, R2, R3 | depends_on: none
  - surfaces: crates/quaqua/src/ (new memory module, init.rs, main.rs CLI), crates/quaqua/tests/ | avoided: gate/, grid/, pipeline/, worker/, merge/
  - phase check: `cargo test -p quaqua memory_check` `cargo test -p quaqua memory_hooks` `cargo test`

## Log
- 2026-11-11 advisor review → fixes: G5 must also catch Bash writes (verify post-hoc on PostToolBatch/Stop, chars not bytes, or one quaqua write path); G1 fires only if memory/## Now unchanged since reset, guard `stop_hook_active`, coexist with cos-inbox --stop, Stop block cap 8; G4: PreCompact stdout = compaction-summary instructions (deterministic steer), extend existing compact re-read hook.
- 2026-11-11 mods (Claude Code plugin API 2.1.322) give more than hooks: `$.tool.register` (true Hermes memory_add/replace/remove tool with hard cap + secret scan), `prompt.section` (frozen memory snapshot, cache-safe), `session.compact`/`session.append` hooks, `$.model.fork({effort:'low'})` (review fork reusing prompt cache), `$.store` across sessions, `$.ui.status` (show memory % used). CoS-only (Claude Code); workers unaffected.
- 2026-11-11 split (script vs LLM), shipped by `quaqua init` hooks (crates/quaqua/src/init.rs):
  - G5 cap+secret gate: PreToolUse Write|Edit on memory/rules.md, USER.md, rules/** → compute post-edit chars, deny over cap or on secret regex. Pure script.
  - G4 compact: PreCompact script writes state snapshot (active plans, INDEX Now, worktrees, today's memory tail, last N user msgs from transcript_path) to memory/.compact/<sid>.md; SessionStart(compact) injects it via additionalContext. Pure script.
  - G1 nudge: Stop/PostToolUse counter (10 turns or 15 tools) → Stop decision:block with checklist "rewrite ## Now / log outcome"; main CoS writes. Counting is script; writing is the CoS turn.
  - Correction capture: UserPromptSubmit regex (vi+en: "không phải", "sai rồi", "no, use", "đừng") → append verbatim to memory/corrections.md queue. Script; promotion stays CoS at cleanup.
  - G3 recall: `quaqua recall --transcripts` SQLite FTS5 (trigram) over ~/.claude/projects/*.jsonl, anchors mode (path:line). Rust, no LLM.
  - G2 learning: Auditor already counts [case] recurrences deterministically; LLM only drafts the rule wording.
  - Claude Code hook facts: events PreCompact(trigger,custom_instructions)/PostCompact/SessionStart(source=compact)/Stop/UserPromptSubmit/PreToolUse; additionalContext ≤10k chars; Stop supports decision:block; async/asyncRewake for background. src: code.claude.com/docs/en/hooks
- 2026-11-11 research: pi-hermes-memory (github.com/chandra447/pi-hermes-memory, 504★) = Hermes port for Pi. Steal: STANDING.md pins (user-only, 2000c, always injected), policy-only + memory_search, flushOnCompact, nudge 10 turns/15 tools via side-channel LLM w/ memory-only tools, correction detection, failure category, secret scan, usage hit counts. Lesson: prohibitions must be injected, not recalled.

## Validation

## Current State and Next Action
- active_phase: none
- lifecycle_status: locked
- blockers: none
- open_items: none
- exact_next_action: work full phase main
