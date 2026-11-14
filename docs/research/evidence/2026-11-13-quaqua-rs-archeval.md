Outcome:         COMPLETE
Scope:           read only: ~/Lab/.wt/truonglao-archeval (engine @43a60bd, = ~/Lab/quaqua main), ~/quaqua/{team.toml,AGENTS.md,playbooks/,rules/_all/,projects/quaqua-rs.md,.monows/plans/active/team-config-single-source.md,reports/,runs/}, ~/quaqua/runs/quaqua.db (sqlite3 -readonly, mode=ro), `herdr --help` 0.9.5. Written: this file only.
Verification:    every number below comes from a command run in this session (listed under "Re-run" at the end of Findings). Deliberately skipped: `quaqua status --all` (I queried the DB directly, read-only), cargo build/test (no code changed), and `herdr agent` calls other than --help (brief forbids touching agents).
Unknown / risk:  the prod/test LOC split is a heuristic: lines before the first column-0 `#[cfg(test)]`, plus tests.rs, testkit.rs and test_env.rs counted as test. "Unused in practice" covers this one workspace's DB (2026-10-20..2026-11-13) only. Other workspaces or a Linux host (systemd timers) may differ.
Ownership:       scope released; nothing held.

## Findings

### Overview: the verdict first
- The hypothesis is half right. engine/ carries no harness coupling, and pipeline/ carries none in production code. The harness coupling sits in worker/prompt/cost.rs, launch.rs, auditor.rs, worker/prompt/retry.rs, usage.rs and the closed harness/ enum. What pipeline/ and engine/ do carry is duplicated workflow state: a JSON `PipelineState` beside journal snapshots, and about 30 sidecar file types in runs/. (measured, sections 3a and 3b)
- The hard limit on "any harness from config" is herdr itself plus one quaqua enum. `herdr agent start --kind` accepts a closed list of 24 kinds (`herdr agent start --help`). quaqua passes the harness name straight into it (`crates/quaqua/src/worker/spawn.rs:812` @43a60bd). The `Harness` enum is closed too (`crates/quaqua/src/harness/mod.rs:9-15`), and `resolve_launch` rejects any other name (`crates/quaqua/src/launch.rs:25-31`). (measured/cited)
- The binary has grown about 20× past its charter. The locked plan was to replace "~3.9k lines of bash/jq" (`docs/research/herdr-grid-rust-migration-plan.md:6`) with an explicit non-goal of "No daemon, mailbox or SQLite" (`:21`). Today there are 78,829 Rust LOC (measured: `find crates/quaqua/src -name '*.rs' | xargs cat | wc -l`), and runs/quaqua.db holds 96,265 journal events and 666 bus triggers (measured: sqlite3 count). (measured/cited)

### 1. Where the goal came from and how it evolved (git log, plan, card)
- 2026-10-14 `7637e3c` "initial public release of Quaqua (SLP orchestration on Herdr)": bash scripts plus `plugins/herdr-grid` (plan.jq, hook.sh, apply.sh). (cited: `git log --reverse`)
- 2026-10-15 `9990ade` "rewrite Rust plan as one quaqua binary". The plan was locked with anh iu: one stateless binary, a big-bang cutover, `team.toml` as the single source, the CLI plus the PreToolUse hook as the fence, and the goal "correctness and testability, not a security boundary" (`herdr-grid-rust-migration-plan.md:3-17`). Merge queue and taskforce were folded in from piggery (`:160-170`). (cited)
- R0–R5 landed in 2 days: R0 `d2ad7a8` 10-15, R2 grid `df3bb03` 10-16, R3 worker `1cee9c4` 10-16, R4 lead/pipeline/merge/taskforce `6cb7688` 10-16, R5 gate `c4075c1` 10-16. Cutover `a35a54d` 2026-10-18 "remove 9 bash wrappers"; auditor and usage were ported the same day (`ae9a7ef`, `49db2fd`). (measured: `git log --diff-filter=A/D`)
- After cutover the scope went past the plan's non-goals:
  - SQLite workflow journal: `0c7c71a` 2026-10-20, "engine core rewrite W0–W2".
  - harness registry: `25e4035` 10-21.
  - playbooks: `ed2b955` 10-22.
  - SQLite mail bus: `e97391d` 10-24.
  - farm layout v3: `9441c9c` 10-24.
  - identity/depth: `972bd3e` 10-24.
  - github-ci source: `be87d7a` 11-09.
  - transcripts: `037f660` 11-11.
  - memory gate: `232fd77` 11-11.
  - ship: `9876df3` 11-12.

  The repo now has 899 commits, with v2.5.0 on 10-27 and v2.6.0 on 11-05 (`CHANGELOG.md`). (measured)
