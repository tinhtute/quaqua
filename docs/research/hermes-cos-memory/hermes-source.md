Outcome: COMPLETE
## Changes
No repo, code or commit changes (read-only research). Only this report was written.

Labels: [src] read in Hermes source, `file:line` at commit f7e2898. [docs] a docs page (round 1 only; not re-fetched here). [local] local file. [inferred] my reasoning. Hermes paths are in `NousResearch/hermes-agent`; `sp` = `agent/system_prompt.py`, `pb` = `agent/prompt_builder.py`, `mt` = `tools/memory_tool.py`, `ms` = `tools/memory_tool_store.py`.

### Verdict
- Round 1's picture of Hermes holds up against source. Hermes injects SOUL, context files, the MEMORY/USER snapshot and a skills index into the **system prompt**, frozen per prompt build.
- Round 1 got the memory tool, the "who writes" and the "what enforces" parts wrong or incomplete. Details in section 5.
- The biggest finding for quaqua: **Hermes does not enforce prohibitions by injection.** Prose rules sit in SOUL/AGENTS text (advisory). Hard floors are code (`tools/approval.py` `_floor_block`), and user-authored instruction files are write-gated.
- quaqua already has the code half of that split (`herdr-gate.sh` PreToolUse). Plan v3 only covers the prose half.
- Recommendation: ship variant **F** (slim standing block via the already-wired SessionStart hook), written as a pure renderer that D (launch flag) or C (mod) can reuse later. Add **E-lite** (move the approval list into the gate) as a separate item. Do **not** inject the whole INDEX.
- One fact can sink option 1's "AGENTS.md is the floor" assumption: `~/quaqua` has **no CLAUDE.md** [local: `ls` says no such file]. Claude Code reads CLAUDE.md, not AGENTS.md. How AGENTS.md reaches the CoS is unverified. Probe P1 settles it first.

### Sources
| # | Source | Tier | Date |
|---|---|---|---|
| H | `NousResearch/hermes-agent` @ `f7e2898aaefc8f967dbf7345e3999a65672fe60e` (main, pushed 2026-11-18T07:32Z), read raw via `raw.githubusercontent.com`: `sp`, `pb`, `mt`, `ms`, `agent/subdirectory_hints.py`, `agent/memory_context.py`, `agent/conversation_compression.py`, `agent/agent_init.py`, `tools/write_approval.py`, `tools/file_tools_write_guards.py`, `tools/approval.py` | T1 primary source | 2026-11-18 |
| R1 | `hermes-inject-round1.md` (docs-only round) | T3 prior report | 2026-11-18 |
| P3 | `~/quaqua/docs/plans/active/ctx-trim.md` (plan v3) | T3 plan | 2026-11-18 |
| L | `crates/qq-core/src/memory/{drift.rs:73-96, mod.rs:95-110, caps.rs:187-195}`, `crates/quaqua/src/launch.rs:337,356,451`, `~/quaqua/.claude/settings.json:10`, `~/quaqua/AGENTS.md:15,20,26,28,84,86`, live file sizes | T3 local | 2026-11-18 |
| W | Hermes docs pages, pi-hermes-memory README (via R1 only) | [docs] T1/T2 | not re-fetched |

Search note: no web search was needed. The repo was read directly, as the brief allowed. No private text went into any URL.

### 1. How hermes-agent builds the system prompt
Layout found (the brief asked me to find, not guess): `agent/system_prompt.py` assembles; `agent/prompt_builder.py` supplies SOUL, context-file discovery and the skills index; `tools/memory_tool.py` + `tools/memory_tool_store.py` own memory.

