# Live rules to code checks (hermes-cos-3 map Q20)

Read-only research, built on the Q18 inventory (`git show f6d4dd9:docs/research/hermes-cos-memory/rules-inventory.md`) after Q19 (#304, 2ecc892) deleted the dead rules. Every row is a live prose rule still in force. anh iu decides row by row. No engine code was changed.

Principle (anh iu, USER.md): a rule becomes a tool wherever a tool can check it; text stays only when no tool can.

Counts: 103 rows. code check 40, keep as prose 45, CoS may skip under autonomy 7, delete 11.

## Verdicts

- **code check**: build a check (the Check column names the command, hook or gate function and the file). Planned work in the plan (R4, R5, R7, R10) is marked `planned`.
- **keep as prose**: judgment no tool can check; the rule stays.
- **CoS may skip**: under the autonomy decision (plan line 10: the CoS acts by itself at low and medium tier) the rule stops binding; the sentence is removed from AGENTS.md and nothing replaces it.
- **delete**: the text is wrong or redundant against current code.

Paths: engine paths are relative to `crates/qq-core/src/`, line numbers at HEAD 2ecc892 of this worktree. Rows that share a verdict and a check are grouped, so one row can name several bullets.

## Findings that matter for the decision

- **The workspace mirror is ahead of the engine template.** `~/quaqua/rules/_all/worker-timebox.md` says `--check-brief` rejects a prototype brief whose Time budget exceeds `timers.pilot_budget`; the template says only "30 min". `grep -rn pilot crates/qq-core/src` finds nothing, so the workspace sentence describes a check that does not exist. It is a false claim, not prose (row W-pilot).
- **Hidden enforcement points found** (grep before calling prose): `quaqua pipeline` already refuses a reviewer on the builder's harness (`task/pipeline_io.rs:375` `same_harness`) and a kind with `can_review = false` (`:369`); `worker spawn --review` needs `can_review` (`task/spawn_cmd.rs:468`); the merge-evidence check is planned as R10.
- **No check reads the coordinator rules.** `grep -rn 'poteto\|handoff' crates/qq-core/src` finds only the file table (`init/table.rs:43`, `:322`) and the auditor's `handoff` mail kind (`auditor/precheck.rs:763`).
- **The only Write/Edit hook is the memory cap hook** (`memory/caps.rs:142` `pre_decision`, matcher `Write|Edit|MultiEdit` in `templates/workspace/.claude/settings.json`). It reads `file_path` but knows no caller and fences no path.
- **`authority/gate.rs` is Bash-only** (`:1-12`): push, `gh` merge, herdr pane ops, `runs/qq.db`, main/master deletes. Deploy, spend, outward send, release and tag have no check yet; R7 (planned) adds them.
- **Stale text found, not fixed here (outside the three fixes):** the README `tests/run.sh` section (no such file); the six-field report shape in `brief.md` (engine skeleton is `report_contract.rs:34-40`); `herdr agent read` in the template `worker-timebox.md` (the workspace copy already moved on).

## The four coordinator rules and the USER.md line

| Rule | Verdict | Why and check |
| --- | --- | --- |
| Run `/poteto-mode` before every brief | CoS may skip | Nothing reads it (grep above). The real brief guard is `--check-brief` (`task/prompt.rs:96`), which checks headers, not style. A "poteto ran" marker would be a check on the ritual, not on the brief. Drop the Route bullet; keep the skill available. Brief lint ideas that do catch what poteto-mode guards live in shortlist item 1. |
| Session per wave: `handoff`, then a fresh session | CoS may skip | No check, and the standing block plus computed situation (`memory/standing.rs`, `memory/situation.rs`) already rebuild context at any session start. The cost it guards is context size, which `situation.rs` can show as a computed line. Drop the sentence; keep the `handoff` skill. |
| CoS never edits code or config | code check (new) | Prose today; the only Write/Edit hook is the cap hook. Add a caller-aware path fence in `memory/caps.rs:142` `pre_decision` (or a sibling hook on the same matcher): for a `cos` caller, `Write`, `Edit` and `MultiEdit` under any worktree (`identity/mod.rs:130` `is_worker_worktree`), `crates/`, `tests/`, `.github/` or outside the workspace root returns `ask`. Same hook shape as plan R5 (`instruction_file_gate`); reuse its caller lookup. Effort M. |
| Review diffs in a separate worker task | code check (partly built, rest planned) | Inside `quaqua pipeline` it is enforced: reviewer must be on another harness and `can_review` (`task/pipeline_io.rs:369`, `:375`), review is bound to the builder's sha (`task/pipeline.rs:12-13`). The gap is a CoS that merges without a pipeline. Plan R10 closes it: the daemon `pr.merge` verb refuses a sha that has no reviewed-sha row in the workflow journal. Once R10 lands, delete the prose. The `builder_reviewer_differ = "kind"` key (`templates/workspace/team.toml`) is read by no code; delete the key or wire it into `check_kinds` (`task/pipeline_io.rs:357`). |
| USER.md: "Wants to be asked before anything that leaves the machine or cannot be undone" | code check (planned R7), then rewrite the line | The enumerable parts become gate verdicts: `authority/gate.rs:411` `check_git`, `:480` `check_git_branch`, `:352` `check_gh` (today push, `gh` merge, main/master delete only) plus R7's floor class (spend, deploy, send outward → `ask` at every tier) and an MCP matcher for outward tools (inventory `mcp-outward-tools.md`: 9 tools). Under autonomy "leaves the machine" is no longer a blanket ask: push of a non-main branch and opening a PR are low tier and allowed. Proposed USER.md text (CoS edits, 1375 cap, now 739): "Ask before spend, deploy, sending or publishing outward, and anything the gate asks on; everything else decide and log." |

## ~/quaqua/memory/rules.md

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Clean up each worker right after its run is verified and tests pass | rules.md bullet 1 | code check | `worker cleanup` warns when the worker's report has no passing `Verified:` level; `task/cleanup.rs:285` `cleanup_worker_live`, level list `task/worker_flow.rs:118` | `worker cleanup` exists, nothing links it to verification |
| Keep the branch; branch delete, push, merge need approval | rules.md bullet 1 | code check (planned R7) | `authority/gate.rs:480` `check_git_branch` gains the merged-into-default test (low tier allow, else ask) | gate blocks only main/master deletes; push `:411`, merge `:352` already enforced |
| A `wait` timeout or `agent_not_found` is not failure: confirm with `herdr agent list` and `pane read` before respawning | rules.md bullet 2 | keep as prose | none | the watchdog applies the idea (`herdr/liveness.rs:3`); the CoS step is judgment |
| Ownership: orchestrators work only in their own repo; check `git worktree list`; one file, one branch | rules.md bullet 3 | keep as prose | none (the `worker spawn` worktree already isolates, `task/spawn_cmd.rs:116`) | text also seeded by `init/seeds.rs:18`; deleting it there is an engine change |
| Gardener: a bug class found twice becomes a rule or test; a workaround used twice a script | rules.md bullet 4 | CoS may skip | none | meta rule, no counter, duplicates the AGENTS.md "encode repeated instructions" bullet |
| Context pointers: prompts point to spec, rules, runs, commits, never restate; keep verification output short | rules.md bullet 5 | keep as prose | none | style rule |
| Every retro finding becomes a guard worker, never a rule-only fix | rules.md bullet 7 | keep as prose | none | process rule, no machine-readable retro |
| Briefs use worktree-relative paths only; never an absolute main-checkout path | rules.md bullet 10 | code check | `--check-brief` rejects a brief body containing a `/home/` or `/Users/` path: `task/prompt.rs:96` `check_brief`, new loop after `:109` | `check_brief` reads headers only; a worker edited the main checkout from such a brief |
| Hand anh iu a `gh pr merge` command only after every CI check is green | rules.md bullet 11 | delete | none | superseded by plan R10: the CoS no longer hands merges over; `pr.merge` requires `gh pr checks` green on the sha |

## ~/quaqua/AGENTS.md: Approval and Identity

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Ask before deleting data | AGENTS.md Approval | code check (planned R7) | gate: `rm`, `unlink`, `git clean` on a path outside scratchpad is medium tier `ask`, `trash` allowed: `authority/gate.rs:65` `check` | no delete check; only `sqlite3` on `qq.db` is guarded (`:235`) |
| Ask before push or force-push | AGENTS.md Approval | code check (built) | `authority/gate.rs:411`; planned R7 allows a non-main push through the daemon door, denies main and force | enforced today by blanket block |
| Ask before deleting a branch | AGENTS.md Approval | code check (planned R7) | `authority/gate.rs:480` `check_git_branch` | only main/master blocked |
| Ask before merge to main | AGENTS.md Approval | code check (built, planned R10) | `authority/gate.rs:352` `check_gh`; daemon `pr.merge` | merge only through the daemon |
| Ask before deploy | AGENTS.md Approval | code check (planned R7) | floor class → `ask` at every tier; patterns in `authority/gate.rs:65` `check` | no deploy check |
| Ask before spending money | AGENTS.md Approval | code check (planned R7) | MCP PreToolUse matcher on the 7 spend tools (`mcp-outward-tools.md`) in `templates/workspace/.claude/settings.json` | no check |
| Ask before sending or publishing anything outward | AGENTS.md Approval | code check (planned R7) | same MCP matcher (2 publish tools) plus `gh api` write patterns in `authority/gate.rs:352` | gate covers push and `gh api` writes only |
| Ask before cutting a release or tag | AGENTS.md Approval | code check (planned R7) | `git tag`, `gh release create` patterns in `authority/gate.rs:411` and `:352`; medium tier allow under autonomy | no tag or release check |
| "Don't stop" or "run until done" never waives the list | AGENTS.md Approval | delete | none | becomes true by construction once the gate is code; prose adds nothing |
| No approval needed: spawn, prompt, cleanup, local commits, memory and plan writes, scratchpad files | AGENTS.md Approval | delete | none | permission statement; the tier table (R7) is the source of truth |
| Accuracy and task completion outrank persona; verify package, version, formula path | AGENTS.md Identity | keep as prose | none | judgment |
| Keep private data private; report goal or scope change, red main, decisions on the Principal's behalf | AGENTS.md Identity | keep as prose | none (`memory/threat.rs` already scans memory writes for secrets) | judgment |

## ~/quaqua/AGENTS.md: Memory

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| After each outcome rewrite `## Now` (5 items) and append one `- [slug] <worker> <outcome>` line to today's memory | AGENTS.md Memory | code check | `worker cleanup` appends the outcome line itself: `task/cleanup.rs:285` `cleanup_worker_live` (outcome from the report's `Outcome:` field, `task/worker_flow.rs:132`); `## Now` stays CoS judgment | no writer; `auditor/precheck.rs` only reads |
| Copy accepted `runs/<name>.md` to immutable `raw/`; cite `(<sha> · raw/<file>)` | AGENTS.md Memory | code check | accept step copies the report to `raw/<date>-<proj>-<task>.md` and refuses overwrite; accept path in `task/worker_flow.rs:118-169` | auditor flags missing raw after the fact (`auditor/precheck.rs:763`) |
| At cleanup promote at most one lesson to `rules/`; fix a contradicting older rule | AGENTS.md Memory | keep as prose | none | needs judgment |
| Log worker failures as `- [case] <worker> <exit N\|reject\|kill>: <cause>` | AGENTS.md Memory | code check | `worker prompt` exit path appends the case line: `task/prompt.rs:208-240` `decide` (cause stays free text, exit code is known) | exit codes are machine-known, the line is hand-written |
| "Nhớ cái này" goes to `memory/rules.md`; Principal facts to `USER.md`; no Claude auto-memory | AGENTS.md Memory | keep as prose | none (cap hook guards size, `memory/caps.rs:27`, `:29`) | routing judgment |
| Do not store live state from herdr or git, trivia, session-only details | AGENTS.md Memory | keep as prose | none (`memory/threat.rs` scans secrets and injections only) | judgment |

