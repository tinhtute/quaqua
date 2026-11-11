---
status: active
lane: high-risk
---
# CoS persistent memory and learning loop

## Goal
- outcome: the CoS session persists what matters and learns from its runs, Hermes-style, with deterministic scripts/hooks first and LLM only where judgement is irreducible; G5 (hard memory caps + secret gate) is shipped, and this lock adds G3 transcript recall, G4 pre-compaction flush, and the Claude Code mod (G1 review fork, capped memory tools, G2 correction capture).
- success_signal: (a) an over-cap or secret-bearing write to memory/rules.md or USER.md is blocked (Write/Edit) or flagged in the same turn (Bash) in a `quaqua init` workspace; (b) `quaqua transcripts search` answers a question about a past CoS session with `path:line` anchors in under 1s on the live corpus; (c) after a compaction in a `quaqua init` workspace, `memory/.compact/<sid>.md` exists and the compacted session still names its open items and exact next action; (d) the mod's memory tool refuses an over-cap write with the engine's own reason string and no cap number of its own.
- actors: anh iu, CoS (Quả Quả), Auditor.
- authority: anh iu 2026-11-11 (wayfinder Q4, Q5; "lock rồi spawn worker G5"); refine 2026-11-11 (cosmem-lead brief: G3/G4/mod fan-out after G5 landed)
- requirements:
  - R1: `quaqua memory check [--workspace DIR]` counts Unicode chars (not bytes) of memory/rules.md (cap 2200) and USER.md (cap 1375), and scans both for secrets (API keys, tokens, private keys); exit 0 clean, non-zero listing each violation | acceptance: `cargo test -p quaqua memory_check` | source: Q3 G5, Q4
  - R2: `quaqua init` installs hooks: PreToolUse Write|Edit|MultiEdit on those files simulates the post-edit content and denies over-cap/secret writes; PostToolBatch runs `quaqua memory check` and on violation returns additionalContext naming the file, size and cap (catches Bash/sed writes) | acceptance: `cargo test -p quaqua memory_hooks` | source: Q4, advisor 2026-11-11
  - R3: caps are constants in one place, never raised by config | acceptance: `cargo test -p quaqua memory_check` | source: AGENTS.md Memory
  - R4: `quaqua transcripts index [--projects DIR] [--db FILE]` reads `~/.claude/projects/*/*.jsonl` and writes `runs/transcripts.db` (FTS5 table `messages`: session, project, cwd, ts, role, text); it is incremental — a file whose (mtime, size) is unchanged is skipped, so a second run over an unchanged corpus inserts no rows. It never writes the bus DB. | acceptance: `cargo test -p quaqua transcripts_index` | source: Q1 session archive, Q3 G3
  - R5: `quaqua transcripts search <query> [--limit N] [--project DIR] [--since YYYY-MM-DD]` prints one line per hit, `path:line  <ts> <role>  <excerpt ≤200 chars>`, ranked by bm25; no hit exits 0 with `no matches`; a missing index is built once before the first search. | acceptance: `cargo test -p quaqua transcripts_search` | source: Q1, Q3 G3
  - R6: `quaqua transcripts tail --session <id|path> --last N` prints the last N user messages verbatim, one per line, skipping tool results and sidechain entries; an unknown session exits 1 with empty stdout. | acceptance: `cargo test -p quaqua transcripts_tail` | source: G4 snapshot input
  - R7: `quaqua init` installs a `PreCompact` handler and a `SessionStart` handler running `scripts/hooks/memory-compact.sh`, idempotently (a second run writes nothing), `--dry-run` prints without writing, and a settings file that is not a JSON object is left untouched. | acceptance: `cargo test -p quaqua compact_hook` | source: Q1 pre-compaction pass, advisor 2026-11-11
  - R8: on PreCompact the script writes `memory/.compact/<session_id>.md` (≤8 KiB) holding the `## Current State and Next Action` block of every `.harness/plans/active/*.md`, the non-done rows of `projects/INDEX.md`, `git worktree list`, the last 10 lines of today's `memory/<date>.md`, and the last 5 user messages from the hook's `transcript_path` via `quaqua transcripts tail`; a missing input shortens the snapshot instead of failing, and nothing outside `memory/.compact/` is written. | acceptance: `bash tests/test_memory_compact.sh` | source: Q1, Log 2026-11-11 G4
  - R9: the same run prints the compaction steer on stdout (≤2000 chars) naming the snapshot path and the must-keep items (decisions, open items, exact next action, correction lines); it prints nothing in `--session-start` mode. | acceptance: `bash tests/test_memory_compact.sh` | source: advisor 2026-11-11 (PreCompact stdout = summary instructions)
  - R10: on `SessionStart(source=compact)` the script injects the newest snapshot as `additionalContext` (truncated at 9000 chars with a marker), and emits nothing when `memory/.compact/` is empty. | acceptance: `bash tests/test_memory_compact.sh` | source: Q1 lineage, Log 2026-11-11 G4
  - R11: the mod's `$.tool.register` exposes `memory_add`, `memory_replace`, `memory_remove` for `memory/rules.md` and `USER.md`; each proposed content is validated by piping `{"tool_name":"Write","tool_input":{"file_path":…,"content":…}}` to `quaqua memory hook pre`, and a `permissionDecision: deny` is returned as the tool error verbatim; no cap number, secret pattern or char count appears in the mod's source. | acceptance: `node --test mods/cos-memory/tests/` | source: Q5, R3
  - R12: after 10 Stop events or 15 tool calls since the last review, and only when `memory/rules.md` and today's `memory/<date>.md` are unchanged since then, the mod runs `$.model.fork({effort:'low'})` with only the memory tools; it never fires while `stop_hook_active` is true, and at most once per 10 turns. | acceptance: `node --test mods/cos-memory/tests/` | source: Q1 periodic nudge, advisor 2026-11-11
  - R13: a UserPromptSubmit matching a vi/en correction pattern (`không phải`, `sai rồi`, `đừng`, `no, use`, `not that`) appends one verbatim line `- <YYYY-MM-DD> <project> <text>` to `memory/corrections.md`, skips an identical consecutive line, and never blocks the prompt (exit 0, no stdout). | acceptance: `node --test mods/cos-memory/tests/` | source: Log 2026-11-11 correction capture
  - R14: the mod is a local marketplace at `mods/` (`mods/.claude-plugin/marketplace.json` plus `mods/cos-memory/.claude-plugin/plugin.json`), installable with `/plugin marketplace add ~/Lab/quaqua/mods`; it declares no permission changes and no network access, and `quaqua init` does not change because of it. | acceptance: `bash mods/cos-memory/tests/install_smoke.sh` | source: Q5 (CoS-only, Claude Code)
