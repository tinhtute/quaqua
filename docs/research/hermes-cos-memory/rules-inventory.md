# Rules inventory (hermes-cos-3 map Q18)

Inventory of every rule in the CoS rule sources, tagged `dead`, `enforced` or `live prose`. Feeds Q19 (which dead rules to delete) and Q20 (which live rules become code checks). Read-only research: no rule, memory or AGENTS.md file was edited.

Counts (one row = one rule):

- dead: 35
- enforced: 94
- live prose: 138
- total: 267

## Method

- Row unit: one rule bullet; a compound bullet is split into sub-rows when its parts get different tags.
- `dead`: the rule names a command, file, flag, env var or flow that this engine no longer has. Evidence is the grep (or `quaqua <cmd> --help`) that shows it is gone.
- `enforced`: a code check covers the rule's operative clause; evidence is `file:line`. Gaps in coverage are named in the evidence cell.
- `live prose`: no code check on the operative clause (includes partial checks that miss it, named in the evidence cell).
- Engine paths in Evidence are relative to `crates/qq-core/src/` unless they start with `~/` or `crates/`. Line numbers are from this worktree at HEAD 7491347.
- Command checks ran against the installed `quaqua` (usage text equals the worktree CLI table in `cli/mod.rs`). The CLI has: auditor, call, config, daemon, gate, grid, identity, inbox, init, lead, mail, memory, merge, ping, pipeline, playbook, push, roster, signal, status, stop, task, transcripts, worker (`worker` = spawn, prompt, cleanup, stop).
- herdr commands were not run (brief forbids); herdr subcommands are checked only through the client in `herdr/mod.rs`. An absent subcommand there is tagged `live prose`, never `dead`.
- Source scope: all files named in the brief. For `~/quaqua/AGENTS.md` the brief names the approval and coordinator prose; those two sections come first, the other sections follow in a separate table (listed for Q20, outside the named scope).

## Findings that matter for Q19 and Q20

- Whole files that are dead: `~/quaqua/rules/_all/taskforce.md` (the `taskforce` command is not ported) and the Farm scoping half of `lead-inbox.md` (no farm column on mail, no `mail inbox`, `mail ls`, `mail ack` or incident scoping).
- The cost watchdog is dead: no reader of `timers.cost_buffer`, no `ESCALATE`, no code that acts on `on_cost_cap` (only stored and validated in `config/`). `worker-timebox.md` and `brief.md` still describe it.
- The playbook append, `.playbook` pin, `## Playbook` report block and `## Blockers` stop-the-pipeline rules are dead: `task/` only validates the `Playbook:` header (`task/prompt.rs:115-126`), and `task/report_contract.rs:9-12` says those sections are not machine-read.
- `GRID_SKIP`, `runs/<name>.owner`, `runs/<name>.grid`, `quaqua ship`, `tests/run.sh`, `crates/quaqua/`, `config::load_team_config()` and `quansu` are gone; they appear in six rule files.
- Approval list: push and `gh` merge are code-checked; branch delete only for main/master; deploy, spend, outward send, release and tag have no check (`authority/gate.rs`).
- Stale citations inside rules: AGENTS.md cites `memory/caps.rs:25` (the caps are at `memory/caps.rs:27` and `:29`) and `task/spawn_cmd.rs:713` (`check_cap` is at `task/spawn_cmd.rs:707`); `brief.md` cites `VerdictLevel::ALL` (no such name).
- `~/quaqua/rules/_all` holds five files the engine template does not (`herdr-agent-status.md`, `herdr-tokens.md`, `lead-inbox.md`, `taskforce.md`, `test-isolation.md`); the other 15 files match `crates/qq-core/templates/workspace/rules/_all/` (`diff -rq` shows no content difference).
- Rules the engine itself seeds: `init/seeds.rs:18` ships the Ownership line of `memory/rules.md`; deleting it there needs an engine change, not only a workspace edit.

## ~/quaqua/memory/rules.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Advisor workers (quansu) only advise, never edit code | dead | `grep -rln quansu crates ~/quaqua/models.md ~/quaqua/team.toml` returns nothing; `quaqua roster` lists ni, sude, suhuynh, truonglao |
| Clean up each worker (pane + worktree) right after verified run and passing tests | live prose | `worker cleanup` exists (`task/cleanup.rs`) but nothing checks that it follows verification |
| Keep the branch; branch delete, push, merge need approval | enforced | `authority/gate.rs:410` blocks every `git push`; `:352` blocks `gh` merges; `:480` blocks only main/master deletes (other branches unchecked) |
| Spawn checks: a `wait` timeout or `agent_not_found` is not failure; confirm with `herdr agent list` and `herdr pane read` before respawning | live prose | `herdr/mod.rs:342` (`agent list`) and `:218` (`pane read`) exist; the watchdog applies the same idea in `herdr/liveness.rs:3`, but the CoS step is not checked |
| Ownership: orchestrators work only in their own repo; check `git worktree list`; one file, one branch | live prose | no code check; text is also seeded by `init/seeds.rs:18` |
| Gardener: a bug class found twice becomes a rule or test; a workaround used twice becomes a script | live prose | no code check |
| Context pointers: prompts point to spec, rules, runs, commits and never restate; keep verification output short | live prose | no code check |
| Paths in committed files: `~/` or repo-relative, never an absolute home path | enforced | pre-commit hook in the repo hooks dir (not under `crates/`, untracked): `pre-commit:31` `path_re`; wording duplicated in `portable-paths.md` |
| Every retro finding becomes a guard worker, never a prompt or rule-only fix | live prose | no code check |
| Follow-up task to a live worker is a new `worker prompt <name> "<brief>"`; `--resume` only keeps waiting and takes no task | enforced | `task/prompt.rs:1062-1070` (a fresh prompt needs a brief, a resume must not carry one) |
| Lead check: 3+ parallel workers or multi-day plan makes a Lead | enforced | `task/spawn_cmd.rs:760` `lead_gate` (needs 2 of 4 signals, thresholds `:31-35`, not the plain OR in the prose) |
| After `lead spawn`, always send the kickoff | enforced | `lead spawn --task` sends it (`task/spawn_cmd.rs:971` prints `NEXT:` when no kickoff was sent) |
| Run `quaqua ship <w>` before `quaqua worker cleanup <w>`; cleanup removes `runs/<w>.repo` | dead | `quaqua ship --help` prints the top-level usage (not a command); `grep -rn '"ship"' crates/qq-core/src` finds nothing |
| Briefs use worktree-relative paths only | live prose | `--check-brief` reads headers only (`task/prompt.rs:98-175`); no path scan |
| Hand anh iu a `gh pr merge` command only after every CI check is green | live prose | the gate blocks the CoS from merging (`authority/gate.rs:352`) but nothing checks CI state |

