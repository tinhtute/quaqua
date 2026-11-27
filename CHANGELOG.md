# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- **A blocked worker's approval prompt goes to anh iu**: the workspace template's `recovery.md` and `operations.md` no longer tell the orchestrator to answer a blocked agent's prompt with `herdr agent send-keys <name> <key>`; it reads the pane with `herdr agent read <name>`, hands the text to anh iu, and resumes with `quaqua worker prompt <name> --resume` once they answer. `babysit` and `shipping` name one background `gh pr checks <n> --watch --interval 60 >/dev/null; gh pr checks <n>` that reports once, instead of pstack's `watch-pr` script, which is not installed.
- **`gh pr merge` asks the CoS only for one canonical line**: `gh pr merge <number>` with, in this order, an optional `--repo`/`-R <owner>/<name>`, at most one of `--merge`/`--squash`/`--rebase`, an optional `--delete-branch` and an optional `--match-head-commit <7-40 hex>`, single spaces and nothing before or after. That line reaches the native permission prompt at every tier, lane and full grant; every other merge spelling — a chained or quoted command, a wrapper, `xargs`/`find -exec`, a comment or heredoc, a `-c` script, a function or alias named gh, `--admin`/`--auto`/`--disable-auto`, `--body`/`--subject`, bare or reordered flags — stays hard-blocked, and every non-CoS caller blocks the canonical line too.

