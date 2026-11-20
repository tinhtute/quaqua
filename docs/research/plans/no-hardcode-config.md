---
status: proposed
lane: normal
---
# No-hardcode config — templates fetched from a remote repo, policy out of Rust

## Goal
- outcome: the engine holds mechanism only; every rule, vocabulary, file list and cap a rule author reads lives in config or in a template repo the user points the engine at. Editing a rule never needs a Rust rebuild.
- evidence: 4 scout reports `~/quaqua/raw/2026-11-20-hermes-cos-hc-{core,guard,rest,ext}.md`, merged in `docs/research/evidence/2026-11-20-hardcode-inventory.md` (456 rows, base 408a1e6; lines drift).
- authority: anh iu, 2026-11-20.

## Decisions (anh iu, 2026-11-20)
1. Templates are no longer embedded. They are fetched from a remote git repo. Machines always have network: no offline fallback. (The rest lane asked for an embedded `include_bytes!` fallback; rejected by this decision, recorded here.)
2. Everything is configurable, the fetch link included. Global config `[template] url / ref / path / manifest / cache`. No default URL in the binary. No config → error that says how to create it. `quaqua init --from <url> [--ref]` writes it.
3. Only bootstrap: `QUAQUA_CONFIG` env, else `$XDG_CONFIG_HOME/quaqua/config.toml` (fallback `~/.config/quaqua/config.toml`).
4. TOML, not YAML (`team.toml` is TOML, schema v2).
5. A manifest `quaqua.toml` in the template repo owns managed/seed/private file lists, standing order, caps and `engine = ">=x"` compat.

## Home definitions
- **G** global config (`config.toml`): per machine/user: template source, cache dir, timeouts, limits, env-var replacements.
- **W** `team.toml` (workspace): roles, kinds, models, notify, merge, grid; already the home of `[roles]`, `[kinds.*]`, `notify.*`, `merge.check`.
- **M** manifest `quaqua.toml` in the template repo: file roles, section names, report contract, caps, standing order, threat/fence additions.
- **K** keep in code, mechanism: wire protocol, SQL, journal kinds, herdr/git/gh syntax, Claude hook JSON, CI pins.
- **?** doubtful, needs CoS judgment (about 27 rows across lanes).

## Target architecture
Global config (`~/.config/quaqua/config.toml`, written by `quaqua init --from`). Manifest values mirror today's engine (checked against origin/main 800a410) so phase `standing` is behavior-preserving; global-config values are illustrative:
```toml
[template]
url      = "git@github.com:org/quaqua-template.git"  # no default in the binary
ref      = "main"                 # branch, tag or SHA; resolved to a SHA at sync
path     = "workspace"            # subdir of the repo holding the template
manifest = "quaqua.toml"          # relative to path
cache    = "~/.cache/quaqua/templates"   # <cache>/<sha>/

[limits]                          # code defaults stay, these override
rpc_timeout_sec = 30
grid_lock_sec   = 15
busy_timeout_ms = 5000
```
Manifest in the template repo:
```toml
engine = ">=0.9"                  # sync refuses a template newer than the engine

[files]                           # mirrors init/table.rs, init/seeds.rs, init/mod.rs today
managed = ["AGENTS.md", "identity.md", "team.toml", ".claude/rules/*.md", "scripts/hooks/*.sh"]  # full list: init/table.rs
seed    = ["memory/rules.md", "projects/INDEX.md", "projects/_template.md", "USER.md"]  # written once, never overwritten
private = ["memory", "projects", "raw", "runs", "reports"]   # PRIVATE_DIRS
executable = ["scripts/hooks/*.sh"]   # sync asks before changing these

[roles]                           # one list, queried by role (gate, caps, drift, fence...)
instruction = ["AGENTS.md", "identity.md"]

[standing]                        # memory/standing.rs SECTIONS, render order; INDEX last, the budget drops it
budget = 8000
sections = [ { title = "persona",  path = "identity.md" },
             { title = "owner",    path = "USER.md" },
             { title = "rules",    path = "memory/rules.md" },
             { title = "projects", path = "projects/INDEX.md" } ]

[caps]                            # the only cap table (memory/caps.rs RULES_CAP, USER_CAP)
"memory/rules.md" = 2200
"USER.md"         = 1375

[formats]                         # the one report contract
# Engine skeleton today (report_contract.rs report_skeleton): Changes/Verify/Risks/Lessons.
# brief.md says Blockers/Lessons (issue #317). [formats] must pick ONE; shown here: brief.md.
brief_headers = ["Topology","Playbook","Context","Goal","Hard constraints","Current design (open to challenge)","Unknowns","Evidence","Time budget","Cost budget"]
outcomes   = ["COMPLETE","NO_CHANGES","BLOCKED","DISPUTED","REOPEN_REQUEST","DEPENDENCY_REQUEST"]
headings   = ["## Blockers","## Lessons"]
severities = ["critical","major","minor","trivial"]    # brief.md `Must fix:`

[threat]   add = [ { id = "x", pattern = "..." } ]   # ADD only, see Q2
[fence]    add = ["scripts/hooks/"]                  # ADD only, see Q2
```

