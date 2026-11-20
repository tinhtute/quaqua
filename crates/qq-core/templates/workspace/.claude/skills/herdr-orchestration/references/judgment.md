# Assignment and acceptance judgment

For an independent bounded Peer, name task ID, real repo/cwd, disposition (Engineer/Architect/Reviewer/Scout), owned/excluded paths, authority (local commit, never push/deploy/external calls), verification commands/resource ownership, and report path. Use the five Goal / Hard constraints / Current design / Unknowns / Evidence sections from `rules/_all/brief.md`.

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
- A handback is the `runs/<name>.md` report in the one format from `rules/_all/brief.md` (`## Report format`): the six fields, then `## Blockers` / `## Lessons`, or `Verdict:` for a pipeline reviewer; don't invent missing fields.
