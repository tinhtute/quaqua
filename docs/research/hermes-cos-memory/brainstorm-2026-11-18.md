# CoS memory and instructions, Hermes style: round 3 (2026-11-18)

Read-only design round after PR #286 (SessionStart standing block) landed. Decisions by the Principal are marked.

## Gap table: Hermes vs the quaqua CoS today

| Hermes (src @f7e2898) | quaqua CoS today | Gap |
|---|---|---|
| SOUL in the stable tier (sp:782) | identity + SOUL in the block (`memory/standing.rs`) | none; AGENTS.md still repeats identity |
| MEMORY 2200 chars with a `[NN% — x/y]` header (ms:675-681) | `memory/rules.md` at 2139/2200 chars (97%), no fill shown in the block | the CoS learns it is full only when a write is refused |
| USER 1375, agent-writable (mt:321-336) | `USER.md` was the unfilled template since 2026-10-15, so the block skipped it | now filled (owner-approved, no project detail) |
| add / patch / remove + batch; over-cap returns current entries (ms:332-340) | mod tools add / replace / remove; no batch; deny returns a reason only (`mods/cos-memory/src/tools.js`) | no one-shot consolidation loop |
| session_search for history | `quaqua transcripts search` exists; the block never points to it | history is read by tailing dated journals |
| workspace snapshot / env probe in the context tier (sp:804-817) | none; session-start duties are prose in AGENTS.md `## Memory` | 2026-11-18: an audit sat unhandled until a manual recap; a lead and peer idled 6 h unseen |
| prohibitions enforced in code (approval.py:1308) | `authority/gate.rs`; E-lite queued | covered |

## Variants

- **V1 memory parity.** Fill header on capped sections; batch write checked on the final state; a denied write returns current content.
- **V2 computed situation.** One engine function computes unhandled audits, live agents with idle time, pending template conflicts and memory fill. It feeds a CLI subcommand and a `## Now (computed)` section at the end of the block (≤600 chars, each line names its action). The prose duties it replaces are deleted.
- **V3 journal as recall.** The block points to transcript search; the engine writes outcome lines from run reports. Largest cost, fixes none of the observed misses.

## Decisions (Principal)

- V2 + V1 as one slice; V3 later.
- The CoS drafts USER.md, the Principal approves. USER.md holds no project detail and no jargon-heavy rules.
- The computed section is frozen on resume, as Hermes keeps stored bytes.
- Principle recorded in USER.md: prefer reusable deterministic tools. One tested interface performs the known steps; the agent spends judgment on what to inspect and what to change. Research ends in a tool or harness change, not in instructions alone.

## Weakest assumption

A snapshot frozen at session start only shows state at that moment. A worker that stalls mid-session still needs the watchdog and liveness checks; the block cannot catch it.

## Next

ctx-trim P7 (plan in this folder): builder `sude-situation`, one review, PR.