## Resolution order
1. `QUAQUA_CONFIG` env → else `$XDG_CONFIG_HOME/quaqua/config.toml` → else error: "no config; run `quaqua init --from <url>`".
2. `[template]` → `git ls-remote` resolves `ref` to a SHA → `<cache>/<sha>/` (fetched once, immutable).
3. Manifest read from the cached SHA; `engine` compat checked; mismatch → error naming both versions.
4. Per setting, highest wins: `team.toml` (W) > manifest (M) > global config (G) > code default (K-adjacent numerics only). Never per call: resolved once at startup.
5. `quaqua sync` applies the SHA with the existing 3-way md5 lock (`.quaqua/template.lock`, conflicts to `.quaqua/pending/`): shows a diff first, asks before changing any `executable` file. The session-start hook only compares the cached SHA with the locked one and prints "template behind".

## Rule for the audit ("what counts as hardcode")
"No policy or vocabulary in code; mechanism stays": wire protocol, SQL, journal/activity kinds, herdr/git/gh syntax, Claude hook JSON, CI pins are K (3 of 4 lanes converged; see Q1). 181 K rows exist (core 35, guard 55, rest 45, ext 46), so "engine hardcodes nothing" would be wrong.

## Open questions
- Q1. Rule wording: adopt "no policy/vocabulary in code; mechanism stays", or something stricter? Does the CI/herdr/Claude wire constant list need a named K allowlist so reviews do not flag it?
- Q2. Guard lane: a remote manifest may only ADD threat-scan rules and fences, never remove. The engine keeps a built-in floor (`prompt_injection`, `hardcoded_secret`, `pr_merge.rs` FENCE_PREFIXES) unioned with the manifest. Accept?
- Q3. Report-contract vocabulary lives in 3 drifting copies (`pipeline.rs:763`, `worker_flow.rs:135`, `report_contract.rs:35`) plus `auditor/precheck.rs` (about 25 literals, stringly coupled to messages). Merge into one `[formats]` block; does precheck become manifest-driven or a template script?
- Q4. `gate.rs` and `herdr-gate.sh` mirror the same policy (24 and 13 commits, the real churn driver). Single source: manifest → both. Shell hooks cannot read a fetched manifest per call (<300 ms): generate the fail-closed regexes at `sync` into a cached file the script sources, or thin wrappers over `quaqua gate`?
- Q5. Hook failure on the safety path when the snapshot is missing: fail closed with an engine built-in minimal fence (guard lane), or bare "exit 2 unless binary present" (ext lane)?
- Q6. Env vars (`QUAQUA_PLAYBOOKS_DIR`, `PROMPT_WORKER_*`, `QQ_LEAD_READY_MS`, `QUAQUA_BUSY_TIMEOUT_MS`, `QQ_STALL_SEC`) contradict "only bootstrap is `QUAQUA_CONFIG`": fold into G, and drop `QUAQUA_TEAM_TOML`?
- Q7. `cos-memory` mod loads outside engine config: ask the engine (`quaqua memory targets --json`, `quaqua config get review.*`) instead of reading config itself?
- Q8. `plan.jq` layout geometry is coupled to `plan.rs`/`reconcile.rs`: move both together or keep K.
- Q9. Harness-specific strings (trust dialog, error tables, glyphs) are marked ?: they belong to drivers, not the template repo. Confirm.
- Q10. Bootstrap circularity: `.claude/settings.json:10` and `herdr-plugin.toml` hardcode the engine-repo hook path and bare `quaqua`. Keep as bootstrap and honour `QUAQUA_BIN` in the plugin toml too?
- Gap: `authority/tokenizer.rs` (351 lines) is unaudited: a hook blocked the read. Needs anh iu's approval or a manual scan before phase 3.
- Gap: Phase-brief prose (`pipeline.rs:277-365`) is rule text in Rust; becomes templates with `{placeholders}`, counted M.

