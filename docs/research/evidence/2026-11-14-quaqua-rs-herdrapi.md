Outcome:         COMPLETE
Snapshot:        none written (read-only); quaqua @43a60bd on quaqua/tieude-herdrapi, worktree ~/Lab/.wt/tieude-herdrapi
Scope:           read only: herdr 0.9.5 `--skill`, `api schema --json` (protocol 22), `integration status`, `pane report-agent --help`, `agent --help`, Cellar CHANGELOG.md, installed hooks (claude/pi/omp/opencode), ~/Lab/quaqua/plugins/herdr-quaqua/herdr-plugin.toml, herdr.dev docs (agents, add-herdr-support, api), herdr source at tag v0.9.5 (github.com/herdrdev/herdr, Apache-2.0, public) read via `gh api` GET only. Only file written: this report (plus scratch copies in the session scratchpad).
Verification:    Read-only commands only; no herdr command that creates/moves/closes/prompts anything was run (no `agent start`, no `agent prompt`, no `pane run`). Schema fragments and source lines below are quoted from real output. NOT measured: the behaviour of `agent get/wait/prompt` on a pane labelled by `pane report-agent` was not run (gate hook + brief forbid it); it is read from v0.9.5 source and marked "cited (source)". Docs pages were fetched through a summarising tool; quotes taken from them are short and marked "cited (docs)".
Unknown / risk:  (a) Source is v0.9.5 tag; the installed binary is 0.9.5 (`herdr --version` -> herdr 0.9.5) but a build-vs-tag mismatch is not excluded. (b) `agent prompt` body (src/api/wait.rs:177-323) and `agent_target_error_body` were not read in full; the `agent_not_ready` conclusion rests on `validate_agent_prompt`, which runs first. (c) The HERDR_AGENT spoof route is inferred, not tried. (d) Manifests update remotely (CHANGELOG.md:580), so screen-detection behaviour for kinds can move without a binary change.
Ownership:       nothing held.

## Findings

### Verdict
1. `agent get` and `agent wait` WILL act on a pane labelled only by `pane report-agent` (any label, including one that is not a kind), addressed by pane ID. `agent prompt` will NOT: it refuses with `agent_not_ready` unless the effective label parses to one of the built-in kinds and that kind is the foreground process. (cited, source, below)
2. There is no documented or coded way to register a new kind at runtime. `agent start` rejects an unknown kind with `unsupported_agent_kind`. Docs: "Adding a completely new agent still requires a Herdr binary update for process detection, labels, and integration behavior." (cited, docs/agents)
3. For an arbitrary CLI the three channels are: state out = `pane.report_agent`; prompt in = raw `pane.send_input`/`pane run` (not `agent prompt`); completion = reported `idle` read through `agent wait <pane_id>` / `events.subscribe pane.agent_status_changed`, plus a sentinel via `pane.wait_for_output`.
4. Premise check: the brief's "agent reports its own state class (Command Code, Crush, Prime Agent)" is two things in herdr. Docs list Command Code, Crush, Kiro CLI, Muse Code and Prime Agent as reporters; of those only Kiro and Muse are also in the 24-kind enum (`lookup_agent`: "kiro"|"kiro-cli", "muse"|"muse-code"|"muse-cli", src/detect/mod.rs:214,224). Command Code, Crush and Prime Agent have no kind. They use the generic `report_agent` path with their own label, so they hit the limits in verdict 1.

### 1. How herdr decides a pane holds an agent
- Target resolution (cited, source src/app/terminal_targets.rs:74-100): `resolve_agent_target` accepts (a) a pane ID whose terminal passes `target_is_agent`, or (b) a unique `agent_name`. No bare kind labels, no terminal IDs.
- `is_agent_terminal()` = `self.agent_name.is_some() || self.effective_agent_label().is_some()` (cited, src/terminal/state.rs:2349-2351). So any pane with a reported label OR a screen-detected agent counts.
- `effective_agent_label()` (state.rs:2039-2050): the hook-reported label if the authority is effective, else the screen-detected agent's label unless a process exit was seen recently. `effective_known_agent()` (state.rs:2052-2055) = that label run through `parse_agent_label`, i.e. `Some(kind)` only for the 24 kinds and their aliases (src/detect/mod.rs:188-216).
- A reported unknown label overrides a detected kind and yields no known kind. Source test (cited, state.rs:2647-2662): detected Pi + `set_hook_authority("herdr:custom","custom-agent",Working)` gives `effective_agent_label()==Some("custom-agent")`, `effective_known_agent()==None`, `state==Working`.
- Names: `agent rename` goes through `resolve_agent_target`, so it works on a reported pane by pane ID (inferred from src/app/agents.rs:64-110 + terminal_targets.rs:74). Docs/skill: names match `[a-z][a-z0-9_-]{0,31}`, are unique among live agents, and are cleared when the occupant exits, is released or is replaced (measured, `herdr --skill`).