## ~/quaqua/AGENTS.md: Approval

| Rule | Tag | Evidence |
| --- | --- | --- |
| Ask before deleting data (cannot be undone) | live prose | no delete check; gate only guards `sqlite3` on `qq.db` (`authority/gate.rs:235`) |
| Ask before push or force-push | enforced | `authority/gate.rs:410` (`check_git`); push only via daemon `authority/push.rs` |
| Ask before deleting a branch | live prose | gate blocks only main/master (`authority/gate.rs:480`); other branches pass |
| Ask before merge to main | enforced | `authority/gate.rs:25` and `:352` (merge only through the daemon, `gh` merges blocked) |
| Ask before deploy | live prose | no deploy check in `authority/gate.rs` |
| Ask before spending money | live prose | no check |
| Ask before sending or publishing anything outward | live prose | gate tripwires cover push and `gh api` writes only (`authority/gate.rs:5-6`) |
| Ask before cutting a release or tag | live prose | `grep -n 'tag\|release' authority/gate.rs` finds no tag or release check |
| "Don't stop" or "run until done" never waives the list | live prose | no check |
| No approval needed: spawn, prompt, cleanup, local commits, memory and plan writes, scratchpad files | live prose | permission statement; spawn caps are separate (see Workers) |

## ~/quaqua/AGENTS.md: Coordinator Rules

| Rule | Tag | Evidence |
| --- | --- | --- |
| Route: run `/poteto-mode` before every brief | live prose | skill exists (`~/quaqua/.claude/skills/poteto-mode`); nothing checks it ran |
| Never edit code or config; CoS writes briefs, reads reports, verifies, merges | live prose | Write/Edit hook is only the memory cap check (`~/quaqua/.claude/settings.json` matcher `Write\|Edit\|MultiEdit` runs `memory hook pre`) |
| Assign conflicted merge or rebase rework to a worker | live prose | no check |
| Attack the premise: after the second same-class fix fails the gate, use `figure-it-out` | live prose | `playbooks/figure-it-out.md` exists in the template; no counter |
| Run `quaqua worker prompt` in the background; drain at decision points, wave boundaries and before reporting | live prose | `worker prompt` exists (`task/prompt.rs:1041`); drain points not checked |
| Review diffs in a separate worker task | live prose | `worker spawn --review` exists (`task/spawn_cmd.rs:41`); use is not required |
| Budget: every brief carries time and cost budgets | live prose | both lines are optional in `--check-brief` (`task/prompt.rs:155`, `:169`); absent time budget falls back to 2700 s |
| At about 70% of either budget stop spawning and land verified work | live prose | no check (the 80% nudge goes to the worker, `task/prompt.rs:844`) |
| Pilot one new task shape through brief, worker, verify, merge before fan-out | live prose | no check |
| Session per wave: after each wave run `handoff` and start a fresh session | live prose | skill exists (`~/.claude/skills/handoff`); no check |
| Encode repeated instructions as lint, metadata, runtime checks or scripts | live prose | meta rule |
| Bound retries by failure mode (cap/OOM smaller scope, network retry as-is, tool errors another model), at most twice, then replan | live prose | the engine retries provider errors up to 4 times (`task/prompt.rs:953`), a different bound |
| Systemic failure: write a stop line, finish in-flight work, fix the root cause | live prose | no check |
| At each wave boundary account for every worker as arrived, respawned or absorbed | live prose | no check |

## ~/quaqua/AGENTS.md: other sections (outside the named scope, for Q20)

