# Research: dynamic harness / model / effort via toml (quaqua fanout)

Engine @43a60bd. Scope: orchestrator (CoS/Lead on herdr) fans out TUI agent CLIs into panes. Not API calls.

## Verdict

Today `[kinds.*]` in team.toml already makes **model and effort** dynamic per kind. What is closed is the **harness set**: `enum Harness {Claude, Opencode, Omp, Pi, Agy}` plus `match` arms across the source tree.

Every prior-art tool splits harness behaviour the same way. A harness is code (a driver). A kind is data that picks a driver and fills in model, effort, args and env. No tool drives a harness's whole lifecycle from config alone.

Recommendation, in two layers:
1. Keep real drivers in code behind one `HarnessDriver` registry. Open everything that is data to toml.
2. Add a **generic driver** so that a new herdr kind (codex, gemini...) runs from toml alone. Without it, every new harness still needs a code change, which does not meet the ask.

## What is toml-only

| Change | Today | After R10–R13 | After the generic driver (R14) |
|---|---|---|---|
| model / effort of an existing kind | yes | yes | yes |
| new kind on an existing harness | yes | yes | yes |
| extra args, env for a kind | no | yes | yes |
| auditor models, opencode allowlist, default provider | no (src literals) | yes | yes |
| **new harness (codex, gemini...)** | no | no (needs a driver module) | **yes, degraded**: no cost, no retry parse, no verified stop, `can_review = false` |
| full-fidelity new harness | no | code: 1 module + 1 registry line | same |

How a driverless harness gets `model`, `effort` and the rules file (design, **untested**):

```toml
[harnesses.codex]
herdr_kind = "codex"
efforts    = ["none","minimal","low","medium","high","xhigh"]
args       = ["--model", "{model}", "-c", "model_reasoning_effort={effort}",
              "-c", "developer_instructions_file={rules_file}"]   # codex key name unverified
# rules_mode = "arg" | "agents_md_dir"  (agents_md_dir writes runs/.<h>/<name>/AGENTS.md, like agy)

[kinds.cadao]
harness = "codex"
model   = "gpt-6.1-sol"
effort  = "high"
```

Without the `{model}`, `{effort}` and `{rules_file}` placeholders, the kind's model and effort would be silently ignored. Validation must therefore require `{model}` in a generic harness's args.

## Evidence: the current code (measured, grep @43a60bd)

Per-harness branch sites (inventory from this session; regex line counts are not branch counts, and harness/mod.rs is the registry itself):
- `launch.rs` `build_launch_cmd`
- `worker/prompt/cost.rs`, `worker/prompt/retry.rs`
- `worker/spawn.rs:757,825`, `worker/stop.rs:21`
- `status.rs:1398`, `workflow.rs:228`, `pipeline/phases/mod.rs:318`
- `init.rs:1129`, `doctor.rs:358-402`, `usage.rs:350,389,1107,1372`
- `engine/activities.rs:674`, `bus/events.rs:1287`
- `config.rs:50,295-304,403,919,941`, `auditor.rs:277-444`

Hardcoded defaults outside toml:
- `auditor.rs:277-444`: model ids and CLI order.
- `config.rs:295-304`: opencode model allowlist.
- `launch.rs`: opencode default provider `opencode-go`, and the claude model prefix rewrite.

Launch path:
- `herdr agent start --kind <K> --pane <id> -- <args>` (herdr 0.9.5).
- `--kind` is "supported agent kind **and canonical executable**". Inferred from that help line: herdr does not accept a custom binary or wrapper. A PATH shim is untested.
- herdr knows 24 kinds and owns readiness detection for them.
- quaqua applies env per pane through `LaunchCmd.split_envs` (`launch.rs:273`). Per-kind env is therefore possible.

## Prior art

| Tool | Driver (code) | Instance (config) | Generic fallback |
|---|---|---|---|
| T3 Code | provider adapter per agent (Claude SDK, Pi RPC mode per fetched page; Codex protocol inferred) | "instance" = one configuration + account | any ACP agent |
| workmux (tmux fanout) | built-in `type` (claude, codex, pi, agy, omp, gemini...) owns prompt injection, resume, skip-perms | `agents.<name>: {type, command, args, env{from_env}}` | bare command, prompt injection only |
| vibe-kanban | executor per agent | `executors.<AGENT>.<VARIANT>` (model, effort, sandbox); universal `append_prompt`, `additional_params`, `base_command_override` | none |
| herdr | 24 kinds with state detection | args after `--` | none |

T3's split maps directly onto quaqua. Its rule is that "a driver kind identifies an integration; an instance identifies one configuration and account lifecycle", which is quaqua's `harness` and `kind`.

Most transferable to quaqua: **workmux**, the same pane fanout model. T3's adapter contract transfers, but its launch mechanics (SDK/RPC, not panes) do not.

## Data vs code split for quaqua

