# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[2.6.0]: https://github.com/tinhtute/quaqua/compare/v2.5.0...v2.6.0
[2.5.0]: https://github.com/tinhtute/quaqua/releases/tag/v2.5.0
