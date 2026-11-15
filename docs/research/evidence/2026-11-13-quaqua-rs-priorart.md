Outcome:         COMPLETE
Snapshot:        none (nothing written in any repo; worktree ~/Lab/.wt/tieude-priorart2 @43a60bd, branch quaqua/tieude-priorart2, clean)
Scope:           read-only web research; only file written is this report. Pages fetched 2026-11-13 via TinyFish (public URLs only).
Verification:    every claim below cites the URL fetched; labels: measured = seen in fetched page text, cited = taken from a search snippet or third-party post, inferred = my reading. Skipped: cloning repos, running any tool, reading piggery docs/reference.md and Gas Town's Molecules/Seance docs, Hermes worker-lanes page.
Unknown / risk:  see "Unknown / risk" list at end of Findings. Star counts are as of fetch, not stable.
Ownership:       nothing held.

## Findings

### Verdicts
- **piggery is a multi-agent orchestrator** ("the farm": one Go binary + SQLite, mailbox + gate across pi/Claude Code/Codex/omp/dsh/opencode) — measured, https://github.com/sting8k/piggery. Its `slp` / `lead-peer` / `gastown-like` templates are the closest match to quaqua's CoS+worker model found.
- **"Hermes" = Nous Research Hermes Agent** (github.com/NousResearch/hermes-agent). Decision: (a) it is the only "Hermes" in 2026 results that is an agent/orchestrator with 263k stars — measured, https://github.com/NousResearch/hermes-agent; (b) it has a Kanban board with an "orchestrator profile" that routes work to worker profiles — measured, https://hermes-agent.nousresearch.com/docs/user-guide/features/kanban; (c) the community already uses it as the boss agent over Claude Code/Codex — cited, search snippet https://www.reddit.com/r/hermesagent/comments/1t9chdk/ ("Hermes is good as the orchestrator… delegate the coding part to Claude Code"). Other "Hermes" hits (Meta/dev.meta.ai recipe, hermesatlas.com lists) are all built on the same Nous agent — cited, search snippets. Not Hermes the Meta model / Hermes-3 weights: those have no coordinator role. One project, many wrappers.
- **T3 Code is not a CoS**: it is a control plane / UI over harnesses; a human is the coordinator — measured, https://github.com/pingdotgg/t3code ("open-source control plane for coding agents"; "Let Claude hand a task to Codex").

