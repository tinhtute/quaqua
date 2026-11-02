---
status: active
lane: high-risk
---
# Superagent bus: Source → Trigger → Thread → Subscription

## Goal
- outcome: CoS and Leads get every bus message once, in one channel (the `triggers` table rendered as a batched Inline & Spill digest). Idle agents are nudged when something new arrives. A CI failure on a `quaqua/*` branch reaches that branch's owner. Legacy `farm_events`, `mail`, ack and `<lead>.inbox.md` are gone.
- success_signal: in one live Lead run after P5, (a) CoS types into the Lead pane 0 times by hand (`herdr pane send-text` absent from CoS transcript for that run), (b) each message appears in exactly 1 channel (`ls runs/*.inbox.md` empty; `sqlite3 runs/quaqua.db ".tables"` shows no `farm_events`/`mail`), (c) a forced CI failure on that run's branch produces a `ci_failed` trigger to the owner within 1 poll.
- actors: CoS (Quả Quả), project Leads, task workers (Peers), Auditor, anh iu (approves seams, migration timing, merge).
- authority: anh iu decisions 2026-11-02 (docs/research/superagent-audit.md §6); spec docs/research/superagent-spec.md; prior art deepseek-ai/dsh-superagent-prototype@6337fc8 docs 001–003.
- requirements:
  - R0: the full test suite never writes the live workspace DB. | acceptance: row counts of every table in `~/quaqua/runs/quaqua.db` identical before and after `cargo test --locked` (script `tests/test_hermetic_db.sh`); the `workspace.rs:106` home fallback is unreachable under test | source: remaining-work-2026-10-25.md row "Test hermeticity"; audit F7
  - R1: `triggers`, `threads`, `subscriptions`, `cursors(consumer, trigger_id)`, `source_state` replace `farm_events`, `mail` and the old `cursors`; migration backs up to `raw/`, copies rows, drops legacy tables, and is idempotent. | acceptance: migration test on a fixture DB: row counts map 1:1, second run no-op, legacy tables absent; `--dry-run` on a copy of the live DB prints counts and leaves the original byte-identical | source: spec §3–4; decision 2
  - R2: one channel per message. `mail send` writes only a trigger; `<lead>.inbox.md` writes/reads, `mail ack`, ack columns, `mail_unacked`/`mail_acked` and their doctor/auditor/template references are deleted; the #199 auditor precheck reads the DB. | acceptance: `rg -n "inbox\.md|ack_mail|read_unacked|mail_unacked|acked_ms" crates templates` returns 0 hits; mail/inbox/auditor tests pass | source: spec §5, §11; decisions 2, 5; audit F1, F3
  - R3: an idle agent with unread triggers gets exactly one one-line nudge per batch; a busy agent gets none. | acceptance: events_hook tests with a fake HerdrOps: idle+unread → 1 send-text; busy → 0; second hook with no newer trigger → 0 | source: spec §7; decision 4; audit F4
  - R4: `mail inbox` and the CoS UserPromptSubmit hook render one batched digest (`#id kind actor thread summary≤80 → ref`, grouped by thread, ≤20 lines) and advance the cursor. | acceptance: golden-output test of the renderer; `cos-inbox.sh` test asserts it calls the renderer; summary >80 rejected at insert | source: spec §6
  - R5: `quaqua recall <id>` prints the trigger, its thread and its ref within 2 hops. | acceptance: CLI test on a fixture: trigger → thread owner + report path + commit SHA from report | source: spec §8
  - R6: `worker spawn` opens thread `quaqua/<name>` and subscribes the owner for 14 days; `worker cleanup` keeps it; each new trigger extends it; expired subscriptions deliver nothing. | acceptance: spawn/cleanup tests on thread+subscription rows; clock-injected expiry test | source: spec §9; decision 7; audit F5
  - R7: `quaqua source github-ci poll` writes a `ci_failed` trigger only for `conclusion=failure` runs on `quaqua/*` branches with an existing thread, deduped by run id, read position persisted, network error leaves the position unchanged; a systemd user timer template runs it. | acceptance: poll tests with a fake `gh` on PATH: failure on known branch → 1 trigger; success, unknown branch, repeat run id → 0; `gh` exit 1 → 0 triggers and unchanged `source_state` | source: spec §9; decisions 7, 8; audit F2
  - R8: the watchdog reports `waiting`, not stall exit 3, for an agent with a live subscription on a thread whose Peer is live or had a trigger inside the quiet window. | acceptance: prompt watchdog test: Lead quiet 1400s, Peer live → no exit 3; Peer gone and no trigger → exit 3 as today | source: spec §10; decision 9; audit F6
- non-goals:
  - NG1: cloud runtime, BYOM.
  - NG2: removing the Lead tier.
  - NG3: LLM drift watchdog (decision 6).
  - NG4: speculative CI auto-fix.
  - NG5: per-channel reply norms.
  - NG6: PR comments/reviews as a Source (decision 8).
  - NG7: `triggers` retention/pruning.
