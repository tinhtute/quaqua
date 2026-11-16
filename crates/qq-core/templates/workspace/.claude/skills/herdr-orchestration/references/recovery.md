# Recovery

Always look first: `herdr agent get <name>` then `herdr agent read <name> --source recent-unwrapped --lines 80`.

| Signal | Meaning | Do |
|--------|---------|----|
| `blocked` | approval/question UI on screen | Safe in-worktree choice → `herdr agent send-keys <name> <key>`. Destructive/outward (push, delete, network publish) → ask anh iu. |
| `timeout` / `agent_prompt_stalled` | prompt may or may not have landed | Never re-prompt blind. Text sitting unsubmitted → `send-keys <name> enter`. Still working → wait again. |
| `unknown` | herdr can't tell turn end | Wait for a nonempty, byte-stable `runs/<name>.md` via `quaqua worker prompt --resume`; read it and re-run its test before accepting. |
| `agent_not_ready` at start | startup dialog (e.g. a first-run trust prompt) | `herdr pane read <pane> --source visible`, answer, then `herdr agent wait <name> --until idle`. |
| `runs/<name>.md` missing but agent idle | worker skipped the handoff | Prompt once: "Write the summary file now; reply only the path." |

Watchdog exits 2–5: read the pane, act only within the brief's authority, then `quaqua worker prompt --resume <name>` keeps waiting without a fresh task prompt or clock. Text still sitting unsubmitted → `herdr agent send-keys <name> enter`, then resume. A still-working worker needs another wait, not another task.

For a provider/stream error (`agent_blocked`), inspect the UI first. If the one safe recovery input cannot use `agent prompt`, use `herdr pane send-text <pane_id> "<message>"` followed by `herdr pane send-keys <pane_id> Enter`, then wait. Still stuck → escalate, never loop retries.

Long answer not visible in read → ask the worker to publish a file atomically and reply with the path.

## CoS pane protection & recovery

`worker cleanup`, `grid reconcile`, and the PreToolUse `gate` refuse to close or move a pane carrying `grid_role=cos`. A manual close from the herdr UI cannot be intercepted or blocked by quaqua (herdr lacks pane pinning). Recovery if closed manually:
1. Open a new shell in the checkout root.
2. Run `claude --resume` to restore the CoS session.