- The project card still states the original vision, "One quaqua Rust binary replaces the bash bodies… team.toml is the single config source" (`~/quaqua/projects/quaqua-rs.md:7`). The active line is the 2026-11-13 bigbang: generic driver, SQLite config, inner/outer loop, omp/sude removal (`:12`). The plan being mapped is `team-config-single-source.md:8-9`. (cited)
- Inferred from the commits above: the Rust port did reach its goal. Growth since then came from adding durability (journal, bus) and observability (status, estimate, usage, auditor) on top of the stateless design, without retiring the file-based state it replaced.

### 2. Current architecture as it is

#### Module map (one line each; LOC = wc -l on the whole module, measured; prod/test is a heuristic)
| subsystem | LOC (prod/test) | job | verdict | reason |
|---|---|---|---|---|
| worker/ | 14,411 (8,938/5,473) | spawn, prompt watchdog (exits 0–8), brief check, playbook pin/check, verdict, cost, provider retry, cleanup, Lead gate | rewrite | Holds most of the harness coupling: cost.rs has 48 harness-specific prod lines, with per-harness spend and session parsers (`worker/prompt/cost.rs:143-986`); retry.rs 5; spawn.rs 2 (`:757` omp, `:825` claude). The watchdog rules and brief/playbook/verdict logic are worth porting as-is. Session and spend reading should move behind a driver. |
| engine/ | 10,390 (4,388/6,002) | event-sourced workflow journal, runner, activities, clock seam, testkit | keep (reshape) | Harness-agnostic: 1 prod harness line (`engine/activities.rs:674`, opencode.db probe). Three Workflow impls run on it (pipeline `pipeline/mod.rs:2378`, taskforce `taskforce/workflow.rs:345`, worker `worker/prompt/workflow.rs:289`). Fix the journal bloat (section 3c). `engine/telemetry_otel.rs` (440) is dead: `JsonlSpanSink` has no caller outside its file, and `~/quaqua/runs/traces.jsonl` does not exist. Delete it. |
| pipeline/ | 8,126 (4,340/3,786) | build → review → fix state machine | rewrite | `run_pipeline` is one function from `pipeline/mod.rs:301` to `:2360`, about 2,059 lines (measured: `grep -n '^fn '`). It keeps a 32-field `PipelineState` JSON (`pipeline/state.rs:5-36`) and also seeds a journal snapshot from it (`pipeline/mod.rs:157,964`): two homes for one state. It drives workers by building CLI arg vectors (`pipeline/mod.rs:1251,1326` `worker::spawn::parse_spawn_args`). |
| bus/ | 5,541 (5,204/337) | SQLite triggers, threads, mail, recall, farm migration, GUI views | reshape | Trigger log is used: 666 triggers, mostly worker spawned (187), report_written (128) and herdr blocked/agent_status (219). Mail is barely used: 36 mail-source triggers. migrate.rs (949) is a one-time v1→v2 farm migration. gui.rs serves the parked GUI (plan NG1/NG2). Keep events and triggers; delete migrate and gui. |
| grid/ | 4,909 (3,001/1,908) | reconcile worker panes into tabs; herdr plugin hook | reshape (shrink) | Cosmetic pane placement, called by the herdr plugin (`plugins/herdr-quaqua/herdr-plugin.toml`: `command = ["quaqua","grid","hook"]`). herdr 0.9.5 has imperative `pane move`, `tab create` and `pane split` but no declarative layout (`herdr pane --help`, `herdr tab --help`), so herdr cannot own the policy (inferred). reconcile.rs (2,659) is far larger than the job needs. |
| layout/ | 2,582 (2,582/0) | spawn/prompt/reports_to authority graph, v2→v3 toml migration | reshape | This is the agent topology graph, not pane geometry, so herdr cannot own it (`layout/model.rs` `Layout{depth,roles}`, `edge_set`). Live `~/quaqua/team.toml:2` is `schema_version = 2`, so the v3 `[layout]` path, `layout/migrate.rs` (488) and `legacy.rs` (103) never run here (measured). The graph belongs in the SQLite config (Q9 decided). Delete the toml rewriter. |
| lead/ | 2,623 (1,738/885) | Lead spawn/prompt/card/tag/cleanup | reshape | A second spawn path beside worker spawn: both go through `launch.rs`, and lead calls herdr `agent start` itself (`lead/spawn.rs:467`). Fold into one spawn with a role parameter (inferred). |
| taskforce/ | 2,610 (1,811/799) | advisor / dual-lens / council read-only runs | delete | Unused in practice. Its workflow_id is `{mode}:{task}` (`taskforce/driver.rs:413`) in runs/quaqua.db, and that DB has 0 rows with `advisor:`, `dual-lens:` or `council:` prefixes. Only the `worker` (407 workflows) and `pipeline` (93) prefixes exist. There are no `runs/council-*` or `runs/taskforce-*` files. The single `runs/suhuynh-pstackfit.taskforce` file holds the owner tag `pstackfit`, not a run (measured). It also keeps a third state home, `runs/taskforce-<task>.state` (`driver.rs:194-196`). This can be re-expressed as a playbook over N read-only workers. |
| gate/ | 1,863 (1,784/79) | PreToolUse(Bash) fence over herdr and outward git/gh calls, plus rule-drift warning | keep | It is the fence (`gate/mod.rs:1-8`). The PreToolUse JSON is a Claude Code hook (inferred from `gate/mod.rs:1-3` and `launch.rs:191` `claude_settings`), so non-claude workers are not fenced by it. That is a known gap for "any harness". |
| transcripts/ | 1,112 (787/325) | index, search and tail past CoS sessions | keep | Outer-loop input (input for gardener jobs). |
| merge/ | 814 (570/244) | merge queue into `<slug>-integ` | keep | Used: 6 `*-integ` branches on origin (`git branch -a --list '*integ*'`). |
| playbook/ | 569 (569/0) | `quaqua playbook list/show` | keep | Inner-loop core. |
| source/ | 268 (268/0) | github-ci poll → `ci_failed` triggers | delete or park | Unused on this workspace: 0 triggers from `github-ci`, and source_state has only `mail_delivery` (8) and `wake` (3) rows. The installer is systemd-only (`templates/workspace/scripts/github-ci-poll/systemd/*`), and this host is darwin. |
| harness/ | 143 (94/49) | closed enum: name, effort set, stop keys, fallback idle | rewrite | A closed enum (`harness/mod.rs:9-15`) blocks config-only harnesses. It becomes the HarnessDriver registry plus the generic driver (Q11 decided). |
| status.rs | 5,233 (2,199/3,034) | status overview from journal, sidecars and herdr | reshape | Size follows from state spread across the journal, `runs/*.owner` and herdr (`status.rs:1-4`). It shrinks once state has one home (inferred). |
| auditor.rs | 2,297 (1,370/927) | read-only drift/pipeline audit run by a headless harness | keep (reshape) | Outer loop that runs: 47 `audit-*` reports in `~/quaqua/reports` (measured). Has 12 harness-specific prod lines (headless claude/opencode/agy, `auditor.rs:1-6`). Should launch through the driver. |
| doctor.rs | 2,101 (1,186/915) | binary, PATH, systemd and auditor checks | reshape | Has 3 harness-specific prod lines. Keep a small core. |
| usage.rs | 1,907 (1,907/40) | OpenCode usage audit | delete (merge) | Header reads "OpenCode usage audit" (`usage.rs:1`), so it covers one harness, and it overlaps cost.rs spend parsing. Fold into the driver's cost capability. |
| config.rs | 1,597 (1,114/483) | team.toml load and validate | rewrite | SQLite becomes the source after seeding (Q4/Q5 decided). |
| identity.rs | 1,324 (819/505) | identity and depth from pane tokens and owner chains | keep | Needed for the authority graph. |
| main.rs | 1,285 | clap CLI, top-level commands at `main.rs:35-196` | reshape | The command surface follows from the verdicts above. |
| init.rs | 1,189 | workspace init and `--update` | reshape | Q2 found conflicts on every update. |
| launch.rs | 786 (477/309) | resolve kind → launch; per-harness argv `match` | rewrite | One `match launch.harness.as_str()` arm per harness (`launch.rs:299`, arms opencode/omp/pi/agy/claude). This is driver code. |
| liveness.rs | 629 | "absence ≠ broken" agent checks | keep | — |
| herdr.rs | 592 (455/137) | herdr CLI adapter | keep (reshape) | Two traits for one CLI (`HerdrOps` `:5`, `HerdrDriver` `:375`). Unify them. |
| memory.rs | 584 | CoS memory caps and secret gate | keep | — |
| state.rs | 496 | atomic sidecar reads and writes | reshape | Used by 23 files (`grep -rl crate::state::`). Its sidecars duplicate the journal (section 3b). |
| cos.rs, estimate.rs, workspace.rs, hash.rs, ship.rs, cap.rs, git.rs, rules.rs, verify.rs, util.rs | 461/394/359/271/269/257/236/222/145/55 | CoS tag, ETA, workspace root, hashing, PR ship, cap, git, rules dump, verify, util | keep | Small single-purpose modules. cap.rs is one of four authority checks (section 3d). |

