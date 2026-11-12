# Questions for the owner

**Verdict: PARTIAL, 3/5.** The repository answered most of the audit. Nine questions remain where the evidence is ambiguous or where the answer changes a recommendation. The score reflects how much of the audit could be settled from evidence, not the engine's quality.

Audited at `41a5327`.

## Scope

- Brief deliverable 11: "questions for the owner, only where the evidence is truly ambiguous."
- Brief step 5: "Do not invent facts. If you can't verify something, say so."

## Findings

| Question | Status | Evidence | Gap |
|---|---|---|---|
| 1. The live workspace was not available to this audit. `~/quaqua` and `~/Lab/quaqua` do not exist in the environment I ran in; the engine was at `/home/user/quaqua`. Do you want a second pass run on your machine against the real `runs/`, `memory/` and `reports/`? | missing | `ls ~/Lab ~/quaqua` returned "No such file or directory" for both. | Every "current value" in `08-reference-targets.md` that depends on telemetry, and every statement about what the CoS actually does, is unverified. |
| 2. Should review be mandatory for every change that merges, or is skipping review for small diffs intended? | partial | `--review auto` skips at 150 changed lines or fewer in one top-level directory (`crates/quaqua/src/pipeline/gate.rs:198-205`). The requirement says "Review is independent" with no size exception, and the target says "0 self-reviewed merges". | If skipping is intended, the target needs a carve-out for mechanical work. If not, the default should be `always`. |
| 3. Is "different harness" enough for independence, or must the model vendor differ too? | partial | The check compares harness names (`crates/quaqua/src/pipeline/mod.rs:631-633`). In the template, `ni`, `suhuynh` and `truonglao` are all `claude · opus-5.5` (`templates/workspace/team.toml:68-85`), and a `claude` builder with an `omp` reviewer running a Claude model would pass. | The requirement says "a different harness or model". The config option `builder_reviewer_differ` accepts `none`, `harness` or `kind`, with no `model` or `vendor` value (`crates/quaqua/src/config.rs:794`). |
| 4. Must the Chief of Staff be replaceable by a non-Claude harness? | partial | "Provider neutrality: no workflow logic depends on one vendor" versus CoS hooks, router skill and memory mod that are Claude Code specific (`templates/workspace/.claude/settings.json`, `mods/cos-memory/README.md:38-60`). A dormant "Claude out" plan exists (`templates/workspace/models.md:29-35`). | If yes, the gate, inbox, memory and review fork need a harness-neutral core. If the CoS may stay on Claude Code, I would score provider neutrality higher. |
| 5. Is a prompt-only tool restriction on the review fork acceptable? | partial | The fork API takes no tool list (`mods/cos-memory/README.md:123-129`). Your own plan lists this as open (`.harness/plans/active/harness-mvp.md:20`). | Decides whether the fork stays in the mod or moves to a separate headless process. |
| 6. Is the pane grid a product requirement or a convenience for watching? | partial | `crates/quaqua/src/grid/` is 4720 lines and sits on the pipeline's critical path (`crates/quaqua/src/pipeline/mod.rs:71`, `:83-87`). The intent asks only for "no manual layout or cleanup". | If watching panes matters to you, keep it as an optional viewer. If not, it can leave the spawn path entirely. |
| 7. When a cost cap is reached on a harness that cannot be interrupted (Claude, pi), may the engine kill the pane? | partial | No stop sequence exists for Claude and pi (`crates/quaqua/src/harness/mod.rs:68`). Your plan asks the related question (`.harness/plans/active/harness-mvp.md:26`). Killing a pane can lose uncommitted work. | "No task overruns its hard cap" cannot be met for those kinds without an answer. |
| 8. Which of delete, deploy, spend and publish does the system actually perform today? | partial | The gate classifies only `git push` and `gh` writes (`crates/quaqua/src/gate/policy.rs:1-6`). The requirement lists six irreversible actions. | If workers never deploy or spend, a deny-by-default rule is enough. If they do, each needs an approval path. |
| 9. Who made the 6 commits on `main` that have no PR reference, and is direct commit to `main` allowed for docs? | partial | `fe188ba`, `966d965`, `160c3ed`, `1ee6b8d`, `cad5ea7`, `fcdb4a0` (`git log --first-parent --oneline main`). `fe188ba` is a `feat(layout)` change, not docs. The workflow file says the checks are required by branch protection (`.github/workflows/test.yml:3-6`). | I cannot see branch protection settings or tell an owner commit from an agent commit by author name. This affects "versioned and reviewed like code" and "every harness change ships only with green evals". |

## Refs

- Each row cites its evidence.
- Environment facts: primary working directory `/home/user/quaqua`; no `sqlite3` on PATH; no herdr; no workspace directory.

## Fixes

Ranked by how much each answer unblocks.

1. Answer question 1 first. A workspace pass would turn most "not available to this audit" cells into numbers.
2. Answer questions 2, 3 and 7. They set three defaults that guard the quality and cost targets.
3. Answer questions 4 and 6. They decide the size of the redesign in `09-drift-architecture.md`.
4. Answer questions 5, 8 and 9 as they come up in the related work.
