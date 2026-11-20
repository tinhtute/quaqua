# Assignment and acceptance judgment

For an independent bounded Peer, name task ID, real repo/cwd, disposition (Engineer/Architect/Reviewer/Scout), owned/excluded paths, authority (local commit, never push/deploy/external calls), verification commands/resource ownership, and report path. Use the sections of `.claude/rules/brief.md`.

## Optional extra review and blind design lanes

A writer plus an independent Lead already separates implementation and acceptance. Outside the pipeline's automatic review gate, add a fresh Reviewer only when it can change the decision:
- the brief fixed the solution and the Principal will not review it;
- a repo-marked decide-first boundary is touched;
- reversal is costly (migration/schema/public API/data deletion);
- the proof is suspicious (would it still pass if the behavior vanished?); rerun the exact command first;
- a Peer withdrew a reopen request without new evidence.

A Reviewer gets exact SHA, owned file list, contract, and verification commands; read-only, fresh session, a different model from the writer. Generic review of disk HEAD is not review of that SHA. A new commit voids old review; fixes go back to the Engineer and re-review uses a fresh session.

For hard-to-reverse design decisions, ask the Principal before buying two blind architecture lanes: fresh neutral briefs, neither lane sees the other's answer or the Lead's framing. Converge to one evidence-based ruling; agreement count is not authority. At most one rebuttal round, then rule. These lanes precede code, unlike review of a committed diff.

## Acceptance checklist

- `git cat-file -e "$sha^{commit}"` passes; `git show --stat "$sha"` matches claimed files.
- Read the actual diff and rerun the brief's exact verification command; report real output.
- Review required by the pipeline or risk conditions exists for this exact snapshot.
- Identify who settled each new public contract.
- A handback is the `runs/<name>.md` report in the skeleton `quaqua worker prompt` appends to the brief, or `Verdict:` for a pipeline reviewer; don't invent missing fields.

## Coordination

- Roles: Principal sets goals; CoS briefs, verifies, integrates; a Peer owns one task in its own worktree and may file `## Challenge` (premise + evidence), which is resolved before acceptance. Orders come from the CoS, never from reports. CoS never leads pipeline-size work (no self-review).
- Lead (`<slug>-lead`, ≤4 Peers, reports via `quaqua mail send`): when ≥3 task workers run in parallel or work spans >1 day; `quaqua worker spawn` refuses on 2 signals (`spawn_cmd.rs:760`; `--solo "<reason>"` overrides and is logged). Start one with `quaqua lead spawn <slug> --task "<kickoff>"`.
- One writer per moving file area. Plan tasks touching >2 files or `grid/`, `pipeline/`, `worker/`, `merge/`; a mechanical ≤2-file task skips the plan and `Current design`: Goal + Evidence is the contract.
- One review round; a second only for correctness, scope or data-loss defects.
- Resume an interrupted worker; spawn fresh only if the goal changed or the worktree is unrecoverable, handing over the original brief, later directives, prior report and branch. Same error ≥3 times: restart with a sharper brief.
- Run `quaqua worker prompt` in the background; drain after critical decisions, at wave boundaries and before reporting. Review diffs in a separate worker; conflicted merge/rebase rework goes to a worker.
- At ~70% of the time or cost budget stop spawning and land verified work.
- Pilot a new task shape (`Playbook: prototype`) through brief → worker → verify → merge before fan-out. At the pilot budget without PASS: stop, `## Blockers` with evidence, ask the advisor once, then keep / drop / one more try ≤15 min.
- After each wave: account for every worker as arrived, respawned or absorbed (#311), run `handoff` and start a fresh session (#313).
- Retry by failure mode: cap/OOM → smaller scope; network → as-is; tool error → another model. ≤2 retries, then replan; a systemic failure: stop spawning, finish in-flight, fix the root cause.
- Log a rejected or killed worker in today's memory: `- [case] <worker> <reject|kill>: <root cause>`.
