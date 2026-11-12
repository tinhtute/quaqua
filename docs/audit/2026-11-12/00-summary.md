# quaqua intent audit, 2026-11-12: summary

**Verdict: PARTIAL, 3/5.** quaqua is a well-tested orchestration CLI with real machinery for briefs, isolation, review, time limits and memory caps. It is not yet the self-driving lifecycle the intent describes: the stages are separate commands chained by a coordinator from prose, cost is not a hard cap, merges are not gated on a verdict, and most reference targets cannot be measured.

Audited at commit `41a5327` on `main`, engine version 2.6.0.

## Scope

The whole brief: intent, all requirement groups, reference targets, drift, and architecture. This file links the ten detailed files.

Limits of this audit:
- The live workspace (`~/quaqua`) and the path `~/Lab/quaqua` did not exist in the environment. The engine repo was at `/home/user/quaqua`. Nothing about real runs, memory contents, telemetry values or installed timers could be checked.
- No herdr and no agent harness were installed, so no worker was spawned. Behaviour claims rest on code, tests and docs.
- `cargo test --locked --no-fail-fast`: 1183 passed, 7 failed, 1 ignored. All 7 failures are in `crates/quaqua/tests/usage.rs:59` and are caused by the missing `sqlite3` CLI in this container.

## Findings

### Scorecard

| File | Goal | Verdict | Score |
|---|---|---|---|
| [01-harness-engineering.md](01-harness-engineering.md) | Harness principles and standards | PARTIAL | 3/5 |
| [02-adlc.md](02-adlc.md) | Standard ADLC, stage by stage | PARTIAL | 3/5 |
| [03-slp-team.md](03-slp-team.md) | SLP team | PARTIAL | 3/5 |
| [04-fanout.md](04-fanout.md) | Fanout | PARTIAL | 3/5 |
| [05-cos-memory.md](05-cos-memory.md) | Chief of Staff memory and learning | PARTIAL | 3/5 |
| [06-pstack.md](06-pstack.md) | Playbooks, router, verdicts, warn-to-gate | PARTIAL | 3/5 |
| [07-rules-self-improvement.md](07-rules-self-improvement.md) | Rules and self-improvement | PARTIAL | 2/5 |
| [08-reference-targets.md](08-reference-targets.md) | Reference targets | FAIL | 2/5 |
| [09-drift-architecture.md](09-drift-architecture.md) | Drift and architecture | PARTIAL | 3/5 |
| [10-owner-questions.md](10-owner-questions.md) | Owner questions | PARTIAL | 3/5 |

### Requirement status by group

| Group | Met | Partial | Missing | Contradicted |
|---|---|---|---|---|
| Harness engineering (5) | 0 | 5 | 0 | 0 |
| Harness standards (11) | 2 | 9 | 0 | 0 |
| Standard ADLC (12) | 2 | 9 | 1 | 0 |
| SLP team (6) | 3 | 2 | 1 | 0 |
| Fanout (5) | 0 | 4 | 1 | 0 |
| CoS memory (6) | 2 | 4 | 0 | 0 |
| pstack (5) | 1 | 4 | 0 | 0 |
| Rules and self-improvement (5) | 1 | 3 | 1 | 0 |
| **Total (55)** | **11** | **40** | **4** | **0** |

### Top 10 gaps

| # | Gap | Status | Evidence | Detail |
|---|---|---|---|---|
| 1 | No end-to-end lifecycle. Stages are separate commands; 6 human steps remain between brief and merge. | missing | `.harness/plans/active/harness-mvp.md:17`; `crates/quaqua/src/pipeline/mod.rs:41-58` | [02](02-adlc.md) |
| 2 | A merge without a verdict at the head commit only warns. | partial | `crates/quaqua/src/merge/mod.rs:240`, test at `:580` | [02](02-adlc.md), [06](06-pstack.md) |
| 3 | Cost is not a hard cap. Default action is `warn`; Claude and pi cannot be interrupted; agy spend is unreadable. | partial | `crates/quaqua/src/config.rs:29-36`; `crates/quaqua/src/harness/mod.rs:68`; `crates/quaqua/src/worker/prompt/cost.rs:776-781` | [04](04-fanout.md) |
| 4 | No supervising process. Limits are enforced only while a coordinator's backgrounded `quaqua worker prompt` is alive. | partial | `templates/workspace/AGENTS.md:62`; `crates/quaqua/src/status.rs:58` | [09](09-drift-architecture.md) |
| 5 | The engine never reruns verification; acceptance trusts the coordinator to rerun. Small diffs skip review. | partial | `templates/workspace/AGENTS.md:78`; `crates/quaqua/src/pipeline/gate.rs:198-205` | [01](01-harness-engineering.md) |
| 6 | "Coordinators never write code" has no enforcement. | missing | `templates/workspace/AGENTS.md:60`; `templates/workspace/.claude/settings.json:28-33` | [03](03-slp-team.md) |
| 7 | No task id and no merge, approval, resume or wave events, so most targets cannot be measured. | missing | `crates/quaqua/src/status.rs:1103`; `crates/quaqua/src/worker/cleanup.rs:13-34` | [08](08-reference-targets.md) |
| 8 | The learning loop is manual: failure logging, lesson promotion and mechanism switch-off are prose. | partial | `templates/workspace/AGENTS.md:24-25`; `crates/quaqua/src/status.rs:1070` | [07](07-rules-self-improvement.md) |
| 9 | The review fork is not installed by `quaqua init`, not tested in CI, and not shown firing live. No skill creation exists. | partial | `mods/cos-memory/README.md:32-33`; `.harness/plans/active/harness-mvp.md:19-24` | [05](05-cos-memory.md) |
| 10 | No recorded end-to-end eval. CI skips the shell suites, Node tests, clippy and fmt. Wave accounting and intake do not exist in code. | partial | `.github/workflows/test.yml`; `.harness/PROJECT.md:14-18` | [01](01-harness-engineering.md), [04](04-fanout.md) |

