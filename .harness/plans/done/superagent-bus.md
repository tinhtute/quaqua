---
status: done
lane: high-risk
---
# Superagent bus: Source → Trigger → Thread → Subscription

## Goal
- outcome: CoS and Leads get every bus message once, in one channel (the `triggers` table rendered as a batched Inline & Spill digest). Idle agents are nudged when something new arrives. A CI failure on a `quaqua/*` branch reaches that branch's owner. Legacy `farm_events`, `mail`, ack and `<lead>.inbox.md` are gone.
- success_signal: in one live Lead run after P5, (a) CoS types into the Lead pane 0 times by hand (`herdr pane send-text` absent from CoS transcript for that run), (b) each message appears in exactly 1 channel (`ls runs/*.inbox.md` empty; `sqlite3 runs/quaqua.db ".tables"` shows no `farm_events`/`mail`), (c) a CI failure on a PR from that run's `quaqua/<slug>-integ` to `main` produces a `ci_failed` trigger to the Lead within 1 poll. (c) needs a push + PR, so it runs only with anh iu's approval (`.github/workflows/test.yml` triggers only on `pull_request`/`push` to `main`).
- actors: CoS (Quả Quả), project Leads, task workers (Peers), Auditor, anh iu (approves seams, migration timing, merge).
- authority: anh iu decisions 2026-11-02 (docs/research/superagent-audit.md §6); spec docs/research/superagent-spec.md; prior art deepseek-ai/dsh-superagent-prototype@6337fc8 docs 001–003.
- lead: `superagent-lead` holds the state across phases on integ branch `quaqua/superagent-integ` (base `da7a053` = main `5b7bd1e` + R0 wave 1 `5e5e8cd`); this file is its plan.
- requirements:
  - R0: the full test suite never opens the live workspace DB; the guard is enforced in CI only. | acceptance: `tests/test_hermetic_db.sh` traces `openat` of the live path (`~/quaqua/runs/quaqua.db*`, resolved from the passwd home as `workspace.rs:106` does) under `cargo test --locked` and asserts 0 hits; the `.github/workflows/test.yml` job installs `strace` on ubuntu and sets `HERMETIC_REQUIRE_STRACE=1`; on a host without strace (macOS ships none, `dtruss` needs root and SIP off) the script prints `skipped: strace unavailable` and exits 0, so a local run skips and CI is the enforcement point; row counts are not used because herdr hooks and the CoS write the live DB concurrently | source: remaining-work-2026-10-25.md row "Test hermeticity"; audit F7; review correction 2
  - R1: `triggers`, `threads`, `subscriptions`, `cursors(consumer, trigger_id)`, `source_state` replace `farm_events`, `mail` and the old `cursors`; only an explicit `quaqua bus migrate` backs up to `raw/`, copies rows, drops legacy tables, and is idempotent. Opening a legacy DB through `open_bus` never drops a table: it refuses with a "run quaqua bus migrate" error (exit 2), and hooks exit 0 without writing. | acceptance: `cargo test --locked --test bus_migrate` on a fixture DB: row counts map 1:1, new ids ordered by `ts_ms`, no unread row lost or replayed per consumer, second run no-op, legacy tables absent; `open_bus` on a legacy fixture leaves `.tables` unchanged; `--dry-run` on a copy of the live DB prints counts and leaves the original `sha256sum` unchanged | source: spec §3–4; decision 2
  - R2: one channel per message. `mail send` writes only a trigger; `<lead>.inbox.md` writes/reads, `mail ack`, ack columns, `mail_unacked`/`mail_acked` and their doctor/auditor/template references are deleted; the #199 auditor precheck reads the DB. | acceptance: `rg -n "inbox\.md|ack_mail|read_unacked|mail_unacked|acked_ms" crates templates -g '!crates/quaqua/src/bus/migrate.rs'` returns 0 hits; `cargo test --locked --test mail --test inbox --test doctor` pass | source: spec §5, §11; decisions 2, 5; audit F1, F3
  - R3: an idle non-CoS agent with unread triggers gets exactly one one-line nudge per batch; a busy agent gets none; the CoS pane (where anh iu types) is never nudged. | acceptance: `cargo test --locked --test events_hook` with a fake HerdrOps: idle+unread → 1 send-text; busy → 0; second hook with no newer trigger → 0; consumer `cos` → 0 | source: spec §7; decision 4; audit F4
  - R4: `mail inbox` and the CoS UserPromptSubmit hook render one batched digest (`#id kind actor thread summary≤80 → ref`, grouped by thread, ≤20 lines) and advance the cursor. Consumer rules per spec §5: no `--for` → resolved caller, Unknown → exit 1 with no cursor moved; `--for <n>` advances only when the resolved caller is `<n>`, otherwise peeks with a stderr warning; `--peek` never advances; the CoS hook keeps `--for cos`. | acceptance: `cargo test --locked --test inbox` golden-output test of the renderer; `cos-inbox.sh` test asserts it calls the renderer with `--for cos`; summary >80 rejected at insert; tests: `--for other` leaves that consumer's cursor unchanged, `--peek` leaves it unchanged, Unknown caller without `--for` exits 1 with all cursors unchanged | source: spec §5, §6
  - R5: `quaqua recall <id>` prints the trigger, its thread and its ref within 2 hops. | acceptance: `cargo test --locked --test recall` on a fixture: trigger → thread owner + report path + commit SHA from report | source: spec §8
  - R6: `worker spawn` opens thread `quaqua/<name>` and `lead spawn` (`lead/spawn.rs:222`, creates `quaqua/<slug>-integ`) opens the integ thread owned by the Lead with CoS also subscribed; each subscription lasts 14 days; `worker cleanup`/`lead cleanup` keep them; each new trigger extends them; expired subscriptions deliver nothing. | acceptance: `cargo test --locked --test subscription`: spawn, lead spawn and cleanup assertions on thread+subscription rows; clock-injected extension and expiry | source: spec §9; decision 7; audit F5
  - R7: `quaqua source github-ci poll` writes a `ci_failed` trigger only for `conclusion=failure` runs whose `headBranch` is `quaqua/*` with an existing thread, deduped by run id, read position persisted, network error leaves the position unchanged; a user timer runs it: a systemd unit on Linux and a launchd LaunchAgent on macOS, mirroring the auditor's `templates/workspace/scripts/auditor/{systemd,launchd}`, default cadence 5 min. | acceptance: `cargo test --locked --test github_ci` with a fake `gh` on PATH: failure on known branch → 1 trigger; success, unknown branch, repeat run id → 0; `gh` exit 1 → 0 triggers and unchanged `source_state` | source: spec §9; decisions 7, 8; audit F2
  - R8: the watchdog reports `waiting`, not stall exit 3, for an agent with a live subscription on a thread whose Peer is live or had a trigger inside the quiet window. | acceptance: `cargo test --locked --test watchdog`: Lead quiet 1400s, Peer live → no exit 3; Peer gone and no trigger → exit 3 as today | source: spec §10; decision 9; audit F6
