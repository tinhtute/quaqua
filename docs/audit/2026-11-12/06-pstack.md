# pstack: playbooks, router, verdicts, warn-to-gate

**Verdict: PARTIAL, 3/5.** Playbook structure, step pinning and SHA-bound verdicts are real. The router is advisory by its own description, a missing playbook only warns, and no mechanism promotes a warning to a gate from data.

Audited at `41a5327`. `src/` means `crates/quaqua/src/`. `tpl/` means `templates/workspace/`.

## Scope

- "Workflow first: the playbook is chosen before the agent, and the harness only adapts."
- "Each playbook has a routing trigger, an orchestrator part, and a worker part built from reusable principle leaves."
- "One front-door router picks the playbook for every brief."
- "Verdicts are tied to the exact commit they checked, and a stale verdict is visible."
- "Checks start as warnings and become hard gates once data shows they pay off."

## Findings

| Requirement | Status | Evidence | Gap |
|---|---|---|---|
| Workflow first: playbook before agent | partial | The routing flow lists playbook selection (steps 1–4) before worker spawn (step 6) (`tpl/.claude/skills/poteto-mode/SKILL.md:16-26`). `worker prompt` appends the playbook's worker steps and pins name and step ids (`tpl/rules/_all/brief.md:23`). | The order is not enforced. `quaqua worker spawn` takes a kind and no playbook (`quaqua worker spawn -h`), so the agent can be picked first. Kind selection is by `--purpose` or `models.md`, with no link from playbook to kind (`tpl/team.toml:43-48`). |
| Playbook shape: trigger, orchestrator part, worker part from principle leaves | met | 18 playbooks with `name` and `when` frontmatter, `## Orchestrator` and `## Worker` sections (`tpl/playbooks/feature.md:1-14`, `tpl/playbooks/shipping.md:1-20`). 24 principle leaves in `tpl/principles/`. The parser validates citations against `principles/<name>.md` and caps a playbook at 3 leaves (`src/playbook/mod.rs:1-5`). `quaqua playbook list` and `show` expose trigger and orchestrator part (`quaqua playbook --help`). Tests `playbook`, `prompt_playbook` and `prompt_playbook_check` passed in my run. | None on shape. |
| One front-door router for every brief | partial | `/poteto-mode` is the named front door (`tpl/AGENTS.md:59`, `tpl/.claude/skills/poteto-mode/SKILL.md:8`). An unresolvable playbook name is rejected when the index is present (`tpl/rules/_all/brief.md:22`). | The skill states it is "not technically enforced by tool restrictions or permission gates" (`tpl/.claude/skills/poteto-mode/SKILL.md:10`). A brief with no `Playbook:` header only warns (`src/worker/prompt/brief.rs:150-152`). The router is an LLM skill on Claude Code, so it is vendor-bound and non-deterministic. Six routes are marked deferred (`tpl/.claude/skills/poteto-mode/SKILL.md:51-58`). |
| Verdicts tied to the exact commit; stale verdict visible | partial | Builder `Verified: <level> <sha>` and reviewer `Verdict:` are validated against worktree HEAD or the review baseline and journaled (`src/worker/prompt/verdict.rs:1-6`, `:250-262`). Six fixed levels (`tpl/rules/_all/brief.md:63`). `quaqua status` flags a verdict whose SHA differs from HEAD as `stale` (`src/status.rs:296`, `:909-910`). `quaqua merge` looks up the strongest verdict at the branch head (`src/merge/mod.rs:197-291`). `status --metrics` counts merges without a verdict at the incoming head (`src/status.rs:1099-1106`). | At merge a missing or stale verdict only warns, "exit code unchanged" (`src/merge/mod.rs:240`, test `:580`). A journal failure skips recording without blocking (`src/worker/prompt/verdict.rs:8-9`). |
| Checks start as warnings and become gates from data | partial | One check has made the trip: a playbook step gap now exits 6 on `worker prompt` (`src/worker/prompt/playbook_check.rs:1`, `:10-12`) while it still only warns in `quaqua pipeline` (`:12-13`, `src/pipeline/mod.rs:1695`). Gap events are counted per playbook (`src/status.rs:1090-1095`). | Promotion was a code change, not a mechanism. No check carries a warn or gate mode in config, and no threshold or report says "this warning paid off, promote it". The same check is a gate in one command and a warning in another. |

## Refs

- Playbook list: `ls templates/workspace/playbooks` shows 18 files; `ls templates/workspace/principles` shows 24.
- `quaqua playbook list` could not run here: `error: no quaqua workspace at /root/quaqua: projects/INDEX.md is missing`. Shape evidence is from the template files and the parser.
- Verdict path: `src/worker/prompt/verdict.rs:188-205`, `src/merge/mod.rs:197-291`.

## Fixes

1. Make the merge verdict check a gate (same as fix 2 in `02-adlc.md`). This is the highest-value warn-to-gate promotion and the data source already exists in `status --metrics`.
2. Give every check a declared mode in `team.toml` (`off`, `warn`, `gate`), journal each firing with whether the outcome changed, and have the scheduled auditor recommend promotions and demotions from those counts.
3. Make `Playbook:` required in `--check-brief` (keep `none, <reason>`), and make the pipeline apply the same playbook-gap gate as `worker prompt`.
4. Add a deterministic first pass to the router: `quaqua playbook route "<goal>"` matching `when:` triggers, with the LLM skill used only to break ties. That also removes the Claude Code dependency for routing.
5. Let a playbook declare a preferred purpose, and have `worker spawn` resolve the kind from the brief's playbook, so the workflow picks the agent.
