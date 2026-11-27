# Recovery

Always look first: `herdr agent get <name>` then `herdr agent read <name> --source recent-unwrapped --lines 80`.

| Signal | Meaning | Do |
|--------|---------|----|
| `blocked` | approval/question UI on screen | Never answer it for the worker: `herdr agent read <name>`, hand the pane text to anh iu, then `quaqua worker prompt <name> --resume` once they answer. |
| `timeout` / `agent_prompt_stalled` | prompt may or may not have landed | Never re-prompt blind. Text sitting unsubmitted → `send-keys <name> enter`. Still working → wait again. |
| `unknown` | herdr can't tell turn end | Wait for a nonempty, byte-stable `runs/<name>.md` via `quaqua worker prompt <name> --resume`; read it and re-run its test before accepting. |
| `agent_not_ready` at start | startup dialog (e.g. a first-run trust prompt) | `herdr pane read <pane> --source visible`, answer, then `herdr agent wait <name> --until idle`. |
| `runs/<name>.md` missing but agent idle | worker skipped the handoff | Prompt once: "Write the summary file now; reply only the path." |

`worker prompt` exits: `1` brief not landed (send it again) · `64` usage error or rejected brief (fix the command or brief, never retry as is).

Any daemon verb: `69` daemon unreachable (start it, never retry); `1` with `rpc failed:` means the daemon was reached but the call did not finish (check `quaqua ping`, don't blind-retry). Watchdog exits: `2` hard cap (7200 s) · `3` stall · `4` time budget · `5` blocked · `6` malformed report · `7` provider error needing a human · `8` core died (killed, OOM, panic). After any of 3–8 (2 is the absolute cap; `--extend <sec>` cannot pass it): read the pane, act only within the brief's authority, then `quaqua worker prompt <name> --resume` keeps waiting without a fresh task prompt or clock. Text still sitting unsubmitted → `herdr agent send-keys <name> enter`, then resume. A still-working worker needs another wait, not another task.

For a provider/stream error (`agent_blocked`), inspect the UI first. Raw `herdr pane` input verbs are blocked by the gate for every caller, CoS included. `agent prompt` is rejected while the agent is `blocked`, and `herdr agent` has no `send-text`, so free text cannot reach a blocked agent: use `herdr agent send-keys <name> <key>` for a key-only fix (e.g. `enter`), otherwise escalate to anh iu with the pane read. Still stuck → escalate, never loop retries.

Long answer not visible in read → ask the worker to publish a file atomically and reply with the path.

## CoS pane protection & recovery

The PreToolUse `gate` blocks every `herdr pane close` (and pane move/split); only the daemon closes panes. A manual close from the herdr UI cannot be intercepted or blocked by quaqua (herdr lacks pane pinning). Recovery if closed manually:
1. Open a new shell in the checkout root.
2. Run `claude --resume` to restore the CoS session.

