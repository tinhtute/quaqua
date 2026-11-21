Outcome: COMPLETE
## Changes
No repo or workspace file changed. This report is the only output.

It holds three architect reports, written read-only against origin/main cf61565, the locked plan `~/quaqua/.monows/plans/active/hermes-cos.md` (cited as `plan:N`), and the Hermes trail at 5a04085 (cited as `brainstorm:N` = `docs/research/hermes-cos-memory/brainstorm-2026-11-18.md`, `hermes-source:N` = `…/hermes-source.md`). Each report argues against its leading option. None of them picks the final option.

Reversal cost at a glance:
- **A is hard to reverse.** The written files have no history, and a second writer already runs unattended.
- **B is cheap to reverse per pattern**, but its framing gets two facts wrong: the gate has no `ask` tier, and pushes to main are not hard-denied at the door that actually pushes.
- **C costs a moderate amount to reverse**, because it adds a new persisted event contract, and it does not catch the miss that motivated it.

### Report A — Q10: the CoS writes USER.md and memory/rules.md with no per-write approval

#### Problem
Decision: plan:16 (R4) and plan:42 (Q10). It reverses a decision made the same morning: "The CoS drafts USER.md, the Principal approves" (brainstorm:26). It also reverses the earlier research recommendation "Agent-writable USER profile | Skip" (hermes-source:107).

Constraints that the plan's framing omits:
1. **The write path has no caller identity.** The cos-memory tools write with `$.fs.write` after a check that never receives an identity (`mods/cos-memory/src/tools.js:100-112`; the root is `$.session.root()`, `:121-127`). The check pipes a synthetic `Write` payload to `quaqua memory hook pre` (`mods/cos-memory/src/memory-gate.js:16-20`), which tests the cap and secret patterns only (`crates/qq-core/src/memory/caps.rs:137-163`). The engine does have real caller identity: the socket peer resolved to a herdr pane (`crates/qq-core/src/identity/peer.rs:1-7`), which the daemon push verb uses (`authority/push.rs:100-102`). That mechanism is not on this path. R4's acceptance case "a worker, Lead or reviewer write is refused" has no enforcement point today.
2. **Plugin enablement is the only isolation.** cos-memory is enabled per project in `~/quaqua/.claude/settings.local.json:3`. Workers and Leads run in worktrees (`task/spawn_cmd.rs:899-900`), where the driver installs only a Bash gate (`driver/claude.rs:48-69`). That they do not load the mod is inferred. They also never get the `memory hook pre` PreToolUse hook, which exists only in `~/quaqua/.claude/settings.json:28-33`. So a worker's plain Write, Edit or Bash to the absolute path `~/quaqua/USER.md` goes through with no cap check, no secret check and no identity check.
3. **A second writer already runs unattended.** The review fork runs on `turn.complete` (`mods/cos-memory/hooks/register.js:55-95`). It asks a model fork for add, replace and remove operations on both files and applies them with no tool call and no human (`mods/cos-memory/src/review.js:1-20, 54-84`). Its input is the session. Under A, this path becomes sanctioned with no approval.
4. **Adds force deletions.** memory/rules.md is at 2193 of 2200 chars (plan:54). Almost every add needs a replace or remove in the same batch. In practice, A means unattended consolidation, which deletes owner-authored rules, not just appends.
5. **There is no rollback.** `~/quaqua` is not a git repository (`git -C ~/quaqua log -- USER.md` → "fatal: not a git repository"), and neither file has a `.bak`. A bad write can be recovered only from ad hoc backups such as `.quaqua/backup-2026-11-18-sync/` (plan Log).
6. **R4 sidesteps R5.** Both files are injected into the system role at every session start, under the header "these outrank the global persona" (`crates/qq-core/src/memory/standing.rs:12-18, 60`). memory/rules.md lands in the same tier as SOUL.md and identity.md, which R5 approval-gates (plan:17). A persona-level change written as a "rule" goes through R4 without approval (inferred).
7. **The plan's drift guard does not match the Hermes notes.** The plan says the guard refuses when the file was edited outside since load and keeps a `.bak` (plan:42). The Hermes notes at f7e2898 say it refuses when a free-text edit breaks the `§` entry format (hermes-source:64, Hermes `ms:51-62`). The plan cites `_detect_external_drift` from a later read of main, which is not re-verified here. The engine's own `drift.rs` tracks AGENTS.md, team.toml and rules/_all only (`memory/drift.rs:13-31`), and it warns but never blocks (`:1-3`). Nothing existing can be reused for this guard.