### 2. Per-command behaviour on a non-kind reported pane
| command | result | evidence |
|---|---|---|
| `agent get <pane_id>` | works; returns AgentInfo with `agent`=label, `agent_status`, `state_change_seq`, `completion_seq`, `interactive_ready` | cited source src/app/api/agents.rs:45-53 -> `agent_info_for_target`; schema AgentInfo (measured below) |
| `agent wait <pane_id>` | works; first reads `agent get`, returns at once if state already in `until` (default idle/done/blocked), else subscribes to events; `agent_not_running` if pane closes/released/occupant changes | cited source src/api/wait.rs:132-175 (match at :150), :364-440, :523-535, :679-690 |
| `agent prompt <pane_id|name>` | REFUSED: `agent_blocked` if state blocked (agents.rs:168-177); else `terminal.effective_known_agent()` is None -> `agent_not_ready` (agents.rs:178-180); also needs the kind to be the live foreground process, else `agent_not_ready` "no longer the pane foreground process" (agents.rs:187-196, `runtime_hosts_agent` src/app/agents.rs:431-439) | cited source, src/app/api/agents.rs:140-207 |
| `agent start --kind X` | closed set: `parse_agent_label(kind)` else `unsupported_agent_kind` | cited source src/app/agents.rs:153-154, :247-249; schema `AgentStartParams.kind` is a bare string (measured) |
| `agent send-keys` | same known-kind + foreground checks as prompt | cited source agents.rs:398-412 |
`agent prompt` only ever works on screen-detected or hook-reported KINDS. That settles the open question in raw/2026-11-13-quaqua-rs-herdrkind.md:35.

### 3. Socket methods and exact params (measured, `herdr api schema --json`, protocol 22, request $defs)
- `agent.get` / `agent.explain` / `agent.focus`: `AgentTarget {target:string}` required target.
- `agent.prompt`: `{"target":string,"text":string,"wait":AgentPromptWaitOptions|null}` required target,text. `AgentPromptWaitOptions {timeout_ms:uint64|null, until:[AgentStatus]}`.
- `agent.wait`: `{"target":string,"timeout_ms":uint64|null,"until":[AgentStatus]}` required target. `AgentStatus` = idle|working|blocked|done|unknown.
- `agent.start`: `{"name","kind","pane_id" required; "args":[string]; "timeout_ms": >3000 and <=300000}`.
- `pane.report_agent`: `{"pane_id","source","agent","state" required; "message":string|null; "seq":uint64|null; "agent_session_id","agent_session_path":string|null; "resume_argv":[string]|null}`; `PaneAgentState` = idle|working|blocked|unknown (NO `done`).
- `pane.report_agent_session`: `{pane_id,source,agent required; seq, agent_session_id, agent_session_path, session_start_source, resume_argv}`.
- `pane.release_agent`: `{pane_id,source,agent required; seq}`. `pane.clear_agent_authority`: `{pane_id required; seq, source}`.
- `events.subscribe`: `{"subscriptions":[Subscription]}`; Subscription includes `{"type":"pane.agent_status_changed","pane_id":string,"agent_status":AgentStatus|null}`, `pane.agent_detected`, `pane.exited`, `pane.output_matched {pane_id,source,match,lines,strip_ansi}`.
- `events.wait`: `{"match_event":EventMatch,"timeout_ms":uint64|null}`; EventMatch includes `{"event":"pane_agent_status_changed","pane_id","agent_status"}`, `pane_agent_detected {pane_id, agent}`, `pane_exited`, `pane_output_changed {pane_id,min_revision}`.
- `pane.wait_for_output`: `{pane_id,source,match required; lines, strip_ansi, timeout_ms}`; `match` = `{"type":"substring"|"regex","value":string}`; `source` = visible|recent|recent_unwrapped|detection.
- `pane.send_input`: `{pane_id required; text:string; keys:[string]}`; `pane.send_text {pane_id,text}`; `pane.send_keys {pane_id,keys}`.
- AgentInfo response fields worth using (measured): `agent`, `agent_status`, `completion_seq` ("The current idle transition completed work, independently of who has viewed it."), `state_change_seq`, `interactive_ready`, `launch_pending`, `screen_detection_skipped`, `revision`, `pane_id`, `terminal_id`, `name`, `agent_session`.
- CLI equivalents (measured, `herdr pane report-agent --help`): `herdr pane report-agent <PANE_ID> --source <ID> --agent <LABEL> --state <idle|working|blocked|unknown> [--message T] [--seq N] ... [-- RESUME_ARG...]`.
- Docs add: `agent.prompt` with `wait` "submits the prompt and starts the wait in one request, avoiding a race between separate calls"; `agent.wait` "is server-owned and event-driven" and "pins the resolved pane occupant so a replacement cannot satisfy the wait"; subscriptions "do not replay events retained before that point" and a lagging client gets `events_lost` then the connection closes (cited, docs/api).

