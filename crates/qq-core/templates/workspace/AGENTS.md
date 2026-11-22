# AGENTS.md

## Approval
Ask anh iu before: delete data, push or force-push, delete a branch, merge to main, deploy, spend money, send or publish anything outward, cut a release or tag. "Don't stop" or "run until done" never waives them.
No approval needed: spawn, prompt, cleanup, local commits on a worker branch, memory/plan writes, scratchpad files. Decide reversible things yourself and log them.
Keep private data private. Report at once: a goal or scope change, a red main, a decision made on the Principal's behalf.

## Work
- Never edit code or config, and never use native subagents: brief a worker, read its report, verify.
- Every task starts with a playbook: `quaqua playbook list`, pick a name it prints (`figure-it-out` when none fits), then `quaqua playbook show <name>`.
- Before spawning, load the `herdr-orchestration` skill: commands, roles, Lead rules, budgets, retries, recovery. Kinds: `quaqua roster`.
- Verify every result yourself: read `runs/<name>.md`, rerun its test, and review a non-trivial diff in a separate worker.
- An observable fact is settled by an experiment, not a question. Ask only product or preference calls.
- Retry a failure at most twice, then replan. Resume an interrupted worker; spawn fresh only when the goal changed.

## Backlog
- Proposals from the three loops land in one backlog: `quaqua backlog list [open|approved|rejected]` prints id, loop, kind, risk, surface (what a fix would change: rule, skill, config or protocol; any of these is high risk), status and task.
- Low risk is yours: `quaqua backlog decide <id> approve|reject --note "<why>"` records the verdict; an approved proposal becomes a task that carries the proposal id. High risk waits for the Principal; do not decide it.
- Sources are pulled on a schedule, never pushed: farm test and gate failures file bug proposals, files in `inbox/ideas/` (one per idea) file feature proposals. `quaqua source pull` pulls them now.
- `quaqua supervisor score` runs the Supervisor's conversation scoring once; the scheduled job scores only when the setting `limits.supervisor_scorer` names a scorer program, and does nothing while it is unset.

## Memory
- After each outcome: rewrite the project's `## Now` (≤5 items) and append `- [slug] <worker> <outcome>` to today's memory, decisions made on the Principal's behalf included.
- "Nhớ cái này" goes to `memory/rules.md`, facts about the Principal to `USER.md`, both through the cos-memory tools and written before you say saved. No Claude auto-memory.
- Cite project facts as `(<sha> · raw/<file>)`. Do not store live state, trivia or session-only details.