- non-goals:
  - NG1: Honcho-style user model, vector/graph memory, automatic extraction and any memory daemon (Q2 rejected for one CoS on files + SQLite).
  - NG2: changing worker settings or caps of rules/ files

## Map
- Q1: How do Hermes Agent's memory tiers, skill self-creation, session search and subagent delegation work? | type: research | by: AFK | blocked_by: none | status: decided → 4 layers: MEMORY.md 2200 + USER.md 1375 frozen into system prompt at session start, written only via a memory tool that errors over cap (agent consolidates); procedural skills in ~/.hermes/skills (progressive disclosure, created after ≥5 tool calls / error recovery / user correction, patched in use); SQLite+FTS5 session archive searched on demand with LLM-summarized hits; optional Honcho user model. Loop driver = periodic nudge: a background review fork (shares prompt cache) every N turns decides what to persist; pre-compaction pass extracts memory before summarizing, lineage kept in SQLite; subagents = isolated delegation tool | evidence: hermes-agent.nousresearch.com/docs/user-guide/features/memory, mranand.substack.com/p/inside-hermes-agent-how-a-self-improving (2026-11-11)
- Q2: How do other open-source agents (Letta/MemGPT, mem0, OpenHands/Claude Code memory, Zep/Graphiti, others) handle persistent memory, learning and orchestration? | type: research | by: AFK | blocked_by: none | status: decided → Letta: agent edits its own resident memory blocks + archival paging (same shape as Hermes); Mem0: automatic extraction to vector+graph, lossy, per-user; Zep/Graphiti: temporal facts with validity windows (fact succession, no contradiction); Hindsight: banks of facts/experiences/observations for learning from past runs. Fit for quaqua (one CoS, files+SQLite, no vector infra): Hermes/Letta agent-curated files + FTS search; Zep's validity idea maps to project `## Now` | evidence: coworker.ai/blog/mem0-vs-zep-vs-letta (2026-11-11)
- Q3: What does quaqua already do for CoS memory, context, learning and orchestration, and where are the gaps vs Q1/Q2? | type: research | by: AFK | blocked_by: none | status: decided → Has: rules.md 2200 / USER.md 1375 caps (Hermes numbers, convention only), dated memory log, project `## Now`, immutable raw/, lessons promoted by hand at cleanup, hourly Auditor (expired rules, recurring cases), bus triggers + `quaqua recall`, compact hook = re-read list, workers via herdr (Agent tool denied). Gaps: G1 no periodic nudge / background review; G2 no skill/rule auto-draft from runs; G3 no search over past CoS transcripts (~/.claude/projects/*.jsonl); G4 no pre-compaction memory flush; G5 caps not enforced by a tool | evidence: ~/quaqua/.claude/settings.json hooks, memory/rules.md, crates/quaqua/src/auditor.rs (2026-11-11)
- Q4: Which destination and first slice does anh iu pick (memory, context, learning, orchestration)? | type: grill | by: HITL | blocked_by: Q1, Q2, Q3 | status: decided → port all Hermes/pi-hermes-memory mechanisms (G1–G5) into Claude Code hooks + quaqua engine; deterministic script first, LLM only where judgement is irreducible (poteto/pstack) | evidence: user 2026-11-11
- Q5: Should the CoS get a sub-session (Hermes-style review fork or a read-only herdr helper) for background review, given the Agent tool is denied? | type: grill | by: HITL | blocked_by: Q4 | status: decided → Claude Code mod: `$.model.fork(effort:low)` review fork + `$.tool.register` capped memory tools; hooks/scripts for counting, gating, compact steer, recall | evidence: user 2026-11-11
- Q6: Does G3 extend the shipped `quaqua recall <id>` or add a subcommand? | type: think | by: AFK | blocked_by: Q3 | status: decided → a new `quaqua transcripts` command family (`index`, `search`, `tail`). `quaqua recall <id>` is the bus trigger→thread→ref reader (`crates/quaqua/src/bus/recall.rs`, `main.rs:1162`); overloading it with a transcript corpus would give one verb two unrelated outputs and break its exit-code contract. | evidence: crates/quaqua/src/bus/recall.rs, crates/quaqua/src/main.rs (2026-11-11)
- Q7: How does the mod reach the G5 caps without duplicating them? | type: think | by: AFK | blocked_by: Q5 | status: decided → CLI, through the shipped hook entry: the mod pipes a synthetic `PreToolUse` JSON to `quaqua memory hook pre` and returns its `permissionDecisionReason` verbatim. `memory::TARGETS` is the only place a cap is written (R3); no constant is exported to JS and none is re-typed there. | evidence: crates/quaqua/src/memory.rs:33-47 (`TARGETS`), `:148` (`hook_pre`) (2026-11-11)

## Phases and Verification
- approach: Rust subcommand + init-installed command hooks (deterministic); rejected: prompt/agent hooks (LLM) and Write-only gate (Bash bypass). The refine keeps that split and adds one rule: a phase owns its files outright, so no two phases ever write the same file.
- file ownership (one writer per file; no shared file between the three phases):
  - `crates/quaqua/src/main.rs`, `crates/quaqua/src/lib.rs`: `transcripts` only (one `Command::Transcripts` block, one `pub mod transcripts;` line). `compact` and `mod` never edit them.
  - `crates/quaqua/src/init.rs`, `templates/workspace/.claude/settings.json`: `compact` only.
  - `crates/quaqua/src/memory.rs`: no phase edits it. The mod reuses the shipped `quaqua memory hook pre` entry (Q7), so the caps stay in one file.
  - `mods/`: `mod` only.
  - Order: `transcripts` forks from integ first; `compact` forks after `transcripts` merges (its script calls `quaqua transcripts tail`); `mod` runs in parallel with both because it touches no Rust file.
- delivery: the same cadence ends every phase — the phase's worker commits on `quaqua/<name>` off `quaqua/cosmem-integ` → `suhuynh` reviews the diff → the Lead accepts the branch into integ → anh iu approves the integ→main PR → the binary is installed from merged `main`. Live steps (`quaqua init --update` in `~/quaqua`, installing the mod, the first real compaction) come after that install.
- risks:
  - the transcript corpus is large (one live session is 29k JSONL lines, many project dirs) → the index is incremental by (mtime, size) and lives in its own `runs/transcripts.db`; measure the first full build on the live corpus before accepting, and never write the bus DB.
  - the compact snapshot copies private content into a file that SessionStart re-injects → the snapshot stays inside `memory/` (already private), is capped at 8 KiB and injects at most 9000 chars; it carries paths, not file bodies.
  - the review fork spends tokens every 10 turns → `effort: low`, memory tools only, no file reads, counter reset on every memory write; the kill switch is disabling the plugin.
  - a PreCompact hook that fails must never block compaction → the script always exits 0 and degrades to a shorter snapshot.
  - `$.model.fork` / `$.tool.register` are asserted for Claude Code 2.1.322 from the 2026-11-11 Log entry, not from an installed schema → the mod phase opens with a spike that records the real signatures; no mod code is written before it passes.
- constraints: one writer per file; no phase touches `gate/`, `grid/`, `pipeline/`, `worker/`, `merge/`; targeted tests only (`cargo test -p quaqua <filter>`, `node --test mods/cos-memory/tests/`, `bash tests/test_memory_compact.sh`), foreground; no push.
- phase_slug: `main`
  status: done (3b7d685 · PR #233)
  - goal: R1, R2, R3 | depends_on: none
  - surfaces: crates/quaqua/src/ (new memory module, init.rs, main.rs CLI), crates/quaqua/tests/ | avoided: gate/, grid/, pipeline/, worker/, merge/
  - phase check: `cargo test -p quaqua memory_check` `cargo test -p quaqua memory_hooks` `cargo test`
- phase_slug: `transcripts`
  status: planned
  - goal: R4, R5, R6 | depends_on: none
  - surfaces: new `crates/quaqua/src/transcripts/`, `crates/quaqua/src/main.rs` (one `Transcripts` command block), `crates/quaqua/src/lib.rs` (one `pub mod` line), new `crates/quaqua/tests/transcripts.rs` | avoided: gate/, grid/, pipeline/, worker/, merge/, `memory.rs`, `init.rs`, `templates/`
  - escalate_when: the bundled SQLite has no FTS5 or no trigram tokenizer and the fallback (`unicode61` + `LIKE`) cannot meet R5
  - wave 1:
    - T1 probe first, then build: `CREATE VIRTUAL TABLE … USING fts5(text, tokenize='trigram')` under the pinned `rusqlite 0.32.1` (`bundled`), recorded as a test that fails if the tokenizer is missing; then `index`/`search`/`tail` in `transcripts/` (owner: tieude worker, writes the four paths above only) — output: commit — check: `cargo test -p quaqua transcripts` — stop_if: the probe fails and the fallback cannot rank a known live query
  - review: suhuynh diff review → Lead accepts into `quaqua/cosmem-integ` → anh iu approves the integ→main PR → install the binary
  - phase check: `cargo test -p quaqua transcripts`
- phase_slug: `compact`
  status: planned
  - goal: R7, R8, R9, R10 | depends_on: transcripts (the script calls `quaqua transcripts tail`; branch from integ after it merges)
  - surfaces: new `templates/workspace/scripts/hooks/memory-compact.sh`, `templates/workspace/.claude/settings.json` (PreCompact + SessionStart entries), `crates/quaqua/src/init.rs` (`install_memory_hooks` gains the two entries), new `crates/quaqua/tests/compact_hook.rs`, new `tests/test_memory_compact.sh` | avoided: gate/, grid/, pipeline/, worker/, merge/, `main.rs`, `lib.rs`, `memory.rs`
  - escalate_when: the PreCompact event JSON carries no `transcript_path` or no `session_id` in the installed Claude Code, or a second SessionStart handler cannot coexist with `herdr-gate.sh --session-start`
  - wave 1:
    - T1 script + init wiring: `memory-compact.sh` with `--pre-compact` (default) and `--session-start`, the two `ensure_hook` calls in `init.rs`, and the template settings entries (owner: tieude worker, writes the five paths above only) — output: commit — check: `cargo test -p quaqua compact_hook` and `bash tests/test_memory_compact.sh` — stop_if: `ensure_hook` cannot express the SessionStart handler without touching the herdr-gate entry
  - review: suhuynh diff review → Lead accepts into `quaqua/cosmem-integ` → anh iu approves the integ→main PR → install the binary, then `quaqua init --update` in `~/quaqua`
  - phase check: `cargo test -p quaqua compact_hook` `bash tests/test_memory_compact.sh`
- phase_slug: `mod`
  status: planned
  - goal: R11, R12, R13, R14 | depends_on: none (reuses the shipped G5 gate; runs parallel with `transcripts` and `compact`)
  - surfaces: new `mods/` — `mods/.claude-plugin/marketplace.json`, `mods/cos-memory/.claude-plugin/plugin.json`, `mods/cos-memory/hooks/hooks.json`, `mods/cos-memory/src/*.mjs`, `mods/cos-memory/tests/*.test.mjs`, `mods/cos-memory/tests/install_smoke.sh`, `mods/cos-memory/README.md` | avoided: gate/, grid/, pipeline/, worker/, merge/, `crates/`, `templates/`
  - escalate_when: `$.model.fork` or `$.tool.register` is absent from the installed Claude Code, or the plugin API needs a version above 2.1.322
  - wave 1:
    - T1 spike: read the installed Claude Code plugin surface and record the exact signatures of `$.tool.register`, `$.model.fork`, `$.store` and the hook events in `mods/cos-memory/README.md`; no mod code before it passes (owner: sude worker, writes `mods/` only) — output: README section + the probe output — check: `node --test mods/cos-memory/tests/` (probe case) — stop_if: the fork or tool API is missing
    - T2 tools + install: the three memory tools over `quaqua memory hook pre`, the local marketplace manifests, `install_smoke.sh` (owner: same sude worker) — output: commit — check: `node --test mods/cos-memory/tests/` and `bash mods/cos-memory/tests/install_smoke.sh`
    - T3 review fork + corrections: the 10-turn/15-tool counter with the unchanged-memory guard and `stop_hook_active`, the `$.model.fork({effort:'low'})` call, the UserPromptSubmit correction hook (owner: same sude worker) — output: commit — check: `node --test mods/cos-memory/tests/`
  - review: suhuynh diff review → Lead accepts into `quaqua/cosmem-integ` → anh iu approves the integ→main PR → the CoS installs the marketplace from merged `main`
  - phase check: `node --test mods/cos-memory/tests/` `bash mods/cos-memory/tests/install_smoke.sh`

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
- 2026-11-11 refine (unlock → refine → relock), decisions:
  - G5 landed: main `3b7d685` (PR #233, commit `232fd77`); phase `main` closed. NG1 emptied of G1/G3/G4/correction capture/mod; those are now phases `mod`, `transcripts`, `compact`.
  - G3 is a new command family (`quaqua transcripts index|search|tail`), not an extension of `quaqua recall <id>` (Q6); the index is its own `runs/transcripts.db`, never the bus DB.
  - G4 is a script plus one `init` wiring edit, and it depends on G3 for `transcripts tail`; the JSONL parsing stays in Rust, so no phase duplicates it.
  - the mod reuses the caps through `quaqua memory hook pre` with a synthetic PreToolUse JSON (Q7); no constant is exported and none is re-typed in JS.
  - file ownership is disjoint by construction: only `transcripts` writes `main.rs`/`lib.rs`, only `compact` writes `init.rs`/the settings template, only `mod` writes `mods/`, and no phase writes `memory.rs`.
  - correction to the 2026-11-11 split entry above: `memory::TARGETS` is two files (`memory/rules.md`, `USER.md`), so the G5 gate does not cover `rules/**` writes; the mod's tools are scoped to the two capped files and claim nothing about `rules/`.
  - lane raised to `high-risk`: three phases, one new external API surface, and a live-corpus index build.

## Validation

- 2026-11-11T08:56:34Z — phase `main` — verdict: APPROVE_WITH_REQUESTS — mode: full (lane high-risk)
  - `cargo test --locked -p quaqua --test memory_check` — 10 passed, 0 failed
  - `cargo test --locked -p quaqua --test memory_hooks` — 17 passed, 0 failed
  - `cargo test --locked -p quaqua memory_hooks` — 2 passed (the init-install pair only; 15 filtered out)
  - scope: on target — caps `RULES_CAP` 2200 / `USER_CAP` 1375 live only in `crates/quaqua/src/memory.rs:27-29`; `grep -rnwE '2200|1375'` over `crates/quaqua/src`, `mods/cos-memory/src`, `mods/cos-memory/hooks`, `templates/workspace/scripts` finds nothing else
  - proof_classes: output, unit, integration
  - requirements: R1 met (`--test memory_check`: chars-not-bytes, per-file cap, secret, missing-dir cases) | R2 met (`--test memory_hooks` 17 cases; init installs PreToolUse `Write|Edit|MultiEdit` + PostToolBatch, `init.rs:867`) | R3 met (`no_env_var_raises_a_cap`, grep above)
  - requests: 1. [major] the acceptance command for R1/R3, `cargo test -p quaqua memory_check`, runs 0 tests (every binary reports `0 passed`): the filter matches test names, not the file; use `--test memory_check`. 2. [minor] the R2 acceptance filter `memory_hooks` covers 2 of 17 tests, none of the deny/batch behaviour; use `--test memory_hooks`. 3. [major, outside R1–R14] `cargo test --locked --workspace --no-fail-fast` on 004e614 is not green on this machine: with the real HOME 1186 passed / 3 failed (`worker::prompt::tests::test_prompt_{caps_retries_after_a_harness_exhausted_line,escalates_a_signed_out_turn_without_retry,retries_an_overloaded_turn_with_backoff_then_escalates}`, exit 5 ≠ 7) because they read the live workspace `team.toml` (kind `sude` → harness `agy`, classifier `retry.rs:298` returns None since #236); under `env -i` with a temp HOME those pass and `events_hook::test_events_hook_drops_foreign_cos_pane_to_prevent_cross_cos_inbox_leak` fails instead (`events_hook.rs:673`, Ok(true) ≠ Ok(false)). Both are test-isolation defects, not R regressions. 4. [minor] #235 (`41cf36d..8d68283`) edited `memory.rs` (+`memory scan`) and `main.rs`, which the file-ownership rule above says no phase edits; caps did not move.
  - rollback_point: 3b7d685
  - proof_gaps: PostToolBatch additionalContext on a real Bash write and the PreToolUse deny in a live `quaqua init` workspace were not exercised (tests only); `judge: independent` is testimony
  - judge: independent (runs/suhuynh-cmval.md)
  - judge_model: claude-opus-5-5
  - judged_sha: 004e614d19d82a39d44a9d102f90b2378e952f49

- 2026-11-11T08:56:34Z — phase `transcripts` — verdict: APPROVED — mode: full (lane high-risk)
  - `cargo test --locked -p quaqua --test transcripts` — 12 passed, 0 failed
  - `cargo test --locked -p quaqua transcripts_index` — 3 passed (incremental, default db under runs/, never writes the bus DB)
  - `cargo test --locked -p quaqua transcripts_search` — 7 passed
  - `cargo test --locked -p quaqua transcripts_tail` — 2 passed
  - live corpus (read-only, scratch `--db`, 891 MB / 609 files under the Claude projects dir): first `index` 58.6 s → 13851 rows; second `index` 0.29 s, `1/609 files, 608 unchanged` (the one changed file is this judge's own live session); `search "PreCompact snapshot" --limit 3` wall 0.00 s with `path:line  <ts> <role>  <excerpt>` lines
  - edge queries: `"unbalanced`, `NEAR(`, `*`, `a AND` return hits or `no matches`, exit 0 — FTS syntax is escaped, no SQL error; a leading `-x` is taken as a clap flag (needs `--`)
  - scope: on target
  - proof_classes: output, unit, integration
  - requirements: R4 met | R5 met (success signal (b) < 1 s on the live corpus) | R6 met
  - requests: none blocking. 1. [trivial] some hits print `-` for ts (e.g. compaction-summary user rows); allowed by the format but worth knowing.
  - rollback_point: 3b7d685
  - proof_gaps: the live first-build timing is a one-off measurement, not a re-runnable proof
  - judge: independent (runs/suhuynh-cmval.md)
  - judge_model: claude-opus-5-5
  - judged_sha: 004e614d19d82a39d44a9d102f90b2378e952f49

- 2026-11-11T08:56:34Z — phase `compact` — verdict: APPROVED — mode: full (lane high-risk)
  - `cargo test --locked -p quaqua --test compact_hook` — 5 passed (idempotent, dry-run, non-object settings untouched, beside herdr gate, template matches)
  - `bash tests/test_memory_compact.sh` — 7 ok lines, `PASS: memory-compact.sh` (writes only memory/.compact/, 496-char steer, snapshot ≤ 8192 B, additionalContext ≤ 9000, inject on source=compact only, degraded inputs exit 0)
  - scope: on target — `init.rs` and the settings template changed only in 185bb91 (this phase)
  - proof_classes: output, unit, integration
  - requirements: R7 met | R8 met | R9 met | R10 met
  - requests: none
  - rollback_point: 3b7d685
  - proof_gaps: no real Claude Code compaction ran; success signal (c) is unproven live until the first compaction after `quaqua init --update`
  - judge: independent (runs/suhuynh-cmval.md)
  - judge_model: claude-opus-5-5
  - judged_sha: 004e614d19d82a39d44a9d102f90b2378e952f49

- 2026-11-11T08:56:34Z — phase `mod` — verdict: APPROVE_WITH_REQUESTS — mode: full (lane high-risk)
  - `node --test mods/cos-memory/tests/` — 35 tests, 35 pass, 0 fail (node v26.13.0)
  - `bash mods/cos-memory/tests/install_smoke.sh` — 10 ok lines incl. `claude plugin validate` (marketplace, plugin) and `claude plugin test`, `all checks passed`
  - scope: on target — no `crates/` or `templates/` file in 3d06cf4/42d983c/ce05c34; `quaqua init` unchanged by the mod
  - proof_classes: output, unit, integration
  - requirements: R11 met (gate refusal returned verbatim; unparsable/failed gate = refusal; no cap literal in mod source per smoke) | R12 partial (see requests 1–2) | R13 met with a deliberate deviation: since #235 the line is screened by `quaqua memory scan` (`corrections.js:5-8`) — a secret-bearing correction is refused and a long prompt is cut, so "verbatim" holds only for clean text | R14 met
  - requests: 1. [major] R12 never fired in a live CoS session (needs 10 Stops / 15 tool calls + 10 user turns); only the counter/guard logic is unit-tested with a fake `$`. 2. [major] `$.model.fork` takes `{ prompt, effort? }` and cannot restrict tools (`review.js:1-5`); "with only the memory tools" is enforced by prompt plus a JSON-op schema filter scoped to the capped pair, not by the fork's toolset — the fork could still call tools it is told not to. 3. [minor] R12 counter adds a 5-min in-flight marker and a prompts≥10 spacing (`counter.js:13-19,84-86`) consistent with "at most once per 10 turns".
  - rollback_point: 3b7d685
  - proof_gaps: live fork behaviour, live `$.tool.register` tool calls in a CoS session, marketplace install via `/plugin marketplace add`
  - judge: independent (runs/suhuynh-cmval.md)
  - judge_model: claude-opus-5-5
  - judged_sha: 004e614d19d82a39d44a9d102f90b2378e952f49

## Current State and Next Action
- active_phase: none
- lifecycle_status: implemented (all phases merged to main; not validated)
- blockers: none
- open_items: 3 — (1) independent full validation of phases transcripts/compact/mod (none recorded); (2) R12 review fork never fired live (needs 10 turns / 15 tool calls in a real CoS session); (3) agy reports no usage, so cost caps never fire for kind `sude`
- exact_next_action: spawn a `suhuynh` worker for `check` full mode on main `004e614` against R4–R14, writing one `## Validation` entry per phase with `judge: independent`; then run handoff step 5–6 to close the plan