## ~/quaqua/AGENTS.md: Roles and Workers

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Authority comes from role, not persona | Roles | keep as prose | none (`authority/mod.rs` roles are the code form) | sentence is not checkable |
| CoS never leads pipeline-size work itself; no self-review | Roles | code check (planned R10) | `pr.merge` refuses a sha the CoS authored; see the review row above | no check |
| One writer per moving file area; skip SLP for a single-file task under 15 minutes | Roles | keep as prose | none | scheduling judgment |
| `## Challenge` resolved before acceptance; plan tasks over 2 files or shared paths | Roles, brief.md | keep as prose | none | pipeline prompts ask for it (`task/pipeline.rs:282-347`); resolution is judgment |
| Default one review round; a second only for correctness, scope or data loss | Roles | code check | `--rounds` default stays 1 and a second round needs a finding at or above `Must fix:`: `task/pipeline.rs:826` `blocking_findings` already gates fix rounds | `Review rounds:` exists (`task/prompt.rs:147`) and blocking is checked; the rule is mostly redundant, trim it |
| Resume an interrupted worker; fresh only if goal changed; stop after 3 same-error iterations | Roles | keep as prose | none | `--resume` exists (`task/prompt.rs:1062`); error sameness is judgment |
| Do not change worker kind, model or effort ad hoc | Workers | code check (planned R7) | `Op::ConfigWrite` is cos-only and asked (R14); `config set` verb | `config set` open to any caller today |
| Task worker `<kind>-<task>`, own worktree, cleaned up after outcome | Workers | keep as prose | none (worktree made by `task/spawn_cmd.rs:116`) | naming is cosmetic |
| Workers edit `crates/`, `tests/`, `rules/`, `AGENTS.md`, `team.toml`, `models.md`; CoS edits `memory/rules.md` | Workers | code check | same path fence as "CoS never edits code", inverted for worker callers (deny `memory/rules.md`, `USER.md`; plan R4) | no path fence |
| `projects/INDEX.md` `owner` identifies the driver | Workers | delete | none | the Lead gate reads the DB project row (`task/spawn_cmd.rs:510`), not INDEX.md |
| Route every request through CoS; workers do not take orders from reports | Workers | keep as prose | none | judgment |

