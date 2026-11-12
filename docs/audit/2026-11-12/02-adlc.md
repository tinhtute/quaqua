# Standard ADLC, stage by stage

**Verdict: PARTIAL, 3/5.** Brief, plan, build, review and integrate have code behind them. Intake, routing, verification rerun, release and lesson promotion depend on the coordinator following prose, and no single command carries a task through all eleven stages.

Audited at `41a5327`. `src/` means `crates/quaqua/src/`. `tpl/` means `templates/workspace/`.

## Scope

- "Every task follows one lifecycle: intake → size and route → brief → plan → build → independent review → verify → integrate → approval → release → lesson."
- "Intake turns a fuzzy request into a clear goal, a success signal, and non-goals before any work starts."
- "Sizing decides the shape: one worker for small work, a pipeline for medium work, and a Lead with Peers for project-size work."
- "A brief states the outcome, constraints, unknowns, and how to check the result, carries time and cost budgets, and never pre-solves the work."
- "Risky or multi-file work is planned and approved before building; mechanical work skips planning."
- "Build happens in isolation, with a local commit and a written report as the only handback."
- "Review is independent, runs on a different harness or model from the builder, and is capped at a set number of rounds."
- "Verification reruns the checks at the exact commit; a verdict on an older commit does not count."
- "Integration happens through a merge queue into an integration branch, never straight into main."
- "A human approves only irreversible actions: push, merge to main, deploy, delete, spend, or publish."
- "Every finished task leaves a retrospective trace: what worked, what failed, and at most one lesson worth keeping."
- "The lifecycle itself is measured (lead time, retries, review hit rate, cost per task) and tuned from data."

## Stage map: code or prose

| Stage | Enforced by | Where |
|---|---|---|
| Intake | prose | `tpl/AGENTS.md:38` (CoS "owns ... intake"). No intake command or artifact check in `src/` (`grep -rni "intake\|non-goals\|success_signal" src` finds nothing). |
| Size and route | code (partial) + prose | Lead gate on `worker spawn` (`src/worker/leadgate.rs:1-19`, call at `src/worker/spawn.rs:673`). Playbook routing is a skill that says it is "not technically enforced" (`tpl/.claude/skills/poteto-mode/SKILL.md:10`). |
| Brief | code (partial) | `quaqua worker prompt --check-brief` (`src/worker/prompt/mod.rs:48`). `Topology:` is required; `Playbook:` and `Context:` only warn (`src/worker/prompt/brief.rs:147-177`). |
| Plan | code inside `quaqua pipeline`, prose outside | Plan phase pauses with exit 5 `plan-ready` until resumed (`src/pipeline/mod.rs:50-51`, `:1043-1052`). |
| Build | code | Worktree and branch per worker (`src/worker/worktree.rs:21-25`, `:95-96`). Commit required after build (`src/pipeline/gate.rs:180-194`). |
| Independent review | code inside `quaqua pipeline` | Different harness (`src/pipeline/mod.rs:631-633`), `can_review` (`:624-629`), rounds cap (`:53-54`, `:603-606`). |
| Verify | code records, prose reruns | Verdict journaled against HEAD (`src/worker/prompt/verdict.rs:1-6`). Rerun is an instruction (`tpl/AGENTS.md:78`). |
| Integrate | code | `quaqua merge` (`src/merge/mod.rs:11-22`). |
| Approval | code (partial) | Bash gate: push asks, force and main deny (`src/gate/policy.rs:746-841`). |
| Release | code for the engine's own binary only | `.github/workflows/release.yml` on a `v*` tag (`README.md:132-135`). No release step for a task's change. |
| Lesson | code checks the heading, prose promotes | `## Lessons` required, exit 6 without it (`src/worker/prompt/cost.rs:1220-1230`, `src/worker/prompt/mod.rs:66-67`). Promotion is `tpl/AGENTS.md:24`. |

## Findings

