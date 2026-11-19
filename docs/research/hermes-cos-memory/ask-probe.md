# ask-probe: does a hook `ask` prompt in auto / bypass mode?

Gate T1 for hermes-cos ship 3 (B1: caller-aware gate returning `ask` for the CoS).
Run 2026-11-19, claude **2.1.328 (Claude Code)**.

## Result

| mode | prompt yes/no | evidence |
|---|---|---|
| `--permission-mode auto` | **yes** (inferred; not seen on screen) | Hook fired (`hook fired: ask` logged). `echo probe` did NOT run; it landed in `permission_denials` of `claude -p --output-format json`. Decided by the `-p` method. |
| `--dangerously-skip-permissions` | **yes** (inferred; not seen on screen) | Same: hook fired, command not run, `permission_denials` lists it. Hook `ask` overrides bypass mode. Decided by the `-p` method. |

Verdict: `ask` is not silently allowed or dropped in either mode, so B1 stands. No ESCALATE.

Caveats:
- `-p` has no surface to answer a prompt, so it resolves `ask` as a denial. That shows the decision was routed to the permission step, not auto-approved. It does not show a rendered dialog.
- Interactive confirmation was **not obtained**. A tmux-driven session died at start-up (no tmux server) and was not pursued further. Re-check by hand: `cd <scratch> && claude --permission-mode auto`, then ask it to run `echo probe`.
- A headless CoS (`-p`) therefore sees `ask` as deny. B1 only works as a prompt if the CoS runs interactively.
- `--model haiku` with `--permission-mode auto` failed (`Error: No messages returned from query`, exit 1). The auto row was decided with `--model sonnet`. The skip row used haiku.

## Setup

Scratch dir from `mktemp -d` outside the repo, `git init`ed. `$S` is that dir.

`$S/.claude/settings.json`:

```json
{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"$CLAUDE_PROJECT_DIR/.claude/hooks/ask-probe.sh"}]}]}}
```

`$S/.claude/hooks/ask-probe.sh` (chmod +x):

```bash
#!/usr/bin/env bash
# PreToolUse Bash hook: return permissionDecision "ask" for 'echo probe' only.
set -euo pipefail
cmd=$(jq -r '.tool_input.command // ""')
if [ "$cmd" = "echo probe" ]; then
  echo "$(date +%s) hook fired: ask" >> "${ASK_PROBE_LOG:-/dev/null}"
  jq -n '{hookSpecificOutput:{hookEventName:"PreToolUse",permissionDecision:"ask",permissionDecisionReason:"ask-probe: forcing a prompt"}}'
fi
```

## Commands

```bash
cd "$S"
P='Run the bash command: echo probe . Then reply with only the command output, or the word DENIED if it was not run.'

# auto
ASK_PROBE_LOG=$S/hook-auto.log claude -p --model sonnet --permission-mode auto \
  --max-budget-usd 0.6 --output-format json "$P" > out-auto.json
# bypass
ASK_PROBE_LOG=$S/hook-skip.log claude -p --model haiku --dangerously-skip-permissions \
  --max-budget-usd 0.4 --output-format json "$P" > out-skip.json

jq -r '[.result, (.permission_denials|tostring)]|@tsv' out-*.json
cat hook-*.log
```

Observed (both): `.result` = `DENIED`, `.permission_denials` = `[{"tool_name":"Bash","tool_input":{"command":"echo probe",...}}]`, hook log has one `hook fired: ask` line. Total cost about $0.13.
