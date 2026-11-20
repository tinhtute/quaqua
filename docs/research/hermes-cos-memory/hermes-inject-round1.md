Outcome: COMPLETE
## Changes
No repo, code or commit changes (read-only research). Only this report was written.

Labels: [measured] seen in fetched text or local file:line. [cited] stated by a source I read. [inferred] my reasoning. I read Hermes docs, not its Python source. The docs quote simplified `prompt_builder.py` code, so file:line for Hermes is not available.

### Verdict
Hermes injects always-on context into the **system prompt**, in three cache tiers, frozen at session start. It injects the full MEMORY/USER snapshot. It does not rely on recall for them. On quaqua's side, `quaqua memory hook session-start` is already wired but injects nothing at startup (only a re-read list after compaction). Recommendation: option 1 below. An engine-rendered standing block delivered by SessionStart `additionalContext`. The approval list stays as a tiny floor in AGENTS.md. PR #285: keep the dedupes, rework persona inlining, revert the SOUL/identity deletion.

### Sources
| # | Source | Tier | Date |
|---|---|---|---|
| S1 | hermes-agent.nousresearch.com/docs/developer-guide/prompt-assembly (fetched in full) | T1 official docs | fetched 2026-11-18 |
| S2 | hermes-agent.nousresearch.com/docs/user-guide/features/memory (fetched) | T1 official docs | fetched 2026-11-18 |
| S3 | github.com/chandra447/pi-hermes-memory README v0.7.10 (fetched) | T2 third-party port, 536 stars | fetched 2026-11-18 |
| S4 | github.com/NousResearch/hermes-agent issues/66025, search snippet only ("frozen snapshot at session creation", stale in long desktop sessions) | T2 | 2026-07-16 |
| S5 | `~/.claude/mods/ninja-kit/.claude-plugin/types/claude-code/index.d.ts` :1610, :1682, :5026-5042, :10460-10520, :11274-11300 (generated from Claude Code 2.1.322) | T1 local types | 2026-11-18 |
| S6 | `mods/cos-memory/README.md` :38-61 (Mod API table) | T3 local | in worktree |
| S7 | `crates/qq-core/src/memory/mod.rs`, `drift.rs:6,68-105` | T3 local | in worktree |
| S8 | `~/quaqua/.claude/settings.json:5-13` | T3 local | 2026-11-18 |
| S9 | origin/main `.harness/plans/done/cos-memory-loop.md` log lines for 2026-11-11 | T3 local | 2026-11-11 |
| S10 | `~/quaqua/docs/plans/active/ctx-trim.md`, `git diff origin/main...origin/quaqua/sude-ctxtrimfix --stat` | T3 local | 2026-11-18 |

### 1. How Hermes assembles always-on context
```
system prompt = stable -> context -> volatile                      [S1 cited]
 stable   : SOUL.md (else DEFAULT_AGENT_IDENTITY) | tool/model guidance | coding brief
 context  : system_message | ONE project file (first match wins:
            .hermes.md > AGENTS.md > CLAUDE.md > .cursorrules)
            | git workspace snapshot | operator instr | platform hints
 volatile : skills index | MEMORY.md snapshot | USER.md snapshot
            | memory-provider block | time/session/model | runtime env (last)
 NOT in system prompt (per API call): ephemeral_system_prompt, prefill,
            pre_llm_call plugin context -> appended to the current user message
```
- Order and why: project files sit before anything worktree-specific, so worktrees of one project share a cache prefix [S1 cited]. The runtime env block always ends the prompt [S1 cited].
- SOUL: `~/.hermes/SOUL.md` is the first section. It replaces the default identity, is scanned (warn, load, never block) and truncated. `skip_soul=True` stops a second copy as a context file. Subagents (`skip_context_files`) get the default identity, not SOUL [S1 cited].
- Caps: MEMORY.md 2,200 chars, USER.md 1,375 chars. Memory never auto-compacts. An over-cap write errors and the agent makes room [S2 cited]. Context files are capped by `context_file_max_chars`, 70/20 head/tail with a marker, floor 20k chars, ceiling 500K. Project files get an injection scan and are replaced by `[BLOCKED]` on a hit [S1 cited].
- Freeze and refresh: the snapshot is captured once at session start. Mid-session writes hit disk but not the prompt. Refresh happens on a new session or a compression-triggered rebuild [S1, S2 cited]. A skills-index change rides a one-shot note on the next user message and converges at the next rebuild [S1 cited].
- Skills: the compact `<available_skills>` index (name + one line) is in the prompt. Bodies load on demand via `skill_view` [S1 cited]. S2 says a per-task location is better as a skill than as a memory entry, to save the memory budget.
- Prohibitions: Hermes has no separate slot. They live in SOUL.md or the project file, so they are injected, not recalled [inferred from S1].
- pi-hermes-memory: by default it injects **no** MEMORY/USER, only a `<memory-policy>`, and the agent must call `memory_search`. A `legacy-inject` mode restores the Hermes behavior. For prohibitions it adds `STANDING.md`: user-authored only (the agent cannot write it), max 20 entries / 2,000 chars, injected every session in every mode, after the policy, fenced. Its stated reason: "for a prohibition, that is exactly the moment it has no reason to look" [S3 cited]. It is deliberately not tool enforcement.

