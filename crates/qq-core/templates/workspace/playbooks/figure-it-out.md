---
name: figure-it-out
when: Large migrations, multi-part changes, or work when no narrower playbook fits.
---

## Orchestrator

When the task matches no narrower playbook, design an auditable workflow. The deliverable before any code is the workflow itself: a sequence of phases that scales rigor to the task, runs the scientific method, and leaves an audit trail a human can inspect after stepping away (commit the trail with the change so the reviewer reads it).
### Phase A: Frame
1. Ground first, then commit. Don't start the run until stating:
   - The definition of done as a falsifiable predicate.
   - Scope quantified: rough units and effort, plus blockers that grounding surfaced.
   - Rigor level, biased high. One-way doors and high blast radius get more rigor; reversible low-stakes steps get less. Rigor is gates and artifacts, not "try harder".
2. Present the framing and trade-offs before committing to a long run. Reversible work proceeds without blocking on the human, but a multi-hour run earns one checkpoint.

### Phase B: Design the workflow
3. Decompose the goal into atomic, independently landable units. Sequence riskiest-unknown-first. Scaffold and verification checks come before features.
4. Build the verification harness before the work, capturing pre-change baseline measurements so checks read as "old value vs new value".
5. For one-way-door architectural decisions, explore competing designs before settling implementation. Skip exploratory bakeoffs for mechanical work whose shape is already concrete.
6. Decide what fans out: parallelize only across seams, and assign each worker its own worktree or branch. Never over-fan.
7. Write the designed phase list down: that list is what the human reviews before and during execution.
8. If two or more fixes or hypotheses share one premise and fail the same gate, suspect the premise, stop patching, write the premise down, and test it directly before proceeding (citing principle-attack-the-premise).
9. Pair delegated work with a judge (independent, not optional). If a worker games the gate, reset and harden the contract; if the gate itself is wrong, fix the gate in its own change rather than routing around it.
10. Final check: verify the whole against the Phase A done predicate on the real product, not just the harness. Encode any recurring correction structurally as a gate, lint rule, check, or script.

## Worker

Execute each unit under experimental discipline, recording decisions as units land.

1. Frame the unit outcome as a falsifiable done predicate and establish baseline measurements before modifying code. Quantify scope in units, effort, and known blockers.
2. Order execution into atomic, independently landable units sequenced riskiest-unknown-first, verifying each before advancing, citing principle-sequence-verifiable-units. Scaffold and verification checks come before feature logic.
3. When multiple attempts fail the same gate, stop and test the shared premise directly, citing principle-attack-the-premise.
4. Run each unit as a hypothesis experiment: state the hypothesis, make the smallest change, measure against the predicate on the real artifact (never rely on a self-report; when something passes too easily, suspect the observation method before the system). Keep the change if it advanced the predicate; revert it if it did not.
5. Record verdicts as VERIFIED, NOT VERIFIED, or INCONCLUSIVE. Inconclusive is not a pass. Don't hide a negative. Maintain an audit trail row as each unit lands, and commit the trail with the change so the reviewer reads it.
6. Check the whole against the done predicate on the real product, not just the harness. Propose recurring corrections as structural gates rather than prose reminders.
7. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.

**Reply:** the playbook you designed, the rigor level and why, the decision trail path, what is verified against the predicate, and what is still open.