#### Data flow CoS → quaqua → herdr → worker CLI → report → verify/merge (cited, with inferred glue)
1. The CoS (a claude pane in herdr) runs `quaqua worker spawn`. The PreToolUse hook `quaqua gate` first vets the Bash call (`gate/mod.rs:1-8`). The authority checks are cap (`cap.rs`), the Lead gate (`worker/leadgate.rs:1-8`) and layout roles (`layout/gate.rs`).
2. `config.rs` loads `team.toml`. `launch::resolve_launch` maps kind → harness/model/effort (`launch.rs:20-60`). `build_launch_cmd` builds per-harness argv (`launch.rs:289-440`).
3. quaqua makes the git worktree itself (`git.rs:97-138`), even though herdr has `herdr worktree create` (`herdr worktree --help`). It then splits a pane and starts the agent: `herdr agent start <name> --kind <harness>` (`herdr.rs:249-271`, `spawn.rs:812`).
4. `quaqua worker prompt` checks the brief headers (`worker/prompt/brief.rs`), pins the playbook, and composes the prompt with the rules dump in `runs/<name>.rules.md` (408 such files in runs/). It submits through `herdr agent prompt`.
5. The `SingleWorkerWorkflow` runs on the engine runner (`worker/prompt/workflow.rs:1-12,289`). Each poll is a set of journaled activities: Herdr AgentWait (24,443), Git DiffHash (2,350), ReportFile CheckMtime (2,350), Session Mtime (2,326) (measured: activity_scheduled payloads grouped by family). The run ends on the `runs/<name>.md` report or an exit code from 2 to 8.
6. On acceptance it records the verdict, playbook gaps and cost (`worker/prompt/verdict.rs`, `playbook_check.rs`, `cost.rs`), plus bus triggers (`bus/events.rs`).
7. `quaqua pipeline` repeats steps 1–6 for builder → reviewer → fix rounds (`pipeline/mod.rs:1251-1800`). `quaqua merge` merges into `quaqua/<slug>-integ` with a project check (`merge/mod.rs`). `quaqua ship` pushes and opens a PR after approval (`ship.rs`).
8. Running beside this: herdr plugin events → `quaqua grid hook` / `identity heal` (`plugins/herdr-quaqua/herdr-plugin.toml`), the auditor twice daily, and the status, doctor and usage reads.