#### Alternatives
- **A0 (alternative on record):** the CoS drafts each write and the owner approves it (brainstorm:26). Cost: approval fatigue, and the review fork can no longer run unattended.
- **A1, Hermes's own middle ground:** the agent may write, behind a `memory.write_approval` toggle that defaults to off and can stage writes for review (hermes-source:65). Built as a flag from day one, reversal is a flag flip.
- **A2, split by file:** memory/rules.md is agent-writable (A), and USER.md needs owner approval (A0). Hermes itself separates the two: "your personal notes" versus "who the user is" (hermes-source:44).
- **A3, gate destructive edits only:** add and replace apply automatically. A remove, or a replace that drops existing text, returns `ask`. This targets the 2193/2200 consolidation risk in Problem 4.
- **A4, a daemon door:** a `memory.write` verb that names the caller from the socket peer (as `push.rs:100-102` does) and refuses non-cos callers, plus a PreToolUse deny on the two paths in every session template, worker worktrees included. This gives R4 a real enforcement point.
- **A5, a write journal** alongside any option: each write appends its before and after text to an append-only log, so a bad write can be found and undone.

#### Strongest counterargument
A lets one unattended model turn rewrite the system-role prompt of every future CoS session, and no human ever sees the diff.

The path: injected text in a worker report or a fetched page enters the session. From there it reaches the review fork (`review.js:63`, no tool call) or a CoS `memory_add`. It passes the strict regex scan; Q11 tested its patterns for false positives on five clean files, not for adversarial coverage (plan:43). It lands in the file, then rides into the standing block "outranking the global persona" (`standing.rs:60`) from the next session on.

Along that path there is no identity check (Problem 1), no history and no rollback (Problem 5). Hermes accepts this risk for a single-user local agent. The quaqua CoS reads the output of many agents and of the web, so its taint surface is larger (inferred). The owner chose the opposite that same morning (brainstorm:26), and the only evidence the plan records for the reversal is Hermes parity (plan:42).

#### Reversal conditions
Reverse to A0, A2 or A3 when any of these is observed:
- a line in either file that the owner did not expect, or a load-time `[BLOCKED` render (R6) of a line that came in through cos-memory;
- a review-fork remove or replace that deletes an owner-authored line (visible only if A5 exists; that it is otherwise invisible is itself a reason to ship A5 with A);
- a write from a non-CoS session (observable only with A4).

The code is cheap to reverse if approval ships as a toggle in cos-memory. The data is not: writes made before the reversal cannot be identified or undone without A5 (Problem 5). Classified as **hard to reverse** unless A5 ships with A.

#### Evidence gaps
- **Unknown: can cos-memory tell the CoS session from a worker? Settled: no.** `tools.js:121-127`, `memory-gate.js:16-20` and `caps.rs:137-163` take no identity input. The `QQ_AGENT` claim exists (`cli/mod.rs:148-150`), but it is unused on this path, and the engine treats it as "a hint, never authority" (`cli/mod.rs:157-158`).
- Unverified: whether worker and Lead sessions load cos-memory. This is inferred from the project-scoped `enabledPlugins`. To settle: list the `mcp__cos-memory__*` tools inside a worker session.
- Unverified: whether the fork sees tool results. The `$.model.fork` request is `{ prompt, effort? }` (README.md:53), and what transcript it sees was not read. To settle: read `index.d.ts:6738`.
- Unverified: the semantics of Hermes `_detect_external_drift` at main. This review had no network access.
- Unverified: how adversarial inputs fare against the strict scope. Only false positives were measured (plan:43).