### 4. What the shipped integrations call (measured, local files)
- claude (v11, ~/.claude/hooks/herdr-agent-state.sh:80-84): ONLY `pane.report_agent_session` on SessionStart, source `herdr:claude`, agent `claude`, seq = time_ns. It never reports state; Claude state comes from screen detection. Not in `full_lifecycle_hook_authority`.
- pi (v9, ~/.pi/agent/extensions/herdr-agent-state.ts:114-148, :69, :79): `pane.report_agent_session` on session_start, `pane.report_agent` {source `herdr:pi`, agent `pi`, state, message, seq} on `agent_start` (working) and `agent_settled`. No `pane.release_agent` call in any of pi/omp/opencode (grep, rc=1).
- omp (v10, herdr-omp-agent-state.ts:147-202): same two methods, source `herdr:omp`; idle debounce 250 ms and retry grace 2500 ms (`HERDR_OMP_IDLE_DEBOUNCE_MS`, `HERDR_OMP_RETRY_GRACE_MS`) so provider-retry gaps do not report idle.
- opencode (v13, herdr-agent-state.js:105-207): `pane.report_agent_session` plus `pane.report_agent` with working/blocked/idle, source `herdr:opencode`.
- All of them gate on `HERDR_ENV=1`, `HERDR_SOCKET_PATH`, `HERDR_PANE_ID`, speak newline-delimited JSON over the unix socket with `{"id","method","params"}`, and use a monotonically increasing `seq` (Date.now()*1000 + n). They use the reserved `herdr:` source prefix that third parties must not use (cited, docs/add-herdr-support).
- ~/Lab/quaqua/plugins/herdr-quaqua/herdr-plugin.toml (measured): subscribes via plugin `[[events]]` to pane.created, pane.agent_detected, pane.agent_status_changed, pane.moved, tab.closed, workspace.closed; it reports nothing.

### 5. Lifecycle authority: screen vs reported state
- Docs: "When an integration also reports state, Herdr uses those reports instead of reading the screen." and "For agents that support Herdr, the agent's own reports decide the state." (cited, docs/agents). "Blocked detection is deliberately strict for screen-manifest agents."
- Full-lifecycle set (cited, src/detect/mod.rs:330-340): only (herdr:pi,pi), (herdr:omp,omp), (herdr:mastracode,mastracode), (herdr:opencode,opencode), (herdr:kilo,kilo), (herdr:kimi,kimi). Under these, detected screen state is ignored unless the process exited or the detected kind conflicts (`should_ignore_detected_state_under_full_lifecycle_hook`, state.rs:~846-853); a visible blocker does not override (state.rs:3829, test).
- Session-identity-only (detect/mod.rs:342-350): hermes, qwen, letta, antigravity report session only; `set_hook_authority_at` returns early so they never own state (state.rs:701-711).
- Any other (source,label) pair, including a non-kind label with your own source, is a plain hook authority: always "effective" (`hook_authority_is_effective`, state.rs:2032-2037, `!full_lifecycle || ...`), so the reported state wins; a weak screen blocker does not override it, and for the same known agent a visible blocker newer than the hook can (visible_blocker_overrides_hook, state.rs:~2080-2092).
- A known-kind label that disagrees with the detected agent is dropped (`known_agent_label_conflicts_with_detected_agent`, state.rs:~722), so reporting `--agent claude` from a non-claude process is rejected when something else is detected; a non-kind label is not subject to that check.
- Clearing: `pane.release_agent` clears name/state/resume at once; if the process exits without release herdr notices when the pane is back at an idle shell (cited, docs/add-herdr-support). Process-exit validation stays active even under lifecycle authority (CHANGELOG.md:595). `report` ignored unless seq increases per source (cited, docs).
- `done` is server-derived (idle plus unseen completion); a reporter can only send idle/working/blocked/unknown (schema PaneAgentState, measured), so `agent wait` on a reported agent settles on `idle`.

