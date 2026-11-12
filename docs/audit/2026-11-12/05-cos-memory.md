# Hermes-style memory and learning for the Chief of Staff

**Verdict: PARTIAL, 3/5.** Caps, the secret gate, transcript recall and the compaction snapshot are shipped and tested in the engine. The review fork and memory tools live in a mod that `quaqua init` does not install, CI does not test, and whose live firing is still an open question. There is no skill creation.

Audited at `41a5327`. `src/` means `crates/quaqua/src/`. `tpl/` means `templates/workspace/`.

## Scope

- "Tiered memory: small, capped core rules and an owner profile frozen at session start, plus dated logs and project cards."
- "Past sessions are searchable and recallable."
- "A background review fork learns from each session without blocking the main conversation."
- "The system writes its own rules and skills from lessons, at a controlled pace."
- "Secrets never enter memory; caps are hard limits."
- "Deterministic scripts and hooks come first; the LLM is used only where judgment is irreducible."

## Findings

| Requirement | Status | Evidence | Gap |
|---|---|---|---|
| Tiered memory: capped core rules and owner profile, dated logs, project cards | partial | Caps are constants: `memory/rules.md` 2200, `USER.md` 1375 Unicode chars (`src/memory.rs:27-29`, `:44`). Tiers are defined in `tpl/AGENTS.md:20-29`: rules, `USER.md`, dated `memory/YYYY-MM-DD.md`, project cards with `## Now`, immutable `raw/`. `quaqua init` seeds them (`templates/seed/`). `USER.md` is imported into the session prompt (`tpl/AGENTS.md:9-11`). | `memory/rules.md` is loaded by instruction ("Always read", `tpl/AGENTS.md:20`), not imported, so it is not frozen at session start by the harness. Dated logs and project cards have no cap and no engine check. |
| Past sessions searchable and recallable | met | `quaqua transcripts index\|search\|tail` over an FTS5 trigram index in `runs/transcripts.db`, incremental, ranked by bm25 (`src/transcripts/mod.rs:1-17`). `quaqua recall <id>` returns a trigger, its thread and ref (`src/bus/recall.rs:1-10`). Pre-compaction snapshot and re-injection (`tpl/.claude/settings.json:6-20`, `.harness/plans/done/cos-memory-loop.md:20-22`). Tests `transcripts` and `recall` passed in my run. | The corpus is Claude Code sessions only (`src/transcripts/mod.rs:3-5`). Recall is a command the CoS must choose to run; nothing prompts it when the owner asks "what did we decide about X". |
| Background review fork that does not block | partial | `mods/cos-memory/src/review.js:1-22` runs `$.model.fork` and accepts only `add`, `replace`, `remove` on the two capped files. Trigger and spacing rules are in `mods/cos-memory/README.md:130-145`. It fires on `turn.complete` (commit `2fdda74`). | The mod is not installed by `quaqua init` (`mods/cos-memory/README.md:32-33`). The fork cannot be given a restricted toolset; restriction is by prompt plus output filtering (`mods/cos-memory/README.md:123-129`), and whether that is acceptable is open (`.harness/plans/active/harness-mvp.md:20`). How the mod loads into the real CoS session and why its counter stayed at 0 live is open (`.harness/plans/active/harness-mvp.md:24`). No live firing is recorded in the repo. |
| Writes its own rules and skills from lessons, at a controlled pace | partial | Memory tools `memory_add`, `memory_replace`, `memory_remove` go through the engine gate (`mods/cos-memory/README.md:87-103`). Pace: at most one review per 10 user turns (`mods/cos-memory/README.md:134-137`). Correction capture appends one screened line (`CHANGELOG.md:14`). | Worker rules under `rules/` are promoted by hand (`tpl/AGENTS.md:24`). No skill is ever created or patched by the system: `grep -rn "skills/" crates/quaqua/src --include=*.rs` returns nothing, and the mod writes only the two memory files (`mods/cos-memory/src/review.js:18-19`). |
| Secrets never enter memory; caps are hard limits | partial | `PreToolUse` simulates the post-edit content and denies an over-cap or secret-bearing write (`src/memory.rs:9-12`). Secret patterns cover Anthropic, GitHub, Slack, AWS, bearer tokens and private keys (`src/memory.rs:378-391`, `:425`, tests `:565-576`). No config raises the caps (`src/memory.rs:17-18`). The mod treats a gate that cannot run as a refusal (`mods/cos-memory/README.md:102-103`). | Both engine hooks fail open (`src/memory.rs:18-20`). A `Bash` or `sed` write is flagged after it lands, not prevented (`src/memory.rs:13-15`). Only `memory/rules.md` and `USER.md` are gated; dated logs, project cards, `raw/` and `runs/` are not scanned. The scanner is prefix-based, so an unprefixed secret passes. |
| Deterministic scripts and hooks first | met | The caps, secret scan, transcript index, compaction snapshot and correction capture use no LLM (`src/transcripts/mod.rs:6`, `tpl/scripts/hooks/memory-compact.sh`). The plan states the rule (`.harness/plans/done/cos-memory-loop.md:9`). The LLM is used only in the review fork. | None. |

## Refs

- Plan and requirement list R1–R14: `.harness/plans/done/cos-memory-loop.md:10-23`. Independent validation commit: `9d75953`.
- Mod tests: `node --test mods/cos-memory/tests/` (32 tests per `mods/cos-memory/README.md:155`). I did not run them, and CI does not (`.github/workflows/test.yml` has no Node step).
- Engine tests `memory_hooks`, `transcripts`, `recall`, `compact_hook` passed in my `cargo test --locked --no-fail-fast` run.
- The live `memory/`, `USER.md` and transcript corpus were not available in this environment, so cap compliance, stored secrets and recall quality on real data are unverified.

## Fixes

1. Ship the review fork as part of the workspace: have `quaqua init` install or register the mod, and have `quaqua doctor --workspace` fail when it is not loaded. Record one live firing as evidence.
2. Close Q4 and Q8 in `.harness/plans/active/harness-mvp.md`. If the fork cannot be tool-restricted, run the review as a separate headless process with an explicit tool list.
3. Extend the secret scan to every file under `memory/`, `projects/`, `raw/` and `runs/`, on write and in the scheduled audit.
4. Make the caps hard for shell writes too: on a `PostToolBatch` violation, restore the last good copy instead of only flagging.
5. Add a rule and skill proposal path: the review fork may propose a `rules/` entry or a skill patch as a pending file, promoted by a worker task, with a weekly cap.
6. Add a recall hook: on a `UserPromptSubmit` that asks about a past decision, inject the top transcript hits so recall does not depend on the CoS thinking to search.
7. Run the mod's Node tests in CI.