### Report B — Q9: the gate returns `ask` on approval-list actions, with force push and pushes to main as hard `deny`

#### Problem
Decision: plan:19 (R7) and plan:41 (Q9). The approval list is `~/quaqua/AGENTS.md:13-21`.

Constraints that the plan's framing omits:
1. **The gate has no `ask` tier, and the port dropped it on purpose.** `Verdict` is `Allow | Block` (`authority/gate.rs:24-28`), and a block is exit 2 (`:15-16`). The port says the old ask and deny prompts "belong to the old CLI's surface, not to the daemon's doors" (commit 1e53ed3, gate.rs header; now `gate.rs:8-10`). R7 brings back a design the port rejected. The old tiers and their required precedence, Deny > Block > Ask > Allow, are documented in `docs/research/evidence/2026-10-19-quaqua-outgate.md:9-16, 120-121`.
2. **"Pushes to main stay hard deny" is true only on the Bash surface.** Every raw `git push` is blocked today, not only force pushes and pushes to main (`gate.rs:261-301`). The real door is the daemon `push` verb. The gate lets `quaqua …` through, because `check_words` has no `quaqua` arm (`gate.rs:181-192`). That verb has no main guard: it pushes `refs/heads/{b}:refs/heads/{b}` for any plain branch name, `main` included (`authority/push.rs:53-61, 133-136`). It checks only role, fence and own repo (`push.rs:97-131`, `authority/mod.rs:464-491`). The "worker pre-push guard … installed by the spawn path" (`init/hooks.rs:4-5`) has no implementation: `grep -rln pre-push crates/` finds only that comment. So the approval list's push item gets no ask, and main gets no deny, at the door that actually pushes.
3. **The gate is shared with workers and Leads, and a worker cannot answer an `ask`.** The driver installs the same `gate` in every worktree (`driver/claude.rs:48-69`), and the engine repo's own settings run it (`.claude/settings.json:6-12`). The gate is stateless and does not know who is calling (`gate.rs:1-6`). An `ask` in an unattended worker pane leaves the worker `blocked` until its deadline, and the flow then fails as BLOCKED (`task/worker_flow.rs:261-270`). Workers launch with no permission flag (`driver/claude.rs:40-46`), so they inherit `defaultMode: auto` (`~/.claude/settings.json:21`), where a hook `ask` forces a prompt (outgate.md:64-67).
4. **The matcher is Bash only** (`~/quaqua/.claude/settings.json:21-27`):
   - Outward sends and money spends mostly go through MCP tools (mail, chat, artifact publish, wallet top-up), which a Bash gate never sees.
   - Data delete through Bash (`rm`, `trash`, `sqlite3 … DELETE`, `>` redirects) has no bounded pattern set.
   - Deploy commands are an open-ended list.

   The Hermes research made the same call: "Spending money, sending externally stay prose" (hermes-source:118, variant E).
5. **The live CoS does not run with bypassed permissions** (the "ask under bypass" Unknown). `qq()` launches `claude --model claude-opus-5-5 --effort low` with no skip flag (`~/.zshalias:54-61`), and `defaultMode` is `auto` (`~/.claude/settings.json:21`). The template still says "the CoS runs with bypassed permissions" (`crates/qq-core/templates/workspace/.claude/skills/poteto-mode/SKILL.md:10`; `docs/research/plans/pstack-integration.md:167`), which is stale or unverified.
   - **Auto mode:** the docs say a hook `ask` forces a prompt (outgate.md:64-67, quoting docs and CHANGELOG v2.1.211; second-hand, 2026-10-19).
   - **bypassPermissions:** the docs say neither way (outgate.md:74-79). Unverified.

