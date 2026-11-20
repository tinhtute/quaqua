---
name: eval
when: Testing how a skill, structure, or prompt change affects agent behavior before promoting it, or auditing playbook spread and telemetry across runs.
---

## Orchestrator

1. You own the experiment design: plan, blind, run, and synthesize. The deliverable is an empirical verdict with a recommendation, not an ungrounded opinion.
2. Two modes:
   - Active comparative experiment: test a prompt, rule, or skill change before promotion with N parallel candidates across different kinds per `quaqua roster` and one blinded judge on a different model family.
   - Observational telemetry: read `runs/*.md` and the journal (`runs/qq.db`) to audit playbook spread, adoption, and brief defects.
3. Blinding non-negotiables:
   - No `eval`, `test`, `judge`, `experiment`, `rubric`, `score`, `compare`, `benchmark`, `candidate`, or `arena` in any directory, file, or prompt a candidate sees. Use realistic project names.
   - The candidate prompt reads like an organic user request: the goal, not the meta. Do not reveal other candidates exist.
   - Do not ask candidates which skills, principles, or files they applied; grade chain-following from code shape and tool usage.
   - The judge may know it judges but sees outputs by sanitized label only, never by model or kind name. Two variants are scored by one judge in one pass on one scale.
4. Run: frame the variant or audit scope and the success behavior; write a 3-6 criteria rubric for the judge only; set up sanitized per-candidate directories or data sources; author one organic prompt or telemetry query; spawn the candidates, then the judge, via `quaqua worker spawn`.
5. Read candidate transcripts off-context per principle-guard-the-context-window, never inline. Verify the chain from the files each candidate actually opened and the tools that ran, not self-report.
6. Read every output end to end, compare it to the judge's verdict, and interpret numbers per principle-explain-the-number. Disagreement means a biased model or an ambiguous rubric. Synthesize.

## Worker

1. Frame the evaluation: the variant under test or audit scope, what counts as success, and for comparative runs a 3-6 criteria rubric held back from candidates. For telemetry, define the window and metrics.
2. Set up sanitized environments or data sources: per-candidate working directories with the variant in place, or the `runs/` reports and `runs/qq.db`.
3. Author one organic prompt, with no leakage of what is measured, or the telemetry queries: playbook spread (every name, `none`, `figure-it-out`), `playbook_gap` events (note absence if not yet recorded), and briefs with a missing header that ended in a failed or respawned unit.
4. Run the candidates, each in its own directory with the identical prompt, or aggregate counts across reports and journal events.
5. Judge blind on a separate session of a different model family, by sanitized label only.
6. Verify the chain from transcripts under the active workspace only (principle-guard-the-context-window): which files each candidate opened and which tools ran, never self-report.
7. Read every output end to end against the verdict (principle-explain-the-number) and synthesize.
8. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.

**Reply:** variant under test, rubric, per-candidate notes (or telemetry summary), the judge's verdict, your synthesis, and a recommendation on promotion.
