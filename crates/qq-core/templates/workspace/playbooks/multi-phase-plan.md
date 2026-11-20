---
name: multi-phase-plan
when: Work that spans phases or stacked PRs; the plan is a checklist an owner runs box by box.
---

## Orchestrator

You own the plan, not the code. The plan is the deliverable; the coordinator audits it from the evidence. Do not implement.

1. When the change is one or two files with an obvious approach, skip the plan. State so and stop.
2. Settle open questions by prototype before writing; keep the branch, the SHA, and the observations. Ask the human only about a product or preference call no run can settle, with concrete options per principle-never-block-on-the-human.
3. Explore in focused subagents that return file pointers, conventions, test commands, and entry points, not dumps (principle-guard-the-context-window).
4. Copy the skeleton below into `docs/plans/active/{slug}.md` (or the named path) and fill every placeholder, keeping every heading in order. One section per unit; one unit is one change with its own evidence (principle-sequence-verifiable-units).
5. Write short declarative sentences. The body is a how-to checklist; appendices explain. Each heading states the task or the finding.
6. Every box needs concrete evidence and explicit dependencies (principle-encode-lessons-in-structure).
7. Verification rule, opening every verification block: Tests alone are not sufficient verification. A unit is verified only when its unit, live, and perf boxes are all checked. The live block drives the real surface (CLI/TUI/API/Web). The perf gate is dual-sided: base and head both produce the named metric. If base lacks the feature, gate the behavior the diff adds and the end state the user waits for, with an absolute budget, instead of a ratio.
8. Hand back the plan path and a summary, then stop. Execution starts on explicit confirmation under the coordinator or project Lead (`<slug>-lead`).

```markdown
# <Program> plan

<Under ten lines. What changes, for whom, the rule the program enforces, and the units in order.>

## How to read this

One box is one unit of work and names its evidence. Check a box only when the evidence exists: a file, a log line, a test run, or a SHA. Tests alone are not sufficient verification. A unit is verified only when its unit, live, and perf boxes are all checked.

## Program checklist

### Arm the program
- [ ] State this plan to the coordinator, then stop. Start only on explicit go.
- [ ] Re-read referenced specifications and playbooks at every milestone.
- [ ] Audit against the plan each cycle: probe every worker by side effects, stand down a stuck one and replace it, report status only on a tracked change, and log the tick in Appendix D.
- [ ] On a hold order, send every worker a stop order at once.

### Spawn owners
- [ ] Spawn one worker per unit via `quaqua worker spawn <name> <kind> <repo>` per `quaqua roster`.
- [ ] Follow the dependency graph: <Unit 1> and <Unit 2> are independent and first; <Unit 3> after <Unit 1>.
- [ ] Hold the file boundaries (<Unit> touches only `<glob>`) and the review gate for critical interfaces.

### Every unit
- [ ] Branch from the integration base; run targeted lint and tests; keep commits single-purpose and rebased.
- [ ] Verified at the code-ready head: unit, live, perf. Reviewer verdict clean.
- [ ] Merge via `quaqua merge <slug> <branch>...`, one unit at a time.

## <Task as a verb phrase> (<Unit id>)

**Depends on.** <Unit id, or None.> **Files.** Edit, create, or delete `<path>`, one box each.

- [ ] Build. <One change. Name the symbol and the file.>
- [ ] You see. <One observable result, with the exact log line or screen state.>

**Verify, unit.** Tests alone are not sufficient verification. A unit is verified only when its unit, live, and perf boxes are all checked.

- [ ] <Test file and the case it gains.> Run `<command>`.

**Verify, live.** Mandatory.

- [ ] Lane 1. Regression check: run <scenario> at base and head on the matching surface. Pass when <predicate>.
- [ ] Lane 2. <Scenario on the matching surface.> Pass when <predicate>.

**Verify, perf.** Base and head must both produce the named metric.

- [ ] Metric, probe (run interleaved at base and head), base value first. Rule: <head against base, failing threshold>.

**Review and merge.**
- [ ] Independent reviewer verdict recorded at head SHA.
- [ ] Verified at that SHA, rebased onto the integration branch, integrated via `quaqua merge`.

## Close the program

- [ ] Every box is checked with its evidence; report summary, verify outputs, and integration status.

Appendices: A. Prototype evidence (branch, SHA, links; questions left unproven). B. Alternatives rejected. C. Risks, with the unit and what the owner watches. D. Decision trail: each audit tick's items reported, workers stood down, or none. E. Reading list.
```

## Worker
