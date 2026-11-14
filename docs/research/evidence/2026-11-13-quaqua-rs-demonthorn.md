# Demonthorn orchestration model: digest + fit against the quaqua spec

Source: `~/Downloads/demonthorn-agent-orchestration-deep-dive.md` (local file, read 2026-11-13). It is a secondhand synthesis dated 2026-08-02 of Demonthorn's Discord messages; its profiles and templates are marked [SYNTHESIS], not verbatim, and are written for Codex + Paseo. Research note only; the plan is unchanged.

## Model
- Roles: Human (owner authority), Supervisor (governance, observes workflow and reasoning across projects), Lead (binding authority inside one project), Peer (independent co-worker on one bounded outcome). Supervisor and Lead are two kinds of authority, not a rank order.
- Three instruction layers: role profile (stable, cross-repo: identity, authority, invariants), `WORKSPACE_PROTOCOL.md` (per repo, read by the Lead only: criticality, topology per task class, model/effort policy, review and proof rhythm, escalation, Human decision boundaries), task prompt (one assignment: objective, owned scope, exclusions, authority, verification, handoff).
- One Peer profile; the disposition (Engineer, Architect, Reviewer, Scout) comes from the task prompt. Peers do not know the control plane and cannot spawn or manage agents.
- Control plane (Paseo) owns lifecycle, workspace, parentage, timeline, schedule. Tactics stay in the protocol, so they evolve without forking infrastructure.
- Peer escalation verbs: REOPEN_REQUEST, DEPENDENCY_REQUEST, BLOCKED. Disagreement is evidence to reconcile.
- Acceptance: exact diff, stable candidate identity (commit or snapshot digest), exact command + output, independent review when risk requires, an owner with the right authority. Status ("finished", exit 0) is only an attention signal.
- Supervisor output: observation, evidence, suspected mechanism, impact, question for Lead, recommendation, escalation. Notebook keeps causal context. It may propose profile/protocol patches and, when a Lead cannot recover, a new Lead with a bounded handoff. No implementation, no project acceptance.
- Skill topology by attention: Lead macro skills, Supervisor strategy skills, Peer micro skills.
- Topology by difficulty: tiny (one Engineer), bounded (+ Reviewer if risk), architecture-sensitive (Architect -> Engineer -> independent Reviewer), council (sealed seats, distinct mandates, one binding verdict).
- 17 anti-patterns (sheep compliance, pre-solving, parachute optimization, architecture lock-in, architecture fog, moving-scope collision, self-acceptance, test-shaped proof, overengineering, polling debt, ceremony capture, framing capture, forked independence, root attention dilution, skill pollution, status-as-acceptance, supervisor overreach).

## Fit with the quaqua spec
Already matching:
- One control plane, native subagents off (R11; Agent tool denied in AGENTS.md).
- Event-driven, no polling (R7).
- Lead does not implement; one writer per moving scope; own worktree per writer (AGENTS.md).
- Models are data, discovered not hard-coded (R3, R5).
- Supervisor = observe and propose only (anh iu, round 2).
- Peer challenge right (`## Challenge` in reports).

New for the spec:
- A per-project protocol layer read by Leads only. Today the plan puts topology in one global SQLite config (R8).
- Disposition as a brief field, separate from kind (kind = harness/model/effort routing).
- Candidate identity (sha) on every review and acceptance; fits the run entity in the ontology.
- Escalation verbs as report states.
- Supervisor finding format and the 17 anti-patterns as the seed for scoring rubrics and proposals (covers part of the open rubric question).
- Human decision boundaries per project as the home of the low/high risk rule (covers part of the open approval question).
- Task classes that pick the smallest useful topology.

Tensions to decide:
1. Role mapping: Demonthorn has no CoS. Human talks mostly to the Supervisor, which relays decisions to Leads. In quaqua the CoS does intake, verification and integration, which is Lead-like at portfolio level.
2. Supervisor reach: anh iu chose "no orders, no blocking". Demonthorn's Supervisor still asks the Lead open questions live and can propose replacing a Lead. Whether a supervisor -> Lead mail edge exists is undecided.
3. R12 injects one merged skill set into every driver; Demonthorn splits skills by role and calls the opposite "skill pollution".
4. R8/Q31 allow peer-to-peer mail on topology edges; Demonthorn keeps Peers off orchestration tools entirely.
5. Several teams in one project (anh iu, round 1) vs one Lead as the binding arbiter of a project: who owns project-level integration and the verdict is undecided.
6. Protocol in a repo file (versioned with the repo, Lead-only) vs in the SQLite config (single source, `config set`).