- adr candidates: drop ack (cursor only); drop legacy tables in the same wave; Thread = branch.

## Phases and Verification
- approach: subtract first, then rebuild on one model. P0 makes the DB safe to change; P1 swaps the storage model and migrates every caller in one wave; P2 removes the duplicate channels and introduces the digest; P3 (wake + watchdog) and P4 (recall) are independent consumers of the new model; P5 adds the first external Source on top. Rejected: adding `triggers` beside `farm_events`/`mail` and migrating later (anh iu decision 2; leaves two read paths and the ack debt).
- risks:
  - live DB migration loses rows → `VACUUM INTO raw/` backup, `--dry-run` on a copy first, run only with 0 live workers; stop_if counts differ → restore from backup, escalate.
  - integ `quaqua/quaqua-rs-integ` (#197–#200, 0af6002) unmerged → P2 depends on it (the #199 auditor precheck reads `*-lead.inbox.md`); if still unmerged at P2 start, escalate to anh iu (merge first, or P2 also lands the precheck rewrite on top of integ).
  - wake types into the wrong pane → nudge only through HerdrOps with the agent's own pane id; tests on fake HerdrOps; first live run watched by CoS.
  - `gh` rate limit or auth failure → poll exits 0 and keeps position; never on the gate's write path.
- seams: existing `quaqua mail send`, `quaqua mail inbox`, `handle_event_hook` (herdr hook), `worker spawn`/`worker cleanup`, `worker prompt` watchdog, `quaqua doctor`, auditor, `templates/workspace/scripts/hooks/cos-inbox.sh`. New, need anh iu's confirmation before P4/P5 start: `quaqua recall <id>`, `quaqua source github-ci poll` (+ systemd user timer), `quaqua bus migrate [--dry-run]` (only if no migration entrypoint exists).
- constraints: workers do all edits; one writer per file area per wave; no push/merge without anh iu; committed paths use `~/` or repo-relative; gates `cargo test --locked`, `cargo clippy --locked -p quaqua`, `cargo fmt --check`, `cargo build --locked -p quaqua`.
- recovery: every phase lands on its own branch off the previous phase; a failed phase is reverted by dropping its branch; the DB is restored from the `raw/` backup taken in P1.
- phase_slug: `hermetic`
  status: planned
  - goal: R0 | depends_on: none
  - surfaces: `crates/quaqua/src/workspace.rs`, test harness (`crates/quaqua/tests/common/`), `lead/prompt.rs` (QUAQUA_OWNER leak), `tests/test_hermetic_db.sh` | avoided: `bus/`, schema
  - escalate_when: a test needs the live DB by design
  - wave 1:
    - T1 find every test path that resolves the live DB or leaks QUAQUA_OWNER, fix at the root (fixture `QUAQUA_HOME`/`QUAQUA_RUNS_DIR`, no home fallback under test) (owner: tieude worker, writes `workspace.rs`, `tests/common/`, `lead/prompt.rs` only) — output: commit + list of fixed paths — check: `bash tests/test_hermetic_db.sh` — stop_if: fix requires schema change
    - T2 add `tests/test_hermetic_db.sh` (snapshot table counts of `~/quaqua/runs/quaqua.db`, run suite, compare) (owner: same worker, writes `tests/test_hermetic_db.sh` only) — output: script — check: script exits 0 on fixed tree and 1 when a planted live write is added
  - phase check: `cargo test --locked && bash tests/test_hermetic_db.sh`
- phase_slug: `model`
  status: planned
  - goal: R1, R6 (thread/subscription rows on spawn/cleanup) | depends_on: hermetic
  - surfaces: `bus/mod.rs`, `bus/events.rs`, `bus/mail.rs` (storage calls only), `worker/spawn.rs`, `worker/cleanup.rs`, `worker/prompt/mod.rs` (event writes), `layout/gate.rs`, `main.rs` | avoided: digest rendering, templates, doctor
  - escalate_when: live-DB dry-run counts differ from fixture expectations; any worker live at migration time
  - wave 1:
    - T1 schema + migration + `--dry-run` in `bus/mod.rs` (owner: tieude worker, writes `bus/mod.rs`, `tests/bus_migrate.rs` only) — output: commit — check: `cargo test --locked bus_migrate` — stop_if: legacy rows cannot be mapped
  - wave 2:
    - T2 move every writer/reader of `farm_events`/`mail` to `triggers` (codemod or script first, then hand fixes), delete the legacy query functions in the same commit; spawn opens thread + subscription, cleanup keeps it (owner: tieude worker, writes listed surfaces + their tests only) — output: commit — check: `rg -n "farm_events|FROM mail|INTO mail" crates` = 0 and `cargo test --locked` — stop_if: >2 failed attempts at the same test
  - wave 3:
    - T3 run `--dry-run` on a copy of the live DB, report counts (owner: CoS, writes nothing in repo) — output: counts in run report — check: original DB `sha256sum` unchanged
  - phase check: `cargo test --locked && cargo clippy --locked -p quaqua && bash tests/test_hermetic_db.sh`
- phase_slug: `channel`
  status: planned
  - goal: R2, R4 | depends_on: model
  - surfaces: `bus/mail.rs`, `bus/events.rs` (InboxDigest → renderer), `doctor.rs` (inbox.md drift checks), `auditor.rs` (#199 precheck), `main.rs` (drop `mail ack`), `templates/workspace/{AGENTS.md,rules/_all/lead-inbox.md,scripts/hooks/cos-inbox.sh}` | avoided: herdr hook wake logic, watchdog
  - escalate_when: integ still unmerged at phase start
  - wave 1:
    - T1 digest renderer + `mail inbox` on it + CoS hook calls it (owner: tieude worker, writes `bus/events.rs`, `bus/mail.rs` render path, `cos-inbox.sh`, tests only) — output: commit — check: `cargo test --locked inbox` + golden test
  - wave 2:
    - T2 delete inbox.md, ack, ack columns/events and their doctor/auditor/template references (owner: tieude worker, writes `bus/mail.rs`, `doctor.rs`, `auditor.rs`, `main.rs`, templates, tests only) — output: commit — check: `rg -n "inbox\.md|ack_mail|read_unacked|mail_unacked|acked_ms" crates templates` = 0 and `cargo test --locked`
  - phase check: `cargo test --locked && cargo clippy --locked -p quaqua && bash tests/test_hermetic_db.sh`
- phase_slug: `wake`
  status: planned
  - goal: R3, R8 | depends_on: channel
  - surfaces: `bus/events.rs` (`handle_event_hook`), `bus/mail.rs` (post-send wake call), `worker/prompt/mod.rs` (watchdog ~:1347) | avoided: `recall`, CI source
  - escalate_when: herdr hook does not deliver `pane.agent_status_changed` for idle transitions of a non-Claude kind
  - wave 1:
    - T1 nudge-on-idle with per-batch dedupe (owner: tieude worker, writes `bus/events.rs`, `bus/mail.rs`, `tests/events_hook.rs` only) — output: commit — check: `cargo test --locked events_hook`
    - T2 watchdog waiting rule (owner: second tieude worker, writes `worker/prompt/mod.rs`, its tests only) — output: commit — check: `cargo test --locked watchdog`
  - phase check: `cargo test --locked && bash tests/test_hermetic_db.sh`
- phase_slug: `recall`
  status: planned
  - goal: R5 | depends_on: channel
  - surfaces: new `bus/recall.rs`, `main.rs` (subcommand) | avoided: `bus/events.rs`, `worker/`
  - escalate_when: anh iu has not confirmed the `quaqua recall` seam
  - wave 1:
    - T1 `quaqua recall <id>` + `--json` (owner: tieude worker, writes `bus/recall.rs`, `main.rs` subcommand block, `tests/recall.rs` only) — output: commit — check: `cargo test --locked recall`
  - phase check: `cargo test --locked`
- phase_slug: `ci-source`
  status: planned
  - goal: R7, R6 (expiry and extension) | depends_on: wake, recall
  - surfaces: new `source/github_ci.rs`, `main.rs`, `templates/workspace/` systemd timer unit, `bus/mod.rs` (subscription expiry query) | avoided: `gate/policy.rs`
  - escalate_when: anh iu has not confirmed the `quaqua source github-ci poll` seam or timer cadence
  - wave 1:
    - T1 poll command with fake-`gh` tests, dedupe, `source_state`, network-error path (owner: tieude worker, writes `source/`, `main.rs` subcommand block, `tests/github_ci.rs` only) — output: commit — check: `cargo test --locked github_ci`
    - T2 subscription extension on trigger + expiry filter in delivery, clock-injected (owner: second tieude worker, writes `bus/mod.rs` subscription functions + tests only) — output: commit — check: `cargo test --locked subscription`
  - wave 2:
    - T3 systemd user timer template + doctor check, mirroring the auditor unit (owner: tieude worker, writes templates + `doctor.rs` timer check only) — output: commit — check: `cargo test --locked doctor init`
  - phase check: `cargo test --locked && cargo clippy --locked -p quaqua && cargo fmt --check && bash tests/test_hermetic_db.sh`; then the success_signal live run (CoS + anh iu)

## Log

- 2026-11-02 — plan — locked by owner ("Lock và viết luôn"); to-plan full written by CoS; branch quaqua/cos-superagent-plan from main a189d30

## Validation

## Current State and Next Action
- active_phase: none
- lifecycle_status: planned
- blockers: none
- open_items: confirm new seams `quaqua recall`, `quaqua source github-ci poll` (+ timer cadence, default 5 min) before P4/P5; merge integ 0af6002 before P2
- exact_next_action: work full phase hermetic
