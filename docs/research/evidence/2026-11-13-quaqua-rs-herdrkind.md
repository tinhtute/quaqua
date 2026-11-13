Outcome:         COMPLETE
Snapshot:        none written (read-only); quaqua @43a60bd on quaqua/tieude-herdrkind3, worktree ~/Lab/.wt/tieude-herdrkind3
Scope:           read only: herdr 0.9.5 CLI help/schema/config, /opt/homebrew/Cellar/herdr/0.9.5/{README,CHANGELOG}.md, crates/quaqua/src/launch.rs, herdr.dev docs. Only file written: this report.
Verification:    commands below with real output; one planned probe (clap rejection of an unknown --kind) was BLOCKED by the repo PreToolUse hook (herdr-gate.sh: "blocked: spawn/move panes ... only via quaqua worker spawn ..."), so (2) is not measured.
Unknown / risk:  (2) is cited/inferred only. Docs pages are summarized by a fetch tool, not verbatim; treat as cited-with-risk.
Ownership:       nothing held.

## Findings

### Answer in short
1. `--kind` is a closed set of 24 values. State detection for every kind is screen-manifest based; some also get hook/plugin-reported state.
2. An unknown kind or absolute path is almost certainly rejected at CLI parse (inferred, not run). Do NOT launch codex/gemini-like harnesses through a shim; launch them in a plain pane and report state yourself.
3. A non-native CLI CAN feed state via `herdr pane report-agent` (documented extension point, any `--agent` label). It does not need `agent start`.