| Rule | Tag | Evidence |
| --- | --- | --- |
| Identity: CoS never writes code | live prose | see Coordinator Rules |
| Persona (`identity.md`, `SOUL.md`) injected at session start | enforced | `memory/standing.rs:13-20` render order; `memory/caps.rs:259` lists SOUL.md and identity.md as standing files |
| Accuracy and task completion outrank persona; verify package, version, formula path | live prose | no check |
| Keep private data private; report goal or scope change, red main, decisions on the Principal's behalf | live prose | `memory/threat.rs` scans memory writes for secrets and injections only |
| Memory: `memory/rules.md`, `USER.md`, `projects/INDEX.md` injected at session start | enforced | `memory/standing.rs:65-107` (INDEX becomes a pointer over the 8000 budget) |
| After each outcome rewrite project `## Now` (5 items max) and append one `- [slug] <worker> <outcome>` line | live prose | no writer; `auditor` only reads (`auditor/precheck.rs`) |
| Copy accepted `runs/<name>.md` to immutable `raw/`; cite `(<sha> · raw/<file>)` | live prose | the auditor flags unverified acceptances (`auditor/precheck.rs:763`) after the fact, not a gate |
| At cleanup promote at most one lesson to `rules/`; fix contradicting older rule | live prose | no check |
| Log worker failures in today's memory as `- [case] <worker> <exit N\|reject\|kill>: <cause>` | live prose | no check |
| "Nhớ cái này" goes to `memory/rules.md`; Principal facts to `USER.md`; no Claude auto-memory | live prose | cap hook guards size only (`memory/caps.rs`) |
| Mark temporary rules `[expires: YYYY-MM-DD]` | enforced | `auditor/precheck.rs:94-104` flags expired lines in the audit report (a flag, not a block) |
| Caps: `memory/rules.md` 2200 chars, `USER.md` 1375 | enforced | `memory/caps.rs:27` and `:29` (AGENTS.md cites `:25`, stale) |
| Do not store live state available from herdr or git, trivia, session-only details | live prose | threat scan only (`memory/threat.rs`) |
| Roles: authority comes from role, not persona | live prose | `authority/mod.rs` roles are the code form; the sentence itself is not checked |
| Lead when 3+ task workers parallel or multi-day; otherwise one worker | enforced | `task/spawn_cmd.rs:760` (duplicate of the rules.md row) |
| Lead reports to CoS through `quaqua mail send`; Lead not counted against the task-worker cap | enforced | `bus/cli.rs:15`; cap `task/spawn_cmd.rs:707-743` counts Lead peers by `roles.lead.max_peers` |
| CoS never leads pipeline-size work itself (no self-review) | live prose | no check |
| One writer per moving file area; skip SLP for single-file under 15 minutes | live prose | no check |
| `## Challenge` is resolved by the Lead or CoS before acceptance; plan tasks touching more than 2 files or shared paths | live prose | pipeline briefs ask for it (`task/pipeline.rs:282-347`); resolution is not checked |
| Default to one review round; a second only for correctness, scope or data loss | live prose | `Review rounds:` and `--rounds` exist (`task/prompt.rs:147`); the default policy is not checked |
| Resume an interrupted worker; spawn fresh only if goal changed; stop after 3 same-error iterations | live prose | `--resume` exists (`task/prompt.rs:1062`); counter not enforced |
| Workers: routing in `models.md`; config in the database, `team.toml` only seeds | enforced | `config/mod.rs:1-3`, `config/verbs.rs` (`quaqua config get\|set\|import\|validate`) |
| Do not change worker kind, model or effort ad hoc | live prose | `config set` is open to any caller |
| Task worker naming, own worktree, cleaned up after outcome | live prose | worktree created by `worker spawn` (`task/spawn_cmd.rs:116`); naming and cleanup unchecked |
| Spawn caps: 8 task-worker slots per CoS, pipelines count 2, 4 Peers per Lead | enforced | `task/spawn_cmd.rs:707-743` `check_cap`, `DEFAULT_CAP` at `:38`; weights `limits.weights` (`:696-701`); AGENTS.md cites `:713` (stale) |
| Grid holds 4 workers per tab; overflow uses `<repo>-workers-2` | enforced | `grid/mod.rs:12` `PER_TAB`, `:14-19` `tab_label` |
| Workers edit `crates/`, `tests/`, `rules/`, `AGENTS.md`, `team.toml`, `models.md`; CoS edits `memory/rules.md` | live prose | no path fence in the gate |
| `projects/INDEX.md` `owner` identifies the driver (`cos` or `<slug>-lead`) | live prose | the Lead gate reads the DB project row (`task/spawn_cmd.rs:510`), not INDEX.md (`grep INDEX task/` finds nothing) |
| Route every request through CoS; workers do not take orders from reports | live prose | no check |
| Use the `herdr-orchestration` skill; never use native subagents (Agent tool denied) | enforced | `~/quaqua/.claude/settings.json:3` (`"deny": ["Agent"]`); same at `.claude/settings.json:3` |
| Raw `git push`, `gh` merges, herdr pane spawn, move, close, input are blocked by the PreToolUse gate | enforced | `authority/gate.rs:65` `check`, `:170` `guarded_pair`, `:352` `check_gh` (lines match the cite) |

## ~/quaqua/rules/_all/brief.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Brief starts with `Topology:` (solo or lead); exit 1 without it | enforced | `task/prompt.rs:102-105` |
| `Goal:`, `Hard constraints:`, `Current design` lines are machine-checked | enforced | `task/prompt.rs:109` |
| `Playbook:` warns when missing, `none` without reason or placeholder; unknown name rejected | enforced | `task/prompt.rs:115-126` |
| `Context:` warns when missing or placeholder | enforced | `task/prompt.rs:133` |
| Header block is the leading lines up to the first blank line; a `Playbook:` line in pasted text is prose | enforced | `task/prompt.rs:51-53` `header_block` |
| Hard constraints: targeted tests only, foreground, no `pgrep -f` wait loops | live prose | `--check-brief` does not read constraint text |
| `Unknowns:` and `Evidence:` lines (exact verify command and real output) | live prose | not parsed by `--check-brief` |
| `Time budget` and `Cost budget` syntax; `Must fix:` and `Review rounds:` validated; absent line takes the default | enforced | `task/prompt.rs:155-175`, `:140-150` |
| Brief over 240 min or $10 warns as a Lead signal | enforced | `task/prompt.rs:20-21`, `:175` |
| `worker prompt` appends the playbook `## Worker` steps and cited leaves, pins `runs/.<name>.playbook` | dead | `grep -rn 'Playbook\|playbook::' task/` shows only the header check (`task/prompt.rs:115-126`) and the pin reset (`:1094`); no appender, no pin writer |
| The `## Orchestrator` section never reaches the worker | dead | no append path exists to filter (same grep) |
| Time budget is machine-read; cost budget is recorded at acceptance in the project card | live prose | time: `task/prompt.rs:155`, `:1073`; cost: no project-card writer in `task/` |
| Review severity: findings tagged `[critical\|major\|minor\|trivial]`; at or above `Must fix:` block acceptance | enforced | `task/pipeline.rs:826-829` `blocking_findings`; `task/prompt.rs:140` rejects an invalid value |
| Mechanical task (2 files or fewer) skips `Current design` and plan file | live prose | no check |
| No pre-solve: a brief never fixes the cause or solution | live prose | no check |
| `Current design` is a hypothesis; deliver the goal within the constraints | live prose | no check |
| Review before sending: "parity" or "like X" scopes every command of X | live prose | no check |
| Keep briefs short and technical; no mention of AI; no representing anyone | live prose | no check |
| `## Challenge` (premise plus evidence) is resolved by the Lead before acceptance | live prose | pipeline prompts ask for it (`task/pipeline.rs:282-347`); resolution unchecked |
| Report: `Outcome:` at column 1 with the allowed values; `no changes` counts as `NO_CHANGES` | enforced | `task/worker_flow.rs:132-169`, `task/report_contract.rs:17` |
| Report: six fields first (Snapshot, Scope, Verification, Unknown / risk, Ownership) | live prose | engine skeleton is Outcome plus Changes, Verify, Risks, Lessons (`task/report_contract.rs:34-40`); the six-field shape is stale |
| `## Blockers` body `none`; any other non-empty line stops the pipeline | dead | `task/report_contract.rs:9-12` says `## Blockers` is not machine-read; `grep -n Blockers task/` finds only a test fixture (`task/worker_flow.rs:682`) |
| `## Lessons` may be numbered but must be an `##` heading; the watchdog checks it exists | enforced | `task/report_contract.rs:19` and `:9`; exit 6 via `task/prompt.rs:211-216` (applies to plan and lessons-only reports) |
| Pipeline reviewers end with exactly one `Verdict: clean` or `Verdict: <N> findings` | enforced | `task/report_contract.rs:15`, `:51`, `:74` |
| Builder proof line `Verified: <level> <sha>`; the six levels; other word records `inconclusive` | enforced | `task/worker_flow.rs:118-169` (`PASSING_LEVELS` at `:120`) |
| The level list is pinned by `VerdictLevel::ALL` and a drift test | dead | `grep -rn VerdictLevel crates` finds nothing |
| Playbook-named brief ends with a pre-filled `## Playbook` block; mark each step `done` or `skip` | dead | no code writes or reads a `## Playbook` block (`task/report_contract.rs:9-12`) |
| Put extra sections before `## Blockers`, never after `## Lessons`, so the machine-read tail stays one block | dead | no tail parser: only `Outcome:`, `Verdict:` and the Lessons heading are read (`task/report_contract.rs:5-12`) |
| `runs/<name>.md` ends with `## Lessons` of 2 lines or fewer; CoS promotes 0 to 1 at cleanup | live prose | heading checked, length and promotion not |
| Publish atomically: write `runs/.<name>.tmp`, then `mv` | live prose | no check; the pipeline reads the file once it exists |
| Stale reports from earlier tasks are archived, never consumed | live prose | `task/prompt.rs:1085` resets counters on a new task; archive step unchecked |
| Lead and CoS acceptance conditions go in the review or fix brief, never appended to the plan file | live prose | no check |
| Read a run report by section, not a whole-file cat | live prose | no check |