#### Alternatives
- **B0, approval at the daemon door:** keep the Bash block, and require an owner approval token in the daemon verbs (push, merge, a future release verb) before they act. There is one door, it knows the caller (`peer.rs:1-7`), and workers never meet a popup.
- **B1, split by caller:** `ask` in CoS sessions and `deny` in worker and Lead sessions. The gate learns the caller from a marker set by `install_hook` or from a pane lookup. This removes the unanswerable popup.
- **B2, hard deny everywhere:** every approval-list pattern gets `deny`, with the message "ask anh iu, then run it through <door>". The Hermes floor `_floor_block` is a deny that runs before yolo (hermes-source:70); it never relies on an `ask` surviving a permission mode.
- **B3, a narrow ask set:** cover only items with bounded patterns (`git branch -d/-D`, `git push --delete`, `git tag`, `gh release create`, history rewrites on shared refs). Leave send, spend, deploy and data delete as prose, plus per-tool MCP matchers.
- **B4 (needed under every option):** a main guard in `push.rs`. Without it, "pushes to main stay hard deny" is false today.

#### Strongest counterargument
`ask` is the weakest tier the gate could add:
- It is unverified in the mode the template claims the CoS uses (bypass).
- It hangs every worker that trips it (Problem 3).
- It cannot see sends and spends that go through MCP tools (Problem 4).
- It leaves the real push door open to main (Problem 2).

It will also teach the owner to approve without reading. Cleanup is on the no-approval list (AGENTS.md:23), but deleting a worker branch is on the approval list (AGENTS.md:16). The two collide at every worker cleanup, so routine work would raise a steady stream of popups. The port's author had already judged ask prompts to be old-CLI surface (`gate.rs:8-10`). Deny at the edge plus approval at the daemon door (B0 or B2) puts the decision where the caller is known.

#### Reversal conditions
**Cheap to reverse.** Changing one pattern between ask and deny is a one-line `Verdict` change plus a test, because the gate is pure and stateless (`gate.rs:1-6`). Reverse when any of these is observed:
- a worker run ends BLOCKED or TIMEOUT with a gate `ask` in its pane (`worker_flow.rs:261-270`);
- a live probe shows an `ask` passing with no prompt in the CoS's actual mode;
- the owner approves popups faster than they could read them (count `ask` events per day in transcripts);
- an approval-list action happens through an MCP tool or a daemon verb with no prompt, which shows the Bash-only gate gave false assurance.

The one part that is not cheap is B4. It is independent of this choice and already a live gap.

#### Evidence gaps
- **Unknown: is `ask` honoured under bypassed permissions? Unverified.** The only source is outgate.md:74-79, second-hand from 2026-10-19. To settle: in a scratch directory, add a PreToolUse hook that returns `permissionDecision: "ask"` for `echo probe`. Run it once with `--dangerously-skip-permissions` and once in auto mode, and record whether a prompt appears.
- Unverified: whether herdr starts the CoS pane with other flags. Only the `qq()` path was read.
- No qq-core test covers an `ask`. The only decision test asserts `deny` (`crates/qq-core/tests/ported.rs:144`).
- No inventory exists of the MCP tool names for send and spend actions.

### Report C — Q7: a frozen standing block, with a worker-state delta in the cos-inbox digest

#### Problem
Decision: plan:20 (R8) and plan:37 (Q7). The motivating miss: "a lead and peer idled 6 h unseen" (brainstorm:14), and the weakest assumption the brainstorm names: a snapshot frozen at session start cannot catch a worker that stalls mid-session (brainstorm:32).

