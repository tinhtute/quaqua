---
status: active
lane: high-risk
---
# Superagent bus: Source → Trigger → Thread → Subscription

## Goal
- outcome: CoS and Leads get every bus message once, in one channel (the `triggers` table rendered as a batched Inline & Spill digest). Idle agents are nudged when something new arrives. A CI failure on a `quaqua/*` branch reaches that branch's owner. Legacy `farm_events`, `mail`, ack and `<lead>.inbox.md` are gone.
- success_signal: in one live Lead run after P5, (a) CoS types into the Lead pane 0 times by hand (`herdr pane send-text` absent from CoS transcript for that run), (b) each message appears in exactly 1 channel (`ls runs/*.inbox.md` empty; `sqlite3 runs/quaqua.db ".tables"` shows no `farm_events`/`mail`), (c) a CI failure on a PR from that run's `quaqua/<slug>-integ` to `main` produces a `ci_failed` trigger to the Lead within 1 poll. (c) needs a push + PR, so it runs only with anh iu's approval (`.github/workflows/test.yml` triggers only on `pull_request`/`push` to `main`).
- actors: CoS (Quả Quả), project Leads, task workers (Peers), Auditor, anh iu (approves seams, migration timing, merge).
- authority: anh iu decisions 2026-11-02 (docs/research/superagent-audit.md §6); spec docs/research/superagent-spec.md; prior art deepseek-ai/dsh-superagent-prototype@6337fc8 docs 001–003.
- requirements:
  - R0: the full test suite never opens the live workspace DB. | acceptance: `tests/test_hermetic_db.sh` runs `strace -f -e trace=openat -o <log> cargo test --locked` and asserts 0 `openat` of the live path (`~/quaqua/runs/quaqua.db*`, resolved from the passwd home as `workspace.rs:106` does); row counts are not used because herdr hooks and the CoS write the live DB concurrently | source: remaining-work-2026-10-25.md row "Test hermeticity"; audit F7
  - R1: `triggers`, `threads`, `subscriptions`, `cursors(consumer, trigger_id)`, `source_state` replace `farm_events`, `mail` and the old `cursors`; only an explicit `quaqua bus migrate` backs up to `raw/`, copies rows, drops legacy tables, and is idempotent. Opening a legacy DB through `open_bus` never drops a table: it refuses with a "run quaqua bus migrate" error (exit 2), and hooks exit 0 without writing. | acceptance: `cargo test --locked --test bus_migrate` on a fixture DB: row counts map 1:1, new ids ordered by `ts_ms`, no unread row lost or replayed per consumer, second run no-op, legacy tables absent; `open_bus` on a legacy fixture leaves `.tables` unchanged; `--dry-run` on a copy of the live DB prints counts and leaves the original `sha256sum` unchanged | source: spec §3–4; decision 2
  - R2: one channel per message. `mail send` writes only a trigger; `<lead>.inbox.md` writes/reads, `mail ack`, ack columns, `mail_unacked`/`mail_acked` and their doctor/auditor/template references are deleted; the #199 auditor precheck reads the DB. | acceptance: `rg -n "inbox\.md|ack_mail|read_unacked|mail_unacked|acked_ms" crates templates -g '!crates/quaqua/src/bus/migrate.rs'` returns 0 hits; `cargo test --locked --test mail --test inbox --test doctor` pass | source: spec §5, §11; decisions 2, 5; audit F1, F3
  - R3: an idle non-CoS agent with unread triggers gets exactly one one-line nudge per batch; a busy agent gets none; the CoS pane (where anh iu types) is never nudged. | acceptance: `cargo test --locked --test events_hook` with a fake HerdrOps: idle+unread → 1 send-text; busy → 0; second hook with no newer trigger → 0; consumer `cos` → 0 | source: spec §7; decision 4; audit F4
  - R4: `mail inbox` and the CoS UserPromptSubmit hook render one batched digest (`#id kind actor thread summary≤80 → ref`, grouped by thread, ≤20 lines) and advance the cursor. Consumer rules per spec §5: no `--for` → resolved caller, Unknown → exit 1 with no cursor moved; `--for <n>` advances only when the resolved caller is `<n>`, otherwise peeks with a stderr warning; `--peek` never advances; the CoS hook keeps `--for cos`. | acceptance: `cargo test --locked --test inbox` golden-output test of the renderer; `cos-inbox.sh` test asserts it calls the renderer with `--for cos`; summary >80 rejected at insert; tests: `--for other` leaves that consumer's cursor unchanged, `--peek` leaves it unchanged, Unknown caller without `--for` exits 1 with all cursors unchanged | source: spec §5, §6
  - R5: `quaqua recall <id>` prints the trigger, its thread and its ref within 2 hops. | acceptance: `cargo test --locked --test recall` on a fixture: trigger → thread owner + report path + commit SHA from report | source: spec §8
  - R6: `worker spawn` opens thread `quaqua/<name>` and `lead spawn` (`lead/spawn.rs:222`, creates `quaqua/<slug>-integ`) opens the integ thread owned by the Lead with CoS also subscribed; each subscription lasts 14 days; `worker cleanup`/`lead cleanup` keep them; each new trigger extends them; expired subscriptions deliver nothing. | acceptance: `cargo test --locked --test subscription`: spawn, lead spawn and cleanup assertions on thread+subscription rows; clock-injected extension and expiry | source: spec §9; decision 7; audit F5
  - R7: `quaqua source github-ci poll` writes a `ci_failed` trigger only for `conclusion=failure` runs whose `headBranch` is `quaqua/*` with an existing thread, deduped by run id, read position persisted, network error leaves the position unchanged; a systemd user timer template runs it. | acceptance: `cargo test --locked --test github_ci` with a fake `gh` on PATH: failure on known branch → 1 trigger; success, unknown branch, repeat run id → 0; `gh` exit 1 → 0 triggers and unchanged `source_state` | source: spec §9; decisions 7, 8; audit F2
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
- risks:
  - live DB migration loses rows → `VACUUM INTO raw/` backup, `--dry-run` on a copy first, run only with 0 live workers; stop_if counts differ → restore from backup, escalate.
  - integ `quaqua/quaqua-rs-integ` (#197–#200, 0af6002) unmerged → P2 depends on it (the #199 auditor precheck reads `*-lead.inbox.md`); if still unmerged at P2 start, escalate to anh iu (merge first, or P2 also lands the precheck rewrite on top of integ).
  - wake types into the wrong pane → nudge only through HerdrOps with the agent's own pane id; tests on fake HerdrOps; first live run watched by CoS.
  - `gh` rate limit or auth failure → poll exits 0 and keeps position; never on the gate's write path.
- seams: existing `quaqua mail send`, `quaqua mail inbox`, `handle_event_hook` (herdr hook), `worker spawn`/`worker cleanup`, `lead spawn`, `worker prompt` watchdog, `quaqua doctor`, auditor, `templates/workspace/scripts/hooks/cos-inbox.sh`. New, need anh iu's confirmation: `quaqua bus migrate [--dry-run]` (before P1; today migration runs implicitly in `open_bus`, `bus/mod.rs:161–280`), `quaqua recall <id>` (before P4), `quaqua source github-ci poll` + systemd user timer (before P5).
- constraints: workers do all edits; one writer per file area per wave; no push/merge without anh iu; committed paths use `~/` or repo-relative; gates `cargo test --locked`, `cargo clippy --locked -p quaqua`, `cargo fmt --check`, `cargo build --locked -p quaqua`.
- recovery: every phase lands on its own branch off the previous phase; a failed phase is reverted by dropping its branch; the DB is restored from the `raw/` backup taken in P1.
- phase_slug: `hermetic`
  status: planned
  - goal: R0 | depends_on: none
  - surfaces: `crates/quaqua/src/workspace.rs`, test harness (`crates/quaqua/tests/common/`, existing `tests/live_db_guard.rs`), `lead/prompt.rs` (QUAQUA_OWNER leak), `tests/test_hermetic_db.sh` | avoided: `bus/`, schema
  - escalate_when: a test needs the live DB by design
  - wave 1:
    - T1 add `tests/test_hermetic_db.sh` (strace `openat` of the live DB path during `cargo test --locked`, assert 0) and run it on the current tree first; `tests/live_db_guard.rs` may already cover part of R0 (owner: tieude worker, writes `tests/test_hermetic_db.sh` only) — output: script + baseline hit list — check: script exits 1 when a planted test opens the live path, 0 when removed
  - wave 2:
    - T2 fix every hit at the root (fixture `QUAQUA_HOME`/`QUAQUA_RUNS_DIR`, no passwd-home fallback under test, QUAQUA_OWNER leak) (owner: same worker, writes `workspace.rs`, `tests/common/`, `lead/prompt.rs` + failing tests only) — output: commit + list of fixed paths — check: `bash tests/test_hermetic_db.sh` — stop_if: fix requires schema change
  - phase check: `cargo test --locked && bash tests/test_hermetic_db.sh`
- phase_slug: `model`
  status: planned
  - goal: R1, R6 (thread/subscription rows on spawn/cleanup) | depends_on: hermetic
  - surfaces: `bus/mod.rs`, new `bus/migrate.rs`, `bus/events.rs`, `bus/mail.rs` (storage calls only), `worker/spawn.rs`, `worker/cleanup.rs`, `lead/spawn.rs`, `lead/cleanup.rs`, `worker/prompt/mod.rs` (event writes), `layout/gate.rs`, `main.rs` | avoided: digest rendering, templates, doctor
  - escalate_when: anh iu has not confirmed `quaqua bus migrate`; live-DB dry-run counts differ from fixture expectations; any worker live at migration time
  - wave 1:
    - T1 schema in `bus/mod.rs`; `quaqua bus migrate [--dry-run]` in `bus/migrate.rs` (the only file allowed to read legacy tables); `open_bus` refuses a legacy DB without dropping anything (owner: tieude worker, writes `bus/mod.rs`, `bus/migrate.rs`, `main.rs` subcommand block, `tests/bus_migrate.rs` only) — output: commit — check: `cargo test --locked --test bus_migrate` — stop_if: legacy rows cannot be mapped
  - wave 2:
    - T2 move every writer/reader of `farm_events`/`mail` to `triggers` (codemod or script first, then hand fixes), delete the legacy query functions in the same commit; spawn and lead spawn open thread + subscription, cleanup keeps it (owner: tieude worker, writes listed surfaces + their tests only) — output: commit — check: `rg -n "farm_events|FROM mail|INTO mail" crates -g '!crates/quaqua/src/bus/migrate.rs'` = 0 and `cargo test --locked --test subscription` and `cargo test --locked` — stop_if: >2 failed attempts at the same test
  - wave 3:
    - T3 run `--dry-run` on a copy of the live DB, report counts (owner: CoS, writes nothing in repo) — output: counts in run report — check: original DB `sha256sum` unchanged
    - T4 real migration of the live DB with 0 live workers, after anh iu approves the timing (owner: CoS) — output: backup path under `raw/` + before/after counts — check: `sqlite3 ~/quaqua/runs/quaqua.db ".tables"` shows new tables only; `quaqua mail inbox` renders
  - phase check: `cargo test --locked && cargo clippy --locked -p quaqua && bash tests/test_hermetic_db.sh`
- phase_slug: `channel`
  status: planned
  - goal: R2, R4 | depends_on: model
  - surfaces: `bus/mail.rs`, `bus/events.rs` (InboxDigest → renderer), `doctor.rs` (inbox.md drift checks), `auditor.rs` (#199 precheck), `main.rs` (drop `mail ack`), `templates/workspace/{AGENTS.md,rules/_all/lead-inbox.md,scripts/hooks/cos-inbox.sh}` | avoided: herdr hook wake logic, watchdog
  - escalate_when: integ still unmerged at phase start
  - wave 1:
    - T1 digest renderer + `mail inbox` on it + CoS hook calls it (owner: tieude worker, writes `bus/events.rs`, `bus/mail.rs` render path, `cos-inbox.sh`, tests only) — output: commit — check: `cargo test --locked --test inbox`
  - wave 2:
    - T2 delete inbox.md, ack, ack columns/events and their doctor/auditor/template references (owner: tieude worker, writes `bus/mail.rs`, `doctor.rs`, `auditor.rs`, `main.rs`, templates, tests only) — output: commit — check: `rg -n "inbox\.md|ack_mail|read_unacked|mail_unacked|acked_ms" crates templates -g '!crates/quaqua/src/bus/migrate.rs'` = 0 and `cargo test --locked --test mail --test inbox --test doctor`
  - wave 3:
    - T3 after the new binary is installed: confirm every entry of each `runs/*-lead.inbox.md` exists as a trigger, then `trash` the files; deleting data, so only with anh iu's approval (owner: CoS) — output: per-file count match — check: `ls ~/quaqua/runs/*.inbox.md` empty
  - phase check: `cargo test --locked && cargo clippy --locked -p quaqua && bash tests/test_hermetic_db.sh`
- phase_slug: `wake`
  status: planned
  - goal: R3, R8 | depends_on: channel
  - surfaces: `bus/events.rs` (`handle_event_hook`), `bus/mail.rs` (post-send wake call), `worker/prompt/mod.rs` (watchdog ~:1347) | avoided: `recall`, CI source
  - escalate_when: herdr hook does not deliver `pane.agent_status_changed` for idle transitions of a non-Claude kind
  - wave 1:
    - T1 nudge-on-idle with per-batch dedupe, CoS excluded (owner: tieude worker, writes `bus/events.rs`, `bus/mail.rs`, `tests/events_hook.rs` only) — output: commit — check: `cargo test --locked --test events_hook`
    - T2 watchdog waiting rule (owner: second tieude worker, writes `worker/prompt/mod.rs`, `tests/watchdog.rs` only) — output: commit — check: `cargo test --locked --test watchdog`
  - phase check: `cargo test --locked && bash tests/test_hermetic_db.sh`
- phase_slug: `recall`
  status: planned
  - goal: R5 | depends_on: channel
  - surfaces: new `bus/recall.rs`, `main.rs` (subcommand) | avoided: `bus/events.rs`, `worker/`
  - escalate_when: anh iu has not confirmed the `quaqua recall` seam
  - wave 1:
    - T1 `quaqua recall <id>` + `--json` (owner: tieude worker, writes `bus/recall.rs`, `main.rs` subcommand block, `tests/recall.rs` only) — output: commit — check: `cargo test --locked --test recall`
  - phase check: `cargo test --locked`
- phase_slug: `ci-source`
  status: planned
  - goal: R7, R6 (expiry and extension) | depends_on: wake, recall
  - surfaces: new `source/github_ci.rs`, `main.rs`, `templates/workspace/` systemd timer unit, `bus/mod.rs` (subscription expiry query) | avoided: `gate/policy.rs`
  - escalate_when: anh iu has not confirmed the `quaqua source github-ci poll` seam or timer cadence
  - wave 1:
    - T1 poll command with fake-`gh` tests, dedupe, `source_state`, network-error path (owner: tieude worker, writes `source/`, `main.rs` subcommand block, `tests/github_ci.rs` only) — output: commit — check: `cargo test --locked --test github_ci`
    - T2 subscription extension on trigger + expiry filter in delivery, clock-injected (owner: second tieude worker, writes `bus/mod.rs` subscription functions + `tests/subscription.rs` only) — output: commit — check: `cargo test --locked --test subscription`
  - wave 2:
    - T3 systemd user timer template + doctor check, mirroring the auditor unit (owner: tieude worker, writes templates + `doctor.rs` timer check only) — output: commit — check: `cargo test --locked --test doctor --test init`
  - phase check: `cargo test --locked && cargo clippy --locked -p quaqua && cargo fmt --check && bash tests/test_hermetic_db.sh`; then the success_signal live run (CoS + anh iu)

## Log

- 2026-11-02 — plan — locked by owner ("Lock và viết luôn"); to-plan full written by CoS; branch quaqua/cos-superagent-plan from main a189d30
- 2026-11-02 — plan — to-plan verify (advisor) fixed before any phase ran: P1/R2 rg gates exclude `bus/migrate.rs`; checks use `--test <file>` so 0 matched tests fail; R0 check is strace-based (live DB has concurrent writers); cursor migration rule made concrete; `open_bus` never drops legacy tables; integ thread from `lead spawn`; CI fires only on PRs to main; legacy inbox.md trash step (needs approval); CoS pane never nudged
- 2026-11-02 — plan — CoS review vs #163: `mail inbox` consumer + cursor-advance rules added to spec §5 and R4 (`--for` cannot advance another consumer's cursor; `--peek`; Unknown caller exits 1; CoS hook keeps `--for cos`). Evidence: CoS env `HERDR_BIN_PATH` pointing at a missing binary resolves the caller to Unknown, same failure shape as #163

## Validation

## Current State and Next Action
- active_phase: none
- lifecycle_status: planned
- blockers: none
- open_items: anh iu confirms new seams `quaqua bus migrate` (before P1), `quaqua recall` (before P4), `quaqua source github-ci poll` + timer cadence default 5 min (before P5); merge integ 0af6002 before P2; approvals for live migration (P1 T4) and inbox.md trash (P2 T3)
- exact_next_action: work full phase hermetic