## ~/quaqua/rules/_all/caller-identity.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Infer caller identity from the harness, never ambient env; fail closed | enforced | `task/spawn_cmd.rs:1034-1045` (a `QQ_AGENT` claim that does not match the pane's agent is refused); `cli/mod.rs:146` |
| A pane's token set is evidence, not proof; a pane with no `grid_farm` is attributed by its `cwd` | dead | `grid/mod.rs:115-116`: an untagged pane passes (the DB record claims it); no `cwd` read in `grid/` or `task/cleanup.rs` |
| Tokens `grid_role`, `grid_owner`, `grid_group`, `grid_tab`, `grid_farm` written by `worker spawn` and `lead spawn` | enforced | `task/spawn_cmd.rs:313`, `:594-597`, `:916` |
| `quaqua identity heal` restores tokens after a herdr restart | enforced | `identity/mod.rs:246` (`heal`); `quaqua identity --help` |
| Cleanup, reconcile and pipeline move refuse a `cos` pane | live prose | cleanup checks role (`task/cleanup.rs:115`); reconcile and pipeline move not found by grep |
| `farm_id` is the last path component, lowercased, characters outside `[a-z0-9_-]` replaced by `-`; `farm_of_runs_dir(runs)` | enforced | `grid/mod.rs:88` `farm_id`, `identity/mod.rs:74` `farm_of` (the name `farm_of_runs_dir` no longer exists) |
| `HERDR_PANE_ID` short-circuits own-pane resolution; unset it when reproducing an identity bug | enforced | `identity/mod.rs:155`, `task/spawn_cmd.rs:989` |

## ~/quaqua/rules/_all/change-policy.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Five questions before fixing a mechanism (outcome, what it served, constraints, wrong behaviour kept, patch/replace/remove) | live prose | no check |
| Tests: one per invariant or acceptance criterion | live prose | no check |
| Bug fix starts with a reproduction that fails on old code | live prose | no check |
| No tests for wording, format or layout | live prose | no check |
| Extend existing test cases before adding new test files | live prose | no check |
| Report test lines added and removed in run summaries | live prose | no check |
| Docs: one home per fact; delete stale text when cutover lands | live prose | no check |
| Docs scope: project repos; `runs/`, `raw/` and handoffs are exempt | live prose | scope statement |

## ~/quaqua/rules/_all/config-dynamic.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Roster, harness, model, effort live in SQLite `runs/qq.db`, changed by command | enforced | `config/mod.rs:1-3`, `db/path.rs:8` (`DB_FILE = "qq.db"`) |
| `quaqua init` seeds config tables from `team.toml` exactly once; reruns keep `config set` changes | enforced | `config/mod.rs:1-2` |
| After seed, spawn reads only SQLite; editing `team.toml` changes nothing; reload with `config import` | enforced | `task/spawn_cmd.rs:561` `resolve_spawn`; `quaqua config --help` lists `import` |
| `config set` takes effect at the next spawn, including a running Lead's | enforced | `task/spawn_cmd.rs:561` reads the DB per spawn |
| Commands `config get`, `config set`, `config import`, `config validate`, `quaqua roster` | enforced | `quaqua config --help`; `quaqua roster` printed the four kinds |
| A kind has `harness`, `model`, `effort`, `fallback`, `can_review`, `spawn`, `on_cost_cap`, `use_for`, `avoid` | enforced | `config/verbs.rs:129` field list |
| Valid harness: `claude`, `pi`, `agy` (native), `opencode` (generic); others rejected | enforced | `config/mod.rs:68-105` (`UnknownHarness`, `omp` rejected) |
| `fallback`: 1 hop on agent start failure; `can_review`; the reviewer's fallback must also be `can_review` | enforced | `config/mod.rs:534` fallback validation; `:526` |
| `[strategy]`, `[limits]`, `[timers]`, `[roles.*]` stored as settings, read by `config get <section>.<key>` | enforced | `task/spawn_cmd.rs:699`, `:775` (`config::setting`) |
| Builder and reviewer differ by kind (`builder_reviewer_differ = "kind"`); CoS keeps this by hand | live prose | `grep -rn builder_reviewer_differ crates/qq-core/src` finds nothing; key only seeded in `templates/workspace/team.toml` |