### (1) Exact kinds and detection
- Kinds (measured): `herdr agent start --help` @herdr 0.9.5: "--kind <KIND> Supported agent kind and canonical executable [possible values: pi, claude, codex, gemini, cursor, devin, agy, cline, omp, mastracode, opencode, copilot, kimi, kiro, droid, amp, grok, hermes, kilo, qodercli, qwen, letta, maki, muse]" (24).
- Gotcha (measured vs cited): docs list `antigravity-cli` as the integration name, CLI kind is `agy`; docs list Command Code, Crush, Prime Agent as "agent reports its own state" with no kind in the CLI enum. Source: `herdr integration status` output lists antigravity-cli; https://herdr.dev/docs/agents/ (fetched).
- Detection (cited, https://herdr.dev/docs/agents/): all supported agents are "Screen" detected (process found in the pane + bottom of live screen read against a manifest). Extra reported state ("also reports state") for opencode, pi, kimi, kilo; "state requires the integration" for omp, mastracode. Gemini and Cline: screen only, "less tested". Amp, Kiro, Maki: no integration, screen only.
- Hook-installed integrations present locally (measured, `herdr integration status`): pi v9, omp v10, claude v11, opencode v13 current; codex, copilot, devin, droid, kimi, kilo, hermes, qodercli, qwen, cursor, mastracode, antigravity-cli, grok, letta not installed. Codex needs no hook for state: CHANGELOG.md:11 "Herdr no longer installs Codex turn hooks, and Codex status comes from screen detection again" (measured, file in Cellar).
- Manifests update remotely: CHANGELOG.md:580 `[update].manifest_check`; CHANGELOG.md:672-674 `herdr agent explain` shows manifest source, matched rule and evidence (measured text). So gemini/codex detection quality can change without a herdr upgrade.

### (2) Unknown kind / other executable / wrapper
- CLI side (inferred from measured help): the enum is printed as `[possible values: ...]`, which is clap's closed-enum rejection; an unlisted kind or `/path/to/exe` fails with exit 2 before any socket call. NOT run: the herdr-gate hook blocks even a probe `herdr agent start` (see Verification).
- API side (measured, `herdr api schema --json`, protocol 22): `AgentStartParams.kind` is `{"type":"string"}` with required name, kind, pane_id; the server may validate separately (unknown).
- "canonical executable" (cited, help text + skill text from `herdr --skill`): kind names both the agent and the executable herdr launches and then expects to detect; success means "the expected agent was detected in the same terminal and is ready for input" (measured, help). A shim for another harness would either fail detection (timeout, default 30000 ms, max 300000) or be mis-detected as the kind it imitates.
- Wrapper hint (cited): CHANGELOG.md:430 and :581, `HERDR_AGENT=<agent>` env on the foreground process makes herdr use that agent's screen manifest when a wrapper hides the real process (macOS and Linux). Applies to existing kinds only; does not create a new kind.
- Smallest safe experiment if (2) must be measured (not run): `herdr --session probe-<rand>` named isolated server, set HOME/XDG_CONFIG_HOME to a temp dir and unset HERDR_SOCKET_PATH (per the Handoff rule), create one pane, then `herdr agent start t --kind notakind --pane <id>` and `--kind /bin/echo`; expect exit 2 with the enum error. Needs the gate hook lifted for that worktree or run by anh iu; also a second run with a PATH shim named `codex` in the pane to see what `herdr agent get` reports (expect `unknown` or timeout).

### (3) Idle/working/blocked/done detection and feeding state
- States (cited, `herdr --skill`): `idle`, `working`, `blocked`, `done`, `unknown`. `done` vs `idle` is "seen" state tracked by the server; "blocked means Herdr recognized an approval or question UI"; `unknown` means agent present but not classifiable. Settled wait = first of idle/done/blocked.
- Screen detection is the baseline for all kinds; hook/plugin reports override it where an integration owns lifecycle (CHANGELOG.md:595 "lifecycle authority"; :829 screen can override a stale hook `working` for Codex plan questions) (measured text).
- Feed from a non-native CLI (cited, https://herdr.dev/docs/add-herdr-support/ and measured `herdr pane report-agent --help`): `herdr pane report-agent <PANE_ID> --source <ID> --agent <LABEL> --state <idle|working|blocked|unknown> [--message T] [--seq N] [--agent-session-id ID] [--agent-session-path P] [-- RESUME_ARG...]`. Rules from the docs: `--source` stable and unique, must not start with `herdr:`; unknown `--agent` labels are "expected"; `--seq` must increase per source or reports are ignored; `pane release-agent` clears it on quit (otherwise cleared when the pane returns to an idle shell, 1-2 s). Only works under `HERDR_ENV=1` using `$HERDR_BIN_PATH`, `$HERDR_PANE_ID`. Socket equivalents: `pane.report_agent`, `pane.report_agent_session`, `pane.release_agent` (measured, schema keys).
- Display-only extras (measured, CHANGELOG.md:788): `pane report-metadata` for titles/labels, never lifecycle.
- Plugin API: plugins (herdr.dev/docs/plugins) extend panes and workflows (README.md:37); the changelog shows plugin host APIs and event hooks (CHANGELOG.md:615-619), but I found no plugin path that defines a new agent kind. Not verified against the plugin docs page.
- Not measured: whether an agent labelled by `report-agent` in a plain shell pane is accepted by `agent get/wait/prompt` as a live agent (docs say names follow the "current pane occupant"; `agent prompt` is documented for "recognized coding agent"). This is the key open question for a generic driver.

### Recommendation for the generic driver
- Launch unknown harnesses via `herdr pane run <pane> "<cmd>"` (not `agent start`), with a small wrapper that calls `pane report-agent` for working/idle/blocked and `release-agent` on exit; use `pane wait-output` / state-file as the completion signal. Tradeoffs:

| Option | Gets | Costs |
|---|---|---|
| `agent start --kind codex|gemini|cline` | native screen manifest, `agent prompt --wait`, native detection | only for the 24 kinds; gemini/cline "less tested"; harness must be that exact executable |
| `pane run` + `report-agent` wrapper | any CLI, our own state | own `--seq`, release, wrapper; `agent prompt` support unverified |
| PATH shim named as a kind | nothing safe | mis-detection or startup timeout; do not use |

## Playbook
1: done
2: done
3: done (no code changed; Evidence lines checked against the outputs above; the one blocked probe is stated, not hidden)

Verified: inconclusive 43a60bd

## Blockers
none

## Lessons
herdr-gate.sh blocks any `herdr agent start`, even an expected-to-fail probe; read-only briefs about herdr start semantics should say so or name an isolated session.