## anh iu answers on the six tensions (chat, 2026-11-13)
1. The CoS sits at the same level as Demonthorn's Supervisor. CoS reading: both roles exist side by side under anh iu; the CoS owns intake and integration, the Supervisor owns governance.
2. Supervisor reach follows Demonthorn: it asks Leads evidence-backed open questions, relays owner decisions, and may propose replacing a Lead; it gives no implementation orders and owns no acceptance.
3. Skills split by role: macro for Leads, micro for Peers, high-level for the CoS. This replaces R12's single merged set injected into every driver.
4. Peers/workers get only the tools tied to implementation inside their scope; no orchestration tools. This reverses peer-to-peer mail (Q31, part of R8).
5. The Lead is the sole arbiter inside its project scope; the CoS owns project-level integration; verdicts above the Lead come from the CoS, the Supervisor and anh iu. Not yet confirmed: with several teams in one project, each Lead arbitrates its own team's scope and the CoS integrates across teams.
6. Protocol placement: open. anh iu asked what a protocol is and for a proposal. CoS proposal (pending): a per-project record in the SQLite config with daemon-enforced fields (criticality, task classes -> required topology, approval boundaries, required checks) plus a prose section; rendered into the Lead's prompt at spawn, never sent whole to Peers; versioned rows; changed through `config set` or an approved Supervisor proposal; `config export` dumps it for review.

## anh iu follow-up (chat, 2026-11-13)
- Confirmed: CoS and Supervisor are two different roles. The CoS is the chief; the Supervisor manages, observes and owns governance.
- Several teams per project is dropped (supersedes the round 1 team answer in raw/2026-11-13-quaqua-rs-selfimproving.md). Model: one project, one Lead, n workers. The worker limit per Lead goes up from today's 4 and control gets tighter, for projects with many large tasks. The new limit is not yet set.

## anh iu answers, round 3 (question tool, 2026-11-13)
- Protocol placement: a file in each repo (Demonthorn's `WORKSPACE_PROTOCOL.md`), versioned with git. This rejects the CoS SQLite proposal above. Consequence: two config homes (SQLite for roster/harness/topology, the repo file for per-project tactics); the daemon reads the file for whatever it enforces. File format for the enforced part is not yet set.
- Worker limit: per project in its protocol; default 4 per Lead, hard ceiling 8; the daemon refuses spawns above it.
- Low/high risk default: by impact. Low = reversible, inside one project, no rule/skill/config/protocol change, no external effect; everything else is high. A project's protocol may tighten it.
- Triggers: bug sources come in several kinds and get analysed later; for now the daemon pulls on a schedule. No inbound HTTP in this spec.
- Still open: scoring rubrics and the after-merge measurement (17 anti-patterns are the seed); the protocol file format; the per-role tool lists; the bug-source analysis.

## anh iu answers, round 4 (question tool, 2026-11-13, during brainstorm refine)
- Done level: each loop runs for real with one source; the other sources plug in later through the same scheduled trigger mechanism.
- Loop 1 first source: test/gate failures inside the farm.
- Loop 3 first source: anh iu's idea inbox.
- Supervisor runtime: scheduled daemon jobs (scoring, static analysis, auditor) plus a short-lived Supervisor agent the daemon spawns when a job finds an anti-pattern, to question the Lead and write the proposal. No standing pane.

## Carry-over for to-plan (CoS, 2026-11-13; none of these is decided)
- First spike: measure whether `herdr agent get/wait/prompt` accept a pane labelled by `pane report-agent` (raw/2026-11-13-quaqua-rs-herdrkind.md:35). R6 and success clause (a) rest on it.
- Code knowledge graph: what builds and refreshes it, and with which tool (R10, clause (b)).
- R16's parity grep prints 0 while no `- [ ] parity` line exists; to-plan must write the full checklist first.
- Seed source: R3 seeds from the template toml, R17 imports the live team.toml roster at cutover; state the live-workspace path.
- Workspace docs (AGENTS.md, rules/, models.md) still describe taskforce, the old roles, 4 Peers per Lead and one merged skill set; no requirement covers updating them.
- Scoring rubrics (seed: the 17 anti-patterns) and whether a merged proposal's effect on scores is measured afterwards. anh iu has not decided the measurement.
- `WORKSPACE_PROTOCOL.md` format for the part the daemon enforces.
- Tool lists per role, and the Supervisor agent's skill set (R12 leaves it empty).
- Analysis of bug-source kinds beyond the first (NG8).
- Approved by anh iu 2026-11-13 (chat, review gate): the three CoS additions in the refined Goal: R22 as a whole (disposition, escalation states, sha-bound acceptance); R21 reading the protocol from the local default branch; R8 keeping CoS-spawned workers for projects without a Lead.