#### What herdr owns vs what quaqua owns
- herdr owns (cited: `herdr --help`, `herdr agent --help`, `herdr pane --help`):
  - session, workspace, tab and pane lifecycle;
  - agent start and readiness detection for 24 built-in kinds;
  - agent state (idle/working/blocked) through integrations;
  - prompt delivery (`agent prompt --wait`), terminal read, send-keys, notifications;
  - the event subscription and snapshot API;
  - a lifecycle-report hook any process can call: `herdr pane report-agent --state idle|working|blocked|unknown`.
- quaqua owns: roster/config, the authority graph, worktrees, rules and brief injection, the watchdog with budgets, cost and retry, the report contract, the review/fix loop, the merge queue, the journal and bus, grid placement policy, auditor, status and doctor.
- Overlap and opportunity (inferred):
  - quaqua creates worker worktrees with git (`git.rs:97-138`), not with herdr's `worktree create`.
  - quaqua never calls `pane report-agent`; only `report-metadata` appears (measured: herdr subcommand literal count). For a harness outside herdr's 24 kinds, a generic driver could launch it with `pane run` and report its state through `report-agent`. Q20 has to confirm whether herdr then trusts that state for `agent wait`.

### 3. Where it hurts for "any harness, any model, from config"
- a. Harness behaviour is spread across at least 9 files instead of one driver. Production harness-specific lines per file (measured, heuristic split): worker/prompt/cost.rs 48, harness/mod.rs 20, auditor.rs 12, launch.rs 11, worker/prompt/retry.rs 5, usage.rs 5, doctor.rs 3, worker/spawn.rs 2, config.rs 2, status.rs 1, init.rs 1, engine/activities.rs 1, bus/events.rs 1. Each new harness has touched launch argv, effort sets, stop keys, idle fallback, spend parser, session-mtime probe, error classifier and auditor invocation; the agy entry in `CHANGELOG.md` "Harness (agy)" shows the same pattern. (measured/cited)
- b. Duplicated state. The journal (`events` and `snapshots` tables) coexists with about 30 sidecar types in `~/quaqua/runs/`. The biggest counts: `.rules.md` 408, `.md` 394, `.read` 200, `.playbook` 148, `.submit.log` 124, `.elapsed` 112, `.budget` 107, `.progress` 100, `.readonly` 57, `.state` 52, `.prompt.{out,err,lock,exit,pid}`, `.owner` 9, `.layout_role` 9, `.layout.toml` 5, `.grace` 6, `.nudged` 14. On top of that, pipeline keeps its `PipelineState` JSON and taskforce its `.state` file. (measured: `ls -A runs | sed … | uniq -c`; cited `pipeline/state.rs`, `taskforce/driver.rs:194`)
- c. Journal bloat. `activity_scheduled` 31,870, `activity_completed` 31,858 and `continue` 31,820 make up 99% of 96,265 events. That is about 220 events per worker workflow (89,592 events / 407 workflows), and the DB is 25 MB plus a 2.4 MB WAL. No-op polls are being journaled. (measured) The fix is to journal state changes, not polls (inferred).
- d. Four places check authority over the same graph: `cap.rs` (cap and weights), `worker/leadgate.rs` (Lead required), `layout/gate.rs` (role edges), and `identity.rs` (depth). Depth and limits also live twice, in team.toml `[limits]`/`[roles.*]` and in the v3 `[layout]` design (`layout/legacy.rs` synthesizes one from the other). (cited)
- e. Modules too big for their job:
  - `pipeline::run_pipeline` is one function of about 2,059 lines;
  - grid/reconcile.rs is 2,659 lines of tab placement;
  - status.rs has 2,199 prod lines;
  - usage.rs is 1,907 lines for one harness's usage report;
  - bus/migrate.rs is 949 lines for a finished one-time migration. (measured)
