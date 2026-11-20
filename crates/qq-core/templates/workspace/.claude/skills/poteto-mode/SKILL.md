---
name: poteto-mode
description: Routes a task to a playbook before a worker brief is written. Use for /poteto-mode or before writing any worker brief.
---

# Poteto mode

Routing front door for the coordinator, not a fence: compliance shows in `--check-brief` warnings and journal telemetry, not a gate. Use it on `/poteto-mode <goal>` and before any new task.

**Never route inside a worker.** A worker that loads this skill stops immediately; its playbook is fixed by its brief.

## Routing flow

1. `quaqua playbook list` shows the installed playbooks. Pick only a name it prints.
2. No narrow fit, or a large migration or cross-cutting change: `figure-it-out`. A standing multi-day program with stacked PRs and a fleet of workers: a project Lead (`<slug>-lead`).
3. `quaqua playbook show <name>` gives the `## Orchestrator` steps.
4. Write the five-part brief (`rules/_all/brief.md`) with `Playbook: <name>` and `Context:`; check it with `quaqua worker prompt --check-brief "<brief>"`.
5. `quaqua worker spawn <name> <kind> <repo>` with the kind from `quaqua roster`, then `quaqua worker prompt <name> "<brief>"`.

Before asking a "which approach" question, classify it. An observable fact (behavior, timing, output, perf) is settled by an experiment (`prototype`) or a cited read-only answer (`investigation`), not by the human. Ask only for product or preference calls; `AGENTS.md` approvals always pause.

## Rules of the road

- Principles live in `principles/<name>.md`. Read a leaf before citing it, and name each principle that shaped a decision.
- **No is an acceptable answer.** Give your real judgment; decline or push back when the work does not earn its place.
- You own every worker's result: review the diff and verify the evidence yourself.