Constraints that the plan's framing omits:
1. **Delivery is pull-only, on CoS turns.** The digest runs on UserPromptSubmit and Stop (`~/quaqua/.claude/settings.json:42-55`), and the daemon never nudges the CoS (`bus/mod.rs:267-274`: "The CoS is never nudged"). A worker that stalls while the CoS waits for the owner produces no turn, so the delta arrives only when the owner next types. The 6-hour miss repeats whenever the owner is away (inferred from these two facts).
2. **Leads and peers have no watcher.** `watch_agent` is called only by the worker workflow (`task/worker_flow.rs:646`) and the `watch.agent` verb (`daemon/mod.rs:358-366`); nothing else in `src/` calls either. `run_lead` registers no watcher (`task/spawn_cmd.rs:849-1000`). The motivating miss (brainstorm:14) was a Lead and a peer.
3. **The existing source covers only part of R8.**
   - It covers task workers: `watch_agent` signals `herdr/status` on each change among idle, working, blocked and done into that worker's workflow journal (`daemon/watch.rs:44-95`, `worker_flow.rs:225-231`).
   - It reports nothing on exit: `agent_not_found` returns silently (`watch.rs:72-73`), and the watcher stops when its workflow stops (`watch.rs:54`).
   - "Stalled" is computed only inside the blocking `quaqua prompt` wait loop (`task/prompt.rs:18, 224, 785-836`), never as an event.
   - `herdr::liveness::status_from` has no caller outside its own module.
   - `blocked` is a real herdr state (`watch.rs:26`), but R8's list leaves it out.
4. **Silence is ambiguous when the daemon is down.** The hook returns empty on any RPC error (`memory/inbox.rs:66-70`), and the wrapper exits 0 on any failure (`cos-inbox.sh:10-11, 55`). "Silent when nothing changed" then looks the same as "the daemon is down".
5. **The Stop form costs a turn.** On Stop, the digest blocks the turn (`inbox.rs:104-106`), once per turn (`cos-inbox.sh:25-31`). State changes are far more frequent than mail; idle and working flip on every prompt. Every non-empty delta at Stop forces one more CoS turn (inferred cost).
6. **"Frozen for the session" is already false after a compaction.** A `compact` source re-renders the block (`memory/mod.rs:147-151`; `drift.rs:75-80`), as Hermes rebuilds on compression (hermes-source:52). Only `resume` keeps the old bytes. So writes made under A appear mid-session after a compaction, which contradicts "never re-rendered mid-session" (plan:20).

#### Alternatives
- **C0:** a frozen block and no mid-session freshness. Rely on the auditor, which already flags stalled workers (`auditor/prompt.rs:50`), and on herdr notifications.
- **C1 (leading):** the delta in the digest, pulled on CoS turns.
- **C2, push through mail:** on each transition, the daemon mails `cos` a `kind=state` line. This reuses `mail.send` and its stored cursor, so no new event source is needed, and the existing digest already renders mail. For stalled, blocked and exited, it also raises a herdr notification to the owner, which `bus/mod.rs:253-259` already does for escalate and blocked mail. This covers the case where the owner is away.
- **C3, wake the CoS:** let `wake_idle` reach `cos` for stalled and exited only; the switch is `bus/mod.rs:271-274`. Risk: the CoS takes turns while the owner is away.
- **C4, poll on demand:** a `quaqua situation` CLI that exposes the V2 function, called by the CoS when it wants fresh state. It adds no hook contract and is cheap to change.

All of C2–C4 also need a watcher on Leads and peers (Problem 2).

#### Strongest counterargument
C1 answers "how do we keep the cache warm", not "how does anyone notice a stalled agent".

Delivery is pull-only (Problem 1), and Leads and peers have no watcher (Problem 2). So C1 does not catch the exact miss that motivated Q7. It still adds a new engine event contract, a "worker-state event source" with its own "since last digest" cursor, as a hook output that other tools will key on.

A cheaper channel already exists. Mail to `cos` is stored, tracked by cursor, digested by the same hook and notified to the owner today (`memory/inbox.rs`, `bus/mod.rs:250-262`). A second cursor just for states duplicates it.

