# ctx-trim v4: Hermes-style injected CoS context

Status: P1 + P2 DONE (2026-11-18). #286 merged f56e515, #285 closed, live synced, fresh-session check: 🍑 + the Principal. Next: E-lite plan (separate PR); follow-up: init lock keeps old AGENTS base, so every --update re-flags AGENTS.md as a conflict (no resolve command).
Answers already given and kept: poteto-mode as a skill index only; INDEX injected at start; a new PR supersedes #285.

Evidence:
- [hermes-source.md](hermes-source.md): Hermes source @f7e2898, with 6 design variants.
- [inject-probe.md](inject-probe.md): empirical probes on CC 2.1.327.
- [hermes-inject-round1.md](hermes-inject-round1.md): round 1, docs only.
- PR #285 report and PR #285 review (no copy here): PR #285 and its review.

Labels: [V] verified by CoS, [O] observed by a probe, [C] cited from source, [I] inferred.

## What Hermes really does (source, not docs)
- The system prompt runs stable → context → volatile. SOUL fills the identity slot. MEMORY and USER are a snapshot frozen at load and rebuilt only on compression or in a new session [C system_prompt.py:770-845].
- The memory tool has the actions add, patch and remove. `replace` was removed [V memory_tool.py:165].
- An over-cap write is rejected and returns the current entries. A threat scan runs on write and on load [C].
- **Prohibitions are enforced in code, not by injection.** `_floor_block` [V approval.py:1308] blocks them. A write gate protects agents.md, claude.md, soul.md and .cursorrules [C].
- There is no STANDING file in Hermes core [C].

## What the probes showed on our stack
- **Reach.** SessionStart additionalContext reaches the model on startup, resume and compact [O].
  - Compaction drops the earlier copies.
  - The compact-source copy re-injects the text.
  - Not tested: the `clear` source, and two resumes in a row with no compaction between.
