# Never Block on the Human

The human supervises asynchronously. Workers and coordinators must stay unblocked on reversible work. Make reasonable decisions, proceed, and let the human course-correct from the result.

## Why

Waiting on a question for reversible work stalls the pipeline, burns human attention on low-stakes forks, and costs more in delay than a quick, easily reverted decision.

## When

Tempted to ask "should I do X?" on reversible work.

## Rule

- **Proceed, then present.** Do the work, show the result. Do not ask "should I do X?" Do X, explain why, and let the human review.
- **Make the system self-healing.** When you notice a problem, log it and fix it in the next round.
- **Reversible actions proceed.** Writing code, editing notes, running tests, or decomposing tasks should proceed without blocking.
- **Irreversible actions still require approval.** Never bypass approval on irreversible actions (deleting data, push or force-push, branch delete or merge to main, deploy, spending money, sending/publishing anything outward).
- Product direction comes from the human. Technical execution should not block.