### Fixed
- **The owner's shell pane inside Herdr reads the farm**: a same-uid caller in a pane that hosts no known agent is now the same read-only Principal as one in no pane, so `quaqua ping`, `status`, `roster` and the config reads answer there; every farm-changing verb stays refused and now says where to run it — the CoS pane for `stop` and `config`, the agent's own pane for an agent verb — while client-side `daemon install` needs no caller at all (#375).
- **`daemon install` works inside a herdr pane**: a pane exports `HERDR_SOCKET_PATH` and never `HERDR_SESSION`, so install refused with a hint it could not follow. The session now comes from the environment first and from herdr second — `herdr session list` names the running session owning that socket, `herdr status server` supplies a missing socket — through the resolver `quaqua up` shares, which passes its farm's own session so a pane of another farm cannot put its pair in the unit or in the server's env; an explicit name herdr reports as not running still installs, with a `warning:` line, and the error that remains names `herdr session list` plus the two exports that work (#376).

## [3.3.0] - 2026-11-26

### Added
- **`quaqua up` starts the daemon on macOS**: `daemon install` writes a launchd LaunchAgent plist (`com.quaqua.daemon` in `~/Library/LaunchAgents`; `daemon.launchd_label` and `daemon.launchd_dir` move it) and `up` starts it through `launchctl print`, then `kickstart` for a loaded label or `bootstrap` for an unloaded one, never booting out a live daemon. Linux and macOS share one service seam; a target with no backend still gets one line and carries on. See `docs/daemon-service.md` (#369).

### Fixed
- **The `daemon::service` tests pass on macOS**: both backends are plain code tested on every OS, with a recording runner instead of real `systemctl` or `launchctl` calls (#363).

## [3.2.0] - 2026-11-25

### Added
- **The CoS and Lead panes carry a sidebar label**: `identity heal` and `memory hook session-start` now restore the `--display-agent` label as well as the tokens, and a Lead spawn labels its pane `<slug>-lead`, so a herdr sidebar rule matching `CoS` or `-lead` has something to match on. The CoS label comes from the new **`ui.cos_label`** setting (default `🍑 CoS`, blank falls back to it); `quaqua config set ui.cos_label …` moves it. The label is part of the "already complete" check, so a pane with tokens but no label converges on the next run. `ui` joins the `config export`/`import` sections (`[ui] cos_label = "..."`), and the shipped `team.toml` keeps the key as a comment so no workspace drifts.
- **`docs/herdr-sidebar.md`**: the herdr snippet that styles the `agent` label (and the `$grid_role` fallback) — CoS pink bold, Lead orange — with the caveats that a custom `ui.cos_label` needs its rule changed and that a Lead's label is written only at spawn, so a Lead row stays grey after a herdr restart until that Lead respawns.

## [3.1.0] - 2026-11-25

### Added
- **`quaqua up`**: one command brings the farm up — the Herdr session (started when stopped), the daemon (installed from the `y/N` prompt when no unit and a terminal), the workspace labelled after the farm root, the CoS in its pane, focus, and the TUI only outside Herdr. The CoS pane comes from the real listings: the pane tagged `grid_role=cos` first, else the pane recorded in `runs/cos.identity.json` when the agent session in it is the recorded one, else the first pane of the first tab — a pane tagged `grid_role=worker` is never the CoS. A pane that already runs an agent is left alone: `up` prints one line and starts no second CoS there. A CoS that has exited is resumed from `runs/cos.identity.json` through the harness's own resume flag (`claude --resume`, `pi --session`); no record or a refused resume prints one line and starts a fresh CoS.
- **The Herdr server `up` starts is detached**: it runs in its own process group, so a Ctrl-C at the `y/N` prompt cannot take the server, the CoS and every worker with it, and its output goes to `runs/herdr-server.log` rather than a pipe nobody drains.
- **The CoS launch is config**: `[roles.cos]` takes `harness`, `model` and `effort` (seeded `claude` / `opus-5.5` / `low`), so `quaqua config set roles.cos.model …` moves the CoS. A missing key makes `up` exit non-zero with the missing keys and the exact `config set` lines to run; the Herdr session is started first, because those writes need a pane.
- **`notify.daemon_start_sec`**: how long `up` gives a daemon start (default 90 s, well under the 600 s stall). `up`'s wait reads it instead of the stall knob.

### Fixed
- **The owner's terminal can read the farm**: a same-uid caller in no Herdr pane is the Principal (`owner`) instead of being refused with `unmapped_peer`, and it **reads only** — `quaqua ping`, `status`, `roster`, `caller.lookup` and `config get|export|diff|validate` work from a plain terminal. Everything that changes the farm runs from a Herdr pane: `stop`, `config set`, `config import` and every agent verb are refused with a message naming the pane to run them from. A pane that hosts no agent is still refused, and so is another uid (`foreign_uid`) (#368).
- **Caller refusals have their own exit code**: the daemon answering and refusing the caller exits `77` (`EXIT_REFUSED`), distinct from `69` for a daemon that is not running, so `quaqua ping || start-the-daemon` no longer starts a second daemon (#368).
- **Nothing writable is reachable outside Herdr**: no config write skips the authority check, so a process that leaves its pane (an orphan, `nohup`, a double fork, `setsid`, `systemd-run`, a socket handed on by a dead connector) reaches at most the read verbs — data the same uid can already read from `runs/qq.db` and `team.toml`. A pane Herdr cannot describe is likewise not a refusal, so one broken pane cannot turn the owner's `ping` into `77` (#368).

## [3.0.0] - 2026-11-23

Rewrite release: one Rust crate, `qq-core`, now builds the single `quaqua` binary. 479 commits since 2.7.0.

### Breaking
- **One binary, no bash wrappers**: the old `crates/quaqua` crate and the bash wrappers are gone. Build with `cargo build --release -p qq-core`; the binary is still `quaqua` (#279, #324).
- **Config lives in `team.toml` and the config database**: they are the single config source. Move any setting you kept elsewhere into `team.toml`, or set it with `quaqua config`. `config import` shows a before/after diff and refuses any changed or reset key without `--yes` (#335).
- **Harnesses are declared in `team.toml`** under `[harnesses.<name>]` (`herdr_kind`, an args template with a required `{model}`, efforts, stop keys). The `omp` harness is removed and is rejected as unknown; declare `agy` in `team.toml` (#280, #281).
- **Review pair rule**: a review is refused when builder and reviewer share a *model*, not only a harness. A same-model pair such as `ni` + `suhuynh` no longer runs; `worker spawn --review` refuses the builder's model (#324, #349).
- **Push and merge go through doors**: `quaqua push` refuses `main`, `master` and the remote default branch; push and merge are checked against the caller, and an authority gate with grants guards them (#292, #294, #309).
- **Workspace templates reshaped**: one persona file, a short CoS `AGENTS.md`, `rules/_all` removed. Run `quaqua init --update`; settle a hand-resolved conflict with `--update --accept <path>` (#291, #319, #320, #321).
- **Exit codes**: a rejected brief exits 64; only a connection failure exits 69 (#334).

### Outer Loop
- **Backlog**: proposals with a risk split; `quaqua backlog list|decide` (#324).
- **Sources and idea inbox**: `quaqua source pull`; the daemon runs due pulls and Supervisor jobs (#324).
- **Supervisor**: `quaqua supervisor score` scores runs against 17 rubrics; a low score files a high-risk proposal. Scheduled scoring stays off until `limits.supervisor_scorer` is set (#324, #325, #331).

### Factory Core
- **Config surface**: `config export`, `config diff`, named profiles by key or `QUAQUA_PROFILE`, effective at the next spawn (#324).
- **Agent pin**: harness, model and effort are pinned on the agent row at spawn; stop and the push fence read the pinned values (#324).
- **Task graph and run capture**: a task DAG, `run show`, `run list --project` (#324).
- **Worker cap**: the protocol block of `WORKSPACE_PROTOCOL.md` sets the cap (default 4, ceiling 8) and it binds every spawner (#324, #353).
- **Role skill sets** are injected per driver; worker writes to zbrain are refused; claude Peers start with the read-only code-lookup MCP (#324, #354).
- **Auditor** ported into `qq-core`, with stable finding ids and acks across runs (#284, #343).

### Daemon, Bus & Watchdog
- **Daemon**: one-daemon flock on `runs/` (fixes macOS "database is locked"), systemd user unit, sleep inhibitor while workers are live, wake mail, and `HERDR_SESSION`/`HERDR_SOCKET_PATH` carried into the service (#288, #344, #347).
- **Notifications**: the daemon mails the CoS a state line per agent transition; one "daemon down" line per session; every unshown notification leaves a bounded trace mail (#299, #300, #301, #303).
- **Watchdog**: stall limit from `notify.stall_sec`, provider errors end the wait at once, one resume for an unnamed API error, and a stub report when a worker dies (#332, #346).
- **Prompt and cleanup**: a new report is awaited, not the archived one; cleanup reports done only when pane, worktree and agent row are gone, and closes orphan panes (#282, #295, #296, #348).

### Memory & CoS
- Engine-computed Now block and standing block at SessionStart, a threat scan for standing load and memory writes, batched cos-memory writes, and a CoS code fence that asks before editing code or worktrees (#286, #293, #298, #306).
- `--check-brief` lints budgets, home paths, Unknowns/Evidence, test filters and `pgrep -f` (#302, #307).

### Fixes
- `quaqua status`: the idle override key is `timers.status_idle_minutes` (clamped to at least 1); `--metrics` lists the last 24 h and keeps a peer's spend (#329, #337).
- Child processes (herdr, gh) run under a time limit; a timed-out merge is confirmed with `gh pr view` (#336, #341).
- Grid: panes placed by token; empty herdr success replies accepted (#283, #351, #352).
- Report: an empty Challenge section no longer counts as a dispute, so it no longer ends the pipeline disputed (#350); a non-empty "no dispute" section still counts (#357).

### Known issues
- Three `daemon::service` install tests fail on macOS (#363).
- A non-empty "no dispute" Challenge section counts as a dispute (#357); Challenge parsing has fail-open edges (#365).
- Pipeline builder and reviewer agents hold worker-cap slots until cleanup (#360).
- Run capture keeps an empty result for tool calls made inside the REPL tool (#361).
- Grid reads a blank ok herdr reply as Null on pane-get-like readers, where it should be an error (#362); code lookup is unchecked against the allow-list (#364).
- E2E: part d before h fails H1 (#359); the pi Peer may refuse `quaqua push` in D5 (#358).

## [2.7.0] - 2026-11-16

### Superagent Bus
- **Trigger digest and `quaqua mail inbox`**: unified inbox delivery on `deliver_page` with one cursor writer, removed the legacy `inbox.md`, `mail ack` and ack columns, and fixed cursor advance, legacy `msg_key`/`reply_to` migration and dry-run trigger ids (#225, #226).
- **`quaqua recall <id>`**: resolves threads and refs, confined to the workspace and read-only on the caller's farm (#228, #229, #230).
- **Idle nudges and Lead-on-Peer watching**: nudges idle agents once per batch of unread triggers (R3); the watchdog keeps watching a Lead that waits on a live Peer (R8).
- **`quaqua source github-ci`**: 5-minute poll timer for systemd and launchd, checked by `quaqua doctor` (an absent timer is silent, not a warning).
- **GUI read contract (R1)**: views `v_gui_workflows`, `v_gui_inbox`, `v_gui_workers` and `gui_schema_version` (#214, #270).

### Watchdog, Prompt & Pipeline
- **Detached prompt core**: `worker prompt` waits in its own session, the pipeline runs in the detached core so it outlives its coordinator, `--resume` joins the live core, and Ctrl-C leaves the core running and says how to re-attach (#266, #267).
- **Resume lock and exit 8**: the core start is locked so simultaneous resumes join one core; a core that dies without an exit record reports exit 8; the lock proceeds unlocked where flock is unsupported, with platform-correct lock errnos (#270).
- **Exit 5 corroboration**: a screen-inferred `blocked` is corroborated before exit 5; exit 4 records a `status --metrics` row (#256, #255).
- **`strategy.builder_reviewer_differ`**: `pipeline` follows it at run time — `"harness"` keeps the same-harness rule and error text, `"kind"` refuses only builder == reviewer, `"none"` refuses nothing; the explicit `--reviewer` check and the default reviewer pick share one helper (#273).
- **Spawn and cost caps**: a worktree without `--base` starts from the fetched origin default; `on_cost_cap = "kill"` is refused on harnesses without a stop key (#250).

### Status, Cleanup & Auditor
- **`quaqua status`**: hides terminal workflows and resolved verdicts by default, adds `--all` and `--help`, exits 2 on an unknown argument; `playbook_gap` and disputed stay visible (#270).
- **`quaqua cleanup`**: cleans dead worker and Lead orphans (#270).
- **`quaqua ship`**: pushes the worker branch and opens a PR after approval, resolving the repo from telemetry after cleanup (#251).
- **`quaqua verify`**: records a coordinator test rerun as a verifier verdict.
- **Auditor**: git discovery for recorded repos, collapsed historical noise, mail scan scoped to the cos recipient and current farm, Linux service rendered with the install-time PATH (#242, #263, #268, #269).
- **`--check-brief`**: warns on `Budget:` instead of `Time budget:` and on a brief naming the main checkout path (#253, #254).

### Harness (Pi)
- **Pi workers**: workers launch on pi with default extensions (#261).
- **Claude-out suhuynh fallback**: `models.md` and `rules/_all/config-dynamic.md` name `mistral/deepseek-v4-1-flash-1017`, a pi model that launches, instead of an opencode provider id pi has no credential for (#271).

### CI & Tests
- **macOS type-check**: `cargo check` of the workspace on `macos-latest` (aarch64-apple-darwin), not a required check.
- **qq-core crate skeleton**: the `crates/qq-core` workspace member landed (engine port, herdr seam, db schema ladder); it is not shipped as a binary. The legacy-role guard test skips it, where the word names a new role (#274, #275).
- **Clippy gate**: `clippy -D warnings` and cos-memory mod tests run in CI; cleared 96 clippy errors (#256, #258, #259).

### CoS Memory Gate
- **`quaqua memory check [--workspace DIR]`**: counts Unicode chars (not bytes) against the AGENTS.md caps — `memory/rules.md` 2200, `USER.md` 1375 — and scans both for secrets (API keys, tokens, private keys); exit 1 with one line per violation.
- **`quaqua init` installs the gate as hooks**: a `PreToolUse` (`Write|Edit|MultiEdit`) hook that simulates the post-edit content and denies a write that would break a cap or carry a secret, and a `PostToolBatch` hook that re-runs the check and returns `additionalContext` so a `Bash`/`sed` write is flagged in the same turn. Both merge into `.claude/settings.json` idempotently and keep the hooks already there.
- **Caps are constants**: no config key, CLI flag or env var raises them.
- **`quaqua memory scan`**: screens one line for a memory file that is not itself capped — stdin → `{"ok":true,"text":…}` with the line cut to its 200-char bound, or `{"ok":false,"reason":…}` when the secret scan matches. The cos-memory mod's correction capture runs through it, so a prompt carrying a key is never written to `memory/corrections.md` and a long prompt lands as one bounded line.

### Harness (agy)
- **Antigravity CLI joins the registry**: `harness = "agy"` validates in `team.toml`, and `quaqua worker spawn` launches `agy` with `--model`/`--effort` (agy's `low|medium|high|xhigh|max`), `--dangerously-skip-permissions`, and a per-worker `--settings` file that pre-trusts the worktree, so the first-run "do you trust the contents of this project?" dialog never takes the brief. agy has no system-prompt flag, so the worker rules land as the `AGENTS.md` of an `--add-dir` directory (`runs/.agy/<name>/`), which agy loads as context.
- **Idle detection and stop**: herdr's built-in `agy` kind reports working/idle/done through screen detection (its antigravity-cli integration only records the conversation), so the watchdog uses the longer fallback idle limit; `esc` interrupts a running turn, so `quaqua worker stop` works. agy spend is reported as `agy-usage-unavailable`: usage is printed only in headless print mode, and the interactive conversation store is an opaque sqlite blob.
- **Auditor**: `quaqua auditor run --harness agy` runs `agy -p` headless, and `agy` is the last autodetect fallback after `claude` and `opencode`.

### Known issues
- The watchdog can exit 3 on a journal "database is locked" error when an unknown holder keeps the lock for more than 5 s. It predates 2.6.0 and is under investigation.
- `tests/run.sh --slow` has stale harness failures (stub reports lack `Outcome:`, missing `QUAQUA_OWNER`, a same-harness pair). No CI job runs it and no product bug was found.

## [2.6.0] - 2026-11-05

### Bus & Farm Scoping
- **Multi-farm bus scoping & schema migration**: Added farm ID resolution, schema migration, and backfill; scoped bus read queries and constrained lead incidents by farm (#169).
- **Pane attribution ladder**: Resolved farm, workspace, lead, and worker tiers at ingest; constrained `count_owner_slots` by farm (#169).
- **Cross-farm boundaries**: Refused cross-farm mail sends and rewired mail's farm gate onto the merged attribution ladder (#169).
- **Foreign incident filtering**: Filtered foreign pane incidents from inbox reads and dropped foreign pane events at ingest (#168, #169).
- **CoS farm tagging**: Stamped `grid_farm` on CoS panes from CoS tags and init, self-healed missing `grid_farm` tokens during identity heal, and treated `.cos` workspace parents as the pane's own workspace (#169, #171, #181).
- **Bus migration hermeticity**: Isolated `migrate_bus` test backups from live `raw/`, guarded live raw DBs during migration, and pinned legacy bus schema refusal exit 2 (#210, #212, #213).

### Watchdog & Runner Engine
- **Journal sequence conflict rebase**: Rebased runner cursor on journal sequence conflicts to avoid claiming events never applied (#191, #193).
- **Watchdog SQLite concurrency**: Retried SQLite busy errors in journal operations, fixed wake result replay, and ensured non-zero exit on runner failure (#158).
- **Lead owner restoration**: Restored missing `runs/<lead>.owner` during lead repair (#83, #159).
- **Lead identity heal**: Re-derived lead and CoS identity from `projects/INDEX.md` and agent-list tokens on heal, adding `--dry-run` support (#169).
- **Tokenless pane identity**: Recognized tokenless panes as live panes rather than absent panes (#163, #209).

### Liveness & Harness (OMP / Pi / Claude)
- **Hook-reported worker liveness**: Worker hook state serves as primary liveness signal, driving status displays and watchdog timeouts (#176, #182).
- **Blocked reason reporting**: Added structured block reason tokens written by liveness hooks and shown in status rows (#179, #183).
- **Provider error classification & backoff**: Classified OMP and Pi provider errors by HTTP status; added exponential backoff retry with capped delay and exit 7 escalation (#177, #185, #196, #204).
- **OMP harness extensions**: Hook-reported liveness and error classification extension for OMP; dropped `--no-extensions` to preserve user hooks; fixed attempt calculation and resolver races (#195, #204).
- **Claude launch settings**: Handed Claude its settings by file to stay within Herdr's typed-line limits, and surfaced Herdr start errors (#207).

### Taskforce & Council
- **Council neutral seat & sealed reports**: Implemented council neutral seat, sealed member reports, and fallback member deduplication (#200, #201).
- **Configurable council members**: Loaded `[taskforce.council]` member lists from `team.toml` (#167, #168).
- **Cancel signal handling**: Honored cancellation signals in `LiveCouncilMemberRunner::prompt_member` to abort promptly (#166, #168).

### Review & Pipeline Gates
- **Severity-tagged findings & thresholds**: Added severity tags (`[critical|major|minor|trivial]`) for review findings; untagged findings block by default; added `Must fix:` threshold and `Review rounds:` brief headers (#178, #184).
- **Typed Outcome enum**: Enforced typed `Outcome` enum and central parser helper for worker run reports (#138, #160).
- **Pipeline cancellation**: Added `quaqua pipeline --cancel` subcommand to stop running pipelines (#169).
- **Integration merge verification**: Enforced build checks (`cargo check --locked`) on `*-integ` branch merges by default, requiring clean working tree (#170, #186).
- **Worker cleanup ownership guard**: Required repo to prove worker ownership before cleanup, inferring repository from `runs/<name>.repo` (#170, #186).
- **Pipeline generation loop**: Guarded pipeline generation walk against infinite loops on immutable conditions (#209).

### Status, Doctor & CLI
- **Runtime ETA estimation**: Estimated worker remaining runtime in `quaqua status` based on historical journal durations per kind × playbook (#180, #201).
- **Auditor systemd diagnostics**: Added `quaqua doctor` checks for unplugged, dangling, or bad auditor systemd units (#198, #201).
- **Lead inbox timeline precheck**: Added lead inbox timeline precheck and audit item 6 in auditor (#199, #201).
- **CLI prompt arguments & exit codes**: Named missing spawn arguments in CLI errors and documented prompt exit codes (including exit 7) in CLI help and docs (#187, #189).
- **Lead gate build kinds**: Read Lead gate build kinds dynamically from `team.toml` (#165).
- **Orchestrator rule filtering**: Filtered orchestrator-only rules from worker prompt dumps (#197, #201).

### Hermetic Test Suite & CI
- **Live DB access guards**: Added guards at all SQLite openers under test to refuse unintentional connections to live workspace databases; planted decoy live DB in CI to catch existence-gated opens (#205, #209).
- **Platform-specific test gating**: Gated Linux-specific auditor systemd tests by OS to prevent false failures on macOS (#205).
- **Test isolation**: Added temp-dir isolation for grid shadow-mode tests, test environment locks (`TEST_ENV_LOCK`) for env-mutating tests, and identity seams for runner tests without live panes (#169, #209).

### Documentation & Planning
- **Superagent program specifications**: Added architectural specs, bus audits, mail inbox consumer cursor rules, and phased execution plans for the superagent project (#157, #175, #202, #203, #208, #211).
- **Herdr GUI specifications**: Added specs for Shepherdr-style Herdr GUI panels including settings, farm layout, and intent notes.
- **Operational rules reconciliation**: Reconciled rules for sectioned report reads, parity scope, lead gate configuration, and live subcommands in herdr-orchestration skills (#170, #187, #188, #192, #194).

## [2.5.0] - 2026-10-27

### Added
- Council runtime, identity heal, and role/depth gate (#156).

[2.7.0]: https://github.com/tinhtute/quaqua/compare/v2.6.0...v2.7.0
[2.6.0]: https://github.com/tinhtute/quaqua/compare/v2.5.0...v2.6.0
[2.5.0]: https://github.com/tinhtute/quaqua/releases/tag/v2.5.0