### Comparison table
| project | URL | CoS hub | harnesses | inner loop | outer loop | knowledge base | graph |
|---|---|---|---|---|---|---|---|
| piggery | https://github.com/sting8k/piggery | yes: the "gate" session + template roles (lead, supervisor, mayor); farm enforces who may mail/spawn | pi, Claude Code, Codex, omp, dsh, opencode; per-role `spawn.harness`, YAML config (measured) | worker takes mail from a trough; counts as done only when the job is done, not on a sniff; lead judges each result, `rework` on a miss (measured, guide.md + manifests/README.md) | none found: no skill/check self-improvement in README, guide or manifests (inferred from grep; reference.md unread) | none beyond SQLite state + mail; survives crash (measured guide.md:437 "State and mail are in SQLite") | org-chart topology per template (flowchart per manifest) = agent topology; no task DAG (measured manifests/README.md) |
| T3 Code | https://github.com/pingdotgg/t3code (docs/internals/overview.md, providers.md) | no: human drives; "control plane" | Claude Code, Codex, Cursor, OpenCode, Pi, Grok Build, Antigravity, Kiro, any ACP agent; adapter per provider, subprocess/RPC (measured) | per-turn checkpoint (hidden git ref) + thread-per-worktree (cited, AGENTS.md snippet); no verification loop | none | none; event store is session history, not KB | none (event-sourced command→event→projection) |
| Hermes Agent | https://github.com/NousResearch/hermes-agent | yes: kanban "orchestrator profile" routes tasks via `kanban_create/link/unblock`; dispatcher spawns worker profiles (measured, kanban doc) | Hermes profiles on any LLM provider; external coding harnesses only via delegation/ACP, not as native lanes (inferred; T3 issue #6182 asks for Hermes-over-ACP, cited) | worker reads `kanban_show`, must verify contract before `kanban_complete`; commit/diff alone never completes; heartbeat, 90% budget checkpoint, circuit breaker (measured) | yes, built in: agent creates skills after complex tasks, skills self-improve in use, nudges to persist memory (measured, README "closed learning loop") | yes: per-profile MEMORY.md/USER.md, SOUL.md, FTS5 session search, Honcho user model, external memory provider (measured README + profiles doc) | yes: kanban `task_links` parent→child DAG, dispatcher promotes todo→ready when parents done (measured); DAG engine proposed in issue #344 (measured) |
| Gas Town | https://github.com/steveyegge/gastown | yes: the Mayor (Claude Code instance) is the named coordinator | Claude Code default, Codex, Copilot CLI, Gemini; runtime config (measured README) | polecat works one bead, `gt done` -> Refinery runs verification gates, Bors-style bisecting merge queue (measured) | partial: 3-tier watchdog Witness/Deacon/Dogs patrols and recovers stuck agents; escalation routes by severity; not skill improvement (measured) | Beads ledger (git-backed, Dolt) holds work state; `seance` resumes old sessions (measured) | yes: beads/convoys/molecules (formula TOML -> tracked steps) = work graph; Mayor/rig/polecat = topology (measured/inferred) |
| Orchestrator.inc (ex Composio Agent Orchestrator) | https://github.com/ComposioHQ/agent-orchestrator | yes: persistent project orchestrator agent plans, spawns/redirects workers, owns planning while workers own implementation (measured) | 50+ listed (Claude Code, Codex, Cursor, opencode, Pi, Agy, Aider…), worker/chat/reviewer support differs per capability catalog (measured) | worker per branch+worktree; CI/review/merge-conflict facts flow back to the same worker; board card position derived from session/PR/CI facts (measured) | none described | project-scoped orchestrator conversation keeps goals/decisions (measured); no separate KB | Kanban status columns, not a DAG (measured) |
| BHolmes "software factory" (pattern, not a repo) | https://x.com/BHolmesDev/status/2107588784275816461 | supervisors/gardeners | n/a | inner loop = agents building the product | outer loop = one agent batch scores conversations (efficient? good code by our standards?), a scheduled agent proposes skill changes from low scorers; next: architecture + static-analysis agents (measured, post of 6 Oct 2026) | skills are the KB being tuned | none |
| Fowler harness model | https://martinfowler.com/articles/harness-engineering.html | n/a (human steers) | n/a | guides (feedforward) + sensors (feedback), computational vs inferential | steering loop: when an issue repeats, improve guides/sensors; continuous drift sensors; "garbage collection" agents (measured, 2 Apr 2026) | AGENTS.md, Skills as guides | none |