### 2. Local mechanisms
| Mechanism | What it injects | Compaction | Cache | Host |
|---|---|---|---|---|
| AGENTS.md / CLAUDE.md, `@` imports | file text, byte-for-byte | not verified here [inferred: re-read after compaction] | stable | opencode reads AGENTS.md but does not expand `@` [S10 cited] |
| SessionStart hook `quaqua memory hook session-start` | today: startup/resume/clear = nothing (writes a baseline and re-tags the pane). `source=compact` = a 3-file re-read list (AGENTS.md, memory/rules.md, projects/INDEX.md) plus the newest audit [measured S7 drift.rs:8,79,87-95; mod.rs session_start]. Wired: yes, second command in SessionStart [measured S8:10] | re-fires on `compact` (and `resume`), so it can re-inject [measured S7 drift.rs:79] | a message at conversation head; the text is fixed once emitted [inferred] | Claude Code hooks; opencode has no equivalent [inferred] |
| Hook `additionalContext` | text handed to the model; SessionStart supports it [measured S5:1682]. Cap 10k chars [measured caps.rs:191 comment; S9 cited] | per event | as above | Claude Code |
| Mod `prompt.section` (per named engine section, can rewrite or drop) | `{text, breakPromptCache?}` [measured S5:11274-11300] | cached; a changed answer waits for a new thread or compaction unless `breakPromptCache:true` [measured S5:5026-5030] | yes, by design | Claude Code only; mod must be installed [measured S6: "CoS-only"]. Not used by cos-memory today [measured: grep of `mods/cos-memory/{hooks,src}` found no prompt.* hook] |
| Mod `prompt.compose` | adds own sections `<plugin>:<name>`, `scope: shared\|session`; an appended section is `session` scope [measured S5:10478-10520] | re-composed per prompt | `session` sits after the shared cache boundary | Claude Code only |
| Mod `prompt.context` | blocks on the first user message, once per conversation; re-read on compaction and `/clear` [measured S5:5037-5042] | yes | stable | Claude Code only |
| `UserPromptSubmit` additionalContext (cos-inbox.sh) | per-turn digest [measured S8:42-48] | n/a | per turn, no reuse | not for standing rules [inferred] |

Mod facts: mod budget 10 s per dispatch; `$.fs`, `$.process.run` available so the mod can call `quaqua` the way `memory-gate.js` does [measured S6:55-61]. The cos-memory tools are loaded in this session [measured: `mcp__cos-memory__memory_*` present]. S6 says the installed `quaqua 2.6.0` predates `memory hook`, so rebuild before relying on any hook [measured S6:111-121]. I did not check the current Claude Code version against 2.1.322.

Gap [measured/inferred]: `memory/rules.md` and `projects/INDEX.md` are in the post-compaction re-read list, so today they reach the model by being **recalled** (the CoS reads them), not injected. How they load at startup was not verified here. This is exactly the failure S9 and S3 call out.