- f. Dead or half-finished pieces:
  - telemetry_otel (no caller);
  - the v3 layout and migrate path (live config is v2);
  - taskforce (0 runs);
  - source/github-ci (0 rows here);
  - GUI views (parked);
  - agy spend is reported "unavailable" (`CHANGELOG.md` Harness (agy)), and pi has no stop sequence (`harness/mod.rs:67-68`). (measured/cited)
- g. The fence is Claude-specific. The PreToolUse gate and its settings file only fence claude panes (inferred: `launch.rs:191-244` `claude_settings`/`write_claude_settings`). An opencode, pi or agy worker can call herdr or `git push` without passing the gate.

### 4. Verdicts, in brief (reasons in the table)
- keep: engine core, merge, playbook, transcripts, gate, identity, liveness, memory, auditor, small utils.
- reshape: bus (drop migrate and gui), grid (shrink), layout (graph → SQLite config), lead (one spawn path), status, doctor, init, herdr.rs, state.rs.
- rewrite: worker/ (behind a HarnessDriver), launch.rs, harness/, config.rs, pipeline/.
- delete: engine/telemetry_otel.rs, taskforce/, usage.rs (fold into driver cost), source/ (or park), bus/migrate.rs, bus/gui.rs, layout/migrate.rs and legacy.rs (after a one-time import).