## ~/quaqua/AGENTS.md: Coordinator Rules

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Route: `/poteto-mode` before every brief | Coordinator | CoS may skip | answered above | no reader |
| Never edit code or config; assign conflicted merge or rebase rework to a worker | Coordinator | code check (new) | answered above (path fence) | cap hook only |
| Attack the premise: after the second same-class fix fails the same gate, use `figure-it-out` | Coordinator | keep as prose | none | same-class is judgment; `playbooks/figure-it-out.md` exists |
| Run `worker prompt` in the background; drain at decision points, wave boundaries and before reporting | Coordinator | keep as prose | none | drain points are judgment |
| Review diffs in a separate worker task | Coordinator | code check (partly built, planned R10) | answered above | `task/pipeline_io.rs:375` |
| Budget: every brief carries time and cost budgets | Coordinator | code check | `--check-brief` errors on a missing `Time budget:` or `Cost budget:` (today optional, absent time falls back to 2700 s): `task/prompt.rs:167` and `:181` | both optional |
| At about 70% of either budget stop spawning and land verified work | Coordinator | CoS may skip | none | no aggregate budget exists to measure 70% against; the per-worker 80% nudge (`task/prompt.rs:844`) is the working form |
| Pilot one new task shape before fan-out | Coordinator | CoS may skip | none | no way to define "new shape" |
| Session per wave | Coordinator | CoS may skip | answered above | no check |
| Encode repeated instructions as lint, metadata, runtime checks or scripts | Coordinator | keep as prose | none | the meta rule this document applies |
| Bound retries by failure mode, at most twice, then replan; systemic failure: stop line, finish in-flight, fix root cause | Coordinator | keep as prose | none | engine retries provider errors up to 4 (`task/prompt.rs:953`), a different bound; failure mode is judgment |
| At each wave boundary account for every worker as arrived, respawned or absorbed | Coordinator | CoS may skip | none | `quaqua status` lists workers; the CoS reads it anyway |