### What is solid

- Durable, event-sourced workflow runner on SQLite with replay and snapshots (`crates/quaqua/src/engine/runner.rs:1-5`).
- Typed reports and named watchdog exit codes 0–7 (`crates/quaqua/src/worker/prompt/mod.rs:56-69`).
- Worktree-per-worker isolation with credential scrubbing (`crates/quaqua/src/worker/worktree.rs:21-25`, `crates/quaqua/src/worker/spawn.rs:21-31`).
- Cross-harness review enforced at config load and at run time (`crates/quaqua/src/config.rs:825-843`).
- Verdicts bound to a SHA, with stale ones visible (`crates/quaqua/src/status.rs:296`).
- Memory caps and secret gate as constants in the binary (`crates/quaqua/src/memory.rs:27-29`).
- A hermetic test suite with a live-database guard in CI (`.github/workflows/test.yml:41-69`).

## Refs

- Commit audited: `41a532773c48cc7028aeeba17e59862f2cfb4b58`.
- Commands run: `cargo build --locked`, `cargo test --locked --no-fail-fast`, `quaqua --help` and each subcommand's `--help` or `-h`.
- Every claim in the linked files cites `file:line`, a command and its output, or a commit SHA. Claims without one are labelled INFERENCE.
- Scores are my judgment against the brief's requirements. They are not computed from a formula.

## Fixes

### Shortest path to the success signal

The success signal: "the owner hands over a goal, and the system carries it from brief to merged, verified change on its own, with a wide mixed team of agents, independent review, enforced limits, and a Chief of Staff that remembers and improves. The owner's only touch is the approvals."

In order. Each step is usable on its own.

1. **Task record.** Add a `tasks` table and a task id carried by every spawn, pipeline, verdict and telemetry row. Journal `merge` and `approval` events. This unblocks measurement and gives the lifecycle one spine. ([02](02-adlc.md), [08](08-reference-targets.md))
2. **Gate the merge.** `quaqua merge` refuses a branch with no passing verdict at its head, with a journaled override. Make review default to `always`. ([02](02-adlc.md), [06](06-pstack.md))
3. **Engine-run verification.** The engine reruns the brief's verify command at the report SHA and records a verifier verdict. ([01](01-harness-engineering.md))
4. **`quaqua ship <task>`.** Verify at head, merge through the queue, push, open the PR, then stop for one approval. This removes four of the six human steps. ([02](02-adlc.md))
5. **Hard cost cap.** A kill path for every harness and a spend source or estimate for agy. ([04](04-fanout.md))
6. **Supervisor daemon.** Move watchdogs, retries and scheduled jobs into one long-lived process on the existing runner, so limits and recovery do not depend on a coordinator's shell. ([09](09-drift-architecture.md))
7. **Coordinator write deny and wave accounting.** Turn the two most load-bearing prose rules into checks. ([03](03-slp-team.md), [04](04-fanout.md))
8. **Install and prove the review fork.** `quaqua init` installs it, `quaqua doctor` checks it, and one live firing is recorded. ([05](05-cos-memory.md))
9. **Close the learning loop.** The engine writes failure cases; cleanup requires a lesson decision; the auditor proposes switch-offs from outcome counts. ([07](07-rules-self-improvement.md))
10. **Record the evals.** One end-to-end run, one 8-worker mixed wave, one reboot-and-resume, gated in CI. ([01](01-harness-engineering.md), [08](08-reference-targets.md))

Steps 1 to 4 reach "brief to merged, verified change with approvals as the only touch" for a single pipeline-size task. Steps 5 to 7 make it safe to widen. Steps 8 to 10 make it improve and prove it.
