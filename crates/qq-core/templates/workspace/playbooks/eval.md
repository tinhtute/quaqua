---
name: eval
when: Testing how a skill, structure, or prompt change affects agent behavior before promoting it, or auditing playbook spread and telemetry across runs.
---

## Orchestrator

1. You own the experiment and evaluation design: plan, blind, run, and synthesize. The deliverable is an empirical verdict and synthesis with a recommendation, not an ungrounded opinion.
2. Two evaluation modes:
   - Active comparative experiments: test prompt, rule, or skill changes before promotion by spawning N parallel candidates across different worker kinds per `models.md` and a blinded judge on a different model family.
   - Observational telemetry evaluations: read `runs/` reports (`runs/*.md`) and journal telemetry (`runs/quaqua.db`) to audit playbook spread, adoption, and brief defects across completed work.
3. Blinding non-negotiables:
   - No `eval`, `test`, `judge`, `experiment`, `rubric`, `score`, `compare`, `benchmark`, `candidate`, or `arena` in any directory, file, or prompt the candidate sees.
   - The candidate prompt looks like an organic user request: state the goal, not the meta.
   - No chain-eliciting cues: do not ask candidates to list which skills, principles, or files they applied; ask for design notes generally and grade chain-following from code shape and tool usage, not self-report.
   - Sanitize directory and slug names using realistic project names.
   - Do not reveal other candidates exist.
   - The judge can know it's judging but sees outputs by sanitized label only, never by model or kind name.
   - Comparing two variants: one judge scores both sets in a single pass on one scale, blind to which set each came from.
4. Run procedure:
   1. Frame: state what variant is under test or the audit scope, and what behavior counts as success. Write the rubric (3–6 concrete criteria) for the judge only and hold it back from candidates. For telemetry evaluation, define the target window and metrics.
   2. Set up sanitized environments or data sources: for active experiments, create per-candidate working directories with the variant in place, planting organic context. For telemetry evaluation, locate workspace `runs/` reports and journal database `runs/quaqua.db`.
   3. Author one organic prompt or telemetry query: for active experiments, author what a user would type, with no leakage of what is being measured. For telemetry evaluation, prepare queries for the playbook spread (every name, `none`, `figure-it-out`), `playbook_gap` events (note: recorded by S5; note absence if not yet in crate), and briefs with a missing header that ended in a failed or respawned unit.
   4. Spawn N parallel candidates: spawn N parallel candidates across different kinds per `models.md` via `quaqua worker spawn`, each in its own sanitized directory with the identical prompt.
   5. Spawn one blinded judge: spawn one blinded judge on a different model family per `models.md`. The judge evaluates candidate outputs against the rubric by sanitized label only, never by model or kind name.
   6. Verify the chain from transcripts, not self-report: read candidate transcripts off-context per principle-guard-the-context-window. Look at which files each candidate actually opened and which tools ran. Grade chain-following from the files it really read plus the shape of the code, never from candidate self-reports.
   7. Read every candidate output end to end and compare to the judge's verdict, interpreting numbers and scores per principle-explain-the-number. Disagreement means a model is biased or the rubric is ambiguous. Synthesize.
5. In observational telemetry eval, read `runs/` reports and telemetry (`runs/quaqua.db`): report the playbook spread (every name, `none`, `figure-it-out`), `playbook_gap` events (when recorded by S5 report check), and briefs with a missing header that ended in a failed or respawned unit.
6. Protect context per principle-guard-the-context-window: summarize candidate transcripts and runs off-context; do not inline full transcripts in the coordinator thread.
7. Verify from transcripts and evidence, not self-report. Read candidate transcripts and diffs end to end. Compare human review to the judge's verdict: disagreement means a model is biased or the rubric is ambiguous.

## Worker

1. Frame the evaluation. State what variant is under test or the audit scope, and what behavior counts as success. For comparative experiments, write the rubric (3–6 concrete criteria) for the judge only and hold it back from candidates. For telemetry evaluation, define the target window and metrics.
2. Set up sanitized environments or data sources. For active experiments, create per-candidate working directories with the variant in place, planting organic context. For telemetry evaluation, locate workspace `runs/` reports and journal database `runs/quaqua.db`.
3. Author one organic prompt or telemetry query. For active experiments, author what a user would type, with no leakage of what is being measured. For telemetry evaluation, prepare queries for the playbook spread (every name, `none`, `figure-it-out`), `playbook_gap` events (note: recorded by S5; note absence if not yet in crate), and briefs with a missing header that ended in a failed or respawned unit.
4. Execute candidate evaluation or aggregate telemetry:
   - For active experiments, run the candidate task or collect outputs across candidate runs, each in its own sanitized directory with the identical prompt.
   - For telemetry evaluation, aggregate counts across `runs/*.md` reports and journal events for playbook spread, recorded gaps, and brief defects.
5. Execute blinded judging for active experiments: evaluate candidate outputs against the rubric on a separate judge session across a different model family. The judge evaluates candidate outputs against the rubric by sanitized label only, never by model or kind name.
6. Verify the chain from transcripts and evidence, not self-report. Read each candidate's local transcript under the active workspace or session directory per principle-guard-the-context-window. Do not inspect other workspaces or sessions outside the target directory. Look at which files each candidate actually opened and which tools ran. Grade chain-following from the files it really read plus the shape of the code, never from candidate self-reports.
7. Read every candidate output end to end and compare to the judge's verdict, interpreting numbers and scores per principle-explain-the-number. Disagreement means a model is biased or the rubric is ambiguous. Synthesize.
8. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.

**Reply:** variant under test, rubric, per-candidate notes (or telemetry summary across playbook spread, playbook_gap events, and brief defects), judge's verdict, your synthesis, and a recommendation for whether to promote the variant.