## ~/quaqua/rules/_all/brief.md (template `crates/qq-core/templates/workspace/rules/_all/brief.md`)

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Hard constraints: targeted tests only, foreground, no `pgrep -f` wait loops | brief.md header block, worker-timebox.md | code check | `--check-brief` requires `Hard constraints:` to contain a test-filter form (`<cmd> <filter>`) and no `pgrep -f`: `task/prompt.rs:109` | line is required, content unread |
| `Unknowns:` and `Evidence:` lines (exact verify command, real output) | brief.md | code check | `--check-brief` warns when either is missing: `task/prompt.rs:96` | not parsed |
| Cost budget recorded at acceptance in the project card | brief.md | code check | accept step writes `cost=$y/$x` (see shortlist 4): `task/cleanup.rs:285` | no project-card writer |
| Mechanical task (≤2 files) skips `Current design` and plan file | brief.md | keep as prose | none | no check possible without a diff |
| No pre-solve; `Current design` is a hypothesis; keep briefs short, technical, no mention of AI | brief.md | keep as prose | none | style |
| Review before sending: "parity" or "like X" scopes every command of X | brief.md | keep as prose | none | judgment |
| `## Challenge` is a right, resolved by the Lead before acceptance | brief.md | keep as prose | none | see Roles row |
| Report: six fields first (Snapshot, Scope, Verification, Unknown / risk, Ownership) | brief.md Report format | delete | none | the engine skeleton is Outcome plus Changes, Verify, Risks, Lessons (`task/report_contract.rs:34-40`); the six-field stanza is stale |
| `runs/<name>.md` ends with `## Lessons` of 2 lines or fewer; CoS promotes 0 to 1 | brief.md | code check | `well_formed` also rejects a Lessons body over 2 non-empty lines: `task/report_contract.rs:70` | heading is checked, length is not |
| Publish atomically: write `runs/.<name>.tmp`, then `mv` | brief.md | code check | `decide` treats a report that fails `well_formed` (`task/report_contract.rs:70`) as not ready for a file younger than one poll, instead of exit 6: `task/prompt.rs:208-216` | the pipeline reads the report once the file exists (`task/prompt.rs:207`); no write-then-rename check, the Q18 exit-6 path applies to a partial file |
| Stale reports from earlier tasks are archived, never consumed | brief.md | keep as prose | none (new-task reset at `task/prompt.rs:1085`) | counters reset, archive is a CoS step |
| Acceptance conditions go in the review or fix brief, never appended to the plan file | brief.md | keep as prose | none | judgment |
| Read a run report by section, not a whole-file cat | brief.md | CoS may skip | none | cost hint, not a rule |