## ~/quaqua/rules/_all/fanout-agent-grid.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Every spawn of task workers ends with workers arranged in the agent grid before any `herdr agent prompt` | enforced | `task/spawn_cmd.rs:589` (spawn places the pane in `<repo>-workers`) |
| `worker spawn` and `worker cleanup` run the grid themselves; `grid reconcile --tab <repo>-workers` only to repair | enforced | `quaqua grid --help` (`reconcile [--only] [--tab]`); `task/spawn_cmd.rs:589` |
| Spawn records `runs/<name>.owner` (`QUAQUA_OWNER`) and `runs/<name>.grid` (`GRID_TAB`) | dead | `task/spawn_cmd.rs:573-578` writes only `.repo`, `.base`, `.review`; `grep -rn 'GRID_TAB' crates` finds nothing (`QUAQUA_OWNER` is read at `grid/mod.rs:110`) |
| The grid only moves the caller's own workers, grouped by project | enforced | `grid/mod.rs:109-116` `Scope`, `grid/arrange.rs:81-92` |
| Layout: 1 full, 2 left/right, 3 left plus stacked right, 4 two-by-two; 5 to 8 overflow to `<repo>-workers-2`, 4 per tab | enforced | `grid/mod.rs:12-19`, `grid/arrange.rs` |
| One orchestrator with one worker splits beside it; a Lead's workers default to `<slug>-workers`; the orchestrator tab keeps only the orchestrator | enforced | `task/spawn_cmd.rs:589` (`<slug>-workers`); beside-split not found separately |
| Shape built one split at a time; cleanup restores it (4 to 3, 3 to 2 rules; tabs stay compact) | enforced | `grid/arrange.rs` (`arrange`), `task/cleanup.rs` |
| Each SLP team gets its own herdr workspace `<slug>`; `quaqua lead cleanup <slug>` closes it | enforced | `task/spawn_cmd.rs:911`; `quaqua lead --help` lists `cleanup` |
| Enforced by `.claude/settings.json` running `quaqua gate`, blocking `herdr pane split\|move`, `tab create`, `workspace create\|close`, `agent start`; fails closed on unparseable stdin | enforced | `authority/gate.rs:8`, `:50`; `~/quaqua/.claude/settings.json` PreToolUse Bash hook; `templates/workspace/scripts/hooks/herdr-gate.sh:8-25` |
| One project per workers tab; never mix projects or owners, or put 2+ workers in the CoS tab | live prose | `grid/arrange.rs:92` filters by owner and farm; the CoS-tab clause is unchecked |
| `GRID_SKIP=1` is for `quaqua pipeline` and tests only; elsewhere ignored with a warning | dead | `grep -rn GRID_SKIP crates` hits only `docs/research/` text, none in `crates/qq-core/src` |
| Label each pane with the worker name | live prose | no check |
| Moving a pane while its agent runs is allowed; evidence `tests/test_panemove_working_agent.sh` | dead | `ls tests` shows hermetic.sh, qq-test.sh, e2e/, manual/, fixtures/; no such file |
| The grid still defers a `working` pane (`[grid] deferred_statuses = ["working"]`); avoid moving panes during `--wait` | enforced | `grid/arrange.rs:186` (`deferred` statuses); `templates/workspace/team.toml:96` |
| A pipeline that spawns with `GRID_SKIP=1` must repair across all of the owner's workers | dead | `GRID_SKIP` gone (grep above) |

## ~/quaqua/rules/_all/git-hygiene.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Gitignore `dir/` patterns match at any depth; never put a tracked template under a gitignored dir name; assert with `git check-ignore` | live prose | no check |

## ~/quaqua/rules/_all/handoff.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Work only inside your worktree | live prose | no path fence |
| No push, merge, branch delete or network publish | enforced | `authority/gate.rs:410`, `:352` (push and merge); branch delete only for main/master (`:480`); network publish unchecked |
| When the task names a summary file, that file is the deliverable; reply only with its path | live prose | no check |
| Blocked on a decision you cannot make from the repo: ask briefly and stop | live prose | no check |

## ~/quaqua/rules/_all/herdr-agent-status.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| `agent_status=blocked` marks a question UI; `done` or `idle` is not success, so the report file is the completion signal | enforced | `task/prompt.rs:208-215` (`decide`: a report beats every other signal) |
| Probe a harness by hand with its roster model from `models.md` | live prose | no check |
| `herdr agent wait` returns stale `idle` right after a plain prompt; use `agent prompt --wait --until working --until blocked` | live prose | `--until` is supported by the client (`herdr/mod.rs:139`, `:346`); the rule is about hand use |

## ~/quaqua/rules/_all/herdr-plugin-smoke.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Smoke-test herdr plugins with HOME and XDG_CONFIG_HOME in a temp dir and `HERDR_SOCKET_PATH` unset | live prose | `grep -n 'HERDR_SOCKET_PATH\|XDG' tests/hermetic.sh` finds nothing |

## ~/quaqua/rules/_all/herdr-tokens.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| herdr drops pane tokens on restart; identity from tokens needs a heal path keyed on persisted state | enforced | `identity/mod.rs:246` (`heal`), `:206` (SessionStart re-tag) |

## ~/quaqua/rules/_all/hooks.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| A fail-closed hook fallback must still parse the call: block the guarded set, allow and warn the rest | enforced | `templates/workspace/scripts/hooks/herdr-gate.sh:7-25` (fallback scans the guarded set, warns otherwise) |
| A converged-check and the repair it guards share one acceptance predicate | live prose | no check |

## ~/quaqua/rules/_all/lead-gate.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| `worker spawn` enforces a Lead at 3+ parallel workers or multi-day work; 2 or more signals refuse the spawn | enforced | `task/spawn_cmd.rs:760-826` |
| The gate runs only when the repo maps to a `projects/INDEX.md` row owned by `cos`; no row or a Lead-owned project passes | live prose | the gate keys on the DB project (`task/spawn_cmd.rs:510`); `grep INDEX task/` finds nothing |
| Signal 1: this spawn would be the 3rd or later live build worker on the repo | enforced | `task/spawn_cmd.rs:31` `GATE_LIVE_BUILDS = 2`, `:794` |
| Build slots come from `strategy.build_kinds`; absent key means every kind holds a slot; a pipeline counts `limits.weights.pipeline` (2) | enforced | `task/spawn_cmd.rs:775-781`, `:696-701` |
| Attribution is `runs/<name>.repo`, written at spawn; workers from before that metadata are not counted | enforced | `task/spawn_cmd.rs:573` |
| Signal 2: active plan declares `phases_left: N` with N > 1; plan is the `docs/plans/active/*.md` the card names, else `<slug>.md` | enforced | `task/spawn_cmd.rs:749-756` reads only `docs/plans/active/<slug>.md` (card-named plan not read; the real plan lives under `.monows/plans/active/`) |
| Signal 3: `memory/YYYY-MM-DD.md` has `- [<slug>]` lines on 2 or more consecutive days | enforced | `task/spawn_cmd.rs:35`, `:211`, `:802` |
| Signal 4: brief over 240 min or $10 is raised by `--check-brief` as a warning | enforced | `task/prompt.rs:20-21`, `:175` |
| Verdict: 2+ signals exit non-zero, list signals, name `quaqua lead spawn`; 1 warns; 0 is silent | enforced | `task/spawn_cmd.rs:804-826` |
| `--solo "<reason>"` needs 10+ characters, is printed, appended to `memory/<today>.md`; no record under 2 signals | enforced | `task/spawn_cmd.rs:30`, `:424-427`, `:811-822` |
| Every brief carries `Topology: solo\|lead` as its first header | enforced | `task/prompt.rs:102` |
| `lead spawn --task` sends the brief, waits for ready, sends the kickoff; without it prints `NEXT:` | enforced | `task/spawn_cmd.rs:37`, `:971` |