- **Placement.** The text lands in the **system** role [O -p], [V interactive].
  - In `-p` it sits at the head of the system prompt.
  - In this interactive CoS session it sits near the tail of the system message, at about char 52k of 64k (this transcript's api-request-blob lines 446 and 600).
  - AGENTS.md and CLAUDE.md arrive in the user turn.
- **Authority.** The hook text beat CLAUDE.md 6/6, with both word orders [O]. Scope: haiku, `-p`, and a benign formatting rule. This does not license moving prohibitions into the block.
- **Mod `prompt.*`.** Under `-p`, `cc-plugin-sec-default` skipped the hooks [O]. Interactive mode was not probed.
- **opencode.** It reads AGENTS.md and does not expand `@file` [O, 1 run]. It also picked up `~/.claude/CLAUDE.md` [I].
- **Size cap.** The 9000-char cut at caps.rs:193 is on the PostToolBatch cap-violation message, not on the session-start path [V]. Claude Code's own additionalContext limit (~10k) is unprobed.
- **Today's sources** [V wc -m]:

  | File | Chars |
  |---|---|
  | SOUL.md | 542 |
  | identity.md | 554 |
  | USER.md | 327 (template, skipped) |
  | memory/rules.md | 2,139 |
  | projects/INDEX.md | 2,003 |

  Body total is about 5.2k. The ceiling is about 5.9k, with USER at its cap.

## Design decision
- **Pick:** Variant A, using the SessionStart standing block the Principal chose, with F as an automatic fallback.
- **Dropped:**
  - The mod (old P3) and launch-flag injection (D). The hook already lands in the system role, which was their purpose. The mod is also unproven in interactive mode.
  - B (`@` imports): opencode ignores them, and they arrive in the user turn.
- **Worker dissent, noted.** The source worker recommended F (INDEX as a pointer, not the body) to stay small and avoid staleness. Kept: the Principal chose inject. The block freezes per session as Hermes's does. If the block would pass its budget, INDEX degrades to a pointer line on its own.

```
CoS session context
 AGENTS.md (user turn, both hosts) : workflow rules + the ONE approval list (floor; opencode has no hook)
 standing block (SessionStart hook, system role):
   persona (identity + SOUL, overrides global tui/ní) | USER (if filled) | memory/rules.md | INDEX (last; pointer if over budget)
 skills (index; body on invoke)    : poteto-mode, herdr-orchestration
```

## P1 Engine renderer + template (one sude worker, plan-first, one suhuynh review)
Base: `quaqua/sude-ctxtrimfix` (7029843), on a new branch.
- **Renderer.**
  - A pure `render_standing(root) -> String` in qq-core memory. Each section gets a header and its source path.
  - Missing files are skipped. USER.md is skipped while it is still the template.
  - Budget is 8,000 chars total. INDEX is rendered last; if the block would pass the budget, INDEX becomes the pointer line "read projects/INDEX.md".
  - Hook wiring: `session_start` emits the block for every source (the only verified behaviour). On compact it keeps today's re-read hint.
- **Probe gate (first step, before tuning).**
  - Push an ~8.9k-char block with a codeword at its tail through the real hook in a scratch workspace, on startup and on compact.
  - Run 2 resumes with no compaction and count the copies in the api-request-blob.
  - If copies stack, skip emitting on `resume`. Otherwise keep emitting on every source.
- **Template.**
  - Restore `identity.md`, `SOUL.md` and their `init/table.rs` rows.
  - Remove the inlined persona from AGENTS.md. One line says the persona is injected and overrides the global prefix and names.
  - "Always read rules.md/USER.md/INDEX.md" becomes "injected at session start; re-read a file only after editing it".
  - The approval list stays only in AGENTS.md.
- **Threat scan on load:** deferred, stated here on purpose. The block now outranks AGENTS.md, and the CoS writes `memory/rules.md` itself. Today `caps.rs::screen_line` screens writes only. Adding a load-time scan is a follow-up (P4).
- **Verify.**
  - Unit tests:
    - section order;
    - a missing file is skipped;
    - the template USER.md is skipped;
    - the budget fallback swaps INDEX for the pointer;
    - compact adds the hint.
  - `cargo test --locked -p qq-core`.
  - The probe-gate transcript excerpts.
  - Byte table before and after.
- **Budget.** 60 min and $4.

## P2 Land (the Principal approves each outward step)
1. Push the new branch and open a PR that supersedes #285. Close #285 with the Principal's ok.
2. the Principal merges.
3. Rebuild the binary.
4. Back up the live AGENTS.md, skills and settings.
5. Run `quaqua init --update`.
6. Start a fresh interactive CoS session. Confirm the block is in the system role of the api-request-blob, and that the persona holds over the global tui/ní.

## P7 Situation tool + memory parity (the Principal approved 2026-11-18: V2 + V1, frozen on resume, push + PR)
- **Why.** Two prose duties failed on 2026-11-18: the 11-18 audit sat unhandled until `/watzup`, and a lead + peer idled 6 h unseen. `memory/rules.md` reached 97% of its cap with no signal. Principle (USER.md): one tested tool performs the known steps; prose only where no tool can check.
- **V2.** One engine function computes the CoS situation: unhandled audit reports, live agents with idle time, `.quaqua/pending/` files, capped-file fill. Two outputs from it: a CLI subcommand, and a `## Now (computed)` section at the end of the standing block (≤600 chars, each line names its action). Resume keeps the frozen block.
- **V1.** Fill header `[NN% · used/cap chars]` on capped sections of the block. cos-memory mod gains a batch write checked on the final state; a denied write returns the file's current content so the CoS can consolidate in one batch.
- **Subtract.** AGENTS.md (template) drops the prose duties the tool now carries.
- **Delivery.** One sude builder (plan-first) → one suhuynh review → quaqua-rs-lead pushes the branch (CoS is unfenced) → CoS opens the PR. Merge stays the Principal's.

## Later (separate approval)
- **E-lite (Hermes way).** Enforce more approval items in `herdr-gate.sh` (it already blocks push, merge and pane close). Candidates are branch delete and deploy commands.
- **P4.** Load-time threat scan of the block sources.
- **P5.** Lead rules home and the models.md:15 fix.
- **P6 hygiene.**
  - `worker-timebox.md`.
  - The orphan `rules/_all/taskforce.md`.
  - `scripts/auditor`.
  - The README:49 claim.

## Open (labelled, not blocking)
- The real Claude Code additionalContext limit. P1 probe gate covers it.
- The `clear` source behaviour, and whether resumes stack. P1 probe gate covers both.
- Does opencode have a session-start hook? Unknown. AGENTS.md stays the floor.
- An opencode session record left by the probe worker under opencode's data dir. Not removed and harmless.