## ~/quaqua/rules/_all/caller-identity.md

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Cleanup, reconcile and pipeline move refuse a `cos` pane | caller-identity.md token table | code check | add the role check to reconcile and pipeline move: `grid/mod.rs:115` `Scope::admits` already excludes non-worker roles; cleanup uses it (`task/cleanup.rs:116`) | reconcile and pipeline move not found by grep |

## ~/quaqua/rules/_all/change-policy.md

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Five questions before fixing a mechanism | change-policy.md | keep as prose | none | judgment |
| Tests: one per invariant; bug fix starts with a failing reproduction; no tests for wording or layout; extend existing cases first; report test lines added and removed | change-policy.md | keep as prose | none (reviewers check these, `playbooks/bug-fix.md`) | no machine form |
| Docs: one home per fact; delete stale text when cutover lands; scope is project repos | change-policy.md | keep as prose | none | judgment |

## ~/quaqua/rules/_all/config-dynamic.md

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Builder and reviewer differ by kind (`builder_reviewer_differ = "kind"`); CoS keeps this by hand | config-dynamic.md | code check | `check_kinds` in `task/pipeline_io.rs:357` already refuses the same harness; add the same-kind refusal or delete the key from `templates/workspace/team.toml` | `grep -rn builder_reviewer_differ crates/qq-core/src` finds nothing |

