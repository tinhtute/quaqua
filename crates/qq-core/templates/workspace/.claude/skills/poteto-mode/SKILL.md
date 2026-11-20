---
name: poteto-mode
description: poteto's agent style for concise, detailed responses, deliberate subagents, unslopped prose, simple code, and verified work. Use for /poteto-mode or routing before writing any worker brief.
---

# Poteto mode

Routing front door for the coordinator, not a fence: compliance shows in `--check-brief` warnings and journal telemetry, not a gate. Use it on `/poteto-mode <goal>` and before any new task.

**Never route inside a worker.** A worker that loads this skill stops immediately; its playbook is fixed by its brief and pinned in `runs/.<name>.playbook`.

## Routing flow

1. `quaqua playbook list` shows the installed playbooks. Pick only a name it prints.
2. No narrow fit, or a large migration or cross-cutting change: `figure-it-out`. A standing multi-day program with stacked PRs and a fleet of workers: a project Lead (`<slug>-lead`).
3. `quaqua playbook show <name>` gives the `## Orchestrator` steps.
4. Write the five-part brief (`rules/_all/brief.md`) with `Playbook: <name>` and `Context:`; check it with `quaqua worker prompt --check-brief "<brief>"`.
5. `quaqua worker spawn <name> <kind> <repo>` with the kind from `quaqua roster`, then `quaqua worker prompt <name> "<brief>"`.

Before asking a "which approach" question, classify it. An observable fact (behavior, timing, output, perf) is settled by an experiment (`prototype`) or a cited read-only answer (`investigation`), not by the human. Ask only for product or preference calls; `AGENTS.md` approvals always pause.

## Triggers

| Playbook | Use when |
|---|---|
| `feature` | new or changed behavior; name the data shape first |
| `bug-fix` | a reported defect to reproduce and root-cause |
| `refactoring` | behavior-preserving change to structure |
| `perf-issue` | measured slowness or a number to report |
| `hillclimb` | sustained improvement of one metric across hypotheses |
| `babysit` | PR status: get it green, address review comments |
| `shipping` | land a green stack; independent verification per unit |
| `worktree-cleanup` | reclaim disk from merged or abandoned worktrees |
| `multi-phase-plan` | long, autonomous, or multi-phase work with an auditable trail |
| `session-pickup` | resume prior in-flight work from report, branch, journal |
| `pause-safely` | suspend work on explicit pause or before compaction |
| `runtime-forensics` | diagnose a runtime symptom from live instrumentation |
| `trace-forensics` | diagnose a captured profile, trace, or heap snapshot |
| `visual-parity` | pixel-exact UI equivalence, headless only (`rules/_all/no-gui-driving.md`) |
| `eval` | test a skill, prompt, or structure change; bakeoffs of N workers |
| `prototype` | throwaway sketch to settle a design or empirical fork |
| `investigation` | read-only cited answer: how, why, "are we sure?" |
| `figure-it-out` | nothing narrower fits |

## Rules of the road

- Principles live in `principles/<name>.md`. Read a leaf before citing it, and name each principle that shaped a decision.
- **No is an acceptable answer.** Give your real judgment; decline or push back when the work does not earn its place.
- You own every worker's result: review the diff and verify the evidence yourself.
- Resume a stalled or killed worker (`quaqua worker prompt --resume`); spawn fresh only when the goal changed or the worktree is unrecoverable.