### 6. Registering an agent that is not one of the 24 kinds
- No kind registry or plugin hook found. Docs (cited): "Adding a completely new agent still requires a Herdr binary update for process detection, labels, and integration behavior" and "It takes a few calls to Herdr's CLI or socket, and no change to Herdr." for the reporting route. Plugins (herdr-plugin.toml) can run commands on events; none defines a kind (inferred from the manifest fields seen).
- Server enforces the closed set on start (`unsupported_agent_kind`, app/agents.rs:153-154) even though the schema type is a plain string.
- `HERDR_AGENT=<kind>` on the foreground process (CHANGELOG.md:430, :581; read from process env in src/platform/mod.rs:603-611, macOS procargs in platform/macos.rs:1005-1011) makes herdr treat the process as that KIND, which would satisfy `runtime_hosts_agent` for prompt. INFERRED, untried: it borrows that kind's screen manifest (mis-detection risk, as in the earlier report's shim finding), so it is a spoof, not a registration.

### 7. Channels for an arbitrary CLI
| channel | herdr primitive | works for a non-kind process? | source |
|---|---|---|---|
| state out | `pane report-agent` / `pane.report_agent` with own `--source` (not `herdr:`), own `--agent`, increasing `--seq`; read back with `agent get <pane_id>` or `agent list` | yes (any label; reported state wins over screen) | measured schema + help; cited docs/add-herdr-support; source state.rs:2647 |
| state stream | `events.subscribe` `pane.agent_status_changed {pane_id}`; or `events.wait` `pane_agent_status_changed` | yes (event is by pane, not kind) | measured schema; cited docs/api |
| prompt in | `agent prompt` | NO: `agent_not_ready` (no known kind) | cited source agents.rs:178-196 |
| prompt in | `pane run <pane> "<cmd>"`, `pane.send_input {text,keys}` / `pane.send_text` | yes (raw terminal; no bracketed-paste/Enter handling, no submit-observed-activity gate) | measured schema; skill text |
| completion | reported `idle` after `working` seen: `agent wait <pane_id>` (default idle/done/blocked) | yes, with caveat: returns at once if already idle (wait.rs:150), so first wait `--until working` or compare `state_change_seq`/`completion_seq` from `agent get` before and after sending | cited source; measured AgentInfo schema; inferred use |
| completion | sentinel line in output: `pane wait-output --match` / `pane.wait_for_output`, or `pane_exited` / `pane.exited` event | yes, independent of any state report | measured schema; skill text |
| completion | `agent_not_running` from `agent wait` when pane closes or the report is released | yes | cited source wait.rs:679-690 |

### Recommendation for Q37 / R6 (generic driver)
- Degraded mode is real and cheap: reported state + `agent get/wait` by pane ID work; `agent prompt` does not. Carry our own prompt delivery (`pane.send_input`) and use `agent wait --until working` then default wait, or sentinel via `pane.wait_for_output`, as the completion signal. R6's wording "pane run plus pane report-agent" holds; add "no `agent prompt --wait`" to the degraded list.
- Use a source like `quaqua:<worker>` and a label that is NOT a kind name (a kind-named label that disagrees with the detected process is dropped, and `claude`-like labels invite the prompt path to run the foreground check).
- Treat "idle after a fresh working" as completion; a reporter cannot emit `done`, so never wait for it on a reported pane.
- Do not use the HERDR_AGENT spoof for non-kind harnesses.

## Playbook
1: done
2: done
3: done (Verified line below; the one unmeasured behaviour, `agent prompt` on a reported unknown label, is stated as cited-from-source, not run)

Verified: review-clean 43a60bd

## Blockers
none

## Lessons
herdr source is public (github.com/herdrdev/herdr, tag v0.9.5); read handlers via `gh api repos/herdrdev/herdr/contents/<path>?ref=v0.9.5` instead of probing a live server when the gate blocks probes.