- non-goals:
  - NG1: cloud runtime, BYOM.
  - NG2: removing the Lead tier.
  - NG3: LLM drift watchdog (decision 6).
  - NG4: speculative CI auto-fix.
  - NG5: per-channel reply norms.
  - NG6: PR comments/reviews as a Source (decision 8).
  - NG7: `triggers` retention/pruning.
  - NG1–NG6 are separate draft tracks T2–T6 in `docs/research/superagent.md`; they stay out of this plan.
- adr candidates: drop ack (cursor only); drop legacy tables in the same wave; Thread = branch.

## Phases and Verification
- approach: subtract first, then rebuild on one model. P0 makes the DB safe to change; P1 swaps the storage model and migrates every caller in one wave; P2 removes the duplicate channels and introduces the digest; P3 (wake + watchdog) and P4 (recall) are independent consumers of the new model; P5 adds the first external Source on top. Rejected: adding `triggers` beside `farm_events`/`mail` and migrating later (anh iu decision 2; leaves two read paths and the ack debt).
- delivery: the same cadence ends every phase — the phase's workers commit on their own branch off integ → `suhuynh` (Claude, a different model family from `tieude`/DeepSeek and `sude`/Gemini) reviews the diff → the Lead accepts the branch into `quaqua/superagent-integ` → anh iu approves the integ→main PR → the binary is installed from merged `main`. No binary is installed from an unmerged branch, so the live workspace never runs unreviewed code; the live steps (P1·T4 migration, P2·T3 trash) come after that install.
- risks:
  - live DB migration loses rows → `VACUUM INTO raw/` backup, `--dry-run` on a copy first, run only with 0 live workers and only after the P1 PR is merged into `main` and that binary installed; stop_if counts differ → restore from backup, escalate.
  - wake types into the wrong pane → nudge only through HerdrOps with the agent's own pane id; tests on fake HerdrOps; first live run watched by CoS.
  - `gh` rate limit or auth failure → poll exits 0 and keeps position; never on the gate's write path.
