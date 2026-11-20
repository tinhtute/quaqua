# Hardcode inventory — four scout lanes (base 408a1e6)

Evidence for [`docs/research/plans/no-hardcode-config.md`](../plans/no-hardcode-config.md). Four read-only scouts inventoried policy literals in the engine at base `408a1e6`; line numbers are at that base and drift as other work lands. Source reports: `~/quaqua/raw/2026-11-20-hermes-cos-hc-{core,guard,rest,ext}.md`.

Home column: **G** global config, **W** `team.toml`, **M** template manifest, **K** keep in code, **?** doubtful.

Tables are verbatim from the reports. Edit: any literal containing an unescaped `|` is escaped as `\|` so rows keep 5 columns (a column-count check over all four tables found 0 broken rows, so no row needed fixing). Each lane's Verify and Lessons sections are left in the source reports; Changes intro, table, summary, risks and challenges are here.

## Core lane — `task/` and `engine/`

Source: `~/quaqua/raw/2026-11-20-hermes-cos-hc-core.md`

### Changes
Read-only scout; no repo file touched. Inventory of hardcoded policy in `task/` and `engine/` (production code only; `#[cfg(test)]` skipped, tests pin most message/section strings). No URLs, hosts or model names found in scope. Home: G global, W team.toml, M template manifest, K keep, ? doubtful.

| file:line | literal | category | home | note |
|---|---|---|---|---|
| engine/activity.rs:324-548 | activity type tags Spawn Cleanup Prompt Herdr Git ... | enum | K | serde wire tags |
| engine/guard.rs:42-63 | forbidden-access scan strings, "rs" ext | enum | K | dev guard |
| engine/journal_sqlite.rs:18-236 | JOURNAL_SCHEMA_SQL + SQL | enum | K | SQL schema |
| engine/runner.rs:100 | max_steps 10_000 | limit | G | runaway-flow guard |
| engine/runner.rs:245-1334 | journal event kinds activity_scheduled workflow_completed si | enum | K | journal wire format; replays pin it |
| engine/runner.rs:326 | SEQUENCE_REBASE_TRIES 16 | limit | K | internal retry |
| engine/sqlite.rs:6,10 | BUSY_TIMEOUT_MS 5000 + env QUAQUA_BUSY_TIMEOUT_MS | env | G | env + default |
| engine/sqlite.rs:25-27 | "database is locked" strings | enum | K | sqlite error text |
| engine/sqlite.rs:43 | 10ms busy retry sleep | limit | G | tuning |
| engine/sqlite.rs:65-76 | journal_mode WAL, synchronous NORMAL | enum | K | sqlite pragmas |
| task/cleanup.rs:20 | SETTLE_MS = 5_000 | limit | G | stop-settle wait; no config key |
| task/cleanup.rs:74,308-353 | git "-C" "worktree" "status --porcelain" verbs | external CLI | K | git syntax |
| task/cleanup.rs:120,492,505 | role = "lead"/"peer" SQL literals | enum | K | SQL schema; mirrors Role in spawn.rs |
| task/cleanup.rs:257,265 | ".{name}." / "{name}.base" runs sidecars | filename | M | sidecar names; also written in prompt.rs:776, spawn_cmd:570 |
| task/cleanup.rs:312,456,626 | "refs/heads/quaqua/{name}" branch prefix | path | M | also spawn_cmd:514,898 pipeline_io:139; quaqua/ = repo name |
| task/cleanup.rs:399 | "ctrl+c" forbidden-key assert | enum | K | debug_assert on herdr key names |
| task/cleanup.rs:420 | agent_wait "idle" status | enum | K | herdr status vocabulary |
| task/cleanup.rs:466,608 | ".wt" worktree dir beside repo | path | M | also spawn_cmd:159 prompt.rs:1178; AGENTS.md names .wt |
| task/cleanup.rs:580-659 | RPC verbs worker.stop/cleanup lead.cleanup grid.* | enum | K | wire protocol; CLI mirrors in prompt.rs:1344 |
| task/create.rs:19,138,172 | "task.create" verb + usage text | enum | K | wire verb, CLI help |
| task/create.rs:20 | DISPOSITIONS engineer/architect/reviewer/scout | enum | M | role vocabulary; also brief text; no test pins it |
| task/create.rs:49 | default disposition "engineer" | enum | M | duplicates default; team.toml could set |
| task/pipeline_io.rs:35,615,655 | VERB "pipeline", "merge", help text | enum | K | wire verbs |
| task/pipeline_io.rs:139 | branch "quaqua/{name}" | path | M | see cleanup.rs |
| task/pipeline_io.rs:155,570 | owner "cos", caller default "cos" | enum | M | CoS name; also spawn_cmd:503,1049,918 |
| task/pipeline_io.rs:170 | fallback slug "pipeline" | enum | ? | project slug fallback |
| task/pipeline_io.rs:379 | "review must run on another harness" | message | W | cross-harness rule is policy; always-on |
| task/pipeline_io.rs:445 | "only the Lead approves" | message | W | authority rule text; real check is role |
| task/pipeline_io.rs:664 | POLL = 500ms | limit | G | --wait poll interval |
| task/pipeline_io.rs:721,744 | journal kinds pause_for_approval approval_resolved | enum | K | journal wire |
| task/pipeline_io.rs:795-813 | exit codes 1,3,4,5 + status names merged/no_changes/cancelle | enum | K | CLI exit contract |
| task/pipeline.rs:30,32-45 | KIND "pipeline", exit codes 1,4,5,6,7,8,10,12,13,14 | enum | K | wire/exit contract; worker_flow.rs:44-48 reuses 10-14 with other meaning |
| task/pipeline.rs:47 | DEFAULT_MAX_ROUNDS = 2 | limit | W | team.toml [pipeline] max_rounds; JSON input overrides |
| task/pipeline.rs:48 | DEFAULT_TIMEOUT_MS = 4h | limit | W | duplicates a timers.* style default; input overrides |
| task/pipeline.rs:89-95 | builder "{task}-b" reviewer "{task}-r{N}" names | filename | M | naming scheme; spawn_cmd name rule NAME_MAX interacts |
| task/pipeline.rs:100 | "{runs}/{task}.{tag}.a{seq}.md" report path | path | M | report naming; tags plan/build/r/fix/mfix at 277-359 |
| task/pipeline.rs:277-365 | phase briefs PLAN/BUILD/REVIEW/FIX prose | message | M | rule-like prose in Rust; pipeline tests pin phrases |
| task/pipeline.rs:282,300,333,347,759 | "## Challenge" section name | section | M | also rg: worker briefs; CoS reads it |
| task/pipeline.rs:300 | "Outcome: NO_CHANGES" in prompt | section | M | dup of report_contract OUTCOME_FIELD |
| task/pipeline.rs:332,840 | "## Findings" + [critical\|major\|minor\|trivial] tags | section | M | dup of prompt.rs SEVERITIES; 817-820 map severity->rank |
| task/pipeline.rs:480 | "quaqua pipeline --resume {}" hint | message | K | self CLI name |
| task/pipeline.rs:759,798 | "Verified:" "Must fix:" field names | section | M | dup worker_flow.rs:150; prompt.rs:213 |
| task/pipeline.rs:763-779 | Outcome words NO_CHANGES BLOCKED DISPUTED REOPEN_REQUEST DEP | enum | M | dup in worker_flow.rs:142,166; report_contract:35 omits last 3 |
| task/pipeline.rs:785-789 | "## blockers" heading, "none" sentinel | section | M | report gate |
| task/prompt.rs:16-19 | DEFAULT_BUDGET_SEC 2700 MAX_SEC 7200 STALL_SEC 600 IDLE_SEC  | limit | W | watchdog; team.toml [timers]; PROMPT_WORKER_* env override only budget |
| task/prompt.rs:20-21 | BIG_TIME_MIN 240, BIG_COST_USD 10.0 (Lead-sized unit) | limit | W | brief warnings 237,248 |
| task/prompt.rs:22,216 | SEVERITIES critical/major/minor/trivial | enum | M | dup pipeline.rs:817; Must fix: values |
| task/prompt.rs:83 | "/home/" "/Users/" absolute-path scan | path | K | portable-path heuristic |
| task/prompt.rs:102-105 | playbook file "<name>.md" and "name: " front matter | filename | M | playbook format |
| task/prompt.rs:119-121 | QUAQUA_PLAYBOOKS_DIR env + "playbooks" dir name | env | G | env name + beside-runs default; AGENTS.md layout |
| task/prompt.rs:131-209 | brief headers Topology: Playbook: Context: Goal: Hard constr | section | M | brief schema; check_brief tests pin each; main churn source |
| task/prompt.rs:133 | Topology values "solo"/"lead" | enum | M | topology vocabulary |
| task/prompt.rs:166 | warning "names no test command" | message | M | brief lint, keyword "test" |
| task/prompt.rs:172 | warning "pgrep -f" ban | message | M | brief lint rule |
| task/prompt.rs:178-199 | Playbook "none" + "prototype" special | enum | M | prototype pilot_budget at 274-310 |
| task/prompt.rs:310 | "timers.pilot_budget" config key | enum | K | config key; config default lives in seeds |
| task/prompt.rs:314,101 spawn.rs | PANE_TAIL_LINES 40, pane_read 40/12 | limit | G | diagnostic tail sizes |
| task/prompt.rs:442-475,516-526,626-646 | FATAL_LINES API_FATAL API_RETRYABLE BARE_MARKERS AGY_* table | enum | ? | harness error strings; per-driver data not policy; M or driver |
| task/prompt.rs:492-509,819 | HTTP status -> reason map (402,408,409,425,429,529,5xx) | enum | K | HTTP semantics; reason names are policy ? |
| task/prompt.rs:705,713-720 | Shape Claude/Pi/Agy/Opencode + glyphs ⎿ ⏺ ● | enum | ? | harness names; belongs to driver/team.toml kind |
| task/prompt.rs:738 | TAIL_LINES = 15 classify window | limit | G | provider-error scan window |
| task/prompt.rs:929-936,1242-1249 | sidecars budget grace autoext nudged elapsed provider_retry  | filename | M | runs/.<name>.<ext>; cleanup list at 1242 must match |
| task/prompt.rs:999 | "Time check: past 80% ... Blockers section" nudge text | message | M | 80% threshold also hardcoded at 997 |
| task/prompt.rs:1055,852 | agent_prompt "continue" | message | M | retry nudge text |
| task/prompt.rs:1104-1106 | env_u64 PROMPT_WORKER_POLL_MS 5000, RETRY_BASE_MS 60000, RET | env | G | env + default; duplicates a team.toml default |
| task/prompt.rs:1149-1158 | runs/{name}.md report, archive "{name}.{ts}.md" | filename | M | report naming; also report_contract:46 |
| task/prompt.rs:1203,1310 | usage strings naming verbs/flags | message | K | CLI help |
| task/prompt.rs:1229,1316 | PROMPT_WORKER_BUDGET_SEC env | env | G | only env-wired budget |
| task/report_contract.rs:15-22 | "Verdict:" "Outcome:" "Lessons" "clean" "findings" | section | M | contract fields; test fixtures pin; 2 commits |
| task/report_contract.rs:35-39 | skeleton "## Changes/Verify/Risks/Lessons" + outcome words | section | M | must match templates/rules; drift risk |
| task/report_contract.rs:46 | "~/quaqua/runs/{name}.md" + atomic-publish prose | path | M | hardcodes home dir+runs dir; contradicts <home>/quaqua default override |
| task/report_contract.rs:65 | "## " heading prefix for lessons check | section | K | markdown syntax |
| task/spawn_cmd.rs:27-29 | NAME_MAX 20, LEAD_NAME_MAX 32, SOLO_MIN_CHARS 10 | limit | W | naming/solo policy |
| task/spawn_cmd.rs:31-35 | GATE_LIVE_BUILDS 2, GATE_PHASES_LEFT 1, GATE_MEMORY_DAYS 1 | limit | W | topology gate; legacy shell parity; strategy.* siblings already in team.toml |
| task/spawn_cmd.rs:37,948 | LEAD_READY_MS 300_000 + env QQ_LEAD_READY_MS | env | G | env + default |
| task/spawn_cmd.rs:38-39 | DEFAULT_CAP 8, DEFAULT_MAX_PEERS 4 | limit | W | duplicates team.toml limits.* at 713,716 (DEFAULT of a runtime value) |
| task/spawn_cmd.rs:41-42,825,960,971 | usage + hint text "quaqua lead spawn" "herdr agent prompt" | message | K | CLI help |
| task/spawn_cmd.rs:147 | git config worktreeConfig, pushurl "no_push" | external CLI | K | block_push; value policy ? |
| task/spawn_cmd.rs:159,466 | ".wt" dir | path | M | see cleanup.rs |
| task/spawn_cmd.rs:212-215,812-819 | "memory/" "<date>.md" "- [{slug}]" "solo override" | path | M | memory layout; same format as memory/standing.rs |
| task/spawn_cmd.rs:250,252,870 | name rule ascii [a-z0-9-] "-lead" suffix | enum | W | naming convention |
| task/spawn_cmd.rs:503,918,1049 | "cos" owner name | enum | M | CoS identity |
| task/spawn_cmd.rs:514,898,872 | "quaqua/{name}" "quaqua/{slug}-integ" "{slug}-lead" | path | M | branch/agent naming |
| task/spawn_cmd.rs:522,900 | default base "HEAD" | enum | K | git ref |
| task/spawn_cmd.rs:560,566 | ".qq/pi-gate.ts" hook comment, install_hook(gate) | path | ? | path inside driver; comment only here |
| task/spawn_cmd.rs:570-578 | sidecar exts repo base review ("1\n") | filename | M | runs/<name>.repo\|base\|review; read by prompt.rs |
| task/spawn_cmd.rs:589,594-598,916-920 | tab "{slug}-workers" tokens grid_role grid_group grid_owner  | enum | K | herdr pane tokens (grid, protocol with herdr); tab suffix M |
| task/spawn_cmd.rs:699-723 | "limits.weights" "default" "roles.lead.max_peers" keys | enum | K | config keys read via setting() |
| task/spawn_cmd.rs:752 | "docs/plans/active/{slug}.md" + "phases_left:" | path | M | workspace plan layout |
| task/spawn_cmd.rs:775 | "strategy.build_kinds" "strategy.lead_kind" | enum | K | config keys |
| task/spawn_cmd.rs:822,895 | messages "memory/" "team.toml" | message | M | cite file names |
| task/spawn_cmd.rs:989 | HERDR_PANE_ID env | env | K | herdr protocol |
| task/spawn.rs:17 | START_TIMEOUT_MS = 60_000 | limit | G | herdr start |
| task/spawn.rs:28-36 | Role lead/peer strings | enum | K | SQL + RPC roles |
| task/spawn.rs:112,124 | "trust" dialog marker | enum | ? | claude trust-dialog text; driver data |
| task/spawn.rs:116 | keys ["down","enter"] to accept trust | enum | ? | claude dialog keys; driver data |
| task/spawn.rs:158-163 | Ready DEFAULT polls 60 poll_ms 500 settle 10x100 | limit | G | trust wait ~30s; no config |
| task/spawn.rs:254 | error code "agent_not_ready" | enum | K | herdr code |
| task/worker_flow.rs:38-48 | KIND "worker_flow", "task.start", exit 10-14, names | enum | K | wire/exit contract |
| task/worker_flow.rs:41 | DEFAULT_TIMEOUT_MS = 2_700_000 | limit | W | dup of prompt.rs DEFAULT_BUDGET_SEC 2700 |
| task/worker_flow.rs:120 | PASSING_LEVELS unit-test-verified type-check-only review-cle | enum | M | verification vocabulary; dup pipeline.rs:798 |
| task/worker_flow.rs:135-169 | Outcome words COMPLETE NO_CHANGES BLOCKED DISPUTED REOPEN_RE | enum | M | third copy; with Verified: field |
| task/worker_flow.rs:181,206,218 | step ids "report" "deadline" "prompt" | enum | K | internal workflow ids |
| task/worker_flow.rs:516 | role "lead or peer" message | message | K | Role enum |