```
build_system_prompt_parts(agent)  sp:770-833 -> {stable, context, volatile}
 stable   : SOUL.md else DEFAULT_AGENT_IDENTITY   sp:782 (_identity_parts sp:576-582, pb:1520)
            + help/guidance + tool-use + pinned skills + coding brief   sp:783-801
 context  : caller system_message | ONE project context type | workspace snapshot | env probe | platform hint
            sp:804-817 (project files: _context_files_part sp:749-762)
 volatile : [skills index, MEMORY block, USER block, memory-provider block]   sp:819 (_memory_parts sp:548-573)
            + plugin "after_memory" sections + profile line + timestamp + runtime-env block (last)   sp:820-831
 joined stable -> context -> volatile   sp:836-845
```
- SOUL: loaded from `HERMES_HOME/SOUL.md`, scanned, truncated [src pb:1520-1569]. When SOUL is the identity slot, `skip_soul=True` stops a second copy as a context file [src pb:1767-1800, sp:749-762].
- Project context: **first match wins**, one type only: `.hermes.md` > AGENTS.md chain > CLAUDE.md (cwd only) > `.cursorrules` [src pb:1792]. Wrapped under "The following project context files have been loaded and should be followed" [src pb:1797-1799].
- AGENTS.md is a **directory chain** git-root → cwd, deeper wins; per directory `AGENTS.override.md` beats `AGENTS.md` beats `agents.md` [src pb:1621-1647]. Round 1 described a single file.
- Caps: context-file limit defaults to 20,000 chars, dynamic up to 500,000 (6% of the window); config `context_file_max_chars` wins [src pb:1040-1068]. Truncation keeps 70% head + 20% tail with a marker that names the path to `read_file` and lists omitted headings [src pb:1470-1500].
- Injection scan: `scope="context"`. Repo files hit → replaced by `[BLOCKED: …]`. A user-authored SOUL hit → warn and load anyway [src pb:84-109].
- Memory block render: header, usage `[NN% — x/y chars]`, entries joined by `\n§\n` [src ms:26, 675-681]. Headers "MEMORY (your personal notes)" and "USER PROFILE (who the user is)" [src ms:23-24].
- Skills: a compact index in the volatile tier, bodies load via `skill_view` [src sp:316-330, pb:1303-1375]. The memory tool text says task-specific learning belongs in a skill, not memory [src mt:380-395].
- Not in the system prompt: `subdirectory_hints.py` loads AGENTS.md/CLAUDE.md of directories the agent walks into and **appends them to tool results**, so the system prompt and cache stay untouched [src subdirectory_hints.py:1-6, 22-25]. Round 1 missed this.

### 2. When the snapshot is frozen and rebuilt, and why it is cache-safe
- Frozen at load: `MemoryStore.load_from_disk()` fills `_system_prompt_snapshot`; live entries and the snapshot are separate [src ms:177-198, 223-259]. Called at agent init [src agent_init.py:1179-1185].
- Mid-session writes hit the file and the live list, **not** the prompt [src ms:177-181, 311].
- Rebuild only at compression commit: `conversation_compression.py:3038` calls `_invalidate_system_prompt()`, which clears the cached prompt and **reloads memory from disk** [src sp:848-866].
- The rebuild is not unconditional. A detached or "seeded" agent keeps the stored bytes [src conversation_compression.py:3025-3033]. The git workspace snapshot is pinned per session so a rebuild re-emits identical bytes [src sp:700-730]. Plugin sections are re-rendered at the same boundary, with a fail-open fallback to the last good bytes [src sp:848-866].
- Cache safety is by construction: three ordered tiers, stable first. Volatile things (skills, memory, time, env) come last so the stable prefix survives a rebuild [src sp:770-833]. Context files sit before worktree-dependent blocks so worktrees share a prefix [src sp:804-817].
- New session: a fresh agent calls `load_from_disk` again [src agent_init.py:1179-1185]. The session's prompt is also persisted on a session row and read back [src sp:658-679]. I did not trace the resume path end to end [inferred: resume reuses the stored prompt].

