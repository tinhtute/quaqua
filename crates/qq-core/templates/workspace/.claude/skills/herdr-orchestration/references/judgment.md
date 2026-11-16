# Assignment and acceptance judgment

`AGENTS.md` owns the SLP authority model: Principal sets goals/trade-offs; CoS handles intake and cross-project coordination; a project Lead owns plans and integration acceptance; each Peer owns one bounded task. This reference adds judgment checks, not another authority model. Pipeline review/plan gates remain mandatory regardless of the optional extra-review guidance below.

## Bootstrap and neutral assignment

1. Resolve the real repo root and read its agent instructions and contract boundaries. Missing authority or an unsettled boundary → ask, not guess.
2. Query live agent names/panes via Herdr. Inspect existing uncommitted work; give writers separate worktrees rather than touching someone else's checkout.
3. Capability is not authority. Repo restrictions beat a seat's tools/skills; a vanished tool may be `permissions.deny`, not a broken environment. Experiment rigs go in `/tmp`.

For an independent bounded Peer, open with `Read ~/quaqua/PEER.md first.` and name task ID, real repo/cwd, disposition (Engineer/Architect/Reviewer/Scout), owned/excluded paths, authority (local commit, never push/deploy/external calls), verification commands/resource ownership, and report path. Use the five Goal / Hard constraints / Current design / Unknowns / Evidence sections from `rules/_all/brief.md`.

- Do not pre-solve the cause or freeze an uncertain design. A detailed plan is provisional; the builder proposes its own short plan before a pipeline Lead approves code.
- New-boundary tests need a settled contract from the brief/spec/code. If those conflict, the Lead rules with evidence before the Peer creates a public method or fixes semantics just to make a test compile. Don't demand compile bridges for each slice; require a buildable final commit and real verification.
- Pick model/effort from the roster, not ad-hoc changes. Split work when a turn cannot fit its context (aim below about 120K peak); parallel isolation does not guarantee throughput.
- A challenge with evidence is data, not disobedience. A reopen request identifies foundation/dependency/lifecycle/API/ownership/verification; a dependency request goes to the missing scope's owner. Resolve the challenge before acceptance. Goal/cost changes escalate through CoS to the Principal. Record steering in Lead state.

## Ownership and evidence

One moving scope has one writer. The Lead never patches a delegated scope. Each writer commits locally; handbacks name SHA + branch + worktree. Review immutable objects (`git show "$sha":path`, `git diff "$sha^" "$sha"`), not an unrelated checkout's current disk. Always quote `"$sha"`; an empty unquoted variable silently selects HEAD. Corrections get a new commit, never an amended handback SHA. Acceptance never implies push, deploy, or a main merge.

A single lane owns shared test resources (full suite, ports, test DB, benchmark); the brief names it. Unknown remains unknown. Idle/done/exit 0 or an agent's claim of green tests is a wakeup signal, not acceptance.

## Optional extra review and blind design lanes

A writer plus an independent Lead already separates implementation and acceptance. Outside the pipeline's automatic review gate, add a fresh Reviewer only when it can change the decision:
- the brief fixed the solution and the Principal will not review it;
- a repo-marked decide-first boundary is touched;
- reversal is costly (migration/schema/public API/data deletion);
- the proof is suspicious (would it still pass if the behavior vanished?); rerun the exact command first;
- a Peer withdrew a reopen request without new evidence.

A Reviewer gets exact SHA, owned file list, contract, and verification commands; read-only, fresh session, different kind/harness from the writer. Generic review of disk HEAD is not review of that SHA. A new commit voids old review; fixes go back to the Engineer and re-review uses a fresh session.

For hard-to-reverse design decisions, ask the Principal before buying two blind architecture lanes: fresh neutral briefs, neither lane sees the other's answer or the Lead's framing. Converge to one evidence-based ruling; agreement count is not authority. At most one rebuttal round, then rule. These lanes precede code, unlike review of a committed diff.

## Acceptance checklist

- `git cat-file -e "$sha^{commit}"` passes; `git show --stat "$sha"` matches claimed files.
- Read the actual diff and rerun the brief's exact verification command; report real output.
- Review required by the pipeline or risk conditions exists for this exact snapshot.
- Identify who settled each new public contract; resolve every `## Challenge` and finding or explicitly escalate it.
- A handback is the `runs/<name>.md` report in the one format from `rules/_all/brief.md` (`## Report format`): the six fields, then `## Blockers` / `## Lessons`, or `Verdict:` for a pipeline reviewer; don't invent missing fields.
- Rewrite the card's `## Now`, append the daily outcome/decisions, promote at most one durable lesson via CoS, and clean up verified task workers immediately.

## Failure smells

Third patch for the same symptom → inspect the mechanism, not one more patch. If ownership/lifecycle cannot be stated in one sentence, use a deletion test to see what behavior the abstraction buys. Fresh neutral framing avoids inheriting the Lead's assumptions. More review without decision-changing evidence is ceremony. Two identical failures mean inspect prerequisites/quota/auth; same error for three worker iterations triggers the kill/fresh-worker rule in `AGENTS.md`.