Summary
- Category: enum 41, limit 17, env 6, external CLI 2, filename 6, path 11, message 12, section 9 (total 104 rows).
- Home: K 35, G 13, M 39, ? 6, W 11.
- Top 5 edit-a-rule-edit-Rust churn: prompt.rs brief headers + provider tables (`git log --oneline -- crates/qq-core/src/task/prompt.rs | wc -l` = 16); pipeline_io.rs/pipeline.rs phase briefs, ## Challenge/Findings/Outcome words (16 / 11); worker_flow.rs PASSING_LEVELS + Outcome words, third copy (12); spawn_cmd.rs gate consts, naming, quaqua/ + .wt + sidecars (11); report_contract.rs skeleton + ~/quaqua/runs path (2 commits, but every rule edit to report format must match it).

### Risks
- Outcome words (COMPLETE/NO_CHANGES/BLOCKED/DISPUTED/REOPEN_REQUEST/DEPENDENCY_REQUEST) exist in 3 copies (pipeline.rs:763, worker_flow.rs:135, report_contract.rs:35 lists only 3); moving to a manifest must unify them or gates disagree.
- Exit codes 10-14 mean different things in pipeline.rs and worker_flow.rs; keep as K but do not merge.
- Harness-specific strings (trust dialog, error tables, glyphs) are marked ? : they belong to drivers, not the template repo.
- Engine journal kinds and activity tags are replay wire format: moving them breaks resume of existing journals.

### Challenge
- "Engine hardcodes nothing" cannot cover the K rows (35): wire verbs, SQL, journal kinds, herdr tokens and git verbs. Suggest the rule: the engine hardcodes mechanism, never vocabulary a rule author reads.
- The manifest should own one report-contract section (fields, outcome words, passing levels, severities, headings, brief headers). Today those span prompt.rs, pipeline.rs, worker_flow.rs and report_contract.rs and drift.
- Phase-brief prose (pipeline.rs:277-365) is rule text in Rust: should be templates with {placeholders}, so it counts as M, not code.
- `QUAQUA_PLAYBOOKS_DIR`, `PROMPT_WORKER_*`, `QQ_LEAD_READY_MS`, `QUAQUA_BUSY_TIMEOUT_MS` contradict the "only bootstrap is QUAQUA_CONFIG" rule: fold them into G config.
- `~/quaqua/runs` (report_contract.rs:46) already contradicts the db/path.rs default that can be moved: derive from config.

## Guard lane — authority, memory, daemon, herdr

Source: `~/quaqua/raw/2026-11-20-hermes-cos-hc-guard.md`

### Changes
Read-only scout (lane: authority/memory/daemon/herdr). No source files touched; only this report. Base 408a1e6. No PEER.md. `authority/tokenizer.rs` (351 lines) NOT read: a ninja-kit hook blocked it as "sensitive" and asked for user approval; I did not bypass it via Bash. Its `const|static` grep showed no hits, so it likely holds few policy literals, but it is unaudited. #[cfg(test)] modules skipped (tests pin: gate.rs 36 lines, situation 165, standing 159, threat 285, push 23, liveness 252, daemon/mod 38 lines; message text in gate.rs/authority/mod.rs is pinned by tests).