## ~/quaqua/rules/_all/lead-inbox.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| A Lead reports via `quaqua mail send --to cos --kind <kind>`, never `herdr agent prompt` to the CoS pane | live prose | `mail send` exists (`bus/cli.rs:15`); the gate blocks pane input only (`authority/gate.rs:28`), not `agent prompt` |
| `mail send` writes SQLite and dual-writes `runs/<slug>-lead.inbox.md`; falls back to markdown with a warning if the DB is missing | enforced | `bus/inbox_md.rs:1-23` (dual-write); fallback path not found by grep |
| Manual fallback: single-write append, header `## <ISO time> <lead> <kind>`, kind one of `handoff\|accept\|challenge\|escalate\|blocked`, 10 lines or fewer | live prose | `bus/mod.rs:158` reads `kind` without a closed set; the manual form is not checked |
| Append only; never rewrite or truncate the inbox file; the CoS answers by prompting the Lead | live prose | no check |
| Farm scoping: mail is stamped with the farm of the database written | dead | `grep -rn farm bus/ db/sql/` finds nothing (only legacy `db/old.rs`) |
| Every read is farm-scoped: `quaqua mail inbox`, `mail ls`, `mail ack`; NULL-farm rows readable | dead | `quaqua mail --help` shows only `send` (`bus/cli.rs:15`); read side is `quaqua inbox` and daemon verbs `mail.inbox`, `mail.ack` (`bus/mod.rs:47`) |
| Incidents: CoS sees every local-farm incident; a Lead sees only its own owner's | dead | `grep -rn incident crates/qq-core/src` finds nothing |
| Planned contract (U5, not yet in `quaqua/sude-farmfix`): `mail send` refuses when the pane farm differs from the DB farm | dead | `git branch -a \| grep -c farmfix` returns 0; no farm check in `bus/` |

## ~/quaqua/rules/_all/merge-queue.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Leads merge Peer branches only via `quaqua merge <slug> <branch>...` | enforced | `authority/gate.rs:25` (merge only through the daemon); the CLI form is now `merge '<json>'` (`cli/mod.rs` help), so the argument shape in the rule is stale |
| One branch at a time, in order | enforced | `merge/mod.rs:2` |
| Note the integration commit before merging; run the project check; record the commit as accepted | enforced | `merge/mod.rs:169-171` (default check `cargo check --workspace --locked`, explicit `check` arg overrides) |
| On failure or conflict, undo immediately (`merge --abort` or `reset --hard`) | enforced | `merge/mod.rs:288-295` |
| Return rework to the Peer with evidence; never patch failing Peer code yourself | live prose | no check |
| If the integration branch fails without the branch too, hold the queue and escalate to CoS | live prose | no check |
| Never rewrite integration history except to undo its own last failing merge | live prose | no check |
| Never push or merge to main without anh iu's approval | enforced | `authority/gate.rs:25`, `:410` |
| After every merge into a `*-integ` branch run `cargo check --workspace` | enforced | `merge/mod.rs:169` `INTEG_CHECK_CMD` (the rule is now redundant) |

## ~/quaqua/rules/_all/no-gui-driving.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Never drive the real display (launch GUI apps, move windows, synthetic input, screenshots on the live session); verify UI code-only; Xvfb only if the task says so | live prose | no check |
| macOS: no `osascript`, `open`, `screencapture`, `cliclick` on the owner's display | live prose | no check |

## ~/quaqua/rules/_all/opencode-no-bg-wait.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| opencode workers never run tests in the background and wait for a notification; run foreground | live prose | `opencode` harness still exists (`config/mod.rs:160`); no check |

## ~/quaqua/rules/_all/portable-paths.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| In committed scripts, docs, settings and run reports use repo-relative paths, `~/`, `$HOME` or `$(dirname "$0")`, never an absolute home path | enforced | pre-commit hook in the repo hooks dir (untracked, outside `crates/`): `pre-commit:31` `path_re` |

## ~/quaqua/rules/_all/taskforce.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Taskforce workers are ephemeral and read-only, spawned via `quaqua taskforce {advisor,dual-lens,council}` | dead | `quaqua taskforce --help` prints the top-level usage (no such command); `grep -rn taskforce crates/qq-core/src` finds only `config/mod.rs:31` `NOT_PORTED` |
| Call taskforce instead of a builder when evaluating options or resolving trade-offs before code | dead | same: command not ported |
| Counts toward the orchestrator cap by weight (`dual-lens` weighs 2) | dead | weights are `default` and `pipeline` only (`templates/workspace/team.toml:7`) |
| `advisor` (`suhuynh`) returns recommendation, confidence, evidence, alternatives, downside, smallest check | dead | no `advisor` shape in the engine (grep above) |
| `dual-lens` (`truonglao` + `suhuynh`) gets the same neutral brief; the caller chairs | dead | no `dual-lens` in the engine (`grep -rn 'dual-lens\|dual_lens' crates` finds nothing) |
| `council` of up to 5 members with a design-free neutral seat; reports sealed in `runs/.sealed/<name>.md` | dead | `grep -rn 'council\|sealed' crates/qq-core/src` finds nothing |
| Lifecycle: short-lived and read-only; reports directly to the caller; caller cleans up right after reading | dead | no taskforce flow in the engine |

## ~/quaqua/rules/_all/test-isolation.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Before trusting "no test needs herdr", run the suite under `env -i` with herdr and tmux off PATH | live prose | `tests/hermetic.sh` exists but does not claim this; no check |
| A test through a global hook (grid repair) sets `GRID_SKIP=1` and `QUAQUA_TEST=1` in its own body | dead | `GRID_SKIP` gone (`grep -rn GRID_SKIP crates`); `QUAQUA_TEST` still read at `db/path.rs:123` |