**Data (toml), per kind or per harness profile:**
- `model`, `effort` (already there)
- `args` (extra, appended)
- `env` (literal or `{from_env = "NAME"}`; never secrets in a checked-in team.toml)
- `efforts` list override
- `model_prefix`
- the auditor's model/CLI defaults
- the opencode allowlist
- the opencode default provider

**Code (driver), irreducible:**
- How the system prompt is delivered: flag (pi, claude), file (claude), generated AGENTS.md dir (agy), JSON env (opencode).
- Read-only review args.
- claude: settings.json and the trust dialog.
- agy: trustedWorkspaces.
- Cost/session readers (`cost.rs`).
- Error-line parsers (`retry.rs`).
- Idle detection mode (hook vs fallback).

## Driver catalogue

**Local drivers (installed):** claude, pi, agy, opencode. omp is being dropped.

**codex (doc-only, not installed, unverified):**
- `--model`, `-c model_reasoning_effort=<none|minimal|low|medium|high|xhigh>`.
- Instructions via `-c developer_instructions=...` or `AGENTS.md`.
- Per-name profiles live in `~/.codex/<name>.config.toml` (official config page, fetched).
- The bypass/yolo flag is unverified.

**gemini (doc-only):**
- `-m`, `--approval-mode=yolo`, `GEMINI_SYSTEM_MD` (docs disagree on whether it overrides or appends).

**Generic driver** for a herdr kind with no quaqua driver: possible, since herdr owns readiness. It would have:
- no stop keys (so the stop gate refuses, as with pi today)
- unknown cost
- no retry parse
- `can_review = false` forced

## Constraints the design must enforce

1. `builder_reviewer_differ = "harness"` compares **harness identity (the herdr kind)**. It must not compare the driver, because codex and gemini would share the generic driver. It must not compare the profile name either, because two profiles wrapping claude would then bypass it.
2. A driver without read-only review args fails validation when `can_review = true`.
3. `stop_keys` stay driver-owned. If toml may ever set them, they are unverified. The `worker stop` "verified stop sequence" gate must treat them as unverified unless a driver vouches. Live evidence: pi has none today.
4. Effort is validated per driver, not per model. The model id stays an opaque string. `doctor` may probe it (`pi --list-models`).
5. Old journal/bus rows naming `omp` (or any retired harness) must still parse. Unknown harness fails only at spawn and validate time.
6. env secrets come only through `from_env`.

## Proposed requirements (to add as R10+ in team-config-single-source.md, after anh iu agrees)

- **R10.** One `HarnessDriver` registry. Every per-harness branch (launch, stop, idle, cost, retry, status, workflow, auditor) goes through it. Adding a driver means one module plus one registry line.
- **R11.** `[kinds.*]` gains `args` and `env` (`from_env`). `[harnesses.<name>]` may override `efforts`, `model_prefix` and `default_provider`.
- **R12.** Auditor defaults and the opencode allowlist move to toml (`[auditor]`, `[harnesses.opencode]`). No model id literals remain in src outside tests.
- **R13.** Validation rules 1, 2, 4, 6 above. Old-data tolerance (rule 5) is covered by a test.
- **R14.** Generic driver: `[harnesses.<name>]` with `herdr_kind`, `efforts`, an `args` template (`{model}` required, `{effort}`, `{rules_file}`), and `rules_mode`. It runs in degraded mode (see table). This is what makes codex toml-only.
- **R15 (later).** A full codex driver (cost, retry, review args) once codex is installed and verified.

## Open questions for anh iu

- Generic driver (degraded, toml-only) or a full driver per harness? This decides whether codex is toml-only.
- Should config ever set stop keys, or do they stay driver-owned?
- Should R10–R14 extend the locked plan, or become a separate plan after the omp removal lands?

## Sources

All URLs below were fetched with TinyFish `fetch_content` unless marked as WebSearch summaries.

- T3 provider constraints: https://raw.githubusercontent.com/pingdotgg/t3code/main/docs/internals/providers.md
- T3 README: https://raw.githubusercontent.com/pingdotgg/t3code/main/README.md
- T3 deep dive: https://flaviocopes.com/t3-code/
- workmux agents: https://workmux.raine.dev/guide/agents
- vibe-kanban profiles: https://vibekanban.com/docs/configuration-customisation/agent-configurations.md
- Codex config basics (official): https://developers.openai.com/codex/local-config
- agentapi README (deprecated; API-shaped, demoted): https://raw.githubusercontent.com/coder/agentapi/main/README.md
- Codex instructions keys, WebSearch summary only: https://codex.danielvaughan.com/2026/05/07/codex-cli-instruction-stack-six-surfaces-agents-md-rules-hooks-skills/
- Gemini CLI, WebSearch summary only: https://geminicli.com/docs/cli/system-prompt/
- Local: `herdr agent start --help`; `pi/agy/claude --help`; grep of ~/Lab/quaqua @43a60bd