## Phases and Verification (cheap first)
- approach: each phase is independently shippable; behavior unchanged until phase 4. Rejected: embedded fallback (Decision 1).
- phase `dups` — delete duplicates that already have a runtime twin; depends_on: none
  - `DEFAULT_DEFERRED` → `grid.deferred_statuses`; `INTEG_CHECK_CMD` → `merge.check`; `ROLES` → `[roles]`; `AUDITOR_MODEL` → kind model; `STALL_SEC`/`EXIT_DEBOUNCE_SEC`/`SWEEP_MS` → `notify.*`; pi stop keys → harness `stop_keys`; usage text 3x in `config/verbs.rs`; low/medium/high repeated in `tier.rs`/`grant.rs`.
  - check: targeted `cargo test -p qq-core` on config, grid, merge, auditor; `rg` for each removed const returns nothing.
- phase `formats` — report-contract manifest; depends_on: `dups`
  - one `[formats]` source read by `pipeline.rs`, `worker_flow.rs`, `report_contract.rs`, `auditor/precheck.rs`; exit codes 10-14 stay K and are not merged.
  - check: fixture with a renamed outcome word passes gate and auditor; the 3 copies are gone.
- phase `standing` — standing order, caps, seeds, private dirs (copy today's values from the manifest example; no behavior change); depends_on: `formats`
  - one `[roles]` source for the 7+ copied file lists (`gate.rs`, `caps.rs`, `drift.rs`, `standing.rs`, `fence.rs`, `journal.rs`, `situation.rs`, `threat.rs`); `"cos"` string compare and the "anh iu" persona text from W/M; `PRIVATE_DIRS`, `SECTIONS`, `RULES_CAP`/`USER_CAP`; seed bodies generated from the manifest sentinels. Audit `tokenizer.rs` first.
  - check: renamed-file fixture passes gate, caps and drift; `rg` for file-name literals outside tests returns only K rows.
- phase `fetch` — remote fetch and sync; depends_on: `standing`
  - `[template]` config, `quaqua init --from <url> [--ref]`, ref→SHA, cache `<cache>/<sha>/`, `quaqua sync` (diff, confirm on executables, 3-way lock), session-start SHA compare, hooks read a sync-time snapshot (Q4, Q5), `engine` compat check, ADD-only threat/fence (Q2).
  - check: e2e against a local bare git repo: init, sync, ref bump, executable-change prompt, missing config error text.
- phase `delete-table` — remove `init/table.rs` and `include_bytes!` templates; depends_on: `fetch`
  - check: `table_matches_directory` test deleted with its subject; `cargo build` contains no template bytes; `rg include_bytes crates/` empty.

## Current State and Next Action
- lifecycle_status: proposed; blockers: Q1-Q10 need anh iu.
- exact_next_action: anh iu answers Q1-Q5, then implements phase `dups`.
