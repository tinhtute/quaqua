# software-factory-rewrite: draft task breakdown (input for to-plan)

CoS analysis, 2026-11-14, from the locked Goal (R1–R24) of `.monows/plans/active/software-factory-rewrite.md`, `raw/2026-11-13-quaqua-rs-archeval.md` (module map @43a60bd) and `raw/2026-11-13-quaqua-rs-herdrkind.md`. Draft only: nothing here is decided, and phases are written by to-plan, not by this note.

## Workstreams (cut by file area, one writer each)

| WS | Area in `crates/qq-core` | Requirements | Acceptance tests it must create | Old modules it draws on | Blocked by |
|---|---|---|---|---|---|
| W0 Foundation | crate skeleton, DB schema (config + state + entities in the one new DB), trait seams (HarnessDriver, Source, daemon RPC), test harness (`QUAQUA_TEAM_TOML` temp DB), the R16 parity checklist | R1, R2, R16, R20 | `legacy_omp_sude_rows_read`, `unknown_harness_rejected`, `ontology_schema_relations`; `cargo build --locked -p qq-core` | hash, util, workspace, git | none |
| W1 Config | `config/` + `config` and `roster` commands | R3, R4, R5 | `config_seed_set_export_roundtrip`, `config_diff_never_overwrites`, `rename_kind_updates_all_refs`, `roster_profiles_next_spawn` | config.rs (rewrite), layout/model.rs | W0 |
| W2 Daemon | daemon process, RPC, dispatch, liveness, journal, triggers, schedules, pane placement | R7 | `daemon_idle_worker_adds_no_poll_events`, `state_single_home` | engine core, bus triggers, liveness, herdr.rs, state.rs, grid | W0; Q42 |
| W3 Drivers | `driver/` registry, claude/pi/agy/opencode drivers, generic driver, skill injection, run capture | R6, R12, R19 | `driver_registry_generic_from_config`, `no_fallback_on_pane_busy`, `role_skill_sets_injected_per_driver`, `run_record_complete_all_drivers`, `cost_unknown_when_unreported`, and R11's `native_hook_installed_per_driver` | launch.rs, harness/, worker/prompt/{cost,retry}.rs, playbook | W0; Q37 |
| W4 Authority | topology graph, identity, fence, protocol file reader, worker cap | R8, R11, R21 | `topology_refuses_off_edge`, `peer_to_peer_mail_refused`, `one_lead_per_project`, `supervisor_question_edge_only`, `unfenced_driver_cannot_push`, `peer_identity_orchestration_refused`, `protocol_read_from_default_branch`, `worker_cap_default_and_ceiling`, `protocol_absent_from_peer_prompt` | gate, identity, cap, leadgate, layout/gate | W1, W2 |
| W5 Task flow | task DAG, one spawn path (Lead and Peer), brief check, report states, review, sha-bound acceptance, merge queue | R9, R22 | `task_dag_order`, `disposition_independent_of_kind`, `report_escalation_states`, `acceptance_binds_candidate_sha` | pipeline (rewrite), worker/prompt brief/verdict, lead, merge | W2, W3 |
| W6 Outer loop | backlog, source interface + 2 sources, Supervisor jobs and agent, sensor order | R13, R14, R23, R24 | `supervisor_scores_and_proposes`, `supervisor_agent_read_only`, `loop_sensor_order`, `backlog_risk_split`, `rule_change_always_high`, `trigger_schedule_pull_only`, `loop1_gate_failure_files_proposal`, `loop3_inbox_files_proposal` | auditor, transcripts | W2, W5; Q41 |
| W7 KB | zbrain + code KG read path, write guard | R10 | `kb_worker_read_only` | memory.rs | W4; Q38, Q39 |
| W8 Surface and cutover | CLI (`main`, status, doctor, init), R15 grep, workspace docs (new requirement, Q40), cutover, E2E | R15, R17, R18, docs requirement | R15 `git grep` prints nothing, `tests/e2e/cutover_rollback.sh`, `tests/e2e/full_pipeline.sh`, `cargo test --locked -p qq-core` | status, doctor, init, ship, estimate | all |

Coverage: R1–R24 each sit in one row (R11 is owned by W4; its hook-install test is built by W3). All 37 named cargo filters and both e2e scripts have an owner.

## Suggested order
1. Phase 0, spikes: the herdr `report-agent` probe (Q37) and the engine-core read (Q42). One writer, no fanout.
2. Phase 1, foundation and walking skeleton: W0, then one thin path `init` → daemon → claude driver spawn → report → run record. Single writer for schema and traits; a second for the daemon after the traits freeze.
3. Phase 2, parallel wave (up to 4 Peers): W1, W3, W4, W5.
4. Phase 3: W6 and W7.
5. Phase 4: W8, with `full_pipeline.sh` and `cutover_rollback.sh` as the phase check, then the one main merge (anh iu approves).

Sizing: more than 3 workers and more than a day, so a `quaqua-rs-lead` on the old binary with `quaqua/quaqua-rs-integ`; the old limits (4 Peers per Lead, 8 slots) bound each wave.

## Findings
- Reshape modules without a behavioural acceptance: grid (pane placement), status, doctor, bus triggers, `init --update`, the Lead spawn path and the watchdog exit codes are covered only by R1's `cargo build` and by the E2E. Not resolved here; to-plan either accepts the E2E as their check or asks for a requirement through `brainstorm refine`.
- R16's grep passes today because no `- [ ] parity` line exists; the checklist is W0's first output.
- Workspace docs come from the engine: `~/Lab/quaqua/templates/workspace/{AGENTS.md,models.md,rules,playbooks,...}`, installed by `init --update`. The live `~/quaqua/AGENTS.md` governs the old farm that builds the new crate, so the new docs must be staged in the engine templates on the integration branch and reach the live workspace only at the swap. They are worker tasks, not CoS edits.
- The Q37 probe may not run unattended: the repo gate hook blocked the last `herdr agent start` probe, and whether an isolated `herdr --session` call passes the gate is not checked. If it is blocked, the line becomes a manual task for anh iu; it is not to be routed through an unfenced harness.
- The E2E (R18) needs live accounts on at least two harnesses and an isolated herdr session, so it costs real tokens on every run.
- "Keep engine core" may be smaller than it reads: today's worker workflow is made of journaled poll activities, which R7 removes (Q42).
- The cutover seed path needs no decision: `init` seeds from the template, then `config import` takes the roster from team.toml, and the topology stays the template's (R3, R17, Q22).