- seams: existing `quaqua mail send`, `quaqua mail inbox`, `handle_event_hook` (herdr hook), `worker spawn`/`worker cleanup`, `lead spawn`, `worker prompt` watchdog, `quaqua doctor`, auditor, `templates/workspace/scripts/hooks/cos-inbox.sh`. New, confirmed: `quaqua bus migrate [--dry-run]` (landed in P1, `bus/migrate.rs`). Still need anh iu's confirmation: `quaqua recall <id>` (before P4), `quaqua source github-ci poll` + user timer (systemd on Linux, launchd on macOS; before P5).
- constraints: workers do all edits; one writer per file area per wave; no push/merge without anh iu; committed paths use `~/` or repo-relative; gates `cargo test --locked`, `cargo clippy --locked -p quaqua`, `cargo fmt --check`, `cargo build --locked -p quaqua`; the local clippy + `--test doctor` gate is green on main `5b7bd1e` (PR #205), and `bash tests/test_hermetic_db.sh` skips locally on macOS by design — CI enforces it.
- recovery: each phase lands on its own `quaqua/<phase>` branch off `quaqua/superagent-integ`; the Lead merges it into integ after `suhuynh`'s review; a failed phase is reverted by dropping its branch; the DB is restored from the `raw/` backup taken in P1.
- phase_slug: `pre`
  status: done (0a main `5b7bd1e` PR #205; 0b main `8f698f6` PR #210)
  - goal: a green local gate before any phase runs (0a, done) and #163 fixed so the caller identity survives a herdr restart (0b, gates P3) | depends_on: none
  - surfaces: `pipeline/mod.rs`, `tests/doctor.rs` (0a); `identity.rs`, `layout/gate.rs` (0b) | avoided: `bus/`
  - escalate_when: #163's root cause is in herdr rather than quaqua
  - wave 1:
    - T0a clippy `while_immutable_condition` error + 3 failing `--test doctor` cases (`doctor.rs:143` returns the macOS note where the test expects systemd behaviour) (owner: tieude) — done on main `5b7bd1e` (PR #205; commits 4610c02, cd101b2) — check: `cargo clippy --locked -p quaqua` and `cargo test --locked --test doctor`
    - T0b done on main `8f698f6` (PR #210; issue #163 closed): #163 `mail inbox` resolves the consumer from the agent name of the pane, and that name is lost after a herdr restart, so the wrong consumer is nudged — root cause: herdr restores panes without tokens and omits the field, which `caller_pane_tokens` read as "no caller pane", so the CoS self-heal never ran and `QUAQUA_OWNER=cos` could grant role=cos from an untagged worker pane; a tokenless pane now reads as a pane (owner: suhuynh, writes `identity.rs`, `layout/gate.rs` + tests only) — output: commit — check: `--test layout` 50 pass, incl. restart self-heal and spawn-from-tokenless-pane deny — stop_if: the fix belongs in herdr (escalate to anh iu)
  - review: done — 0b rode PR #210 (`suhuynh` r3 PASS+NOTES) → main `8f698f6`; binary installed (0a was already on main via PR #205)
  - phase check: `cargo test --locked --test doctor`; 0b is P3's gate
- phase_slug: `hermetic`
  status: done (main `313b9d4`, PR #203)
  - goal: R0 | depends_on: none
  - surfaces: `crates/quaqua/src/workspace.rs`, test harness (`crates/quaqua/tests/common/`, existing `tests/live_db_guard.rs`), `engine/sqlite.rs`, `engine/activities.rs`, `worker/prompt/cost.rs`, `lead/prompt.rs` (QUAQUA_OWNER leak), `tests/test_hermetic_db.sh`, `.github/workflows/test.yml` | avoided: `bus/`, schema
  - escalate_when: a test needs the live DB by design
  - wave 1: landed as `5e5e8cd` (merged into integ as `da7a053`; CI run 41458802512 green, 0 hits)
    - T1 done: `tests/test_hermetic_db.sh` traces `openat` of the live DB under `cargo test --locked`, plus the `.github/workflows/test.yml` step that installs strace on ubuntu and sets `HERMETIC_REQUIRE_STRACE=1`; without strace the script prints `skipped: strace unavailable` and exits 0, so local runs on macOS skip and CI is the enforcement point (owner: tieude)
  - wave 2: landed as `d482885` + `ad0a933` (integ `50144f6`; CI run 41467771856 green, decoy planted, 0 hits)
    - T2 done (`d482885`: the 4 opens guarded at the root, `--test live_db_guard` 8/8; fixture env/QUAQUA_OWNER items not in this diff, carried to P1): close the 4 opens the trace found, at the root: `engine/sqlite.rs:60`, `engine/activities.rs:688`, `worker/prompt/cost.rs:588`, `:856`; fixture `QUAQUA_HOME`/`QUAQUA_RUNS_DIR`, no passwd-home fallback under test, QUAQUA_OWNER leak (owner: tieude worker, writes `engine/`, `workspace.rs`, `tests/common/`, `lead/prompt.rs` + failing tests only) — output: commit + list of fixed paths — check: CI job green with 0 hits — stop_if: a fix needs a schema change
    - T3 done as a decoy instead (`ad0a933`: CI plants a decoy file at the live path so an existence-gated open still shows up; no planted-test red→green pair was run): CI decoy: on a throwaway draft-PR branch, plant the test from the script header (`crates/quaqua/tests/zz_planted_live_db.rs`), assert the CI job fails listing the planted `openat`, then delete it and assert green (owner: tieude, draft PR only) — output: two CI runs — check: red with the planted open listed, green after removal
  - review: done — `suhuynh-p0rev` MERGE, minor follow-ups only (cost.rs:864 guard untested; `test_hermetic_db.sh` not gated to CI under `tests/run.sh`); PR #203 merged → main `313b9d4`; binary installed
  - phase check: CI job green; locally `cargo test --locked` only (the strace step skips on macOS)
- phase_slug: `model`
  status: done (main `8f698f6`, PR #210)
  - goal: R1, R6 (thread/subscription rows on spawn/cleanup) | depends_on: hermetic
  - surfaces: `bus/mod.rs`, new `bus/migrate.rs`, `bus/events.rs`, `bus/mail.rs` (storage calls only), `worker/spawn.rs`, `worker/cleanup.rs`, `lead/spawn.rs`, `lead/cleanup.rs`, `worker/prompt/mod.rs` (event writes), `layout/gate.rs`, `main.rs`, tests | avoided: digest rendering, templates, doctor
  - escalate_when: anh iu has not confirmed `quaqua bus migrate`; live-DB dry-run counts differ from fixture expectations; any worker live at migration time
  - wave 1:
    - T1 done (`8f698f6`; `--test bus_migrate` 19 pass): schema in `bus/mod.rs`; `quaqua bus migrate [--dry-run]` in `bus/migrate.rs` (the only file allowed to read legacy tables); `open_bus` refuses a legacy DB without dropping anything (owner: sude worker, writes `bus/mod.rs`, `bus/migrate.rs`, `main.rs` subcommand block, `tests/bus_migrate.rs` only) — output: commit — check: `cargo test --locked --test bus_migrate` — stop_if: legacy rows cannot be mapped
  - wave 2: T2 is two workers, both sude — ~97 old-table call sites in 10 files plus 29 public functions across `bus/mail.rs` + `bus/events.rs` is too large for one `tieude` unit
    - T2a done (`8f698f6`; `--test mail` 31 pass, `--test inbox` 13 pass): new trigger API inside `bus/`: storage and query functions of `bus/mail.rs`/`bus/events.rs` and their in-crate callers (owner: sude worker, writes `bus/*` + its tests only) — output: commit — check: `cargo test --locked` — stop_if: >2 failed attempts at the same test
    - T2b done (`8f698f6`; the `rg` gate returns 0 hits, `--test subscription` 5 pass): move the outside callers (`worker/`, `lead/`, `layout/gate.rs`; spawn opens thread + subscription, cleanup keeps it), then delete the legacy functions (owner: second sude worker, writes `worker/`, `lead/`, `layout/gate.rs`, tests only) — output: commit — check: `rg -n "farm_events|FROM mail|INTO mail" crates -g '!crates/quaqua/src/bus/migrate.rs'` = 0 and `cargo test --locked --test subscription` — stop_if: >2 failed attempts at the same test, then split further by file (`worker/prompt`, `layout/gate`, spawn/cleanup)
  - wave 3:
    - T3 done 2026-11-03: `--dry-run` on a copy of the live DB reported 319 `farm_events` + 2 `mail` + 1 cursor → 321 triggers; the original DB was left unchanged (owner: CoS, writes nothing in repo) — output: counts in run report — check: original DB `sha256sum` unchanged
    - T4 done 2026-11-04 by CoS (2026-11-03 acbf23a migrated a scratch copy of 321 rows; real T4 on live DB ran 2026-11-04): 1063 farm_events + 112 mail -> 1175 triggers + 245 threads, backup `raw/2026-11-04-quaqua-db-pre-superagent-1793782760947.db`, rerun no-op; `.tables` now shows `bus_migration`/`cursors`/`snapshots`/`source_state`/`subscriptions`/`threads`/`trigger_acks`/`triggers` and no `farm_events`/`mail`; `quaqua mail ls`, `mail inbox` and the CoS hook render from it (owner: CoS) — output: backup path under `raw/` + before/after counts — check: `sqlite3 ~/quaqua/runs/quaqua.db ".tables"` shows new tables only; `quaqua mail inbox` renders
  - review: done — `suhuynh` r3 `PASS+NOTES` (`raw/2026-11-03-superagent-pr210r3-review.md`; minor findings carried to open_items); PR #210 merged → main `8f698f6`; binary installed from it before T4 (that order is what made T4 safe)
  - phase check: `cargo test --locked && cargo clippy --locked -p quaqua` (the hermetic guard runs in CI)
- phase_slug: `channel`
  status: done (main `dabc11b` PR #228/#229; minors `b0b630e` PR #230)
  - goal: R2, R4 | depends_on: model
  - surfaces: `bus/mail.rs`, `bus/events.rs` (InboxDigest → renderer), `doctor.rs` (inbox.md drift checks), `auditor.rs` (#199 precheck), `main.rs` (drop `mail ack`), `templates/workspace/{AGENTS.md,rules/_all/lead-inbox.md,scripts/hooks/cos-inbox.sh}` | avoided: herdr hook wake logic, watchdog
  - escalate_when: `suhuynh`'s review finds a channel still reading inbox.md; the `rg` gate still hits ack outside the excluded files
  - wave 1:
    - T1 digest renderer + `mail inbox` on it + the `--for`/`--peek` rules + CoS hook calls it with `--for cos` (owner: tieude worker, writes `bus/events.rs`, `bus/mail.rs` render path, `cos-inbox.sh`, `tests/inbox.rs` only) — output: commit — check: `cargo test --locked --test inbox`
  - wave 2:
    - T2 delete inbox.md, ack, ack columns/events and their doctor/auditor/template references — ~16 files still hit today (owner: sude worker, writes `bus/mail.rs`, `auditor.rs`, `doctor.rs`, `main.rs`, templates, tests only) — output: commit — check: `rg -n "inbox\.md|ack_mail|read_unacked|mail_unacked|acked_ms" crates templates -g '!crates/quaqua/src/bus/migrate.rs'` = 0 and `cargo test --locked --test mail --test inbox --test doctor`
  - wave 3:
    - T3 after the new binary is installed: `quaqua init --update` so the new `cos-inbox.sh` reaches the workspace, then confirm every entry of each `runs/*-lead.inbox.md` exists as a trigger, then `trash` the files; deleting data, so only with anh iu's approval (owner: CoS) — output: per-file count match — check: `ls ~/quaqua/runs/*.inbox.md` empty and the workspace hook matches the template
  - review: `suhuynh` diff review → Lead accepts into integ → anh iu approves the integ→main PR → install binary (the install is T3's prerequisite)
  - phase check: `cargo test --locked && cargo clippy --locked -p quaqua`
- phase_slug: `wake`
  status: done (main `dabc11b` PR #228/#229; minors `b0b630e` PR #230)
  - goal: R3, R8; needs pre·0b (#163 — without the caller identity, a nudge after a herdr restart names the wrong consumer) | depends_on: channel, pre
  - surfaces: `bus/events.rs` (`handle_event_hook`), `bus/mail.rs` (post-send wake call), `worker/prompt/mod.rs` (watchdog stall branch, `stall: no worktree diff or session write for {quiet}s`, line 1358 at `da7a053`) | avoided: `recall`, CI source
  - escalate_when: herdr hook does not deliver `pane.agent_status_changed` for idle transitions of a non-Claude kind
  - wave 1:
    - T1 nudge-on-idle with per-batch dedupe, CoS excluded (owner: tieude worker, writes `bus/events.rs`, `bus/mail.rs`, `tests/events_hook.rs` only) — output: commit — check: `cargo test --locked --test events_hook`
    - T2 watchdog waiting rule (owner: second tieude worker, writes `worker/prompt/mod.rs`, `tests/watchdog.rs` only) — output: commit — check: `cargo test --locked --test watchdog`
  - review: `suhuynh` diff review → Lead accepts into integ → anh iu approves the integ→main PR → install binary
  - phase check: `cargo test --locked`
- phase_slug: `recall`
  status: done (main `dabc11b` PR #228/#229; minors `b0b630e` PR #230)
  - goal: R5 | depends_on: channel
  - surfaces: new `bus/recall.rs`, `main.rs` (subcommand) | avoided: `bus/events.rs`, `worker/`
  - escalate_when: anh iu has not confirmed the `quaqua recall` seam
  - wave 1:
    - T1 `quaqua recall <id>` + `--json` (owner: tieude worker, writes `bus/recall.rs`, `main.rs` subcommand block, `tests/recall.rs` only) — output: commit — check: `cargo test --locked --test recall`
  - review: `suhuynh` diff review → Lead accepts into integ → anh iu approves the integ→main PR → install binary
  - phase check: `cargo test --locked`
- phase_slug: `ci-source`
  status: done (main `dabc11b` PR #228/#229; minors `b0b630e` PR #230)
  - goal: R7, R6 (expiry and extension) | depends_on: wake, recall
  - surfaces: new `source/github_ci.rs`, `main.rs`, `templates/workspace/` user timer units (systemd `.service`/`.timer` on Linux, launchd agent on macOS), `bus/mod.rs` (subscription expiry query) | avoided: `gate/policy.rs`
  - escalate_when: anh iu has not confirmed the `quaqua source github-ci poll` seam or timer cadence
  - wave 1:
    - T1 poll command with fake-`gh` tests, dedupe, `source_state`, network-error path (owner: tieude worker, writes `source/`, `main.rs` subcommand block, `tests/github_ci.rs` only) — output: commit — check: `cargo test --locked --test github_ci`
    - T2 subscription extension on trigger + expiry filter in delivery, clock-injected (owner: second tieude worker, writes `bus/mod.rs` subscription functions + `tests/subscription.rs` only) — output: commit — check: `cargo test --locked --test subscription`
  - wave 2:
    - T3 user timer for both platforms (systemd units + launchd agent) + doctor check, mirroring the auditor's `templates/workspace/scripts/auditor/{systemd,launchd}` (owner: tieude worker, writes templates + `doctor.rs` timer check only) — output: commit — check: `cargo test --locked --test doctor --test init`
  - review: `suhuynh` diff review → Lead accepts into integ → anh iu approves the integ→main PR → install binary
  - phase check: `cargo test --locked && cargo clippy --locked -p quaqua && cargo fmt --check`; then the success_signal live run (CoS + anh iu) — (c) needs a push + PR, so it runs only with anh iu's approval

## Log

- 2026-11-02 — plan — locked by owner ("Lock và viết luôn"); to-plan full written by CoS; branch quaqua/cos-superagent-plan from main a189d30
- 2026-11-02 — plan — to-plan verify (advisor) fixed before any phase ran: P1/R2 rg gates exclude `bus/migrate.rs`; checks use `--test <file>` so 0 matched tests fail; R0 check is strace-based (live DB has concurrent writers); cursor migration rule made concrete; `open_bus` never drops legacy tables; integ thread from `lead spawn`; CI fires only on PRs to main; legacy inbox.md trash step (needs approval); CoS pane never nudged
- 2026-11-02 — plan — CoS review vs #163: `mail inbox` consumer + cursor-advance rules added to spec §5 and R4 (`--for` cannot advance another consumer's cursor; `--peek`; Unknown caller exits 1; CoS hook keeps `--for cos`). Evidence: CoS env `HERDR_BIN_PATH` pointing at a missing binary resolves the caller to Unknown, same failure shape as #163
- 2026-11-02 — plan — PR #203 review, corrections 1–9 applied and accepted by anh iu: `pre` phase added (0a already done on main `5b7bd1e` via PR #205; 0b = #163); R0 is CI-only with a local skip on macOS; every phase ends `suhuynh` review → Lead into `quaqua/superagent-integ` → anh iu's integ→main PR → install from merged main; Lead `superagent-lead` named; P1 owners moved to sude and T2 split T2a/T2b; the integ-unmerged risk removed (`0af6002` on main via #201); P3 depends on #163 and the watchdog anchor is `worker/prompt/mod.rs:1358` (the review's 1352 does not match the code at `bdb4af1`); P5 timer covers launchd + systemd; P2·T3 runs `quaqua init --update`; live `bus migrate` only after P1 is merged and installed, 0 live workers, time anh iu picks
- 2026-11-02 — hermetic — done: T0 docs `7cffb0d` + wave 1 `5e5e8cd` + wave 2 `d482885`/`ad0a933` on integ `50144f6`; suhuynh review MERGE; CI 41467771856 green (decoy, `HERMETIC_REQUIRE_STRACE=1`, 0 opens); PR #203 merged → main `313b9d4`. `tests/grid.rs:731` fails locally on main too: a leftover `$TMPDIR/test_shadow_mode_dir/shadow` with no write bit, not this phase
- 2026-11-03 — model — done: P1 T1+T2a+T2b and `pre` 0b (#163, issue closed) landed as main `8f698f6` (PR #210, squashed; `suhuynh` r3 PASS+NOTES, `raw/2026-11-03-superagent-pr210r3-review.md`; `--test bus_migrate` 19, `--test mail` 31, `--test inbox` 13, `--test subscription` 5 pass), with the `worker/prompt/cost.rs:864` live-DB refusal test and the `tests/run.sh` hermetic skip (plus the local `tests/grid.rs` shadow-dir failure above, fixed in the same commit); binary installed from main. T3 dry-run on a copy, then acbf23a migrated a scratch copy (321 legacy rows: 319 `farm_events` + 2 `mail`); real T4 ran 2026-11-04 by CoS: 1063 farm_events + 112 mail -> 1175 triggers + 245 threads, backup `raw/2026-11-04-quaqua-db-pre-superagent-1793782760947.db`, rerun no-op; `.tables` shows no `farm_events`/`mail`; `mail ls`/`mail inbox`/CoS hook verified. r3 minors carried to open_items
- 2026-11-09 — channel — done: P2 T1+T2 merged via PR #225 (`0c81ce0`); r3 minors fixed via PR #226 (`984ed73`): legacy `msg_key`→`dedupe_key` and `project` kept in `ref`, cursor and ack ids share `last_insert_rowid`, legacy `reply_to` remapped to new trigger ids, dry-run ids start at `sqlite_sequence`; follow/digest disagreement already gone since `deliver_page` unification, now locked by a test; workspace 1067 pass; live DB needed no repair (backup had 0 `reply_to` mail); binary installed from `984ed73`

## Validation
- 2026-11-10 CoS: code requirements R0–R8 on main `b0b630e`, main CI green; binary 2.6.0 installed from it.
- (b) holds: no `runs/*.inbox.md`; `.tables` has no `farm_events`/`mail`. R2 `rg` gate: only legacy-schema fixtures in `tests/bus_migrate.rs`.
- 2026-11-11 live check (throwaway `livecheck-lead`, draft PR #231 closed, branches deleted): (a) 0 `pane send-text` tool calls by CoS during the run; Lead reported via bus (#3339 accept, #3348 handoff after reading #3344). (c) CI run 42941249704 failed → one `github-ci poll` wrote 1 `ci_failed` trigger #3344 on thread `quaqua/livecheck-integ`; second poll 0 new (dedup). The poll timer stays off by anh iu's choice, so (c) fires only on a manual poll.
- Residual minor (not blocking): `threads` has no farm column.

## Current State and Next Action
- active_phase: none
- lifecycle_status: done (closed 2026-11-10 by CoS, anh iu approved)
- blockers: none
- open_items: threads farm column
- exact_next_action: none
