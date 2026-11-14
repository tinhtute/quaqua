Outcome:         COMPLETE
Scope:           read-only research on ~/Lab/quaqua @ 43a60bd (branch: main). Read: templates/workspace/{models.md,team.toml,AGENTS.md,rules/_all/config-dynamic.md}, templates/layouts/*.toml, crates/quaqua/src/{config,launch,doctor,usage,auditor,rules}.rs, crates/quaqua/src/{worker/spawn.rs,worker/leadgate.rs,lead/spawn.rs,pipeline/mod.rs,layout/builtin.rs,cap.rs}, crates/quaqua/tests/{worker_unit,config}.rs, tests/{test_docs,test_spawn_worker}.sh, git history. No tracked file edited.
Verification:    every claim carries file:line on 43a60bd, or a command with its real output. Runtime proof of the Q3 answer is "Gotcha 1": in a fresh mktemp -d with QUAQUA_TEAM_TOML pointed at a copy of the template whose kinds.suhuynh.model and kinds.truonglao.model read `opus-9.9` while models.md still read `opus-5.5`, `quaqua config validate` printed `ok` (exit 0) and `config get kinds.suhuynh.model` printed `opus-9.9`. Also ran: `config get kinds.suhuynh` -> `error: unknown key 'kinds.suhuynh'`, `config get kinds` -> `error: unknown key 'kinds'`; `diff templates/workspace/{team.toml,models.md,rules/_all/config-dynamic.md} ~/quaqua/` -> all three identical; `rg -n 'models\.md' crates/quaqua/src/` -> 5 hits, none a file read; `rg 'cheaper|tier' crates/quaqua/src/` -> 0 hits; `git show 4debd0b --stat` -> models.md 2 +- , config-dynamic.md 2 +- . Skipped deliberately: no cargo test (no code changed, and the release binary at 43a60bd answered every runtime question); no herdr server, no live panes; no init --update run (Q2 scope).
Unknown / risk: see "Unknowns" section.
Ownership:       scope released; no worktree, no branch, nothing held.

# Q1 — Every place outside `templates/workspace/team.toml` that states or reads a kind's harness / model / effort, or the routing default

## Overview

`team.toml` is the only **mechanical** source of harness/model/effort: `crates/quaqua/src/config.rs:654` says so in the doc comment on `validate` ("team.toml is the only source of harness/model/effort, so every kind is checked here; models.md is prose"), and `config.rs:477` is the only file read in the whole config module — no code path opens `models.md` (measured: `rg -n 'models\.md' crates/quaqua/src/` returns 5 hits, all of them a prompt string, an error message, a comment, or a tool-classification suffix — none is a file read).

But the roster values are still duplicated in **two** mechanical places and in **one prose file that is mechanically delivered**. The rest is prose that agents read.

## Mechanical — code or tests read / pin a roster value

| # | Where | What it duplicates | Note |
|---|-------|--------------------|------|
| M1 | `crates/quaqua/src/config.rs:504` — `const DEFAULT_TOML: &str = include_str!("../../../templates/workspace/team.toml")` | the whole `[kinds.*]` block + `[strategy]` | The shipped template **is** the binary's built-in default (`embedded_team_config`). One file, two roles: engine artifact and user config. Changing a kind on the engine side is a template edit + rebuild; changing it in the workspace copy needs no rebuild. |
| M2 | `crates/quaqua/tests/worker_unit.rs:1183-1240` — `test_team_toml_harness_routing` | every kind's `harness`, `model`, `effort`, `fallback`, `can_review`; `kinds.len() == 5`; `purposes["research"] == "tieude"` | Asserts the literal strings `pi`, `mistral/deepseek-v4-1-flash-1017`, `high`, `omp`, `google-antigravity/gemini-3.8-flash`, `claude`, `opus-5.5`, `medium`, `sude`, `suhuynh`. A model-id change breaks this test → engine PR. |
| M3 | `crates/quaqua/tests/config.rs:42-79` — `config_repo_team_toml_validates_ok`, `config_repo_kinds_carry_model_and_effort` | `kinds.tieude.effort == "high"`, `kinds.tieude.fallback == "sude"`, `kinds.sude.fallback == "suhuynh"` | Same class as M2, via the CLI. |
| M4 | `crates/quaqua/tests/config.rs:105` — `("| \`models.md\`", 10)` | the count of `models.md` rows in the migration plan's rule→key table (`docs/research/herdr-grid-rust-migration-plan.md`, marker `rule-key-table:start`) | Pins the prose inventory itself: adding or removing a routing rule in `models.md` without updating that table fails the test. |
| M5 | `crates/quaqua/src/worker/leadgate.rs:930-935` (test) | `strategy.build_kinds = ["tieude","sude","ni"]` | Comment: "The template's own set: `tieude`, `sude` and `ni` hold a slot, `suhuynh` and `truonglao` do not." Reads the embedded config, so it re-asserts the template. (The *code* no longer hardcodes this — `d760fa1 refactor(worker): read the Lead gate's build kinds from team.toml` removed it.) |
| M6 | `crates/quaqua/src/worker/spawn.rs:229-242` — `is_valid_name(name, kind)` | kind names as name prefixes: `name` must start with `<kind>-`, be ≤20 chars, not end `-lead`, rest `[a-z0-9-]` | Kind names are load-bearing in worker names. Renaming a kind invalidates every existing worker name and every `runs/<name>.*` sidecar. |
| M7 | `crates/quaqua/src/config.rs:376-391` — `kind_of_worker` | — | Infers a worker's kind by longest `<kind>-` prefix over `cfg.kinds` keys. Same coupling as M6, in reverse. Used by `cap.rs:14 is_task_worker_name`. |
| M8 | `crates/quaqua/src/pipeline/mod.rs:685` | `strategy.builder_reviewer_differ = "harness"` | Error text names the prose source: `"builder and reviewer must be different harnesses (models.md): …"`. Reads the real values from `[kinds]`. |
| M9 | `crates/quaqua/src/pipeline/mod.rs:703,710` | `models.md` Naming (≤20 chars) | Error text: `"builder name {builder}-{task} exceeds 20 chars (models.md Naming)"`. |
| M10 | `crates/quaqua/src/launch.rs:289+` — `build_launch_cmd` | — | Maps `harness` → CLI args and `effort` → variant/flag per harness. Pure consumer of `team.toml`; no roster values in it. |
| M11 | `crates/quaqua/src/doctor.rs:352-380` — `check_pi_models_with` | a pi kind's `model` vs `pi --list-models` | Runtime validation of the model id: warns `kind '{name}' runs on pi with model '{model}', which pi --list-models does not list … (or fix kinds.{name}.model in team.toml)`. Fixture table at `doctor.rs:1985-1992` lists `mistral deepseek-v4-1-flash-1017`. |
| M12 | `crates/quaqua/src/config.rs:1041-1070` — `get` | — | The only read surface: `kinds.<k>.{harness,model,effort,fallback,can_review,spawn}`, `strategy.{default_kind,lead_kind,default_reviewer,build_kinds,builder_reviewer_differ}`, `purposes.<p>`. Measured: `config get kinds.suhuynh` → `error: unknown key 'kinds.suhuynh'`; `config get kinds` → `error: unknown key 'kinds'`. **No way to enumerate kinds or read a whole kind.** |
| M13 | `templates/layouts/advisor.toml:19`, `templates/layouts/dual-lens.toml:19-20` | kind names as taskforce members (`suhuynh`; `truonglao` + `suhuynh`) | Embedded in the binary via `crates/quaqua/src/layout/builtin.rs:11-15` (`include_str!`). Not model/harness, but a kind rename needs an engine PR here too. |
| M14 | `crates/quaqua/src/usage.rs:619` | — | `target.ends_with("models.md")` in `is_process_path`: classifies a tool call as touching config, not as reading the roster. No values. |
| M15 | `crates/quaqua/src/worker/spawn.rs:647` | `models.md:15` "slugs are ≤27" | Enforces `<= 32 chars` on the full `<slug>-lead` name — the 27 is arithmetic on that, not a separate limit. |

## Prose — agents read it, no code parses it

| # | Where | Duplicates which `team.toml` value | Delivered how |
|---|-------|-----------------------------------|---------------|
| P1 | `templates/workspace/models.md:5-11` (the table) | `[kinds.<k>].harness`, `.model`, `.effort` for all 5 kinds — **byte-identical today** (measured: `diff templates/workspace/models.md ~/quaqua/models.md` empty; table matches `team.toml:54-85`) | Read by the agent that opens it |
| P2 | `models.md:7` "**default**: every task unless a tier is named" | `strategy.default_kind = "tieude"` (`team.toml:36`) | prose |
| P3 | `models.md:20` "No name given → `tieude`" | `strategy.default_kind` | prose |
| P4 | `models.md:21` "Implementer and reviewer must be different harnesses" | `strategy.builder_reviewer_differ = "harness"` (`team.toml:41`) | prose **and** code (M8) |
| P5 | `models.md:22` "Read-only question → `tieude`; design unclear → `truonglao` before any builder" | partial: `[purposes] research = "tieude"`, `quick = "tieude"`, `design = "truonglao"` (`team.toml:46-48`). The *trigger* ("read-only question", "before any builder") has no key. | prose |
| P6 | `models.md:23` "cheaper tier first (tieude < sude < ni < suhuynh < truonglao)" | **nothing** — measured `rg 'cheaper\|tier' crates/quaqua/src/` → 0 hits | prose |
| P7 | `models.md:26` "`build = "sude"`, `review = "suhuynh"`" | `[purposes] build` / `review` (`team.toml:44-45`) | prose |
| P8 | `models.md:29-34` "Claude out — dormant plan": `suhuynh → pi · mistral/deepseek-v4-1-flash-1017 · max`, `ni` paused, builder = `sude` | **nothing** — no preset/alternate-roster key exists (see Q3) | prose |
| P9 | `models.md:13-17` Naming block | partial overlap with M6/M9/M15 (`<kind>-<task>`, ≤20, no `-lead`, ascii) — but "Old names (`mdtree`, `mdqa`…) stay as-is" and "branch `quaqua/<name>`, report `runs/<name>.md`" have no key | prose + code |
| P10 | `models.md:3` "Update "Seen" after each run (from `runs/*.md` + project Log)" | **nothing** | prose |
| P11 | `templates/workspace/rules/_all/config-dynamic.md:29` — the same Claude-out line, with the model id `pi · mistral/deepseek-v4-1-flash-1017 · max` | duplicates P8 **and** `[kinds.suhuynh]` | **mechanically injected**: `<!-- audience: orchestrator -->` (line 1) + `crates/quaqua/src/rules.rs:45-62` `append_rules_from_dir` concatenates every `rules/_all/*.md` into the orchestrator rules dump. So a prose model id reaches CoS/Lead context on every spawn. |
| P12 | `config-dynamic.md:33` "`quaqua config validate` — so doc↔doc (models.md ↔ team.toml); chưa so runtime" | **stale / false** — see Gotcha 1 | injected prose |
| P13 | `templates/workspace/AGENTS.md:50` "Routing and model settings live in `models.md`; configuration is authoritative in `team.toml`" | contradicts `config.rs:654`; names `models.md` as the routing home | injected prose (AGENTS.md is the root doc) |
| P14 | `templates/workspace/.claude/skills/herdr-orchestration/SKILL.md:12` "1. Pick kind from `models.md`" | — | skill prose |
| P15 | `templates/workspace/.claude/skills/poteto-mode/SKILL.md:123` lists the five kind names literally (`tieude`, `sude`, `ni`, `suhuynh`, `truonglao`) | `[kinds]` key set | skill prose |
| P16 | `templates/workspace/playbooks/{shipping.md:11, eval.md:10,24,25, multi-phase-plan.md:41}` "per `models.md`" | — | playbook prose (pointers, no values) |
| P17 | `crates/quaqua/src/lead/spawn.rs:523` — Lead prompt: "read it, plus models.md for the worker roster" | — | **code emits the pointer**: every Lead spawn is told to read `models.md` |
| P18 | `crates/quaqua/src/auditor.rs:25` — auditor prompt item 2: flag stale `models.md` | — | code emits the pointer |
| P19 | `tests/test_docs.sh:41` (models.md in the mandatory-file list), `:110-111` (must not contain `to CoS → sude fix`) | — | doc test |
| P20 | `tests/test_spawn_worker.sh:3` — comment citing `models.md` Naming | — | doc test comment |
| P21 | `README.md:50` "Worker roster: nickname → kind / model / effort" | — | engine README |

## How it works, end to end (measured)

1. `quaqua worker spawn <name> <kind> <repo>` resolves `kind` from the positional arg, `--purpose` via `[purposes]` (`config.rs:363-372`), or falls back to `strategy.default_kind`; unknown kind → error (`config.rs:838-839`).
2. The kind's `harness`/`model`/`effort` come from `[kinds.<kind>]` and are turned into CLI args by `build_launch_cmd` (`launch.rs:289+`).
3. `models.md` is never opened. The only thing that reaches the worker is `runs/<name>.rules.md`, built by `rules::dump` (`spawn.rs:704-708`), which walks `rules/_all` + `rules/<repo>` only — `models.md` is not in it (measured: `rules.rs:68-80`).
4. The Lead is told to read `models.md` by its prompt (`lead/spawn.rs:523`); the CoS reads it by skill instruction (`SKILL.md:12`, `AGENTS.md:50`). That is the whole prose channel.

## Gotchas

**Gotcha 1 — `config validate` does NOT compare `models.md` to `team.toml`, and `config-dynamic.md:33` says it does.** Measured, in a fresh `mktemp -d` with `QUAQUA_TEAM_TOML` pointed at a copy of the template whose `[kinds.suhuynh].model` and `[kinds.truonglao].model` were changed to `opus-9.9` while `models.md` still said `opus-5.5`:

```
$ quaqua config validate            # release binary built at 43a60bd
ok
exit=0
$ quaqua config get kinds.suhuynh.model
opus-9.9
$ grep suhuynh models.md
| suhuynh | claude · opus-5.5 · medium | ...
```

So the drift check was removed (`git log -S'models.md' -- crates/quaqua/src/config.rs` → `5f05511 feat(worker): team.toml [kinds.*] is single source for model/effort with one-hop fallback`; `crates/quaqua/tests/config.rs:49` comment: "no models.md drift check anymore"), and `config-dynamic.md:33` was not updated. Two independent claims of the same stale fact: `config-dynamic.md:33` and `docs/research/plans/harness-effort-cost-audit.md:8,27,95`.

**Gotcha 2 — PR #271 was a two-file prose edit for a model id.** `git show 4debd0b --stat` → `templates/workspace/models.md | 2 +-`, `templates/workspace/rules/_all/config-dynamic.md | 2 +-`. The value changed was `opencode-go/deepseek-v4.1-flash` → `mistral/deepseek-v4-1-flash-1017` in the **Claude-out** block only. Neither file is a `[kinds]` key; the model id existed in prose twice and in no config key. That is the counter-example in its exact shape: a roster value with no key, duplicated across two Managed files, shipped by an engine PR.

**Gotcha 3 — the same template is both the engine's built-in default and the user's file.** `config.rs:504` embeds `templates/workspace/team.toml`. `init.rs:471` marks every file under `templates/workspace/` as `Kind::Managed`. So `team.toml` and `models.md` are the same class of file: shipped, embedded, and reconciled on `init --update`. Measured today: `diff templates/workspace/{team.toml,models.md,rules/_all/config-dynamic.md}` against `~/quaqua/` → all three identical, i.e. the workspace currently carries no local roster edit.

**Gotcha 4 — kind names are load-bearing in three mechanical places** (M6, M7, M13): worker-name prefixes, `kind_of_worker` prefix matching, and the two embedded layouts. A kind rename is not a config-only edit today.

# Q3 — What `models.md` carries that the `team.toml` schema cannot express

## The schema, measured

`Kind` (`crates/quaqua/src/config.rs:277-293`) has exactly: `harness`, `model`, `effort: Option<String>`, `fallback: Option<String>`, `can_review: bool`, `spawn: String` (`auto`/`approve`), `on_cost_cap: String` (`warn`/`kill`).
`Strategy` (`config.rs:245-257`): `default_kind`, `default_reviewer: Vec<String>`, `builder_reviewer_differ`, `lead_kind: Option<String>`, `build_kinds: Option<Vec<String>>`.
`ProjectOverride` (`config.rs:437-441`): `max_workers_per_orchestrator`, `layout`. **No kinds override.**
Top level (`config.rs:81-112`): `limits`, `timers`, `roles`, `strategy`, `kinds`, `purposes`, `opencode_models`, `taskforce`, `merge`, `grid`, `projects`, `layout`, `layouts`.

## Column by column

| `models.md` carries | Expressible in `team.toml` today? | Evidence |
|---|---|---|
| `harness · model · effort` cell | **Yes, exactly** — `[kinds.<k>]` | `team.toml:54-85`; identical values |
| `Use for` | **No.** Closest is `[purposes]` (task shape → kind, one string), which is the inverse direction and cannot hold free text or a "default" marker | `config.rs:94`; `team.toml:43-48` |
| `Avoid` | **No.** Nothing in the schema expresses a negative constraint on a kind | `config.rs:277-293` |
| `Seen` | **No.** No field, no writer | `config.rs:277-293` |
| Routing: "no name given → tieude" | **Yes** — `strategy.default_kind` | `team.toml:36` |
| Routing: "implementer and reviewer must be different harnesses" | **Yes** — `strategy.builder_reviewer_differ` | `team.toml:41`; enforced at `pipeline/mod.rs:685` |
| Routing: "read-only question → `tieude`" | **Partly.** `[purposes] research/quick = "tieude"` covers the mapping; the *trigger* ("read-only question") is not a key, and `--purpose` is opt-in — nothing routes to a purpose automatically | `team.toml:46-47`; `config.rs:363-372` |
| Routing: "design unclear → `truonglao` before any builder" | **Partly.** `[purposes] design = "truonglao"`; "before any builder" is a sequencing rule with no key | `team.toml:48` |
| Routing: "cheaper tier first (`tieude < sude < ni < suhuynh < truonglao`), escalate only on a failed run" | **No.** No cost/tier ordering field; measured `rg 'cheaper\|tier' crates/quaqua/src/` → 0 hits. Escalation-on-failure is only the one-hop `fallback`, which is *start-failure* only, not run-failure | `config.rs:282-284`; `team.toml:58,65` |
| Naming: `<kind>-<task>`, lowercase, ≤20 chars, no `-lead` | **Mechanically enforced, not configurable** — the rule is the code | `spawn.rs:229-242`; `pipeline/mod.rs:703,710` |
| Naming: "slugs are ≤27 (herdr caps at 32)" | **Enforced as `<= 32`** on the full `<slug>-lead` | `spawn.rs:647` |
| Naming: "old names (`mdtree`, `mdqa`) stay as-is" | **No key** — historical exception, prose only | `models.md:17` |
| Naming: "spawn → branch `quaqua/<name>`, report `runs/<name>.md`" | **No key** — hardcoded in the spawn path | `models.md:16` |
| `[purposes]` / `[opencode_models]` explanation | **Yes** — those are real tables; the prose just documents them | `team.toml:43-51` |
| Claude-out profile (alternate roster: `suhuynh → pi · mistral/… · max`, `ni` paused, builder = `sude`, CoS on opencode) | **No.** There is no preset/profile/roster-alternate concept. `[projects.<slug>]` cannot override `[kinds]` (`config.rs:437-441`). The only mechanism today is a **second file** + `QUAQUA_TEAM_TOML=<path>` (`config-dynamic.md:24`), which is a per-invocation env var, not a key in the file | `config.rs:81-112,437-441` |

## Who writes `Seen`

**A human/agent by hand. No code writes it and no code reads it.** Measured: the string `Seen` appears in exactly three tracked places — `models.md:3` ("Update "Seen" after each run (from `runs/*.md` + project Log)"), `models.md:5` (the column header), and `docs/research/herdr-grid-rust-migration-plan.md:150` (the rule→key table, classified `prose`). The instruction says the writer derives it from `runs/*.md` + the project Log — i.e. the CoS at cleanup, manually. There is no `quaqua` subcommand that touches it (`config get` has no such key, M12).

## Does any code parse `models.md`

**No. Zero parsers.** Measured, all 5 hits of `models.md` under `crates/quaqua/src/`:

- `usage.rs:619` — `target.ends_with("models.md")` in `is_process_path` (classifies a tool call; reads no content)
- `auditor.rs:25` — inside `PROMPT_TEMPLATE`, an instruction to the auditor LLM
- `lead/spawn.rs:523` — inside the Lead prompt string
- `pipeline/mod.rs:685,703` — inside error message text
- `config.rs:654` — a doc comment

And `config.rs:477` (`std::fs::read_to_string`) is the only file read in the config module, and it reads the `team.toml` path. Confirmed at runtime: `config validate` returned `ok` with `team.toml` and `models.md` in open disagreement (Gotcha 1).

## Unknowns

- Whether `quaqua init --update`'s reconcile behaviour on a locally edited `models.md` differs from `team.toml` — out of scope here (that is Q2). Both are `Kind::Managed` (`init.rs:471`), which is the only fact I verified.
- Whether the `[purposes]` table could carry a "read-only" trigger: `resolve_purpose` is only reachable via `--purpose` (`config.rs:363`); I did not trace every caller to prove nothing auto-selects a purpose.
- `docs/research/herdr-grid-rust-migration-plan.md:146-155` already proposes a mapping for most of these rows (`strategy.default_kind`, `strategy.builder_reviewer_differ`, `kinds.<k>.*`, "prose" for the rest). I read it as prior art, not as a decision — it is `docs/research`, and it does not cover `Seen`, `Use for`/`Avoid`, the tier order, or the Claude-out profile.

## Implications for one user-owned toml (facts only)

1. Harness/model/effort is already single-sourced mechanically (`config.rs:654`, `config.rs:477`). The remaining duplication is **prose**, plus **tests that pin the template's literal values** (M2, M3, M5) and **one embedded second copy** of the same file (M1).
2. A model-id change today costs: edit `templates/workspace/team.toml` + `models.md:7-11` (+ `config-dynamic.md:29` if it is the Claude-out id) + update M2/M3 assertions, then rebuild and ship the template. PR #271 is the measured instance of the prose half.
3. `models.md` is not parsed, so nothing enforces agreement between it and `team.toml`; `config-dynamic.md:33` claims otherwise and is stale (measured). Any "the roster text coordinators read shows the new value" success signal cannot be met by editing `team.toml` alone while the table stays hand-maintained.
4. `config-dynamic.md` is injected into orchestrator context (`rules.rs:45-62` + `<!-- audience: orchestrator -->`), so a value written there is delivered mechanically even though it is prose.
5. `config get` cannot enumerate kinds or return a whole kind (M12: `kinds` and `kinds.suhuynh` are both `unknown key`). Any read contract that must show the roster needs a new key shape.
6. Four `models.md` items have **no** schema home at all: `Use for`, `Avoid`, `Seen`, and the cheaper-first tier order. Two more are partial: the read-only→kind trigger and "design unclear before any builder". The Claude-out profile has no preset mechanism; the only lever is `QUAQUA_TEAM_TOML` pointing at a second file.
7. Kind names are load-bearing outside config: worker-name prefixes (`spawn.rs:229`, `config.rs:376`) and two embedded layouts (`templates/layouts/{advisor,dual-lens}.toml` via `layout/builtin.rs:11-15`). A rename is not a config-only edit.
8. `[kinds]` cannot be overridden per project (`ProjectOverride` = `max_workers_per_orchestrator` + `layout` only, `config.rs:437-441`), so an alternate roster cannot live inside the same file as a per-project override today.

## Playbook

1: done
2: done
3: done

## Blockers
none

## Lessons
- A doc that states a check exists can outlive the check: `config-dynamic.md:33` still claims `config validate` compares models.md, removed in 5f05511 — grep the code, not the rule, before trusting a "validate catches this" line.
- Values with no config key get copied into every prose file that needs them (PR #271: one model id, two Managed files); the duplication is the symptom, the missing key is the cause.

Verified: review-clean 43a60bd