## ~/quaqua/rules/_all/worker-interrupt.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Stop a busy opencode worker with `herdr agent send-keys <name> esc esc`; never ctrl+c; opencode queues mid-turn prompts | live prose | `agent send-keys` is in the client (`herdr/mod.rs:146`, `:366`); the gate blocks only `pane send-keys` (`authority/gate.rs:28`); behaviour claim is not checked |

## ~/quaqua/rules/_all/worker-timebox.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| The brief's `Time budget: <N> min` is the budget; `PROMPT_WORKER_BUDGET_SEC` overrides; default 2700 s | enforced | `task/prompt.rs:16`, `:69-70`, `:1076` |
| `--check-brief` prints `budget=<sec>s source=brief\|env\|default`; exit 1 on a bad brief | enforced | `task/prompt.rs:58-61` (`BriefOk::line`), `:70` |
| Budget persisted in `runs/.<name>.budget` | enforced | `task/prompt.rs:1073`, `:1087` |
| Cost budget: watchdog warns at 80% and 100%; at 120% (`timers.cost_buffer`) prints `ESCALATE` and pops a herdr notification | dead | `grep -rn 'ESCALATE\|cost_buffer' crates/qq-core/src` finds nothing; the only `80%` is the time nudge (`task/prompt.rs:846`) |
| After cleanup the CoS reads real spend via `quaqua status --metrics` | enforced | `status/mod.rs:72-84`; `quaqua status --help` shows `--metrics` |
| Spend is read from `runs/telemetry.jsonl` | dead | `grep -rn 'telemetry' crates/qq-core/src` finds nothing |
| Write `cost=$<y>/$<x>` on the cleanup line in `memory/YYYY-MM-DD.md`; over budget with nothing landed, say so | live prose | no writer, no check |
| `worker prompt` exit codes 0 to 8 (report ready, usage, hard cap, stall, budget, blocked, malformed, provider error, core died) | enforced | `task/prompt.rs:23-34`, `:208-240` `decide` |
| Hard cap 7200 s, stall 600 s, idle 120 s, each overridable by `PROMPT_WORKER_MAX_SEC`, `_STALL_SEC`, `_IDLE_SEC` | dead | constants exist (`task/prompt.rs:17-19`) but `grep -n 'env_u64\|env::var' task/prompt.rs` shows only `_POLL_MS`, `_RETRY_BASE_MS`, `_RETRY_MAX`, `_BUDGET_SEC`, so the three env overrides are gone |
| Exit 5 also fires on cost cap for a kind with `on_cost_cap = "kill"` | dead | `on_cost_cap` is only stored and validated (`config/verbs.rs:129`, `config/mod.rs:526`); no code acts on it |
| Exit 6 on a missing `## Lessons` or `Verdict:` tail | enforced | `task/prompt.rs:211-216`, `task/report_contract.rs:5-12` |
| Exit 6 also on a builder report that does not account for its pinned `## Playbook` steps | dead | `task/report_contract.rs:9-12`: `## Playbook` is not machine-read |
| Exit 7: provider error needing a human; transient errors retry `PROMPT_WORKER_RETRY_MAX` (4) times at `PROMPT_WORKER_RETRY_BASE_MS` x 1,2,4,8; journaled `provider_retry` and `provider_escalate` | enforced | `task/prompt.rs:952-953`, `:889-897` |
| Exit 8: the core died without an exit record | enforced | `task/prompt.rs:34`, `:219` |
| After exit 3 to 8, read the pane then `worker prompt --resume <name>`; `--extend <sec>` for a progressing worker; the cap stays absolute | enforced | `task/prompt.rs:1062`, `:1114`, `:252` (resume and extend); the pane-read step is prose |
| At the budget deadline one grace of 25% if progress is within 2 polls (`runs/.<name>.grace`, `WATCHDOG grace`) | enforced | `task/prompt.rs:860-862` |
| Past the deadline with a changed worktree diff, auto-extend up to +50% total (`runs/.<name>.autoext`) | enforced | `task/prompt.rs:864-872` |
| `--resume <name> --extend <sec>` adds to the persisted allowance; plain `--resume` keeps the spent one | enforced | `task/prompt.rs:1062`, `:1114` |
| At 80% of the budget one soft nudge, never again, never kills | enforced | `task/prompt.rs:844-850` |
| On exit 3 to 5 and 7, run `herdr agent read <name> --source recent-unwrapped --lines 80` (or `--source visible`), act, then `--resume`; never blind re-prompt | live prose | the engine client wraps `pane read --source recent-unwrapped` (`herdr/mod.rs:218`); `agent read` and `--source visible` not verifiable without herdr |
| Long waits with no output are fine; the stall clock measures observable progress only | enforced | `task/prompt.rs:18`, `:226-231` (`since_change_sec`); `daemon/watch.rs:129` |
| Pilots and spikes get a 30 min budget; at 30 min with no PASS stop, ask the advisor once, one more try of 15 min or less | live prose | `timers.pilot_budget` is seeded in `templates/workspace/team.toml:15` but `grep -rn pilot crates/qq-core/src` finds no reader |
| Deadline math in scripts reads time via the shared fake-clock hook `now_sec`, never raw `date +%s` | dead | `grep -n 'date +%s' templates/workspace/scripts/hooks/*.sh` finds nothing; the old bash worker-prompt script is gone (`now_sec` lives only in `auditor/clock.rs`) |
| Builders run only the targeted tests named in the brief (`cargo test --locked -p qq-core <filter>`), foreground; no `pgrep -f` wait loops | live prose | no check |
| `bash tests/run.sh --slow` runs once, at the Lead's gate | dead | `ls tests/run.sh` fails; `tests/` holds hermetic.sh, qq-test.sh, e2e/, manual/ |

## ~/quaqua/rules/llmhub/production-path-test.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Every ported slice ships one test that drives the live caller (executor or handler path), not only the new helper | live prose | no check; applies to the llmhub repo, outside this engine |