## ~/quaqua/rules/_all/fanout-agent-grid.md and ~/quaqua/.claude/rules/herdr-fanout-grid.md

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| One project per workers tab; never mix projects or owners, or put 2+ workers in the CoS tab | fanout-agent-grid.md | code check | `grid/arrange.rs:92` already filters owner and farm; add a refusal when the CoS tab would hold 2 workers: `task/spawn_cmd.rs:589` | CoS-tab clause unchecked |
| Label each pane with the worker name | fanout-agent-grid.md | keep as prose | none | cosmetic |
| `herdr-fanout-grid.md` (MANDATORY summary: spawn and cleanup grid the panes, close the pane when verified) | .claude/rules/herdr-fanout-grid.md | delete | none | a pointer to `fanout-agent-grid.md`, which spawn and cleanup already enforce; keeping both duplicates one fact |

## ~/quaqua/rules/_all: git-hygiene, handoff, herdr-agent-status, herdr-plugin-smoke, hooks, test-isolation

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Gitignore `dir/` patterns match at any depth; assert with `git check-ignore` | git-hygiene.md | keep as prose | none | one-line lesson |
| Work only inside your worktree | handoff.md | code check | the worker caller's Write/Edit path fence (shortlist 2, inverted: deny paths outside the worker's worktree) in `memory/caps.rs:142` | no path fence |
| No network publish by a worker | handoff.md | code check (planned R7) | a worker caller gets `deny` on every floor-class hit | gate has no worker/CoS split today |
| When the task names a summary file, that file is the deliverable; reply only with its path | handoff.md | keep as prose | none | style |
| Blocked on a decision you cannot make: ask briefly and stop | handoff.md | keep as prose | none | judgment |
| Probe a harness by hand with its roster model from `models.md` | herdr-agent-status.md | keep as prose | none | hand procedure |
| `agent wait` returns a stale `idle`; use `agent prompt --wait --until working --until blocked` | herdr-agent-status.md | keep as prose | none | client supports `--until` (`herdr/mod.rs:139`); the rule is about hand use |
| Smoke-test herdr plugins with HOME and XDG_CONFIG_HOME in a temp dir and `HERDR_SOCKET_PATH` unset | herdr-plugin-smoke.md | keep as prose | none | `tests/hermetic.sh` does not claim it |
| A converged-check and the repair it guards share one acceptance predicate | hooks.md | keep as prose | none | design lesson |
| Before trusting "no test needs herdr", run the suite under `env -i` without herdr and tmux | test-isolation.md | code check | `tests/hermetic.sh` gets an `env -i` run: file `tests/hermetic.sh` | exists but does not claim this |

## ~/quaqua/rules/_all/lead-gate.md and lead-inbox.md

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| The gate runs only for a repo mapped to a `projects/INDEX.md` row owned by `cos` | lead-gate.md | delete | none | the gate keys on the DB project (`task/spawn_cmd.rs:510`); `grep INDEX task/` finds nothing |
| Signal 2: plan is the `docs/plans/active/*.md` the card names, else `<slug>.md` | lead-gate.md | delete | none | code reads only `docs/plans/active/<slug>.md` (`task/spawn_cmd.rs:749-756`); rewrite to match |
| A Lead reports through `quaqua mail send`, never `herdr agent prompt` to the CoS pane | lead-inbox.md | code check | gate: block `herdr agent prompt` aimed at a `cos` pane for a Lead or worker caller: `authority/gate.rs:385` `check_herdr` | the gate blocks pane input only (`:28`) |
| Manual fallback: single-write append, header `## <ISO time> <lead> <kind>`, kind from a closed set, 10 lines or fewer | lead-inbox.md | delete | none | `mail send` writes SQLite and `runs/<slug>-lead.inbox.md` itself (`bus/inbox_md.rs:1-23`); the manual form is a fallback no one uses |
| Append only; never rewrite or truncate the inbox file | lead-inbox.md | keep as prose | none | the file is a mirror of SQLite |