| Requirement | Status | Evidence | Gap |
|---|---|---|---|
| One lifecycle for every task | partial | `quaqua pipeline` runs plan → build → review → fix as one durable workflow (`src/pipeline/mod.rs:41-58`). | The pipeline ends at `clean`, `disputed` or `blocked`. Intake, routing, merge, approval, release and lesson are separate manual commands. The project's own plan counts "6 steps still need a human (brief text, report acceptance, test rerun, push, PR open, merge to main) plus 4 unwired chores" (`.harness/plans/active/harness-mvp.md:17`). A solo `worker spawn` + `worker prompt` skips plan and review entirely. |
| Intake produces goal, success signal, non-goals | missing | Plan files in `.harness/plans/` carry `outcome`, `success_signal` and `non-goals` (`.harness/plans/active/harness-mvp.md:8-13`). | That format is not produced or checked by the engine. INFERENCE: it comes from a separate planning skill outside this repo. |
| Sizing decides the shape | partial | Two or more topology signals refuse a Lead-less spawn (`tpl/rules/_all/lead-gate.md:33-37`). `--solo` needs a recorded reason (`src/worker/leadgate.rs:312-328`). | The gate decides solo versus Lead only. Nothing chooses between one worker and a pipeline. `Topology:` is "the coordinator's declaration, not a promise the engine can check" (`tpl/rules/_all/brief.md:20`). |
| Brief shape and budgets, no pre-solve | partial | Header contract and machine-checked lines (`tpl/rules/_all/brief.md:3-18`). Time budget is machine-read (`tpl/rules/_all/worker-timebox.md:3`). Review policy parsed (`src/pipeline/mod.rs:603-606`). | A missing time budget falls back to 2700 s instead of failing (`tpl/rules/_all/worker-timebox.md:3`). Cost budget is optional (`src/worker/prompt/cost.rs:102`, returns `Option`). "No pre-solve" is prose (`tpl/rules/_all/brief.md:30`). |
| Plan before risky build; mechanical skips | partial | Pipeline plan gate with approval-by-resume (`src/pipeline/mod.rs:1043-1052`). | Outside a pipeline, the ">2 files or shared code paths" rule is prose (`tpl/AGENTS.md:44`). The approver is whoever resumes; the engine does not check the approver's role. |
| Build in isolation, commit and report as handback | met | Worktree per worker (`src/worker/worktree.rs:21-25`). No-commit build is blocked (`src/pipeline/gate.rs:180-194`). Report is the only accepted handback (`tpl/AGENTS.md:78`), parsed and validated (`src/worker/prompt/outcome.rs:1-3`). | None at this stage. |
| Independent review, different harness, capped rounds | partial | Same-harness pairs are rejected at run time and at config load (`src/pipeline/mod.rs:631-633`, `src/config.rs:825-843`). Fresh reviewer each round (`src/pipeline/mod.rs:48-49`). Reviewer worktree change voids the verdict (`src/worker/prompt/readonly.rs:1-4`). | `--review auto` skips review for small diffs (`src/pipeline/gate.rs:198-205`); `--review skip` and `builder_reviewer_differ = "none"` are allowed (`src/config.rs:794`). Work done outside a pipeline has no review requirement. |
| Verification reruns at the exact commit | partial | A verdict is stored with the head SHA (`src/worker/prompt/verdict.rs:250-262`). `quaqua status` marks a verdict `stale` when HEAD moved (`src/status.rs:296`, `:909-910`). Reviewer is bound to the build SHA (`src/pipeline/mod.rs:63-64`). | The engine does not rerun checks itself. `quaqua merge` only warns on a missing verdict and keeps its exit code (`src/merge/mod.rs:240`, test at `:580`). A journal failure "warns and never turns into a block" (`src/worker/prompt/verdict.rs:8-9`). |
| Merge queue into an integration branch | met | One branch at a time into `quaqua/<slug>-integ`, check after each merge, undo on failure or conflict (`src/merge/mod.rs:11-22`, `:327`, `:526-549`). | The merge queue is not triggered by the pipeline. No merge event is journaled (`src/status.rs:1103`). |
| Human approves only irreversible actions | partial | `git push` asks; force push, push to main and `gh pr merge <main>` are denied (`src/gate/policy.rs:819-841`, `:900-915`). Workers cannot push (`src/init.rs:116-124`). | Delete, deploy, spend and publish have no engine check. The gate covers only Claude Code Bash calls. There is no approval record, so "the owner's only touch is approvals" cannot be counted. |
| Retrospective trace, at most one lesson | partial | Every report must end with `## Lessons` (`tpl/rules/_all/brief.md:61`, `:67`). The auditor flags unpromoted lessons (`src/auditor.rs:204`, test `:1443`). | "What worked, what failed" is not a required field. "At most one" is prose (`tpl/AGENTS.md:24`). |
| Lifecycle measured and tuned from data | partial | Pipeline rows record rounds, `review_changed_outcome`, status and minutes (`src/status.rs:1060-1073`). Worker rows record wall time, dollars and tokens (`src/worker/cleanup.rs:13-34`). `status --metrics` reports verdicts, playbook gaps and merges without a verdict (`src/status.rs:1097-1106`). | Lead time is per run, not per task from intake to merge. There is no task id joining rows. No code tunes anything from the data. |

## Refs

- Pipeline flow and exit codes: `src/pipeline/mod.rs:41-87`.
- Brief contract: `tpl/rules/_all/brief.md:1-34`.
- Human-step count: `.harness/plans/active/harness-mvp.md:17`.
- All other claims cite `file:line` in the tables above.

## Fixes

1. Add a `task` record in `runs/quaqua.db` with a stage column, and make every command advance or refuse based on it. That gives one lifecycle, one id for metrics, and a place to refuse a merge without a fresh verdict.
2. Turn the merge verdict warning into a refusal with an explicit override flag that is journaled.
3. Add `quaqua ship <task>`: verify at head, enqueue the merge, open the PR, then stop for one approval. The plan already chose this shape (`.harness/plans/active/harness-mvp.md:18`).
4. Add `quaqua intake` that writes goal, success signal and non-goals to the task record, and make `--check-brief` fail when the brief's task has none.
5. Require `Time budget` and `Cost budget` in `--check-brief` instead of defaulting.
6. Make review mandatory unless the brief declares mechanical work, and record the skip reason on the task.
7. Record approvals (who, what, SHA) as events.