Shallow-only (cited, snippets from https://github.com/andyrewlee/awesome-agent-orchestrators, not read further): GraphCode (agent sessions wired as a graph; node = live terminal, edge = hand-off/message/spawn), Tempest and Fletch (shared code-knowledge / symbol+call-graph index over MCP), ivy-tendril (plan lifecycle, verification gates, self-improving memory), agent-swarm (lead agent, workers in containers, persistent memory, review gates), ai-maestro (memory search + code-graph queries).

### Per-project notes
- **piggery** — what: Go + SQLite farm; mail between agents of any harness; gate checks every mail/spawn against a YAML layout (measured README). Hub: gate session is "the one that talks to you and to other teams" (guide.md). Drive: harness-native process per worker (`pi --mode rpc`, `claude -p`, `codex app-server`, `opencode serve`, measured table). Templates: lead-peer, slp, council, amp-like, dual-lens, advisor, gastown-like (measured). Closest to quaqua; gap is no learning loop and no KB.
- **T3 Code** — provider adapters normalize native protocols to commands/events; orchestration never sees the provider; instance != driver so two accounts do not share mutable session state (measured providers.md). Already surveyed per the brief; only new fact here: it is deliberately not a CoS, Hermes-over-ACP is requested (issue #6182, cited).
- **Hermes** — kanban.db is the shared state; "every handoff is a row"; reclaim on crash, one SQLite lock for claim/complete (measured kanban doc). Warning: never share one Hermes home between two processes, both write memory (measured profiles doc line 81).
- **Gas Town** — hooks (git-worktree storage) so work survives agent restarts; identity persists, sessions are ephemeral (measured).

### Patterns worth adopting for quaqua
1. Done = verified, never "commit exists": Hermes `kanban_complete` only after contract check, piggery "eaten only when done", Gas Town verification gates at merge (measured). quaqua already has `Verified:` lines; make the acceptance check refuse a report without evidence.
2. Hub-enforced topology: piggery's farm checks who may mail/spawn so the prompt cannot talk around it. Fits quaqua's herdr-gate hook (inferred).
3. Durable shared state in a boring store (SQLite rows / git-backed ledger) so crash/restart loses nothing: Hermes, piggery, Gas Town all do this (measured).
4. Outer loop = score conversations then propose skill/check changes on a schedule, with the score as the metric of whether the change helped (BHolmes) plus "when an issue repeats, improve the guide/sensor" (Fowler). Maps to quaqua `## Lessons` -> rules/ promotion; automate the scoring, keep promotion human-gated.
5. Computational sensors before inferential: cheap deterministic checks on every change, LLM review only on top (Fowler). Aligns with targeted-tests-only for builders and `--slow` at the Lead gate.
6. Watchdog tiers separate from the coordinator (Gas Town Witness/Deacon; Hermes dispatcher reclaim + circuit breaker). quaqua's `worker prompt` watchdog is the same idea.
7. Per-worker isolated workspace + branch (Orchestrator.inc, T3, Gas Town) and instance-scoped state per account (T3).
8. Per-role harness choice in config (piggery `spawn.harness`) so playbooks pick the harness, not the CoS prompt.

### Patterns to avoid
- Human-as-coordinator only (T3): no routing, no verification, nothing to improve.
- Shared memory home between agents (Hermes warning) and unbounded self-written memory; keep memory writes owned and reviewed.
- Full DAG/formula engines before need: the Hermes maintainers themselves flag "over-engineering risk — most usage is 1-3 independent tasks" (measured, issue #344 line 264). Gas Town's mayor/deacon/dogs/wisps vocabulary is heavy.
- Kanban column derived from facts only, no semantic check (Orchestrator.inc): cheap but cannot catch "wrong but green".
- Prompt-only restrictions presented as enforcement: piggery's own guide admits taskforce read-only is "a rule in the prompt, not one piggery enforces" (measured guide.md).
- Marketing-scale claims (20-30 agents, 41-agent swarms) with no sensors: scale without harness.

### What "Graph" most often means here
Ranked by frequency in what I read (inferred from the sample, n≈8 projects + awesome list):
1. **Task/work dependency graph (DAG)**: Hermes task_links, Gas Town beads/convoys/molecules, Hermes issue #344 DAG engine, relay `dag` topology.
2. **Agent topology graph** (who may talk/spawn to whom): piggery manifests, Gas Town rig/mayor/polecat, GraphCode nodes/edges.
3. **Code knowledge graph** (symbol/call-graph index served to agents over MCP): Tempest, Fletch, ai-maestro.
Memory graphs are rare. For quaqua, "Graph" as the third leg of Harness-Loop-Graph is most defensibly (1)+(2): the plan/task DAG the Lead owns and the topology the gate enforces, with (3) optional later (inferred).

### Unknown / risk
- Whether the Principal's "Graph" means a knowledge graph; nothing in the BHolmes post or Fowler article defines it (measured: neither uses the word). The target phrase "Harness, Loop and Graph" was not found; searched only the three sources in the brief plus the engines above.
- piggery: outer loop and KB marked "none" from README/guide/manifests only; docs/reference.md not read.
- Hermes external-harness support is inferred from snippets; I did not read worker-lanes docs.
- Star counts and 2026 dates come from the fetched pages; the Hermes 263k stars figure is unusually high and not cross-checked.
- Search snippets (reddit, medium, dev.to) are untrusted, used only as pointers.

## Playbook
1: done
2: done
3: done

Verified: review-clean 43a60bd

## Blockers
none

## Lessons
none