## ~/quaqua/rules/_all/merge-queue.md

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Return rework to the Peer with evidence; never patch failing Peer code yourself | merge-queue.md | keep as prose | none | covered by the CoS path fence for the CoS; a Lead's judgment otherwise |
| If the integration branch fails without the branch too, hold the queue and escalate | merge-queue.md | keep as prose | none | judgment |
| Never rewrite integration history except to undo its own last failing merge | merge-queue.md | code check (planned R7) | history-rewrite patterns on shared refs in `authority/gate.rs:411` | no check |
| `quaqua merge <slug> <branch>...` argument shape | merge-queue.md | delete | none | CLI form is `merge '<json>'`; the cargo-check bullet is redundant with `merge/mod.rs:169` |

## ~/quaqua/rules/_all: no-gui-driving, opencode-no-bg-wait, worker-interrupt, portable-paths

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Never drive the real display (launch GUI apps, move windows, screenshots); macOS: no `osascript`, `open`, `screencapture`, `cliclick` | no-gui-driving.md | code check | gate patterns in `authority/gate.rs:65` `check` for `osascript`, `screencapture`, `cliclick`, `xdotool` | Bash-only gate exists; no such pattern |
| opencode workers never run tests in the background and wait | opencode-no-bg-wait.md | keep as prose | none | harness behaviour, cannot be read from the call |
| Stop a busy opencode worker with `herdr agent send-keys <name> esc esc`; never ctrl+c | worker-interrupt.md | keep as prose | none | behaviour claim; gate blocks only `pane send-keys` (`authority/gate.rs:28`) |
| Absolute home paths never in committed files | portable-paths.md, rules.md | keep as prose (enforced) | none new | enforced by the pre-commit hook (`pre-commit:31`); the sentence repeats rules.md bullet 6, keep one |

## ~/quaqua/rules/_all/worker-timebox.md

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| Cost budget is the money twin; write `cost=$<y>/$<x>` on the cleanup line; over budget with nothing landed, say so | worker-timebox.md bullet 2 | code check | `worker cleanup` writes `cost=` itself from `quaqua status --metrics`: `task/cleanup.rs:285`, `status/mod.rs:72-84`; blocked on a spend source (shortlist 4) | no writer; `status --metrics` shows 0 runs with cost today (plan open_items) |
| On exit 3 to 5 and 7, run `herdr agent read ...` then `--resume`; never blind re-prompt | worker-timebox.md (template wording) | delete | none | the engine prints the pane tail itself on exit 3-8 (workspace copy says so); remove the manual step from the template |
| W-pilot: pilots and spikes get a 30 min budget (`timers.pilot_budget`); at 30 min with no PASS stop, ask the advisor once, one more try of 15 min or less | worker-timebox.md bullet 9 | code check | `--check-brief` rejects `Playbook: prototype` whose Time budget exceeds `timers.pilot_budget`: `task/prompt.rs:167`; read the key with `config::setting` as `task/spawn_cmd.rs:699` does | `timers.pilot_budget` seeded (`templates/workspace/team.toml:15`), no reader; the workspace text already claims the check |
| Builders run only the targeted tests named in the brief, foreground; no `pgrep -f` wait loops | worker-timebox.md | code check | same lint as the brief Hard constraints row; plus the existing `--check-brief` | no check |

## Workspace templates outside rules/_all