### 5. Inner and outer loop pieces that exist today
- Inner loop (exists, measured or cited):
  - 18 playbooks in `~/quaqua/playbooks/`, and `quaqua playbook list/show` (`playbook/mod.rs`);
  - 17 rules in `~/quaqua/rules/_all/`, dumped per worker into `runs/<name>.rules.md`;
  - the brief contract, checked by `worker prompt --check-brief` (`worker/prompt/brief.rs`);
  - playbook pinning and step accounting, with exit 6 on a report that skips pinned steps (`worker/prompt/playbook_check.rs`);
  - the read-only check (`worker/prompt/readonly.rs`), watchdog exits 0–8 (`worker/prompt/workflow.rs:1-12`), verdict levels (`worker/prompt/verdict.rs`), and review severity with `Must fix:`/`Review rounds:` (`CHANGELOG.md` 2.6.0);
  - the merge queue, the Lead gate, and the PreToolUse gate.
- Outer loop (partial):
  - Auditor: runs, with 47 `audit-*` reports, and counts unpromoted `## Lessons` (`auditor.rs:1124-1154`).
  - The `eval` playbook: manual, comparative or observational over runs/ and the journal (`~/quaqua/playbooks/eval.md:1-12`).
  - Lesson promotion: manual, by the CoS at cleanup, at most one (`~/quaqua/AGENTS.md:24`). No code does it.
  - The journal: 96k events, with `verdict` 204 and `playbook_gap` 1.
  - `runs/telemetry.jsonl` (369 rows), `estimate.rs` (ETA from journal durations), the transcripts index, and correction capture in the cos-memory mod (`mods/cos-memory/README.md:1-6`).
- Missing (measured or inferred):
  - Retro has no engine or workspace wiring. `grep -rln retro ~/quaqua/AGENTS.md ~/quaqua/playbooks ~/quaqua/rules crates/quaqua/src` matches only a comment at `gate/policy.rs:8`. Retro lives outside as a user-level skill (inferred).
  - No gardener agent, no scheduled scoring of worker conversations, and no automatic rule or skill proposal exists. The `playbook_gap` signal is emitted once in total, so it is not yet a working sensor.

### Re-run (the commands behind the numbers)
- LOC per subsystem: `find crates/quaqua/src/<dir> -name '*.rs' -print0 | xargs -0 cat | wc -l`. For the prod/test split, take lines before the first `^#\[cfg(test)\]` per file.
- Prod harness lines: count, per file and before `#[cfg(test)]`, matches of `grep -cE '"(claude|opencode|omp|pi|agy)"|Harness::(Claude|Opencode|Omp|Pi|Agy)|is_(omp|pi|claude|opencode)_worker|(omp|pi|claude|opencode|agy)_(spend|session|settings|cost)'`.
- DB: `sqlite3 -readonly "file:~/quaqua/runs/quaqua.db?mode=ro"` with:
  - `select kind,count(*) from events group by kind`;
  - the `substr(workflow_id,…)` prefix group;
  - `select source,kind,count(*) from triggers group by 1,2`;
  - the activity family/subtype from `json_each(payload_json->'$.activity')`;
  - the `source_state` prefix group.
- herdr: `herdr --version`, `herdr agent start --help`, `herdr pane report-agent --help`, `herdr worktree --help`.

## Playbook
1: done
2: done
3: done

Verified: inconclusive 43a60bd. This is a read-only assessment, so no build or test ran. Following principle-prove-it-works, every figure was read from the source with a re-runnable command (listed above), not from proxies. No verdict level for build or tests applies.

## Blockers
none

## Lessons
- Before assuming a subsystem is used, check its workflow_id prefixes in runs/quaqua.db: taskforce had 0 rows despite 2.6k LOC.
