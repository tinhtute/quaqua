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
  - R8: on PreCompact the script writes `memory/.compact/<session_id>.md` (≤8 KiB) holding the `## Current State and Next Action
- active_phase: none
- lifecycle_status: implemented (all phases merged to main; not validated)
- blockers: none
- open_items: 3 — (1) independent full validation of phases transcripts/compact/mod (none recorded); (2) R12 review fork never fired live (needs 10 turns / 15 tool calls in a real CoS session); (3) agy reports no usage, so cost caps never fire for kind `sude`
- exact_next_action: spawn a `suhuynh` worker for `check` full mode on main `004e614` against R4–R14, writing one `## Validation` entry per phase with `judge: independent`; then run handoff step 5–6 to close the plan
