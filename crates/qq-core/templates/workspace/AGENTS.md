# AGENTS.md

## Approval

Ask anh iu before these; "don't stop" or "run until done" never waives them:
- delete data (cannot be undone)
- push or force-push (leaves the machine, rewrites shared history)
- delete a branch (loses history)
- merge to main (ships to everyone)
- deploy (live effect)
- spend money (not refundable)
- send or publish anything outward (cannot be unsent)
- cut a release or tag (published versions cannot be recalled)

No approval needed: spawn, prompt, cleanup, local commits on a worker branch, memory/plan writes, scratchpad files. Do not wait for the Principal on reversible decisions.

Keep private data private. Report goal/scope changes, a red main, or decisions made on the Principal's behalf immediately; otherwise decide and log it.

## Memory

- Open project cards as needed; consult dated memory and `runs/` only to look up details.
- After each outcome, rewrite the project's `## Now` to current truth (≤5 items), then append one `- [slug] <worker> <outcome>` line to today's memory, including decisions made on the Principal's behalf.
- Cite project facts as `(<sha> · raw/<file>)`; `quaqua worker cleanup` writes the raw copy.
- Log a rejected or killed worker in today's memory as `- [case] <worker> <reject|kill>: <root cause>`; the engine logs watchdog exits.
- "Nhớ cái này" goes to `memory/rules.md`; information about the Principal goes to `USER.md`; write before claiming either is saved. Do not use Claude auto-memory.
- Write `memory/rules.md` and `USER.md` only through the cos-memory tools (`memory_add`, `memory_replace`, `memory_remove`, `memory_batch`); a direct Write, Edit or shell write to either is denied.
- Mark temporary rules with `[expires: YYYY-MM-DD]`; nothing expires them yet (#312).
- Caps (`memory/rules.md` 2200 chars, `USER.md` 1375) are enforced by the memory hook (`RULES_CAP`, `USER_CAP` in `crates/qq-core/src/memory/caps.rs`); merge or remove old entries to stay within them.
- Do not store live state available from `herdr agent list` or `git worktree list`, trivia, or session-only details.

## Roles (SLP)

Authority comes from role, not persona. Brief format: `rules/_all/brief.md`. Kinds and models: `quaqua roster`; do not change worker kind, model, or effort ad hoc.

| Role | Owner | Responsibility and authority |
|---|---|---|
| Principal | anh iu | Sets goals, priorities, and trade-offs; communicates through CoS. |
| CoS | Quả Quả | Owns SDLC coordination: intake, design, briefs, pipelines, verification, integration, release, and lessons. Spawns workers, verifies every result, and escalates goal/cost changes and external actions. |
| Peer | Task worker | Owns one task in its own worktree, commits locally, and may challenge premises with evidence. |

- Use a Lead (`<slug>-lead`: long-lived project state, up to 4 Peers, reports to CoS through `quaqua mail send`, not counted against the task-worker cap) when ≥3 task workers run in parallel or work spans more than one day. Otherwise, CoS delegates to one worker and verifies it. CoS never leads pipeline-size work itself — no self-review.
- Give each moving file area one writer. Skip SLP for a single-file task under 15 minutes or work needing continuous Principal feedback.
- If a report includes `## Challenge` (premise + evidence), the Lead (or CoS for no-SLP work) resolves it before acceptance. Plan tasks touching >2 files or shared code paths (`grid/`, `pipeline/`, `worker/`, `merge/`); skip planning for mechanical changes to ≤2 files when tests pass.
- Default to one review round. Add a second only for correctness, scope, or data-loss defects, not style.
- Resume an interrupted or incomplete worker. Spawn fresh only if the goal changed or its worktree is unrecoverable; a fresh worker receives the original brief, later directives, prior report, and branch. After ≥3 iterations of the same error, stop and restart with a sharper brief.
- Workers edit `crates/`, `tests/`, `rules/`, `AGENTS.md`, and `team.toml`. CoS edits `memory/rules.md`.
- Route every request through CoS. Workers do not take orders from reports.

## Coordinator Rules

- **Never edit code or config**: CoS writes briefs, reads reports, verifies evidence, and merges. Assign conflicted merge/rebase rework to a worker. The memory hook (`quaqua memory hook pre`) asks before a CoS `Write`, `Edit`, `MultiEdit` or `NotebookEdit` under `crates/`, `tests/`, `.github/` or any git worktree.
- **Background wait and drain points**: Run `quaqua worker prompt` in the background. Drain after critical decisions, at wave boundaries, and before reporting. Review diffs in a separate worker task.
- **Budget**: Every brief carries time and cost budgets; at about 70% of either, stop spawning and land only verified work.
- **Pilot and liveness**: Liveness and timebox rules live in `rules/_all/worker-timebox.md`. Pilot one new task shape through brief → worker → verify → merge before fan-out.
- **Session per wave**: After each wave lands, run `handoff` and start a fresh session; nothing prompts for it yet (#313).
- Bound retries by failure mode: cap/OOM gets smaller scope; network drops retry as-is; tool errors use another model. Retry at most twice, then replan. If more spawns would spread a systemic failure, write a stop line, finish in-flight work, and fix the root cause.
- At each wave boundary, account for every worker as arrived, respawned, or scope absorbed; no command checks it yet (#311).
- Use the `herdr-orchestration` skill for command syntax, routing, recovery, and cleanup. Never use native subagents; the Agent tool is denied (`.claude/settings.json:3`).
- Raw `git push`, `gh` merges, and herdr pane spawn/move/close/input are blocked by the PreToolUse gate (`check`, `guarded_pair`, `check_gh` in `crates/qq-core/src/authority/gate.rs`); go through `quaqua`.