#### Reversal conditions
**Moderate cost to reverse.** The hook text is cheap to change, but the event source and its cursor become persisted state in qq.db and a contract that the digest consumes. Reverse or extend when any of these is observed:
- an agent stays stalled, blocked or exited past the stall bound (`STALL_SEC = 600`, `task/prompt.rs:18`) with no CoS turn in between, visible from qq.db agent rows and transcript timestamps;
- a Lead or peer stall goes unseen again (Problem 2), the 6-hour case;
- Stop-time deltas force more than a few extra turns per session;
- daemon-down silence hides a stall (Problem 4).

#### Evidence gaps
- **Unknown: does existing engine state already expose worker-state transitions? Partly.**
  - Yes, for task workers' idle, working, blocked and done: `watch.rs:44-95` feeds the workflow journal, which appends through `daemon/store.rs:29-33`.
  - No, for exit (`watch.rs:72-73`), for stall (`task/prompt.rs:224` computes it locally in the CLI wait), and for Leads and peers, which have no watcher.
  - Not read: whether the journal can be queried across workflows to drive a digest cursor (`engine/journal_sqlite.rs`).
- Unmeasured: whether putting the delta in the system role would break the prompt cache. The plan asserts that the user-turn placement keeps the block cached (plan:37).
- Unmeasured: the rate of idle and working flips per session, which sets the turn cost in Problem 5.

## Verify
Verified: inconclusive cf61565. This is a read-only review: no build or test ran, as the brief requires. Every claim cites a file and line or is marked inferred or unverified.

Commands that settled each Unknown:
- **Is `ask` honoured under bypass?**
  - `grep -rn 'bypassPermissions\|"ask"' docs crates` found the evidence in outgate.md:61-79.
  - `sed -n 54,61p ~/.zshalias` showed that `qq()` passes no skip flag.
  - `grep -n defaultMode ~/.claude/settings.json` found `auto` at line 21.

  Result: unverified under bypass. The live CoS runs in auto mode.
- **Does engine state expose worker-state transitions?**
  - `sed -n 1,120p crates/qq-core/src/daemon/watch.rs`.
  - `grep -rn 'watch.agent\|watch_agent' crates/qq-core/src` found callers only at `worker_flow.rs:646` and `daemon/mod.rs:365`.
  - `grep -rnwE 'stall|stalled|STALL[A-Z_]*' crates/qq-core/src` found only `task/prompt.rs:18, 224` and a comment in `liveness.rs`.

  Result: partly.
- **Can cos-memory tell the CoS from a worker?**
  - `cat -n mods/cos-memory/src/{tools,memory-gate,review}.js` and `mods/cos-memory/hooks/register.js`.
  - `caps.rs:137-163`, `identity/peer.rs:1-7`, `cli/mod.rs:148-158`.

  Result: no.
- **Supporting checks:**
  - `git show 1e53ed3 -- crates/qq-core/src/authority/gate.rs`: the `ask` tier was dropped on purpose.
  - `grep -rln pre-push crates/`: the pre-push guard appears only in a comment.
  - `git -C ~/quaqua log -- USER.md`: not a git repository.
  - `sed -n 849,1000p crates/qq-core/src/task/spawn_cmd.rs`: Leads run in worktrees and get no watcher.

## Risks
- The plan is a live file, and its line numbers may shift. They were read on 2026-11-18.
- Several Claude Code behaviors are second-hand or inferred, and are marked where they appear: `ask` in auto mode, project-scoped plugin enablement, and what the fork sees.
- The Hermes facts come from worker notes at f7e2898 and were not re-read here; the plan's authority line, plan:11, says the same.
- Found outside these three decisions: the daemon `push` verb accepts `main` (`push.rs:53-61, 133-136`), and the documented worker pre-push guard has no implementation. This is a live gap whatever is chosen for B.

## Lessons
- Before keeping a "stays deny" promise, check every door to the action, not just the Bash gate. Here the daemon push verb has no main guard.
- Before arguing a per-write approval policy, look for writers that run unattended. Here the review fork already writes both memory files.