| file:line | literal | category | home | note |
|---|---|---|---|---|
| crates/qq-core/src/authority/gate.rs:1004-1020 | command table herdr/git/gh/quaqua/sqlite3/sudo/... | enum | K | external CLI syntax |
| crates/qq-core/src/authority/gate.rs:1107,1135,1797-1827 | READ_ONLY, GIT_READ, GIT_NOVAL/VAL/GLUED lists | enum | K | CLI flag syntax; safe-read allowlist is policy but huge |
| crates/qq-core/src/authority/gate.rs:1704 | "release" "create" (gh) | enum | K | external CLI verb |
| crates/qq-core/src/authority/gate.rs:2493-2499 | "usage: quaqua gate"; "quaqua: {msg}" | msg-cites | K | binary name in usage text |
| crates/qq-core/src/authority/gate.rs:388,412,446 | "AGENTS.md" \| "identity.md" (instruction files) | file-name | M | 3 copies; protected_kind, check path, live_kind |
| crates/qq-core/src/authority/gate.rs:39 | BLOCK_EXIT = 2 | numeric | K | Claude Code hook protocol |
| crates/qq-core/src/authority/gate.rs:41 | PANE_MESSAGE "...only through the quaqua daemon (quaqua call | msg-cites | M | names binary+verbs; tests pin text |
| crates/qq-core/src/authority/gate.rs:42-52 | PUSH/MERGE/BRANCH/NOT_COS/NO_CALLER/MEMORY/COS_MEMORY/TIER_D | msg-cites | M | cite USER.md, memory/rules.md, cos-memory tool names; persona "anh iu" |
| crates/qq-core/src/authority/gate.rs:46,52,63 | "anh iu's call" / "anh iu's consent" | msg-cites | M | owner persona name baked in messages; ASK_GRANT too |
| crates/qq-core/src/authority/gate.rs:464 | NAMES ["USER.md","rules.md","AGENTS.md","identity.md"] | file-name | M | 4th copy; duplicates standing.rs/caps.rs lists |
| crates/qq-core/src/authority/gate.rs:54-65 | ASK_* "approval list: ..." (12 consts) | msg-cites | M | approval-list wording; ASK_FILE/ASK_DB name files |
| crates/qq-core/src/authority/gate.rs:683,1518,1519,1562 | "qq.db" basename | file-name | K | SQL db file; but runs/ dir location is G/W (see tier.rs) |
| crates/qq-core/src/authority/gate.rs:71-74 | MCP_OUTWARD regexes: tinyfish/claude_ai_* server+tool names | tool-list | M | policy: which MCP tools send/spend; per-install plugin set |
| crates/qq-core/src/authority/gate.rs:778 | GRANT_VERBS grant.full/tier/lane | enum | K | wire verbs registered in daemon/grant.rs |
| crates/qq-core/src/authority/mod.rs:101-105 | Op names spawn/merge/push/config_write/task_create | enum | K | internal type names |
| crates/qq-core/src/authority/mod.rs:294-390 | edge verbs spawn/mail/prompt/question/report | enum | ? | topology_edge values; DB seeds elsewhere |
| crates/qq-core/src/authority/mod.rs:294-483 | refusal reasons "a Peer can spawn nothing", "one Lead per pr | policy | ? | rules in Rust, not topology data; tests pin text |
| crates/qq-core/src/authority/mod.rs:321 | role = 'lead' SQL literal | enum | K | SQL schema |
| crates/qq-core/src/authority/mod.rs:38,40 | "authority:refusals", "refusal" | enum | K | journal stream/kind keys; SQL-like schema |
| crates/qq-core/src/authority/mod.rs:403 | "roles.{}.can_prompt" setting key | policy | K | config key; already config-driven (not hardcoded policy) |
| crates/qq-core/src/authority/mod.rs:53-65 | Role strings cos/supervisor/lead/peer | enum | ? | Rust enum+db role; roster fixed in code, edges in DB (topology_edge) |
| crates/qq-core/src/authority/push.rs:120 | LS_REMOTE_TIMEOUT 20s | numeric | G | daemon-side timeout; no config knob |
| crates/qq-core/src/authority/push.rs:21 | VERB "push" | enum | K | wire verb |
| crates/qq-core/src/authority/push.rs:294 | "qq-push-{}-{}" scratch prefix | file-name | ? | tmp dir prefix |
| crates/qq-core/src/authority/push.rs:399-427 | deny reasons "branch is the repo's main or default branch" | msg-cites | K | refusal reasons, no file cites |
| crates/qq-core/src/authority/push.rs:48-50 | GIT_CONFIG_NOSYSTEM/GLOBAL/GIT_TERMINAL_PROMPT | env-var | K | git hardening, external CLI |
| crates/qq-core/src/authority/push.rs:93-102 | "main","master" default-branch names | policy | G | trunk names; also gate.rs BRANCH_MESSAGE, tier.rs:248 |
| crates/qq-core/src/authority/tier.rs:113-124 | tier words low/medium/high | enum | ? | also daemon/grant.rs:46-48; tiers fixed in Rust enum |
| crates/qq-core/src/authority/tier.rs:131,171 | runs_dir.join("qq.db") | file-name | G | db path/name; duplicated in situation.rs:113,137 |
| crates/qq-core/src/authority/tier.rs:138-158 | tables repo_tier/plan_lane/grant_full | enum | K | SQL schema |
| crates/qq-core/src/authority/tier.rs:242-248 | "refs/remotes/origin/HEAD", "origin/main","origin/master" | policy | G | remote name + trunk fallback |
| crates/qq-core/src/daemon/caller.rs:18,30-58 | VERB caller.lookup; "cos" compared as string (also grant.rs: | enum | ? | CoS agent name literal "cos" in 5 places; role roster may be W |
| crates/qq-core/src/daemon/grant.rs:101 | notification title "quaqua: grant change"; sub "request" | msg-cites | K | herdr notification |
| crates/qq-core/src/daemon/grant.rs:110,121-167 | "anh iu's notification"; verbs grant.full/tier/lane; kinds f | msg-cites | M | persona name in message |
| crates/qq-core/src/daemon/grant.rs:45-51 | tier low\|medium\|high strings | enum | ? | dup of tier.rs |
| crates/qq-core/src/daemon/memory_write.rs:129,134 | "...{}.bak — reload and retry", "(AGENTS.md '## Memory')" | msg-cites | M | third copy of section quote |
| crates/qq-core/src/daemon/memory_write.rs:143 | memory_journal table | enum | K | SQL |
| crates/qq-core/src/daemon/memory_write.rs:22,52 | VERB memory.write; files allowlist (rules/USER) | enum | M | TARGETS list reused; file set policy |
| crates/qq-core/src/daemon/memory_write.rs:95,126 | ".md.tmp", ".bak" suffixes | file-name | K | impl detail |
| crates/qq-core/src/daemon/mod.rs:104,128 | "agent:{wf}:{agent}", "report:{wf}:{path}" stream keys | enum | K |  |
| crates/qq-core/src/daemon/mod.rs:312-387 | verbs ping/status/stop/workflow.start/signal/watch.agent/wat | enum | K | wire protocol |
| crates/qq-core/src/daemon/mod.rs:632 | uid check message | policy | K | SO_PEERCRED |
| crates/qq-core/src/daemon/pr_merge.rs:141,241 | "gh", GH_PROMPT_DISABLED | env-var | K | external CLI; "gh" binary name could be G |
| crates/qq-core/src/daemon/pr_merge.rs:165-192 | workflow stage names gate/merge/blocked/hold, bucket pass\|s | enum | K | workflow wire + gh JSON |
| crates/qq-core/src/daemon/pr_merge.rs:215 | "full 40-hex sha" | numeric | K | git |
| crates/qq-core/src/daemon/pr_merge.rs:26,28 | VERB "pr.merge", "refused" | enum | K | wire |
| crates/qq-core/src/daemon/pr_merge.rs:296 | "--merge" strategy (no squash option) | policy | G | merge method hardcoded |
| crates/qq-core/src/daemon/pr_merge.rs:31-39 | FENCE_PREFIXES crates/qq-core/src/{authority,identity,merge, | file-name | M | self-protecting path fence; layout of engine repo; churn on every move |
| crates/qq-core/src/daemon/pr_merge.rs:41-44 | FENCE_FILES .claude/settings.json, templates/workspace/AGENT | file-name | M | fence list |
| crates/qq-core/src/daemon/pr_merge.rs:71-78 | diff HEADERS "diff --git ", "rename from "... | enum | K | git diff syntax |
| crates/qq-core/src/daemon/push_door.rs:22 | VERB "push.cos" | enum | K | wire |
| crates/qq-core/src/daemon/push_door.rs:38,63-126 | "only the CoS pushes", "force-push never allowed" | policy | ? | role name cos in code |
| crates/qq-core/src/daemon/store.rs:48-49 | WATCH_STREAM "watch:", REPORT_STREAM "watch-report:" | enum | K | journal stream keys |
| crates/qq-core/src/daemon/supervisor.rs:240-243,343-348 | "completed"/"failed" states | enum | K | DB state |
| crates/qq-core/src/daemon/supervisor.rs:242,262,272 | eprintln "quaqua daemon: ..." | msg-cites | K | binary name in logs |
| crates/qq-core/src/daemon/watch.rs:129 | STALL_SEC 600.0 (duplicates setting notify.stall_sec, env QQ | numeric | G | hardcoded DEFAULT dup of runtime config |
| crates/qq-core/src/daemon/watch.rs:130,132,136,138 | SWEEP_MS 2000, SPAWN_GRACE 30000, EXIT_DEBOUNCE 30.0, SWEEP_ | numeric | G | EXIT_DEBOUNCE/SWEEP dup settings notify.exit_debounce_sec/sweep_ms; SPAWN_GRACE has none |
| crates/qq-core/src/daemon/watch.rs:156 | env "QQ_STALL_SEC" | env-var | G | env name hardcoded |
| crates/qq-core/src/daemon/watch.rs:160-176 | setting keys notify.stall_sec/exit_debounce_sec/sweep_ms | env-var | K | config keys (already config) |
| crates/qq-core/src/daemon/watch.rs:276-293 | "working"/"blocked"/"done" state-mail mapping | enum | K |  |
| crates/qq-core/src/daemon/watch.rs:29-33 | REPORT_POLL 200ms, WAIT_TIMEOUT 30s, BACKOFF 100ms/5s | numeric | G | poll/backoff |
| crates/qq-core/src/daemon/watch.rs:33 | STATUSES idle/working/blocked/done | enum | K | herdr status vocabulary (dup of herdr/liveness.rs:26-41) |
| crates/qq-core/src/herdr/fake.rs:173,479-563 | fake status/err codes idle/done/blocked, agent_not_ready, ag | enum | K | test double mirroring herdr protocol |
| crates/qq-core/src/herdr/fake.rs:79-93,805 | workspace "w1"/"main", ids w/t/p, cwd "/work" | file-name | K | test fake compiled into lib; non-cfg(test); not real config |
| crates/qq-core/src/herdr/liveness.rs:185 | runs_dir.join(".{name}.state") | file-name | G | runtime state file naming |
| crates/qq-core/src/herdr/liveness.rs:218-297 | JSON pointers /result/agent/agent_status, "agent_not_found" | enum | K | herdr protocol |
| crates/qq-core/src/herdr/liveness.rs:26-44 | state strings idle/blocked/working/done/dead\|gone/unknown | enum | K | herdr vocabulary; dup watch.rs:33 |
| crates/qq-core/src/herdr/liveness.rs:98-119 | state file keys state/ts/timestamp, "state=", "ts=" | enum | K | file format |
| crates/qq-core/src/herdr/mod.rs:13,16 | PANE_BUSY "pane_busy", NAME_CLASH "agent_name_clash" | enum | K | herdr error codes |
| crates/qq-core/src/herdr/mod.rs:264-267 | env HERDR_BIN_PATH, default "herdr" | env-var | G | env name + binary default |
| crates/qq-core/src/herdr/mod.rs:298-488 | argv: agent/pane/tab/workspace verbs, --kind --pane --timeou | enum | K | herdr CLI syntax |
| crates/qq-core/src/herdr/mod.rs:35-56 | prompt landing: states [working,blocked], 10_000 ms, single  | numeric | G | 10s timeout x3; retry policy hardcoded |
| crates/qq-core/src/herdr/mod.rs:438 | "--source recent-unwrapped" | enum | K | herdr CLI |
| crates/qq-core/src/memory/caps.rs:150-152,184,223 | hook JSON keys tool_name/file_path/hookEventName/PreToolUse/ | enum | K | Claude Code hook protocol |
| crates/qq-core/src/memory/caps.rs:186,209 | "(AGENTS.md '## Memory')" | msg-cites | M | quoted section name; user-listed example |
| crates/qq-core/src/memory/caps.rs:234 | WRITERS Bash/Write/Edit/MultiEdit/NotebookEdit | tool-list | K | Claude Code tool names; duplicated fence.rs:39 |
| crates/qq-core/src/memory/caps.rs:257,272,280 | AGENTS.md is_file \|\| ".quaqua" is_dir (workspace-root prob | file-name | M | root marker files; 3 copies; also fence.rs:50, journal.rs:24 |
| crates/qq-core/src/memory/caps.rs:265 | UNCAPPED_STANDING identity.md, projects/INDEX.md | file-name | M | duplicates standing.rs SECTIONS |
| crates/qq-core/src/memory/caps.rs:27 | RULES_CAP = 2200 | numeric | M | manifest caps; 5 commits churn |
| crates/qq-core/src/memory/caps.rs:29 | USER_CAP = 1375 | numeric | M | manifest caps |
| crates/qq-core/src/memory/caps.rs:338-350 | TOKEN_PREFIXES sk-ant-/ghp_/xoxb-/AIza... + min lens | enum | M | secret scanner patterns; vendor formats; or K |
| crates/qq-core/src/memory/caps.rs:34 | LINE_CAP = 200 | numeric | M | caps; tests pin |
| crates/qq-core/src/memory/caps.rs:354-367 | SECRET_KEYS api_key/password/token... | enum | M | scanner policy list |
| crates/qq-core/src/memory/caps.rs:372 | SECRET_VALUE_MIN = 16 | numeric | M | scanner threshold |
| crates/qq-core/src/memory/caps.rs:44-50 | TARGETS "memory/rules.md","USER.md" | file-name | M | capped-file list; mirrors standing.rs SECTIONS and gate.rs |
| crates/qq-core/src/memory/caps.rs:79 | "{chars} chars > {cap} cap — merge or drop entries" | msg-cites | M | guidance wording |
| crates/qq-core/src/memory/drift.rs:100,153 | "Context was compacted...", "Rule files changed... (issue #7 | msg-cites | M | instruction text + issue number |
| crates/qq-core/src/memory/drift.rs:106-113 | "reports" dir, "audit-" prefix | file-name | M | audit report naming; also situation.rs:101-103 |
| crates/qq-core/src/memory/drift.rs:11 | REREAD AGENTS.md, memory/rules.md, projects/INDEX.md | file-name | M | compact re-read list; 5th copy of file list |
| crates/qq-core/src/memory/drift.rs:142-146 | "quaqua" binary segment, "worker"\|"lead" subcommands | enum | ? | detects spawn commands; K-ish but couples to CLI names |
| crates/qq-core/src/memory/drift.rs:16-26 | "team.toml","AGENTS.md","rules/_all" dir, ".md" ext | file-name | M | rules snapshot set; derive from manifest |
| crates/qq-core/src/memory/drift.rs:52,62 | "runs/.sessions", "{sid}.json" | file-name | G | runtime state dir |
| crates/qq-core/src/memory/drift.rs:88 | `**f == "AGENTS.md"` (block-only rule file) | file-name | M |  |
| crates/qq-core/src/memory/fence.rs:123-127 | ".git" exists | file-name | K | git layout |
| crates/qq-core/src/memory/fence.rs:29 | FENCED_DIRS crates, tests, .github | file-name | M | CoS no-edit zones; repo layout policy |
| crates/qq-core/src/memory/fence.rs:30-31 | REASON "...(AGENTS.md '## Coordinator Rules')" | msg-cites | M | section name quoted; user-listed pattern |
| crates/qq-core/src/memory/fence.rs:33 | MEMORY_REASON (cos-memory tools names) | msg-cites | M | dup of gate.rs:51 text verbatim |
| crates/qq-core/src/memory/fence.rs:39 | tool match Write\|Edit\|MultiEdit\|NotebookEdit | tool-list | K | Claude Code tool names |
| crates/qq-core/src/memory/fence.rs:50 | cwd.join("projects/INDEX.md").is_file() | file-name | M | CoS-workspace detection marker |
| crates/qq-core/src/memory/inbox.rs:127-128 | "New lead mail received", hookEventName | msg-cites | ? | message text policy; hook protocol part K |
| crates/qq-core/src/memory/inbox.rs:144-145 | ".sessions", "{id}.inbox-down" | file-name | G | runtime state |
| crates/qq-core/src/memory/inbox.rs:152 | "daemon not running"/"rpc failed:" prefix match | enum | K | string-coupled error detection (fragile) |
| crates/qq-core/src/memory/inbox.rs:19,23 | MAX_LINES 20, PAGE 100 | numeric | G | inbox render caps |
| crates/qq-core/src/memory/inbox.rs:21,103 | DAEMON_DOWN msg, "quaqua inbox --all" | msg-cites | K | binary name |
| crates/qq-core/src/memory/inbox.rs:35-63 | CLI flags/formats plain\|hook-ups\|hook-stop | enum | K | own CLI |
| crates/qq-core/src/memory/inbox.rs:78-111 | rpc verbs mail.inbox/mail.ack | enum | K | wire verbs |
| crates/qq-core/src/memory/journal.rs:155-175 | "raw" dir, {today}-{slug}-{task}.md | file-name | M | raw capture naming |
| crates/qq-core/src/memory/journal.rs:211 | runs/{name}.md report path | file-name | G | runs dir layout |
| crates/qq-core/src/memory/journal.rs:221-248 | "- [slug] name cleanup:" line formats, "raw/" | msg-cites | M | journal line grammar; situation.rs parses "- [audit]" |
| crates/qq-core/src/memory/journal.rs:24,109 | root "memory" dir; memory/{today}.md | file-name | M | daily journal path |
| crates/qq-core/src/memory/journal.rs:256-260 | exit code->cause 3 stalled,4 budget,5 blocked,7 provider,8 c | enum | K | exit codes of engine; text could be M |
| crates/qq-core/src/memory/journal.rs:270 | "- [case] {name} exit {code}: {cause}" | msg-cites | M | journal grammar |
| crates/qq-core/src/memory/journal.rs:59 | projects/INDEX.md; "active" status col | file-name | M | parses INDEX table format |
| crates/qq-core/src/memory/journal.rs:90 | "Outcome:" report line prefix | enum | M | report schema (task skeleton) |
| crates/qq-core/src/memory/mod.rs:35-41,69-93 | subcommand names memory hook pre\|batch\|session-start; inbo | enum | K | own CLI |
| crates/qq-core/src/memory/situation.rs:113,137 | "runs/qq.db" | file-name | G | dup of tier.rs db path |
| crates/qq-core/src/memory/situation.rs:154 | "memory/{today}.md" | file-name | M | dup of journal.rs |
| crates/qq-core/src/memory/situation.rs:165,217 | ".quaqua/pending" dir | file-name | M | pending-copy dir |
| crates/qq-core/src/memory/situation.rs:17,19 | NOW_CAP 600, FILL_WARN 80 | numeric | M | caps/thresholds |
| crates/qq-core/src/memory/situation.rs:181-183 | 86_400/3_600 duration formatting | numeric | K | unit math |
| crates/qq-core/src/memory/situation.rs:196-229 | advice strings: `- [audit] ...`, memory_batch, `quaqua statu | msg-cites | M | instruction text in Rust; 4 commits churn |
| crates/qq-core/src/memory/situation.rs:21 | NOW_HEADER "## Now (computed)" | section | M | section name; cited by templates |
| crates/qq-core/src/memory/situation.rs:90-106 | "memory" dir, ".md", "- [audit]" prefix, "reports/audit-" | file-name | M | audit handled-detection; couples to journal.rs grammar |
| crates/qq-core/src/memory/standing.rs:106 | "shorten identity.md, USER.md or memory/rules.md" | msg-cites | M | file list in message |
| crates/qq-core/src/memory/standing.rs:11 | BUDGET = 8000 | numeric | M | manifest cap |
| crates/qq-core/src/memory/standing.rs:15-19 | SECTIONS persona/owner/rules/projects -> identity.md, USER.m | file-name | M | user-named example; standing order |
| crates/qq-core/src/memory/standing.rs:22,92 | HEADER "# Standing context" + sentence "these outrank the gl | section | M | rendered preamble |
| crates/qq-core/src/memory/standing.rs:23,99 | INDEX "projects/INDEX.md" | file-name | M |  |
| crates/qq-core/src/memory/standing.rs:48 | "# <Owner name>" template sentinel | section | M | must match seed body in init/seeds.rs |
| crates/qq-core/src/memory/standing.rs:56 | path == "USER.md" template check | file-name | M |  |
| crates/qq-core/src/memory/threat.rs:133-137 | FILLER {0,8}, SECRET_VAR, MODIFY {0,2048} | numeric | K | regex internals |
| crates/qq-core/src/memory/threat.rs:185-230 | threat table ~45 patterns & ids (prompt_injection ... hardco | enum | M | tunable policy; ids pinned by tests |
| crates/qq-core/src/memory/threat.rs:211-226 | env prefixes CLAUDE\|CODEX\|HERMES\|OPENAI\|ANTHROPIC; c2 na | tool-list | M | vendor/product names |
| crates/qq-core/src/memory/threat.rs:225 | agent_config_mod: AGENTS.md\|CLAUDE.md\|.cursorrules\|.cline | file-name | M | instruction-file names (dup of gate.rs) |
| crates/qq-core/src/memory/threat.rs:251 | negator regex | enum | K | NLP internal |
| crates/qq-core/src/memory/threat.rs:28,30 | MAX_SCAN_CHARS 65_536, CHUNK_OVERLAP 4096 | numeric | G | scanner limits |
| crates/qq-core/src/memory/threat.rs:371-373 | "Blocked: ... injected into the system prompt ..." | msg-cites | M | message |
| crates/qq-core/src/memory/threat.rs:47 | invisible code ["gbeng","gbsct","gbwls"] | enum | K | unicode tag block |

#### Summary
- Per category: enum 46, file-name 34, msg-cites 22, numeric 17, policy 7, env-var 5, tool-list 4, section 3
- Per home: ? 10, G 18, K 55, M 55 (total 138 rows; some rows group many literals)
- Top-5 edit-a-rule-edit-Rust churn: (1) authority/gate.rs msgs + AGENTS/identity lists, 24 commits; (2) herdr/mod.rs 10s landing policy + CLI argv, 16 commits; (3) authority/mod.rs refusal reasons/edges, 7; (4) memory/caps.rs RULES_CAP/USER_CAP/TARGETS/'## Memory' quote, 5; (5) daemon/pr_merge.rs FENCE_PREFIXES/FILES (any engine dir rename), 5 (tie: memory/standing.rs SECTIONS+BUDGET, memory/situation.rs advice text, memory/drift.rs REREAD, 4 each). Counts via `git log --oneline -- <file> | wc -l`. gate.rs's high count is mostly parser fixes, not rule edits, so its policy-text share is smaller.

### Challenge
1. **The file list is copied 7+ times, not once.** identity.md/USER.md/rules.md/AGENTS.md/projects/INDEX.md appear in gate.rs (3 places), caps.rs (3), drift.rs, standing.rs, fence.rs, journal.rs, situation.rs, threat.rs. The manifest must expose roles (e.g. `instruction`, `memory.capped`, `standing`, `private`) that these consumers query. Do not give each consumer its own list in the manifest.
2. **Agent name `"cos"` is compared as a string in 5 places** (caller.rs, grant.rs, memory_write.rs, pr_merge.rs, push_door.rs), and the owner persona "anh iu" is embedded in about 8 messages. Both belong to the manifest/persona or a W role roster. Role enum names cos/supervisor/lead/peer are marked `?`: the DB topology_edge already holds edges, but the refusal rules in authority/mod.rs (one Lead per project, Peer mails only its spawner) are policy written in Rust.
3. **Section-name quotes are not just messages.** `## Now (computed)` (NOW_HEADER) and `# <Owner name>` (standing.rs:48) are parse/render sentinels that must match seed bodies in init/seeds.rs. They need one manifest source with the seed body generated from it, or drift is guaranteed.
4. **Messages that cite instruction files are the weakest fit for a remote manifest.** The gate/fence hooks run on every tool call. Fetching or caching remote manifests there makes a hook failure mode (network or stale cache) land on the safety path. Recommend: resolve the manifest at `init`/`sync` into a local snapshot (e.g. `.quaqua/manifest.lock`), and have hooks read only that snapshot, failing closed with an engine-built-in minimal fence if it is missing.
5. **The threat-pattern table and secret/token prefixes should be M (data), but the engine must keep a floor.** If a remote manifest could drop `prompt_injection`/`hardcoded_secret`, a bad template repo disables the scanner. Keep the engine built-in set as an un-removable baseline; manifest adds only. Same for pr_merge.rs FENCE_PREFIXES: it fences the very hook/gate files the manifest would define, so manifest-defined fences must union with, not replace, the engine core.
6. **Defaults duplicating config (flagged per brief):** watch.rs STALL_SEC/EXIT_DEBOUNCE_SEC/SWEEP_MS duplicate `notify.*` settings and `QQ_STALL_SEC`; the defaults and env name still live in Rust. tier.rs/grant.rs repeat low/medium/high.
7. **Not a hardcode problem, do not move:** herdr CLI argv, herdr error codes/status vocabulary (watch.rs:33 and liveness.rs duplicate each other), wire verbs, SQL tables, Claude Code hook JSON/tool names (fence.rs/caps.rs duplicate the writer-tool list), git/gh syntax. herdr/fake.rs is a test double inside non-test code; ignore it.

### Risks
- tokenizer.rs unread (hook block); gate.rs hand-triaged by pattern, so a policy literal inside a long function body may be missed.
- Line numbers are base 408a1e6; other lanes' edits will shift them.
- Row counts group literal families, so they undercount raw literals.

## Rest lane — config, auditor, init, grid, bus, db, driver, playbook, identity, merge, cli, rpc and others

Source: `~/quaqua/raw/2026-11-20-hermes-cos-hc-rest.md`

### Changes
Read-only scout, lane "rest" (config, auditor, init, grid, bus, db, driver, playbook, identity, merge, transcripts, cli, rpc, status, testkit, run, lib.rs, main.rs). No repo file edited. Base 408a1e6. PEER.md absent. `#[cfg(test)]` code skipped; tests pin: init/table.rs (`table_matches_directory`), init/mod.rs (AGENTS.md accept/pending), config validation, auditor precheck fixtures. Excluded (other lanes): memory/, task/, authority/, daemon/, engine/, herdr/.

| file:line | literal (≤60 chars) | category | home G/W/M/K/? | note |
|---|---|---|---|---|
| auditor/headless.rs:18 | "claude" => -p --permission-mode dontAsk --allowedTools Read | tool-list | M | headless auditor argv per harness; allowlist is policy; harness arm should live in harness spec |
| auditor/mod.rs:113,120,159 | AUDITOR_REPO / AUDITOR_OUT / AUDITOR_MODEL | env | G | env overrides; model env duplicates team.toml kind model |
| auditor/mod.rs:122 | root.join("reports").join("audit-<date>.md") | path | M | output dir/name; also quoted in help line 33 |
| auditor/mod.rs:135 | root.join("team.toml") | filename | G | also precheck/config/testkit; location of team config |
| auditor/mod.rs:182-198 | "## Drift/Stale docs/Unhandled/Pipeline/Workers" + substring | section | M | report schema; prompt.rs:67 repeats; routing keys on message text |
| auditor/mod.rs:184-198 | f.contains("doc-rot risk"/"expired rule"/"lost momentum"…) | message | M | router matches precheck message strings; stringly coupled |
| auditor/mod.rs:23,78 | --out/--model/--kind/--repo flags + USAGE | cmd | K | CLI syntax; USAGE duplicates flag list |
| auditor/mod.rs:26 | PURPOSE = "review" | kind-enum | W | auditor reads team.toml purposes.review; name pinned in code |
| auditor/precheck.rs:18,650 | root.join("memory") | path | M | private dir name; PRIVATE_DIRS duplicate |
| auditor/precheck.rs:194,360 | root.join("runs") / "runs/", "raw/" (139) | path | M | private dirs; also in db/path.rs:163, init/mod.rs:43 |
| auditor/precheck.rs:210,433,171,411 | "pipeline-" prefix, ".md", ".rules.md" suffix | filename | M | run-file naming convention |
| auditor/precheck.rs:223-237 | "- status: ", "- started: ", "- task: ", " · repo: " | section | M | run-file line format; bare-bones parser of report skeleton |
| auditor/precheck.rs:256,263,268 | "disputed"\|"blocked"\|"clean" statuses; "Verdict:"; "review | list-enum | M | Outcome vocabulary from playbook skeleton (338,364) |
| auditor/precheck.rs:422,613,619 | "## Challenge", "## Lessons" | section | M | also messages :463,:466,:643 |
| auditor/precheck.rs:450-455 | ".plan" ".build" ".fix" run-name suffixes | list-enum | M | slug naming scheme |
| auditor/precheck.rs:481,497,506 | rules/, projects/, memory/rules.md | path | M | audit scan roots |
| auditor/precheck.rs:516 | n > 100 lines (doc-rot) | numeric | M | duplicates memory RULES_CAP idea; message repeats ">100" |
| auditor/precheck.rs:555-563,265,321,372,398 | message text citing memory/YYYY-MM-DD.md, runs/*.md, '## Cha | message | M | ~12 flag strings cite workspace files |
| auditor/precheck.rs:58 | PIPELINE_CUTOFF_DAYS = 14 | numeric | M | audit horizon; also 14d at :669 |
| auditor/precheck.rs:599 | 7 * 86_400 unpromoted Lessons age | numeric | M | message at :643 repeats "7 days" |
| auditor/precheck.rs:669 | (today - f_days) < 14 recurring window | numeric | M | "14d" quoted in message :691 |
| auditor/precheck.rs:673,691 | "- [case] " memory line marker | section | M | memory file format |
| auditor/precheck.rs:734,740 | runs/qq.db; SELECT … recipient = 'cos' | path | K | SQL schema ok; "cos" role and file name are policy (db/path.rs DB_FILE) |
| auditor/precheck.rs:763,773,780,784 | mail kinds "accept"\|"handoff"; "outcome:" blocked/disputed | list-enum | M | mail kind vocabulary |
| auditor/precheck.rs:801 | 86_400 lead lost-momentum | numeric | M | message :816 says "24h" |
| auditor/prompt.rs:3-70 | PROMPT_TEMPLATE: AGENTS.md, memory/rules.md, team.toml, ## L | message | M | whole prompt is policy prose naming files/sections; 2 commits |
| bus/cli.rs:15,25 | mail send usage; --to/--kind/--body-file/--client-id | cmd | K | CLI syntax |
| bus/inbox_md.rs:20-23 | to != "cos" \|\| from.ends_with("-lead"); "{from}.inbox.md" | list-enum | W | role names + naming suffix; ROLES in config/mod.rs:20 duplicates |
| bus/inbox_md.rs:24,34,51 | "<!-- msg_key: mail:{id} -->", "## {ts} {from} {kind}" | section | M | inbox.md file format; escape rule at :51 |
| bus/mod.rs:157 | body.chars().count() > 4096 | numeric | G | message at :158 repeats 4096 |
| bus/mod.rs:164 | default kind "note" | list-enum | M | mail kind default |
| bus/mod.rs:199,229,282 | DAEMON = "daemon"; kind "state"; recipient "cos" | list-enum | W | role/kind names; also 391 |
| bus/mod.rs:213-216,371 | "stalled\|blocked\|done\|exited"; escalate\|blocked push kin | list-enum | M | state words and which mail kinds notify |
| bus/mod.rs:235,407 | "quaqua: {agent} {state}", "new {kind} mail from {from}; rea | message | M | prompt text typed into a worker pane |
| bus/mod.rs:241 | WHY_MAX = 200 | numeric | G | notification reason cap |
| bus/mod.rs:278 | notification_show(.., "request") | list-enum | K | herdr arg |
| bus/mod.rs:404 | idle\|done agent_status gate | list-enum | K | herdr status wire |
| bus/mod.rs:58 | MAX_PAGE = 100 | numeric | G | inbox page cap |
| bus/mod.rs:80 | WORKER_PREFIX = "worker:" | list-enum | K | mailbox alias; wire form |
| cli/mod.rs:153 | QQ_AGENT | env | K | env set by spawn; contract with task/spawn.rs |
| cli/mod.rs:67 | "{:<8}" | numeric | K | help column width; cosmetic |
| cli/mod.rs:97 | MODULES install list | cmd | K | command registry; Rust code |
| config/mod.rs:116,101-116 | error messages citing team.toml | message | M | cites file name |
| config/mod.rs:20 | ROLES = cos,supervisor,lead,peer | list-enum | W | duplicates team.toml [roles]; also bus/inbox_md.rs, grid/arrange.rs |
| config/mod.rs:25,31,34,37 | MAPPED_TOP, NOT_PORTED "taskforce", ROLE_KEYS, SETTING_SECTI | list-enum | M | team.toml schema; every new section is a Rust edit; 7 commits |
| config/mod.rs:325-331 | strategy.default_reviewer special-case | list-enum | ? | schema key special-cased |
| config/mod.rs:455 | placeholders model\|effort\|rules_file\|prompt | list-enum | K | arg template grammar (driver/generic.rs:32-45) |
| config/mod.rs:475 | name == "omp" reserved | list-enum | ? | harness name hardcoded as reserved |
| config/mod.rs:488 | stop_keys must never send ctrl+c | list-enum | ? | policy; also driver/mod.rs:104, pi.rs:61-64 |
| config/mod.rs:493,525 | error_shape; spawn auto\|approve; on_cost_cap warn\|kill | list-enum | K | validation enums; ErrorShape in driver/mod.rs:41-44 |
| config/mod.rs:56 | "native"\|"generic" driver | list-enum | K | Rust driver kinds |
| config/verbs.rs:122,134 | FIELDS (harness…avoid), NULLABLE | list-enum | K | mirrors SQL kind columns; also config/mod.rs:702 KIND_COLS |
| config/verbs.rs:24,107,183 | config usage strings (3 copies) | cmd | K | CLI syntax; duplicated three times |
| config/verbs.rs:417 | "owner" pseudo-role | list-enum | W | hard-coded role target |
| config/verbs.rs:459 | roles.*.can_prompt\|reports_to\|max_peers | list-enum | M | duplicates ROLE_KEYS |
| db/migrate.rs:12-19 | LADDER 0001..0007 .sql | protocol | K | SQL schema |
| db/mod.rs:49 | BUSY_TIMEOUT_MS = 5000 | numeric | G | db/old.rs:151 repeats 5000 |
| db/old.rs:319-326 | SIDECARS owner,grid,repo,pipeline,review,base,layout_role | list-enum | ? | legacy import; file suffix list |
| db/path.rs:163 | workspace_root().join("runs") | path | M | runs dir name |
| db/path.rs:21,109 | h.join("quaqua"), "quaqua/runs" default home | path | G | ruled example; default workspace location |
| db/path.rs:36-38,123,160 | QUAQUA_HOME, QUAQUA_ROOT, QUAQUA_TEST, QUAQUA_RUNS_DIR, CARG | env | G | bootstrap env set; keep QUAQUA_CONFIG only |
| db/path.rs:8-9 | DB_FILE "qq.db", OLD_DB_FILE "quaqua.db" | filename | K | state db name; precheck.rs:734 + testkit:118 duplicate |
| driver/claude.rs:10,12 | HOOK_REL ".claude/settings.local.json", MARKER "# qq-gate" | filename | K | claude CLI file; marker is engine |
| driver/claude.rs:17-21 | GATE_MATCHER: Bash\|Write\|Edit\|MultiEdit; mcp__plugin_tiny | tool-list | M | tool/MCP name list; most churn-prone; 6 commits |
| driver/claude.rs:25-28 | "claude-" prefix; model.replace('.',"-") | model | K | id spelling rule for claude CLI |
| driver/claude.rs:34,38 | herdr kind "claude" | list-enum | K | herdr wire |
| driver/claude.rs:51-53 | --model, --effort | cmd | K | external CLI syntax; should be harness args_template |
| driver/mod.rs:205-208 | git rev-parse --git-path info/exclude | cmd | K | git verb |
| driver/mod.rs:247 | ATTEMPTS = 3 agent_start retries | numeric | G | no config knob |
| driver/mod.rs:41-44 | "claude\|pi\|agy\|opencode" error_shape | list-enum | K | Rust enum spelling |
| driver/pi.rs:16,21-30 | HOOK_REL ".qq/pi-gate.ts" + TS gate source (toolName "bash") | filename | M | embedded script body; pi tool name |
| driver/pi.rs:51-55 | --model --thinking -e | cmd | K | external CLI; should be harness args_template |
| driver/pi.rs:61-64 | stop keys esc, ctrl+c | list-enum | W | duplicates harness stop_keys in team.toml |
| driver/pi.rs:94-105 | jsonl keys message.role, usage.input/output/cacheRead… | protocol | K | pi transcript wire |
| grid/arrange.rs:14 | DEFAULT_DEFERRED ["working"] | list-enum | W | duplicates setting grid.deferred_statuses default (:259) |
| grid/arrange.rs:55-67 | roles "worker"\|"lead"; "{group}-lead" | list-enum | W | role + naming scheme |
| grid/lock.rs:10,11 | WAIT 15s, POLL 10ms | numeric | G | lock timing |
| grid/lock.rs:30,35 | "grid.lock", "pid" | filename | K | engine-private state |
| grid/mod.rs:110 | QUAQUA_OWNER | env | K | env contract with spawn |
| grid/mod.rs:12 | PER_TAB = 4 | numeric | W | panes per tab; no grid.* setting |
| grid/mod.rs:17-19 | "{repo}-workers", "-workers-{n}" | path | W | tab label scheme |
| grid/mod.rs:407-418,549-556 | tokens grid_role/owner/farm/tab/group; source "quaqua"; role | protocol | K | herdr pane token keys; also identity/*.rs |
| grid/mod.rs:565,679,692,697 | grid/lead usage strings | cmd | K | CLI syntax |
| identity/mod.rs:108-124 | role "cos", owner "cos", grid_* tokens | list-enum | W | role name; herdr token keys K |
| identity/mod.rs:131,145-151 | ".wt", projects/INDEX.md, identity.md, .cos/… | filename | M | workspace-detection heuristics; names template files |
| identity/mod.rs:156 | HERDR_PANE, HERDR_PANE_ID | env | K | herdr env |
| identity/mod.rs:19,21 | cos.identity.json, TOKEN_SOURCE "herdr-quaqua" | filename | K | engine state file / herdr source tag |
| identity/mod.rs:221,253 | ws_root.join("runs") | path | M | runs dir |
| identity/peer.rs:23 | MAX_DEPTH = 64 | numeric | K | ppid walk guard |
| identity/peer.rs:26,20 | FOREIGN_UID, UNMAPPED_PEER | protocol | K | rpc error codes |
| identity/peer.rs:283 | CallerId("cos") | list-enum | W | role literal; peer.rs:268 "cos" token |
| init/hooks.rs:10-72 | PRE_COMMIT_TEMPLATE (bash) incl. "rules/_all/portable-paths. | message | M | hook body in Rust; cites rule file |
| init/hooks.rs:108-118 | "quaqua-hooks" dir, ".git/quaqua-hooks" | path | K | git internals location |
| init/mod.rs:117,191,252,306 | .quaqua/template.lock, .quaqua/pending/ | path | K | engine bookkeeping; names quoted in messages |
| init/mod.rs:324-342 | SETTINGS_REL .claude/settings.json; memory hook PreToolUse " | tool-list | M | hook wiring should come from manifest (template settings.json) |
| init/mod.rs:326 | MARKER "# qq-managed" | filename | K | merge marker |
| init/mod.rs:43 | PRIVATE_DIRS memory,projects,raw,runs,reports | path | M | ruled example; also auditor/precheck, db/path |
| init/mod.rs:44 | ENGINE = "quaqua" | list-enum | G | engine name |
| init/seeds.rs:5-51 | SEEDS: memory/rules.md, projects/INDEX.md, projects/_templat | filename | M | ruled example; 2 commits |
| init/table.rs:1-274 | TABLE: every templates/workspace file embedded via include_b | filename | M | ONE finding; test table_matches_directory pins; 8 commits |
| lib.rs / main.rs | no literals | - | - | none |
| merge/mod.rs:115-118 | GIT_AUTHOR/COMMITTER "qq" / "qq@local" | list-enum | G | git identity |
| merge/mod.rs:169,177 | INTEG_CHECK_CMD "cargo check --workspace --locked"; "-integ" | cmd | G | duplicates setting merge.check (:370); integ naming scheme |
| merge/mod.rs:244 | "qq-merge-{}-{}" tmp dir | path | K | scratch name |
| merge/mod.rs:320 | default remote "origin" | list-enum | G | git remote |
| merge/mod.rs:340 | message "the daemon's merge.check setting" | message | K | cites setting |
| merge/mod.rs:72-76 | codes merge_failed\|check_failed\|base_broken\|merge_conflic | protocol | K | rpc codes |
| playbook/mod.rs:160,195,315,500 | "principle-" prefix | list-enum | M | citation grammar |
| playbook/mod.rs:391,100 | "prove-it-works" exempt leaf | list-enum | M | principle name hardcoded |
| playbook/mod.rs:398 | LeafCapExceeded max: 3 | numeric | M | message :100 names principle-prove-it-works; also :391 |
| playbook/mod.rs:46,276-280,306-308 | "## Orchestrator" / "## Worker"; frontmatter name:, when: | section | M | playbook schema; errors :88,:89 |
| playbook/mod.rs:482,430-436,579-581 | "playbooks", "principles" dirs; QUAQUA_PLAYBOOKS_DIR / QUAQU | path | M | template dir names; env duplicates |
| rpc/mod.rs:127 | "/tmp/qq-{h}.sock" fallback | path | G | tmp socket location |
| rpc/mod.rs:14,18 | SOCKET_FILE "qq.sock", MAX_SOCKET_PATH 100 | filename | K | OS limit |
| rpc/mod.rs:151 | read timeout 30s | numeric | G | rpc timeout |
| rpc/mod.rs:21,23 | unknown_verb, bad_request | protocol | K | wire codes |
| run/mod.rs:68 | run_artifact kind 'base' | protocol | K | SQL |
| status/mod.rs:94 | "running", "${c:.2}" | message | K | cosmetic |
| testkit/mod.rs:12-28,57 | HERDR_* env list; QUAQUA_TEAM_TOML | env | K | test-only; QUAQUA_TEAM_TOML is a real override that should fold into QUAQUA_CONFIG |
| transcripts/mod.rs:13,59-61 | DEFAULT_LAST 10; ~/.claude/projects | numeric | G | default; harness transcript dir (claude-specific path) |
| transcripts/mod.rs:93 | "{arg}.jsonl" | filename | K | claude transcript naming |
| transcripts/parse.rs:118-171 | "type","message","isSidechain" | protocol | K | claude jsonl schema |
| transcripts/parse.rs:97-107 | INJECTED_PREFIXES (<system-reminder>, <task-notification>…) | list-enum | G | Claude harness markers; list grows with Claude Code releases |

Summary
- Per category (123 rows): list-enum:35 numeric:17 path:15 filename:13 cmd:9 message:8 protocol:8 env:6 section:6 tool-list:3 kind-enum:1 model:1 -:1
- Per home: K:45 M:43 G:18 W:12 ?:4 -:1 (K = keep in code with reason; ? = doubtful: config/mod.rs:325 default_reviewer special-case, :475 reserved "omp", :488 ctrl+c ban, db/old.rs:319 SIDECARS)
- Top 5 rule-edit→Rust churn (git log --oneline -- <file> | wc -l): init/table.rs 8; driver/claude.rs 6 (GATE_MATCHER tool list); init/mod.rs 6 (PRIVATE_DIRS, hook wiring, messages); config/mod.rs 7 (ROLES/SETTING_SECTIONS schema); playbook/mod.rs 5 (leaf cap 3, section names). Not churn but biggest hidden coupling: auditor/precheck.rs (1 commit, ~25 literals) + auditor/prompt.rs (2): it parses run-file/memory formats from string matches.

### Challenge
1. Offline/first-run bootstrap: init, identity detection (identity/mod.rs:145-151), the pre-commit hook (init/hooks.rs) and the auditor run with no network or before any template exists. A remote-only template breaks `quaqua init` offline and the gate hook on a flaky fetch. Keep `include_bytes!` TABLE as the fallback/pinned snapshot (generated from the manifest at build) rather than deleting it.
2. "Hardcodes nothing" cannot cover hot-path numerics (rpc timeout 30s, grid lock 15s, busy_timeout 5000, MAX_SOCKET_PATH). Give them code defaults plus optional [limits]/[timers] override in team.toml (the SETTING_SECTIONS already exist), not manifest fetches. Config-resolved at startup, never per call.
3. The auditor is the real schema consumer: precheck.rs encodes run-file format ("- status:", "Verdict:", "## Challenge", ".plan/.build/.fix") and mail kinds. That belongs in the manifest as a `[formats]` block, or the precheck should ship as a template script; otherwise changing the playbook skeleton silently breaks the auditor.
4. Several duplicates already exist, so fix those first, cheap and safe: grid DEFAULT_DEFERRED vs grid.deferred_statuses; INTEG_CHECK_CMD vs merge.check; pi.rs stop keys vs harness stop_keys; ROLES vs team.toml [roles]; AUDITOR_MODEL vs kind model; QUAQUA_TEAM_TOML vs the planned QUAQUA_CONFIG; usage text repeated 3x in config/verbs.rs; claude/pi argv (--model/--effort/--thinking) vs harness args_template.
5. K list is right: herdr token keys, rpc/merge codes, SQL, LADDER, claude jsonl keys, git verbs.

### Risks
Churn counts are low overall (repo history recent); counts are by file, not by literal. Line numbers refer to base 408a1e6. Rows grouped (e.g. precheck messages) may undercount strings. Some homes (M vs W) depend on how the manifest is scoped.

## Ext lane — plugins, mods, workspace scripts, settings, CI, Cargo

Source: `~/quaqua/raw/2026-11-20-hermes-cos-hc-ext.md`

### Changes
None (read-only scout). Inventory of 91 findings, scope: plugins/, mods/, templates/workspace/scripts/, .claude/settings.json, tests/*.sh, .github/workflows/, Cargo build config. No install script exists (`git ls-files | rg -i 'install|setup'` → SETUP-PROMPT.md and install_smoke.sh only). PEER.md absent in worktree root. Tests: mods/cos-memory/tests/*.test.js pin FILES, 10/15 thresholds and the op enum; tests/hermetic.sh pins the db path; no test pins the shell hook literals (hook scripts have no tests in this scope). `#[cfg(test)]` skipped. Rows omitted on purpose: Rust/jq/JS identifiers, license, YAML boilerplate.

| file:line | literal (≤60 chars) | category | home G/W/M/K/? | note |
|---|---|---|---|---|
| .claude/settings.json:3 | deny: ["Agent"] | list/enum | W | permission policy; settings seeded, not code; no test pins |
| .claude/settings.json:8 | matcher "Bash" / "Write\|Edit\|MultiEdit\|NotebookEdit" | list/enum | M | tool list duplicated in herdr-gate.sh:36,88 and gate.rs |
| .claude/settings.json:10 | $CLAUDE_PROJECT_DIR/crates/qq-core/templates/workspace/scrip | path | M | engine-repo layout path; consuming workspace needs its own; 5 commits |
| .github/workflows/release.yml:10 | tags: ["v*"] | list/enum | K | CI trigger syntax; engine-only release policy |
| .github/workflows/release.yml:32 | ubuntu-22.04 (glibc floor) | list/enum | K | deliberate pin; comment explains |
| .github/workflows/release.yml:34 | x86_64-unknown-linux-gnu / aarch64-apple-darwin / x86_64-app | list/enum | K | target matrix; also asserted "expected 3 archives" at :121 |
| .github/workflows/release.yml:50 | crates/qq-core/Cargo.toml (version sed) | path | K | crate path; changes only on crate rename |
| .github/workflows/release.yml:63 | toolchain: 1.99.0 | number | K | duplicated in test.yml:41,102; no rust-toolchain.toml |
| .github/workflows/release.yml:76 | quaqua-${REF}-${TARGET} / bin target/…/release/quaqua | filename | K | binary + archive name; install docs depend on it |
| .github/workflows/release.yml:80 | LICENSE README.md (archive contents) | filename | K | packaging list |
| .github/workflows/release.yml:152 | --title "quaqua $TAG" | message | K | brand string; cosmetic |
| .github/workflows/test.yml:9 | ruleset 25703680 (comment) + 4 check names | repo | K | GitHub ruleset id in comment; names pinned by ruleset |
| .github/workflows/test.yml:39 | dtolnay/rust-toolchain@ff7f6a33… (SHA)  | url | K | supply-chain pin; 3 copies across 2 files |
| .github/workflows/test.yml:49 | cargo test --locked / clippy -D warnings | cmd | K | CI verbs |
| .github/workflows/test.yml:59 | <passwd home>/quaqua/runs/qq.db* (comment, hermetic step) | path | ? | duplicates db/path.rs default; follows lane 1 G decision |
| .github/workflows/test.yml:126 | node-version: 24 | number | K | CI pin |
| .github/workflows/test.yml:136 | mods/cos-memory/tests, *.test.js | path | K | CI guard of mod test dir; must move if mod moves |
| .github/workflows/test.yml:146 | bash mods/cos-memory/tests/install_smoke.sh | path | K | CI step |
| .github/workflows/test.yml:162 | ACTIONLINT_VERSION 1.7.12 + SHA256 | number | K | pinned tool+checksum |
| .github/workflows/test.yml:171 | https://github.com/rhysd/actionlint/releases/download | url | K | pinned third-party URL |
| crates/qq-core/Cargo.toml:3 | version = "0.1.0" | number | K | release.yml:50 reads it; tag must match |
| crates/qq-core/Cargo.toml:14 | rusqlite/serde/toml/regex exact pins | list/enum | K | build config; "toml (team.toml seed)" comment :17 names a file |
| crates/qq-core/templates/workspace/scripts/hooks/cos-inbox.sh:2 | quaqua inbox --for cos | cmd | ? | `cos` role name hardcoded; role set belongs in W/M; 4 commits |
| crates/qq-core/templates/workspace/scripts/hooks/cos-inbox.sh:20 | "hook_event_name" Stop, stop_hook_active | list/enum | K | Claude Code hook wire protocol |
| crates/qq-core/templates/workspace/scripts/hooks/cos-inbox.sh:34 | QUAQUA_OWNER != "cos" | env | ? | env name K (engine sets it); role literal "cos" is policy |
| crates/qq-core/templates/workspace/scripts/hooks/cos-inbox.sh:42 | $HOME/.cargo/bin/quaqua | path | G | binary lookup chain; copy in memory-compact.sh:103 |
| crates/qq-core/templates/workspace/scripts/hooks/cos-inbox.sh:50 | FORMAT hook-ups / hook-stop | list/enum | K | engine CLI format enum |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:2 | rules/_all/fanout-agent-grid.md + AGENTS.md '## Approval' an | section-cite | M | also :125 error text; 13 commits; rename a section → edit script |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:24 | QUAQUA_BIN, bin=quaqua | env | K | bootstrap env + binary name |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:37 | probe JSON Write /dev/null/qq-gate-probe | protocol | K | behaviour probe for stale binary |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:51 | guarded: pane (split\|move\|close\|send-keys\|send-text\|run | list/enum | K | herdr verbs; mirrors gate.rs guarded_pair; edit both |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:52 | outward: git push, gh pr merge, gh api merges, alias push | list/enum | M | approval policy mirrored in gate.rs check_git/check_gh |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:55 | listed (T3): tag, branch -d/-m, release create, filter-branc | list/enum | M | T3 approval list = policy; rule edit → regex edit |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:57 | mcp_outward: tinyfish run_deep_research…, Claude_Data share_ | list/enum | M | MCP names; mirrors MCP_OUTWARD in gate.rs; most churned |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:59 | claude_ai_(Asana\|Atlassian\|…\|Slack\|monday_com)__.*(send\ | list/enum | M | connector roster + verb list; changes per connector |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:67 | trunk_name (main\|master) | list/enum | W | trunk branch names; engine default branch also in gate.rs |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:74 | live_names (AGENTS\|identity\|USER).md, memory/rules.md | filename | M | protected-file list; also :91 and gate.rs; manifest "managed/private" source |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:78 | grant_door socat\|nc\|curl\|python\|node\|… | list/enum | M | interpreter list; policy |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:102 | herdr report-metadata grid_role= | list/enum | K | herdr CLI token name + grid protocol |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:104 | sqlite3 … qq.db (-readonly, .open, ATTACH) | filename | ? | DB file name; follows db/path.rs decision; also :110 |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:116 | grant\.(full\|tier\|lane) | list/enum | K | daemon verb names (wire protocol) |
| crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh:125 | error text citing rules/_all/…md and AGENTS.md '## Approval' | message | M | user-visible cite; same string at :2 and gate.rs |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:6 | memory/.compact/<sid>.md | path | M | snapshot dir; read at :113, written :132,245 |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:26 | hook modes --pre-compact/--session-start/--dry-run | list/enum | K | script CLI + Claude Code hook events |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:101 | $CARGO_MANIFEST_DIR/../target/debug/quaqua; ~/.cargo/bin/qua | path | G | dev-only fallback; copy of cos-inbox.sh lookup |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:121 | trunc 9000 (inject) / trunc 8192 (write) | number | M | two caps; invariant 8192<9000 not stated in one place |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:137 | .harness/plans/active/*.md | path | M | plan dir name |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:139 | ## Current State and Next Action (+ head -n 20) | section-cite | M | markdown section quoted in awk; plan template owns it |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:149 | projects/INDEX.md, status col 5, "done", "_example_", n>=20 | filename | M | table schema + row cap + sentinel values |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:167 | worktree list \| head -n 10 | number | M | row cap |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:171 | memory/$(date +%F).md, tail -n 10 | filename | M | daily log name + line cap |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:186 | transcripts tail --last 5; cut -b1-400 | number | M | prompt count 5, per-line 400B |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:199 | section titles: Active plans / Projects not done / Worktrees | message | M | snapshot headings; no test pins |
| crates/qq-core/templates/workspace/scripts/hooks/memory-compact.sh:221 | steer text "R9 … memory/corrections.md" | message | M | compaction prompt; names rule R9 and file |
| mods/cos-memory/.claude-plugin/plugin.json:2 | name cos-memory, version 0.1.0, author quaqua | repo | K | plugin identity; marketplace.json:2,8 duplicates |
| mods/cos-memory/README.md:1 | cap numbers / file list restated in README | message | ? | README may drift; not checked line by line |
| mods/cos-memory/hooks/register.js:23 | projectOf(root): last path segment | other | K | derives project label |
| mods/cos-memory/hooks/register.js:37 | prefix mcp__${plugin}__ | protocol | K | Claude Code MCP naming |
| mods/cos-memory/hooks/register.js:57 | e.reason !== 'answer' | protocol | K | hook event field |
| mods/cos-memory/src/corrections.js:12 | CORRECTION /không phải\|sai rồi\|đừng\|no, use\|not that/i | list/enum | W | trigger phrases are user-language policy |
| mods/cos-memory/src/corrections.js:23 | `- ${date} ${project} ${body}` | message | M | corrections.md line format; read by memory-compact.sh:226 |
| mods/cos-memory/src/corrections.js:42 | ${root}/memory/corrections.md | filename | M | corrections file name; also memory-compact.sh steer |
| mods/cos-memory/src/counter.js:12 | KEY = 'review' | other | K | private $.store key |
| mods/cos-memory/src/counter.js:13 | PROMPTS/STOPS_BEFORE_REVIEW=10, TOOL_CALLS=15 | number | W | R12 thresholds; 3 commits; tests pin 10/15 |
| mods/cos-memory/src/counter.js:19 | INFLIGHT_MS = 5*60*1000 | number | W | stale-marker timeout |
| mods/cos-memory/src/counter.js:60 | memory/rules.md, memory/<date>.md (stamp) | filename | M | third copy of the guarded-file list |
| mods/cos-memory/src/memory-gate.js:8 | FILES = ['memory/rules.md','USER.md'] | filename | M | capped-file list; mirrors engine TARGETS; used by tools/review; tests pin |
| mods/cos-memory/src/memory-gate.js:25 | quaqua call memory.write / quaqua memory scan | cmd | K | daemon verb names (wire) |
| mods/cos-memory/src/memory-gate.js:33 | replace(/^quaqua: /,"") | message | K | strips engine error prefix |
| mods/cos-memory/src/review.js:21 | REVIEW_PROMPT: memory/rules.md, USER.md, "still true next we | message | M | prompt body is rule text; restates FILES and ops |
| mods/cos-memory/src/review.js:78 | effort: 'low' | model | W | fork effort; no model name pinned (good) |
| mods/cos-memory/src/tools.js:10 | tool names memory_add/replace/remove/batch + descriptions | list/enum | K | tool surface; model-facing text mentions "capped" |
| mods/cos-memory/src/tools.js:64 | op enum ['add','replace','remove'] | list/enum | K | duplicated in review.js:35 |
| mods/cos-memory/tests/install_smoke.sh:1 | claude CLI plugin install, mod path | path | K | tests pin; 4 hits of file/paths |
| plugins/herdr-quaqua/herdr-plugin.toml:1 | id/name herdr-quaqua, version 1.0.0, min_herdr_version 0.9.0 | repo | K | plugin manifest |
| plugins/herdr-quaqua/herdr-plugin.toml:9 | command ["quaqua","identity","heal"] / grid hook / events ho | cmd | K | binary name "quaqua" bare; no QUAQUA_BIN override |
| plugins/herdr-quaqua/herdr-plugin.toml:12 | events pane.created/agent_detected/…/workspace.closed | list/enum | K | herdr event names |
| plugins/herdr-quaqua/plan.jq:32 | def tol: 2 | number | W | rect tolerance |
| plugins/herdr-quaqua/plan.jq:46 | "123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ0" pane id alphabet | protocol | K | herdr pane-id encoding |
| plugins/herdr-quaqua/plan.jq:52 | tokens grid_ready/grid_role=worker/grid_owner/grid_group/gri | list/enum | K | pane token protocol shared with Rust |
| plugins/herdr-quaqua/plan.jq:68 | "-workers", "workers", "-2" tab suffixes, "@workers", "@work | list/enum | ? | tab naming also GRID_TAB in Rust (lane 1) |
| plugins/herdr-quaqua/plan.jq:115 | cells/build_order/split_target for n=1..4 | list/enum | W | layout table 1/2/3/4; max 4 per tab |
| plugins/herdr-quaqua/plan.jq:116 | default area 200x50; cw = w*2/5; ratio 0.4/0.5 | number | W | layout geometry |
| plugins/herdr-quaqua/plan.jq:219 | owner "cos"; roles orchestrator\|cos\|lead | list/enum | W | role names; same as grid_role in gate.sh:102 |
| plugins/herdr-quaqua/plan.jq:358 | $n > 8 error "(max 8)"; chunk 0:4 / 4:n | number | W | group cap 8 = 2 tabs x 4; 1 commit |
| tests/hermetic.sh:4 | <home>/quaqua/runs/qq.db + legacy quaqua.db | path | ? | mirrors db/path.rs default + legacy name; guard pins path |
| tests/hermetic.sh:91 | "see is_live_db in crates/qq-core/src/db/path.rs" | message | K | source path cite; stale if renamed |
| tests/hermetic.sh:104 | decoy_marker text | message | K | test-only |
| tests/hermetic.sh:194 | strace trace='open,openat,openat2' | cmd | K | tool syntax |
| tests/hermetic.sh:211 | cargo test --locked | cmd | K | CI verb |
| tests/qq-test.sh:22 | cargo test --locked -p qq-core $name | cmd | K | crate name; AGENTS.md brief rule |

Rows: 91.
Per category: list/enum 26, number 12, path 11, message 10, filename 9, cmd 7, protocol 4, repo 3, url 2, env 2, section-cite 2, other 2, model 1.
Per home: W 11, M 25, K 46, ? 7, G 2.
Top 5 "edit a rule → edit Rust/script" churn (git log --oneline -- FILE | wc -l):
1. herdr-gate.sh — 13 (regex lists mirror gate.rs; section cites at :2,:125)
2. .github/workflows/test.yml — 10 (pins + check-name comments; mostly K, not rule churn)
3. .claude/settings.json — 5 (hook path + matcher list)
4. cos-inbox.sh — 4 (tied: memory-gate.js 4, tools.js 4, release.yml 4)
5. counter.js / review.js / register.js / herdr-plugin.toml — 3 each (R12 thresholds, review prompt)
Caveat: this scope has low raw churn; the heavy churn is in src/ lanes. Within scope, herdr-gate.sh is the only real outlier.

### Challenge
1. Shell hooks cannot read a git-fetched manifest at run time. They run per tool call (<300 ms budget, cos-inbox.sh:6), possibly without network or the binary. Target design should be: hooks become thin wrappers (`quaqua gate`, `quaqua compact`, `quaqua inbox`) and the fail-closed fallback regexes (herdr-gate.sh:51-110) are GENERATED at `init`/`sync` from the manifest into a cached file the script sources. Hardcoding the fallback in the script is the one place the engine must keep a local copy; otherwise delete the fallback and fail closed with a bare "exit 2 unless binary present".
2. The gate's policy lists exist twice (herdr-gate.sh and gate.rs: guarded_pair, check_git/gh/sqlite/herdr, MCP_OUTWARD). That duplication is the real churn driver. Single source: manifest → both.
3. cos-memory mod loads before/outside engine config. It must not read global config itself; it should ask the engine (`quaqua memory targets --json` for FILES, `quaqua config get review.*` for R12 thresholds, trigger phrases, review prompt). counter.js:13 says "R12 thresholds, not caps" — they are still policy, so W/M, not K.
4. plan.jq layout geometry (n=1..4 cells, max 8, ratio 0.4/0.5) is policy but tightly coupled to plan.rs/reconcile.rs; moving to config means the Rust mirror must read the same file — do it together, or keep K and accept the coupling.
5. K list is large (46/91) because CI pins, herdr verbs and Claude Code hook wire fields are protocol; the "engine hardcodes nothing" ruling should say "nothing policy", or CI/herdr/Claude wire constants will read as violations.
6. Bootstrap circularity: .claude/settings.json:10 and herdr-plugin.toml hardcode the engine-repo hook path and bare `quaqua`; they run before any config. Keep as the bootstrap, with QUAQUA_BIN honoured in the plugin toml too (currently only the shell scripts honour it).

### Risks
- Line numbers are from base 408a1e6; they drift as other lanes land.
- Rows marked ? (7) need CoS judgment; "home" for list rows is a recommendation, not verified against src/ lanes.
- Hook-script literals have no tests in scope, so a manifest migration has no safety net there.
- mods/cos-memory/README.md and docs/ may restate caps or file lists; not audited.