### 3. How the memory tool writes
- One tool `memory`; model-visible actions are **add / patch / remove**, or a batch `operations` list. `replace` was **removed from the schema** ("use 'patch'") and survives only for old staged writes [src mt:165-170, 373-410, ms:421, 435, 470, 568].
- Targets: `memory` (agent notes) and `user` (profile). **The agent can write both** [src mt:321-336, 373-410].
- Caps: MEMORY 2,200 and USER 1,375 chars by default, measured in chars [src ms:188, mt:55-57].
- Over cap: an add is rejected and returns the current entries. The agent must reissue **one batch** that removes or shortens and adds. The limit is checked only on the final state, and an over-limit batch writes nothing [src ms:332-340, 568-575, 624-625].
- A loop guard stops after 3 failed consolidations per turn with a terminal "save skipped" [src ms:177-190].
- Duplicates: an exact duplicate add is a silent success ("already exists") [src ms:412-413]. Duplicates are also dropped on load [src ms:248].
- Threat scan: `scope="strict"` on every add/replace/patch/batch content [src ms:29-32, 405-409, 591]. Again at load: a hit becomes a `[BLOCKED: … use memory(action=remove…)]` placeholder **in the snapshot only**; the live list keeps the raw text so the user can see and delete it [src ms:228-241].
- Over-cap files written by outside tools are loaded in full with a warning; adds stay blocked until under cap. Hermes never truncates memories [src ms:251-257]. A free-text edit that breaks the `§` format makes the tool refuse to rewrite the file ("drift" guard) and save a `.bak` [src ms:51-62].
- Optional approval gate: `memory.write_approval` / `skills`. Default `false`, writes freely; `true` prompts inline or stages the write for later review, with replace/remove pinned to the exact matched entry [src write_approval.py:5-12, mt:84-103, 131-140]. The generic file tools do not consult it (#60440, noted in source) [src write_approval.py:8-9].
- Unattended background-review forks may `add` but not `patch`/`remove` [src mt:179-219].

### 4. Where hard rules live, and whether anything enforces them
- Hermes has **no prohibition slot**. Rules are prose in SOUL.md or the project context file, loaded into the prompt [src sp:782, pb:1792]. Nothing in the prompt layer enforces them [inferred; no code checks compliance with prose].
- Hermes enforcement is separate code. `tools/approval.py` `_floor_block` runs "BEFORE yolo / mode=off / cron approve-mode so no session-level setting can bypass": hardline catastrophic commands, and the user's own `approvals.deny` rules ("never, even under yolo") [src approval.py:1308-1327].
- Instruction files are write-protected: `_PROTECTED_INSTRUCTION_BASENAMES = {agents.md, claude.md, soul.md, .cursorrules}`; writes through the file tools go through an approval gate (config `security.protected_instruction_files`, default on) [src file_tools_write_guards.py:164-176, 184-200]. So SOUL/AGENTS are effectively user-only by gate. MEMORY/USER are agent-writable.
- "Standing"/user-only file: **none in the shipped core.** The only hits are `plugin-catalog/hermes-standing.yaml` and `plugin-catalog/standing-orders.yaml`, which I did not open. The `STANDING.md` idea in round 1 comes from the third-party pi-hermes-memory port [docs, via R1].
- SOUL vs AGENTS conflict: **no arbitration code.** I grepped `pb` for conflict/precedence/override. Hits are only AGENTS-chain "deeper = precedence" [src pb:1621] and a comment that docs are authoritative [src pb:173]. Order is the only mechanism: SOUL in the stable tier (first), project file in the context tier, under a "should be followed" header [src sp:782, 802-817, pb:1797]. Recency may favour AGENTS.md [inferred, untested].

### 5. Round 1 claim → source says → verdict
| Round 1 claim | Source says | Verdict |
|---|---|---|
| Prompt has stable → context → volatile tiers; memory/USER/skills are volatile | Same [src sp:770-833, 819] | confirmed |
| SOUL replaces default identity; `skip_soul` avoids a second copy | Same [src sp:576-582, pb:1767-1800] | confirmed |
| One project file, first match wins: `.hermes.md` > AGENTS.md > CLAUDE.md > `.cursorrules` | Same order [src pb:1792]; AGENTS.md is a git-root→cwd **chain** with `AGENTS.override.md` [src pb:1621-1647] | confirmed, corrected (chain) |
| Context caps: 20k floor, 500k ceiling, 70/20 head/tail | Same [src pb:1040-1068, 1470-1500]; marker also lists omitted headings and the read path | confirmed |
| Memory 2,200 / USER 1,375, never auto-compacts | Defaults confirmed [src ms:188]; a cap lowered later or an outside write loads in full, adds blocked [src ms:251-257] | confirmed |
| Memory ops are add / replace / remove | Model sees **add / patch / remove + batch**; `replace` removed [src mt:165-170] | corrected |
| Over-cap write errors and the agent makes room | Error returns current entries and demands ONE batch; checked on final state; 3-failure turn guard [src ms:332-340, 624, 177-190] | confirmed, detail added |
| Duplicate/injection scans (not covered) | Exact dup add = silent success; strict scan on write; scan again on load → `[BLOCKED]` in snapshot only [src ms:412-413, 29-32, 228-241] | new |
| Snapshot refreshes on new session or compression rebuild | Same [src agent_init.py:1179-1185, conversation_compression.py:3038, sp:848-866]; seeded agents keep stored bytes; workspace block pinned [src conversation_compression.py:3025-3033, sp:700-730] | confirmed, caveats added |
| Hermes has no separate prohibition slot | Confirmed; enforcement is code (`_floor_block`) and write gates [src approval.py:1308-1327, file_tools_write_guards.py:164-176] | confirmed, extended |
| STANDING.md = user-only file, from pi-hermes-memory | Not in Hermes core; agent writes MEMORY **and** USER; user-only only for SOUL/AGENTS via write gate | corrected (Hermes has none) |
| Subagents skip SOUL | `skip_context_files` / `load_soul_identity` choose SOUL or default identity [src sp:576-582] | confirmed |
| "Runtime env block always ends the prompt" | Confirmed; renderer reserves the heading [src sp:824-831] | confirmed |
| (missed) progressive AGENTS.md in tool results | `subdirectory_hints.py`, cache-safe [src :1-6] | new |
| (missed) `<memory-context>` fence for recalled memory, scrubbed from output | `agent/memory_context.py` [src :1-50] | new |
| (missed) memory-write approval gate, default off | [src write_approval.py:5-12] | new |

### 6. What round 1 missed: copy or skip
| Hermes feature | Copy? | Why |
|---|---|---|
| Tool text steering memory to "facts for EVERY session"; task learning → skill [src mt:380-395] | **Copy** | One line in `memory_add`'s description; stops rules.md filling with task notes |
| Atomic batch with final-state cap check and returned current entries [src ms:332-340, 624] | **Copy** if `memory_*` tools lack it | Cheapest fix for the "make room" loop |
| Strict threat scan on write and at load, `[BLOCKED]` only in the injected copy [src ms:228-241] | **Copy** | The injected block is a prompt-injection surface; a poisoned rules.md must not ride into every session |
| Head/tail truncation with a marker naming the file to read [src pb:1470-1500] | **Copy** | Better than a silent `take(9000)` at `caps.rs:191-195` |
| Protected-instruction write gate [src file_tools_write_guards.py:164-176] | Copy if absent | quaqua's AGENTS.md says only CoS edits rules; a gate makes it real |
| Code floor for prohibitions [src approval.py:1308-1327] | **Copy (E-lite)** | quaqua already has `herdr-gate.sh`; extend it |
| Progressive subdirectory hints [src subdirectory_hints.py] | Skip | CoS root is one dir; workers get a rules file |
| Three cache tiers, pinned workspace, plugin-section freeze [src sp:700-833] | Skip | Cache engineering for a harness quaqua does not own; Claude Code owns its prompt |
| `<memory-context>` fence + output scrubber [src memory_context.py] | Skip for now | Needed only if recalled memory is injected per turn |
| Agent-writable USER profile | Skip | quaqua keeps USER.md principal-owned (`AGENTS.md:26`) |

### 7. Design variants
Sizes: identity 579 + SOUL 692 + USER 329 (template, skipped while unfilled) + rules 2,162 + INDEX 2,019 + approval line ~420 [local; R1]. Code costs are rough LOC guesses [inferred].

| # | Variant | Injected | Where | Refreshes | Failure mode | Host coverage | Bytes | Code |
|---|---|---|---|---|---|---|---|---|
| A | Plan v3 as written: engine block on every source | SOUL+identity, USER, rules.md, INDEX | SessionStart `additionalContext` (a message) | startup, resume, clear, compact; frozen between | Stale INDEX in long runs; 9,000-char silent cut drops the tail (INDEX) [local caps.rs:191-195]; resume may duplicate the block [inferred]; old binary has no `memory hook` (README says 2.6.0) | Claude Code only | ~6.2 KB | renderer + tests, ~150 LOC |
| B | **Zero engine code**: template only. CLAUDE.md = `@AGENTS.md`; AGENTS.md `@`-imports `SOUL.md`, `memory/rules.md`, `USER.md`; INDEX stays on demand | same, minus INDEX | Claude Code context file | session start, and re-read after `/compact` [inferred, unverified] | `@` not expanded in opencode (R1); import needs first-use approval; AGENTS.md does not reach Claude without a CLAUDE.md (see P1) | Claude Code; opencode only via the AGENTS.md floor | ~4.2 KB | template edit, 0 LOC |
| C | **Mod**: `prompt.section` / `compose` | same block | real system prompt, cached | new thread or compaction; `breakPromptCache` opt-in [R1 S5] | Silent loss if mod not installed (`quaqua init` does not install it); API pinned to CC 2.1.322 | Claude Code only | ~4–6 KB | mod + reuse renderer, ~100 LOC JS [inferred] |
| D | **Hermes-style launch tier**: `quaqua` renders `runs/.cos-standing.md` at CoS launch; Claude gets `--append-system-prompt-file` (the engine already does this for workers [local launch.rs:451]) | SOUL+identity, USER, rules.md, approval line | real system prompt | only at process start; survives compaction with no re-injection [inferred] | Stale until the CoS restarts; useless if the CoS is started with bare `claude` (P5); rules edits invisible until restart | Claude Code; opencode no flag | ~4 KB | launch change + renderer, ~80 LOC |
| E | **Premise challenge**: enforce, don't inject. Keep AGENTS.md short with the approval list; add the same list to `herdr-gate.sh` PreToolUse deny/ask; rules.md stays recalled, with a one-line SessionStart nudge | approval list as code; 1 line | hook + file | per tool call | Covers only tool-visible actions (push, force-push, merge main, delete). "Spending money", "sending externally" stay prose. Does not make the model *follow* soft rules | Claude Code hooks; opencode text only | ~0.2 KB | gate patterns, ~30 LOC [inferred] |
| F | **Slim A (recommended)**: A minus INDEX body, plus approval line. INDEX = one pointer line | SOUL+identity, USER (if filled), rules.md, approval line, "INDEX at projects/INDEX.md" | SessionStart `additionalContext` | startup, resume (dedupe, P2), clear, compact | Same hook risks as A; less staleness; stays under the cut | Claude Code only | ~4.1 KB | A minus a section |

### 8. Ranking and recommendation
1. **F + E-lite** (do now). Same hook already wired [local settings.json:10], smaller than A, avoids the stale-INDEX class Hermes users hit [docs, R1 S4], and puts the prohibitions in code, as Hermes does. Write the renderer as a pure function `render_standing(root) -> String` so D or C can reuse it.
2. **D** (next, if P5 says the CoS is launched by quaqua). Closest to Hermes: system prompt, compaction-proof, no hook or mod. Its cost is staleness until restart.
3. **A** (as v3 specs). Fine, but INDEX adds staleness and cut risk for little gain. Answer to Q3: **do not inject the INDEX body**.
4. **C**. Best fidelity, worst install story. Keep as P3.
5. **B**. Cheapest, and the real fallback if P1 shows AGENTS.md is not loaded at all. It cannot carry rules.md fresh after edits within a session, and opencode gets nothing.
6. **E alone**. Good as the floor, not enough to carry soft rules.
- Q2 (poteto-mode): index only, as Hermes does (skills index in the prompt, body via `skill_view`) [src pb:1303-1375].
- Hard-rule principle from Hermes: put each prohibition in the **cheapest place that can enforce it**. Prose goes in the injected block. What a tool call can be checked against goes in the gate.

### 9. Open facts a probe must settle
- P1. How does `~/quaqua/AGENTS.md` reach the CoS? No CLAUDE.md exists at `~/quaqua` root. Check `~/.claude/CLAUDE.md`, launch flags, and a canary line in AGENTS.md. This decides whether "AGENTS.md is the floor" is true.
- P2. SessionStart on `resume` and `clear`: does Claude Code keep the earlier `additionalContext` in the transcript (duplicate on re-emit)? After `/compact`, does the original block survive in the summary or vanish?
- P3. Authority: put a canary prohibition once in `additionalContext` and once in AGENTS.md; compare compliance, before and after `/compact`.
- P4. Real size limit: emit 9.5k, 10.5k and 12k chars. Is output cut, saved to a file with a preview, or dropped? The engine's own 9,000 cut at `caps.rs:191-195` is a guess.
- P5. Is the CoS started via `quaqua` (so D can set a flag) or bare `claude`? Not verified. `launch.rs` covers workers.
- P6. opencode: any session-start hook or system-prompt flag? Not researched this round.
- Not verified here: Hermes docs pages, pi-hermes-memory, the two `plugin-catalog/*standing*.yaml` files, `tools/threat_patterns.py` pattern lists, the Hermes session-resume path, and `agent/conversation_loop.py` (a path I guessed was wrong; the file is absent at this commit).

## Verify
- No tests (no code changed).
- Source checks: `curl` of raw files at the pinned SHA, then `grep -n` and `sed -n` for each cited range. File lengths: `prompt_builder.py` 1800, `system_prompt.py` 915, `memory_tool.py` 479, `memory_tool_store.py` 746, `agent_init.py` 2199.
- Local checks: `wc -c` on the six live files (579/692/329/2162/2019/8596 for identity/SOUL/USER/rules/INDEX/AGENTS), `ls ~/quaqua/CLAUDE.md` (absent), `sed -n` on `drift.rs`, `mod.rs`, `caps.rs`, `launch.rs`, `settings.json`.

## Risks
- Line numbers are for commit f7e2898. Hermes moves fast; re-pin before quoting.
- Variant sizes and LOC are estimates; code costs are not measured.
- "Hermes enforces prohibitions by code" rests on `_floor_block` and the write guards. I did not read their full rule sets.
- Variants B, D and the resume behavior depend on P1, P2 and P5, which are unprobed.

## Lessons
Source beat docs: Hermes dropped `replace`, has no user-only file, and enforces prohibitions in code, not prompt text.
Check how AGENTS.md reaches the model (no CLAUDE.md in the CoS root) before calling it the floor.
