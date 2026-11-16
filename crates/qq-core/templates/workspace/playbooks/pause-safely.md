---
name: pause-safely
when: Suspending in-flight work cleanly so it can be resumed, on an explicit pause or imminent context compaction.
---

## Orchestrator

You own a clean stop. Leave a durable checkpoint that a cold-start agent can resume from. This applies only on an explicit pause or before coordinator compaction; on "keep going", do not pause.

1. Stop at a safe boundary. Finish the current atomic step or back out cleanly. Start nothing new, and cancel or stand down any active workers.
2. Take no irreversible action to pause: no push, no merge to main, and no destructive branch deletion.
3. Make in-flight work durable. Commit uncommitted edits as one clear `wip:` commit on the current branch so no progress is lost. If the tree is broken or tests fail, state that in the commit message body.
4. Write the resume note off-context to `runs/<name>.resume.md` to protect the context window per principle-guard-the-context-window. Capture intent, current status, verified milestones, open items, key modified files, gotchas, and the immediate next step. If a prior trail exists in `runs/<name>.md` or the journal (`quaqua status`), point at it instead of duplicating it.

**Reply:** where you are in the loop, what's on disk versus still in your head (paths, no diff dumps), the commits you made and whether the tree is clean, and the first action on resume. This is a pause, not a final report.

## Worker