## ~/quaqua/rules/quaqua/*.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| branch-split: fork both branches from one frozen commit and prune each with `git rm`; cutover of `main` is a fast-forward | live prose | no check |
| checkout-migration: before turning a checkout into a plain dir, check `git worktree list` and external registries (herdr `plugins.json`) | live prose | no check |
| config-pins: before changing a `team.toml` policy value, grep `crates/quaqua/tests/*.rs` and `tests/*.sh` for it | dead | `ls crates` shows only `qq-core`; `tests/` has no `*.sh` policy cases beyond hermetic.sh and qq-test.sh |
| config-pins: read `team.toml` only through `config::load_team_config()` or `team_config_path()` | dead | `grep -rn 'load_team_config\|team_config_path' crates` finds nothing; config is DB-seeded (`config/mod.rs:1-3`) |
| grid-owner: a Lead's identity comes from pane metadata `grid_owner`, never shell env; grid ops planned per (owner, workspace) | live prose | `grid/mod.rs:110` still takes the owner scope from env `QUAQUA_OWNER` and uses tokens only to narrow (`:114-116`), contradicting the "never env" clause |
| harness-adapters: liveness hooks and provider-error classifiers are per harness; key on HTTP status or structured fields, never port the Claude tables | enforced | `task/prompt.rs:332-338` (verdict from HTTP status), `task/prompt.rs:360` (opencode bare-status shape), `herdr/liveness.rs` |
| journal-boundary: a journal boundary that switches an enum to raw JSON must decode each tag to its own inner value | live prose | design lesson; code in `engine/journal_sqlite.rs`, no rule-specific check |
| precommit-paths: the pre-commit gate scans added lines in Rust tests; compose machine paths in fixtures, never write them literally | enforced | pre-commit hook `pre-commit:31` (`path_re`), pathspec at `:14` |
| rustfmt: format only the files you touched; never `cargo fmt --all` while HEAD is not rustfmt-clean | live prose | no check |
| test-stubs: a stubbed git keeps a worktree HEAD pinned at its add sha | live prose | no check |
| test-stubs: a caller-exit test must let the fake worker write its own report | live prose | no check |
| test-stubs: a self re-exec (detach) is opt-in at the CLI entry only | dead | `grep -rln detach crates/qq-core/src` hits only `authority/tokenizer.rs`; no detach flow in `task/` |
| worktree-repo: a worker repo is recorded at spawn in `runs/<name>.repo`; never infer it from the `.wt` path | enforced | `task/spawn_cmd.rs:573` |
| worktree-repo: ask "is this path in a git repo" with `git -C <path> rev-parse --git-dir`, never a `<path>/.git` existence check | live prose | no check; `auditor/precheck.rs:39` has its own `git_is_repo` |

## ~/.claude/CLAUDE.md (global, personal)

| Rule | Tag | Evidence |
| --- | --- | --- |
| Identity: use the configured names for self and user | live prose | no hook; persona text only |
| Casual, short, friendly tone | live prose | no check |
| First line of every reply carries the configured prefix | live prose | no hook (`~/.claude/settings.json` hooks: SessionStart, SubagentStart, UserPromptSubmit, Stop, StopFailure, SessionEnd, Notification; none check replies) |
| Lead with the verdict; add only the evidence that matters | live prose | no check |
| Reply in the language of the user's last prose message | live prose | no check |
| Think before coding; state assumptions and competing readings; mention a simpler approach | live prose | no check |
| Write the minimum code that solves the request; no placeholders or TODOs | live prose | no check |
| Turn the task into a check (failing test, repro, before/after) and iterate until it passes | live prose | no check |
| Prove completion with evidence; say why when verification is impossible | live prose | no check |
| At each phase boundary report done and next; at the end report changed, ran, remaining risk | live prose | no check |
| The shell may be zsh; run bash-only syntax via `bash` with `set -euo pipefail` | live prose | no check |
| Keep changes scoped to the request; leave the user's changes in place | live prose | no check |
| Delete with `trash`, never `rm`; stop if the target list is empty or unverified | live prose | no PreToolUse hook in `~/.claude/settings.json` |
| Run destructive commands only when asked or approved | live prose | gate covers push and merge only (`authority/gate.rs`) |
| Do not send private or internal URLs to third-party services | live prose | no check |
| Ask a clarifying question only when blocked or no safe default exists | live prose | no check |
| Web access: search with TinyFish first, fall back to Exa or Firecrawl | live prose | no check |
| Web access: read pages with the `read` skill; browser automation only for interaction | live prose | no check |

## ~/.claude/rules/english.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Coach quietly at the end of a reply, never in the body | live prose | no check |
| End with a `---` line, then at most 2 correction lines, most important first | live prose | no check |
| Line format: original arrow corrected (Pattern); corrected side is natural English | live prose | no check |
| Correct only the user's own text when it teaches something | live prose | no check |
| Skip URLs, commands, code, logs, names, quotes, already-natural English, bare fragments | live prose | no check |
| Give the correction alone, no explanation, no quotation marks; keep the tone kind | live prose | no check |
| Use only the listed Pattern names | live prose | no check |

## ~/quaqua/.claude/rules/herdr-fanout-grid.md

| Rule | Tag | Evidence |
| --- | --- | --- |
| Applies to every Claude Code session in `~/quaqua` that spawns or prompts herdr agents | live prose | scope statement; the gate hook covers every session (`~/quaqua/.claude/settings.json` PreToolUse) |
| Single source is `rules/_all/fanout-agent-grid.md` (orchestrator-only) | live prose | pointer; duplicates that file's content |
| `quaqua worker spawn` and `worker cleanup` grid the panes themselves; run `quaqua grid reconcile` by hand only to repair | enforced | `task/spawn_cmd.rs:589`; `quaqua grid --help` |
| Close a pane (`quaqua worker cleanup`) as soon as its run is verified | live prose | no check |

## Cross-source duplicates (one home per fact, for Q19)

- Approval list and no push, merge or branch delete: AGENTS.md Approval, `rules.md` (branch row, merge row), `handoff.md`, `merge-queue.md`.
- Lead gate: AGENTS.md Roles, `rules.md` Lead check, `lead-gate.md`.
- Caps: AGENTS.md Memory, `memory/caps.rs` header, `rules.md` header comment.
- Machine paths: `rules.md`, `portable-paths.md`, `precommit-paths.md`.
- Grid: `rules/_all/fanout-agent-grid.md`, `.claude/rules/herdr-fanout-grid.md`, AGENTS.md Workers.
- Report contract: `brief.md` and `worker-timebox.md` both state exit 6 and the `## Lessons` tail.