| Rule | Where | Verdict | Check it becomes (file) | Evidence |
| --- | --- | --- | --- | --- |
| PEER.md boundaries: stay in repo and owned scope, commit locally, never push, deploy or call external services | PEER.md | code check (planned R7) | worker caller gets `deny` on every approval-list hit (R7); path fence above | prose only; gate is not caller-aware |
| PEER.md handback: no `git add -A`, no `reset --hard`, `clean -fd`, `checkout -- .`; frozen sha after handback | PEER.md | code check | gate patterns for `git add -A`, `git reset --hard`, `git clean -fd`, `git checkout -- .` for a worker caller: `authority/gate.rs:411` `check_git` | no check |
| PEER.md: contract before test, whack-a-mole, proof test, never accept your own work | PEER.md | keep as prose | none | judgment for workers; the pipeline's separate reviewer enforces "never accept your own work" (`task/pipeline_io.rs:375`) |
| `playbooks/` (18 files) and `principles/` (25 files) | templates/workspace/playbooks, principles | keep as prose | none | reading material; `--check-brief` already resolves a `Playbook:` name against the index (`task/prompt.rs:115-126`) |
| `herdr-orchestration` and `poteto-mode` skills | templates/workspace/.claude/skills | keep as prose | none | tooling guidance, not rules; `poteto-mode` Route bullet is the only rule and is covered above |

## Shortlist: code checks worth building (ranked by rows retired per effort)

1. **Brief lint in `--check-brief`** (`task/prompt.rs:96`, after `:109`; `:167`, `:181`): require Time and Cost budget, reject an absolute `/home/` or `/Users/` path, require `Unknowns:` and `Evidence:`, require a test filter and no `pgrep -f` in `Hard constraints:`, and reject `Playbook: prototype` over `timers.pilot_budget`. Retires 7 rows (budget, worktree-relative paths, targeted tests, pilot, two brief headers, the stale `--slow` constraint). Effort **S, about half a day**, tests in `tests/qq-test.sh` filters on `task::prompt`.
2. **Caller-aware Write/Edit path fence** (`memory/caps.rs:142` `pre_decision` or a sibling hook; shares the caller lookup with plan R5 `instruction_file_gate`): CoS asks on `crates/`, `tests/`, `.github/`, any worktree or outside the workspace; a worker is denied outside its worktree and on `memory/rules.md` and `USER.md`. Retires 5 rows ("CoS never edits code", workers edit only their scope, work only in your worktree, merge-conflict rework). Effort **M, about 1 day**, mostly the caller lookup R5 already builds.
3. **R7 tier gate** (`authority/gate.rs:65`, `:352`, `:411`, `:480`, plus the MCP matcher): already in the plan. Retires the 8 Approval-list rows, "Don't stop", the Approval permission statement, the history-rewrite and `git add -A` rows and the USER.md line once the USER.md text is rewritten. Effort **L, already scheduled (ship 3)**; listed so the prose is deleted when it lands.
4. **Cost watchdog and automatic `cost=`** (`task/prompt.rs:844` beside the 80% nudge; `task/cleanup.rs:285`): warn at 80% and 100% of the Cost budget and write `cost=$y/$x` on cleanup. Retires 3 rows and answers anh iu's "timebox as a tool". Effort **M, about 1 day, but blocked on a spend source**: `quaqua status --metrics` shows 0 runs with cost today, so item 4 starts by finding where spend is recorded.
5. **Cleanup journal writer** (`task/cleanup.rs:285` `cleanup_worker_live`, exit path `task/prompt.rs:208`): append the `- [slug] <worker> <outcome>` line and the `- [case] ... exit N` failure line to today's memory, copy the accepted report to `raw/`. Retires 4 rows. Effort **M, about 1 day**; do it with item 4 since both touch cleanup.

## Not recommended as code

Poteto-mode before each brief, session per wave, pilot-a-new-shape, 70% stop-spawning and wave accounting have no measurable predicate; the autonomy decision lets the CoS drop them. Everything under "keep as prose" is judgment about what to inspect or change, which anh iu's USER.md line reserves for text.
