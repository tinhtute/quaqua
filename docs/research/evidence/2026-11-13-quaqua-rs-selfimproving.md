# Self-improving software / software factory: source + gap against the locked plan

Fetched 2026-11-13, tier local (`read` fetch.sh). Research note only; the plan is unchanged.

## Source
Holmes, "The road to self-improving software", X article 2026-10-15 (https://x.com/BHolmesDev/status/2110793920930107817).

- Thesis: small loops (computer-use verification, auto-fix CI, adversarial review) "build trust" but are not the factory. The factory is one large loop from task to shipped product, where agents proactively decide what to build next: "what to fix, what costs to optimize, what features to actually build".
- Loop 1, proactive bug fixes: an alert (Sentry, Grafana) fires a webhook that triggers an agent on an always-on machine; it investigates, fixes with the small loops, and sends a PR. Configured as an automations file: trigger (webhook id + payload filter) + prompt + model. Human code review stays before merge.
- Loop 2, agent self-improvement ("gardening", "factory engineering"): aggregate all sessions (traces, tool calls, artifacts) -> score each with owner-defined rubrics (LLM-as-judge, optionally static analysis; example scorer: efficiency = fewest steps) -> an agent reviews low-scoring sessions for common causes -> proposes changes to the codebase, the skills, or AGENTS.md / "factory configuration".
- Loop 3, proactive product work: agents read usage analytics, SEO, and customer interview transcripts (via MCP) on a cadence and propose roadmap tasks onto the board; a human accepts.
- The post does not mention: Chief of Staff, multiple teams, ontology, knowledge base, or observability as a named layer. Those pillars are anh iu's (2026-11-13), not Holmes's.

## anh iu's pillars vs plan R1-R18 (team-config-single-source.md @ lock)

| Pillar | In the plan | Gap |
|---|---|---|
| CoS as owner | daemon hub + CoS/Lead (R7, R8) | none found |
| Many agent teams | Lead + up to 4 Peers per project, topology edges (R8) | "team" is per-project only; no functional teams (product, QA, ops); success_signal covers one task in one project, no cross-project portfolio view |
| Multi-agent runs | drivers, DAG, daemon dispatch (R6, R7, R9) | none found |
| Tooling | fence (R11), inner set (R12) | no per-kind tool/MCP allowlist; no trigger/automation intake (webhook, cron, alert); R15 deletes source/github-ci, the only external event source |
| Observability | event journal (R7) | R15 deletes telemetry_otel and usage.rs; generic driver has no cost (R6); no requirement says what is recorded per run (trace, tool calls, tokens/cost, artifacts) across harnesses, though Loop 2 scoring needs exactly that |
| Harness engineering | R6, R12, R14 | none found |
| Ontology + KB | zbrain + code KG, worker read-only (R10) | "ontology" appears nowhere; no decided entity model; who builds/refreshes the code KG is undecided |
| Self-improvement | gardener: scoring, proposals, static checks, auditor (R13) | rubric undecided; no "did the merged change raise the score" measurement; proposal targets do not name the SQLite config; Loops 1 and 3 absent |

## Holmes loops vs plan
- Loop 1 (bug fixes from alerts): absent.
- Loop 2 (self-improvement): R13, partial (see gaps).
- Loop 3 (product work): absent.
- "Task to shipped product": the plan ends at merge into the project integration branch; push, main merge and deploy stay anh iu-gated (AGENTS.md).

## Open spec questions
1. Which of the three proactive loops are in this program?
2. What is a "team": per-project, per-function, or both?
3. Observability floor: what every run records, on every harness, and where it is read.
4. Ontology: entity model and its store.
5. Scoring rubrics, and how a merged proposal is judged afterwards.
6. Per-kind tool/MCP permissions.
7. Trigger/automation intake shape (webhook, schedule, alert) if Loop 1 or 3 is in.

## anh iu answers, round 1 (question tool, 2026-11-13)
- Intent: bigbang rewrite so quaqua works as a basic software factory.
- Loops: all three (bug fixes, self-improvement, product work) are in the spec. Answers open question 1.
- Teams: CoS -> Leader per team. One project can have several teams; a team is a group of sub-agents whose leader sits between workers and the CoS (intake, orchestrate, verify). A supervisor stands outside the teams. Answers open question 2; the supervisor's role is not yet defined.
- Observability: SQLite + CLI. Every run records events, transcript, tool calls, tokens/cost ("unknown" where a harness has none) and artifacts in the new DB; read through quaqua commands, no GUI. Answers open question 3.
- Ontology: a fixed entity schema in SQLite (project, task, agent, kind, run, rule, skill, score, decision); zbrain holds text knowledge; the code KG holds symbols and call graph. Answers open question 4.
- Effect on the locked plan: these answers change R8 (team model), R13 (loops), R15 (deleted telemetry/usage/event source) and add observability, ontology and trigger intake. The plan needs reopening before to-plan.

## anh iu answers, round 2 (question tool, 2026-11-13)
- Supervisor: observes and proposes only (the outer loop / gardener). It gives no orders to workers and blocks no merge.
- Loop 1 signal sources, all four: GitHub CI failures; test/gate failures inside the farm; external webhook alerts (Sentry, Grafana, any webhook); quaqua's own failures (crash, timeout, repeated non-zero exit in the journal).
- Loop 3 data sources, all four: anh iu's idea inbox; GitHub issues and PR comments; product analytics; interview/meeting transcripts.
- Approval: proposals land in a SQLite backlog split by risk (low/high). The CoS reviews, verifies and approves each with a specific verdict; hard cases escalate to anh iu.
- Still open: the low/high risk rule and what the CoS may approve alone; scoring rubrics and the after-merge measurement; per-kind tool/MCP permissions; trigger intake shape and how the daemon is exposed to webhooks; one writer per file area when several teams share a project.