### 3. Mapping (bytes from `~/quaqua` live files and the template, [measured])
| Content | Bytes | Layer (Hermes analogue) | Mechanism | Note |
|---|---|---|---|---|
| persona `identity.md` + `SOUL.md` | 579 + 692 | stable identity (SOUL slot) | injected standing block, first | Cost-neutral vs inline. Workers must not get it (Hermes skips SOUL for subagents) |
| `USER.md` | 329 (unfilled template; Hermes cap 1,375) | frozen USER snapshot | injected block | Skip if still the template |
| `memory/rules.md` | 2,162 (cap 2,200, same as Hermes MEMORY 2,200 [cited]) | frozen MEMORY snapshot | injected block | Frozen per session, refresh on compact/new session |
| approval list (prohibitions) | ~420 [inferred from AGENTS.md L15] | STANDING.md analogue | injected AND one copy in AGENTS.md | Tiny floor for opencode fallback and mod/hook failure; the other three copies (L69, L84, poteto-mode skill) go |
| poteto-mode routing | skill body 14,215 (PR #285: 12,986) | skills index | CC skill description (already an index) + one always-loaded line (~100) | Body loads on invoke only. Matches Hermes index+`skill_view` [cited] |
| `projects/INDEX.md` | 2,019 | project/context | inject the "Now" part only, or keep read-on-demand | Volatile; frozen copy goes stale (S4) |
| `AGENTS.md` | 8,441 (PR #285: 8,171) | context tier | stays a file | Workflow facts only once the above leave it; cut inferable text |
Injected block total: 692+579+329+2,162+2,019+420 ≈ 6.2 KB, under the 10k hook cap [measured sums; cap from caps.rs:191/S9].

PR #285 (measured stat: 7 files, +48/-101; AGENTS.md 8,441->8,171; poteto body 14,215->12,986; SOUL.md and identity.md deleted; 10 lines out of `init/table.rs`):
- Keep: role-gate/Lead-clause removal, contradiction fixes, single approval list, slimmed poteto body plus one routing line.
- Rework: persona inlining. Under option 1 persona is injected, not inlined in AGENTS.md.
- Revert: deletion of SOUL.md, identity.md and their `table.rs` rows, because the renderer reads them as sources.

### 4. Options (top 3)
1. **Engine-rendered standing block via SessionStart additionalContext (recommended).** Extend `quaqua memory hook session-start` to emit persona + rules + USER + INDEX Now + approval list on every source (startup, resume, clear, compact). Already wired (S8:10), shipped by `quaqua init`, no mod install, re-injects after compaction, frozen by construction, ~6.2 KB of 10k. Weakness: a message, not the system prompt; Claude Code only, so the approval-list copy in AGENTS.md is the opencode floor.
2. **Mod `prompt.section`/`prompt.compose` section.** Closest to Hermes (real system prompt, session scope, change waits for compaction, `breakPromptCache` opt-in). Weakness: mod must be installed (README: `quaqua init` does not install it), a silent loss if the mod fails to load, API pinned to the 2.1.322 bundle, Claude Code only. Good second step once option 1 is stable; the mod can call the same engine renderer.
3. **PR #285 as is (inline in AGENTS.md).** Host-agnostic and zero new code, but memory/rules.md and USER.md stay recalled, not injected, and nothing is frozen or re-injected.

## Verify
`git diff --stat origin/main...origin/quaqua/sude-ctxtrimfix`, `wc -c` on template and `~/quaqua` files, `sed`/`grep` on `drift.rs`, `mod.rs`, `index.d.ts`, `settings.json`, `mods/cos-memory/README.md`: all matched the numbers above. Hermes and pi-hermes claims come from fetched doc text (TinyFish). No test was run (no code changed). Not verified: Hermes source code, how AGENTS.md/rules.md/INDEX load at startup, opencode behavior, the current Claude Code version, prompt.section run live.

## Risks
- Hermes claims rest on docs, not source; the docs' code blocks are marked simplified.
- `prompt.section`/`compose` API is read from a 2.1.322 type file; an upgrade may move it.
- The installed `quaqua` binary (2.6.0 per S6) predates `memory hook`; rebuild before P2.
- A frozen INDEX/rules snapshot goes stale in a long session (S4 reports this in Hermes). Compaction or `/clear` refreshes it.
- Injected text sits in a hook message, not the system prompt, so it may carry less authority than AGENTS.md text. Keep the approval-list floor in AGENTS.md.

## Lessons
Check what the hook already emits before designing an injector: session-start is wired but silent at startup.
Hermes's memory caps (2,200 / 1,375) are already quaqua's caps, so only the delivery path is missing.
