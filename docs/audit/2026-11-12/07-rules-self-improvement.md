# Rules, failure log, lesson promotion, telemetry, auditor

**Verdict: PARTIAL, 2/5.** The auditor's deterministic pre-checks are solid, and rules are one topic per file. The learning loop itself (logging a failure, promoting a lesson, fixing a contradicting rule, switching off a mechanism) is done by the Chief of Staff by hand from prose.

Audited at `41a5327`. `src/` means `crates/quaqua/src/`. `tpl/` means `templates/workspace/`.

## Scope

- "Rules have one topic per file, scoped globally or per project, and temporary rules carry an expiry date."
- "Every failure is logged with its root cause; a cause that repeats without a rule is flagged."
- "At most one durable lesson is promoted per cleanup, and any older rule that contradicts it is fixed."
- "Telemetry turns off any mechanism that does not change outcomes."
- "An auditor checks for drift on a schedule."

## Findings

| Requirement | Status | Evidence | Gap |
|---|---|---|---|
| One topic per file, global or per-project scope, expiry on temporary rules | partial | 17 single-topic files in `tpl/rules/_all/`. Scope is `rules/_all/` or `rules/<repo>/`, with an orchestrator-only audience marker (`src/rules.rs:1-5`). Expiry syntax `[expires: YYYY-MM-DD]` (`tpl/AGENTS.md:27`); the auditor flags expired and malformed dates (`src/auditor.rs:585-595`, tests `:1380-1431`). `quaqua doctor --workspace` checks rule drift against the template (`quaqua doctor --help`). | An expired rule is flagged, not disabled; it keeps being injected until someone removes it. No lint enforces one topic per file. Several shipped rule files are single dated lessons with no expiry (`tpl/rules/_all/git-hygiene.md`, `herdr-plugin-smoke.md`, `opencode-no-bg-wait.md`, each 1 line). |
| Every failure logged with root cause; repeat without a rule is flagged | partial | The auditor counts `- [case]` lines over 14 days and flags a cause seen 2 or more times with no rule (`src/auditor.rs:1077-1120`, test `:1604-1644`). Watchdog exits are in the journal and counted in metrics (`src/status.rs:1097-1099`). | The `[case]` line is written by the CoS by hand (`tpl/AGENTS.md:25`). The engine knows the exit code but does not write the case. Matching is on free text, so two wordings of one cause do not match. Root cause is whatever the CoS types. |
| At most one lesson promoted per cleanup; contradicting rule fixed | partial | Reports must carry `## Lessons` or delivery fails with exit 6 (`src/worker/prompt/cost.rs:1220-1230`). The auditor flags lessons left unpromoted (`src/auditor.rs:204`, tests `:1443`, `:1529`). | Promotion, the "at most one" limit and the contradiction fix are all prose (`tpl/AGENTS.md:24`). `quaqua worker cleanup` does not prompt for or record a promotion decision. Contradiction detection does not exist. |
| Telemetry turns off any mechanism that does not change outcomes | missing | `review_changed_outcome` is recorded per pipeline (`src/status.rs:1070`). The principle is stated (`README.md:40`). | Nothing reads that field to act. No mechanism has an on or off switch driven by data. The decision is left to the owner or the CoS reading `status --metrics`. |
| An auditor checks for drift on a schedule | met | `quaqua auditor run` does read-only deterministic pre-checks then a headless LLM pass, writing `reports/audit-*.md` (`src/auditor.rs:1-6`). systemd timer at 07:00 and 19:00 with `Persistent=true` (`tpl/scripts/auditor/systemd/quaqua-auditor.timer:4-6`), launchd equivalent (`README.md:153-165`). `quaqua doctor` checks the units (`CHANGELOG.md:60`). The CoS must handle the newest report at session start (`tpl/AGENTS.md:21`). | Activation is manual after install (`README.md:149-151`). Handling the report is a prose rule. Whether the timer is active on the owner's machine is unverified; I had no access to it. |

## Prose-only rules that repeat and should be machinery

Each of these is an instruction to a coordinator with no check behind it.

| Rule | Where stated | Machine check today |
|---|---|---|
| Route every brief through the router | `tpl/AGENTS.md:59` | warning only (`src/worker/prompt/brief.rs:150`) |
| Coordinators never edit code or config | `tpl/AGENTS.md:60` | none |
| Stop spawning at about 70% of budget | `tpl/AGENTS.md:63` | none |
| Bound retries by failure mode, at most twice | `tpl/AGENTS.md:66` | provider errors only (`src/worker/prompt/retry.rs:6-7`) |
| Account for every worker at a wave boundary | `tpl/AGENTS.md:67` | none |
| Log `[case]` with root cause | `tpl/AGENTS.md:25` | read by auditor, not written by engine |
| Promote at most one lesson, fix contradictions | `tpl/AGENTS.md:24` | unpromoted flag only (`src/auditor.rs:204`) |
| Rerun the report's test before declaring done | `tpl/AGENTS.md:78` | none |
| Copy accepted reports to immutable `raw/` | `tpl/AGENTS.md:23` | none |

## Refs

- Auditor pre-checks: `src/auditor.rs:551`, `:678`, `:1077-1120`.
- Timer: `templates/workspace/scripts/auditor/systemd/quaqua-auditor.timer`.
- An inconsistency worth noting: a done plan describes an "hourly Auditor" (`.harness/plans/done/cos-memory-loop.md:34`), while the README and timer say twice a day (`README.md:7`).
- I could not read any real `reports/audit-*.md`, `memory/*.md` or `runs/telemetry.jsonl`; they live in the workspace, which is absent here.

## Fixes

1. Have the engine write the failure record: on any non-zero watchdog or pipeline exit, append a structured `case` event (worker, exit code, class, phase) to the journal, and let the CoS add only the root-cause text. The auditor then counts by class, not by wording.
2. Make cleanup the promotion point: `quaqua worker cleanup` shows the report's `## Lessons`, requires `--lesson promote:<rule-file>` or `--lesson none`, records the choice, and refuses a second promotion in the same cleanup.
3. Add outcome tracking per mechanism (reviewer, playbook check, council, advisor): fired count and changed-outcome count in `status --metrics`, with the auditor proposing a switch-off below a threshold over a window.
4. Stop injecting expired rules: `src/rules.rs` should skip a line whose expiry has passed and log that it did.
5. Convert the table above top-down: coordinator write deny, verify rerun, and wave accounting first, since those protect the success signal.
6. Add a rule lint to `quaqua doctor --workspace`: one `#` heading per file, dated provenance on lesson lines, valid expiry.
