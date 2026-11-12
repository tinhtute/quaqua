# cos-memory

A local Claude Code mod for the CoS: three capped memory tools, a periodic review
fork, and correction capture. It decides nothing about memory itself — the
quaqua engine does, through `quaqua memory hook pre` for the capped files and
`quaqua memory scan` for one captured correction line.

```
mods/
  .claude-plugin/marketplace.json      the local marketplace
  cos-memory/
    .claude-plugin/plugin.json         identity only
    hooks/hooks.json                   {"modules":["./register.js"]}
    hooks/register.js                  the entry point
    src/memory-gate.js                 the engine gate and the line screen
    src/tools.js                       memory_add / memory_replace / memory_remove
    src/counter.js                     the review trigger
    src/review.js                      the fork and its operations
    src/corrections.js                 correction capture
    tests/                             node --test
    probe/                             claude plugin test
```

## Install

```
/plugin marketplace add ~/Lab/quaqua/mods
/plugin install cos-memory@quaqua
```

Or load it for one session with `claude --plugin-dir ~/Lab/quaqua/mods/cos-memory`.
Nothing here changes `quaqua init`: the mod lives outside `templates/`, and the
engine's own hooks are the ones `quaqua init` installs.

Kill switch: remove the folder from `CLAUDE_CODE_PLUGIN_DIRS`, or uninstall the
plugin. No setting in the mod turns it off, and none is needed.

## Mod API (Claude Code 2.1.322)

Every row was read from the installed bundle, not from documentation. The types
are `~/.claude/mods/ninja-kit/.claude-plugin/types/claude-code/index.d.ts`
(17719 lines, generated from `~/.local/share/claude/versions/2.1.322`).

| Surface | Contract | Ref |
|---|---|---|
| package | `.claude-plugin/plugin.json` + `hooks/hooks.json` = `{"modules":["./register.js"]}` | `ninja-kit/hooks/hooks.json:1`; the bundle's own scaffold writes the same line (`strings` of the 2.1.322 ELF, offsets 577994 and 664718) |
| module path | a `modules` entry resolves **beside `hooks.json`**, not at the plugin root: `./register.js` means `hooks/register.js` | `claude plugin validate`: `hooks/register.js: no such file` for a root-level file |
| load | the folder named in `CLAUDE_CODE_PLUGIN_DIRS`, `--plugin-dir`, or an installed marketplace | `~/.claude/settings.json:9` |
| entry | `export const register = (on, options) => …` | `index.d.ts:10184` |
| hook | `on(event, [filter], ($, e, next) => …)`; a failed hook is skipped unless it carries `.catch` | `:4211`, `:10210` |
| tool | `$.tool.register(ToolSpec) → { tool }`; the model calls it as `mcp__<plugin>__<name>`; served by a `tool.call` hook | `:3251`, `:14698`, `:3240` |
| tool result | `{ result }` \| `{ deny: string }` (the model reads it as the error) \| `next(e)` | `:14223` |
| fork | `$.model.fork({ prompt, effort? }) → { isAnswered, text?, reason }`; **no tools field** | `:6738`, `:2787` |
| store | `$.store.get/set/delete/keys`, JSON, 4 MiB | `:3555` |
| exec | `$.process.run(argv, { cwd, env, stdin, timeoutMs }) → { exitCode, stdout, stderr }`, no shell, 30 s default / 600 s max | `:3721` |
| fs | `$.fs.read/write/list/exists/stat` | `:3427` |
| session | `$.session.root() / cwd() / id() / model()` | `:2891` |
| plugin | `$.plugin.name`, `$.plugin.root` — and nothing else: the validator rejects `$.plugin?.name` | `:2373` |
| events | `session.start` (the first is awaited, so a `tool.register` is listed by turn one), `turn.complete`, `prompt.submit`, `tool.call` | `:4616`, `:14909`, `:3240` |
| `turn.complete` | `reason === 'answer'` and no `agentId` is the main loop's turn end; a plugin `classic.Stop` is skipped by `cc-plugin-sec-default` on Team/Enterprise | `:14909` |
| budget | 10 s per dispatch, 1 s of `.catch` grace | `:5411` |

Two of these cost real rewrites when guessed, so they are recorded here:

- **`hooks/hooks.json` resolves `modules` relative to itself.** The first draft
  put `register.js` at the plugin root and `claude plugin validate` rejected it.
- **`$.plugin` may only be read as `$.plugin.name` or `$.plugin.root`.** Optional
  chaining on it is a load error, not a style nit.

### Probes

```
$ claude plugin validate mods/cos-memory
(no errors; the hooks module is listed with its events)

$ claude plugin test mods/cos-memory
probe/load.test.ts:
(pass) the mod loads and serves memory_add [32.77ms]
 1 pass
 0 fail
```

`probe/load.test.ts` calls `mcp__cos-memory__memory_add` through the real engine
and gets this mod's own refusal back, so it fails if the module stops loading or
the hook stops being wired.

## The gate

`src/memory-gate.js` pipes a synthetic `PreToolUse` payload to
`quaqua memory hook pre` and relays the verdict:

```json
{"tool_name":"Write","tool_input":{"file_path":"<root>/memory/rules.md","content":"…"}}
```

- The path is absolute, built from `$.session.root()`. The engine matches a
  target by suffix **and** by an ancestor holding `AGENTS.md` or `.quaqua/`
  (`crates/quaqua/src/memory.rs:254`), so a relative path silently matches
  nothing and the write would be allowed.
- Empty stdout is the engine's "no objection"; a `permissionDecision: "deny"` is
  returned as the tool's error text, verbatim.
- A gate that cannot run — no `quaqua` on PATH, a non-zero exit, an unreadable
  verdict — is a refusal. It is never a silent allow.

No cap number, no secret pattern and no character count appears in this mod's
source: `crates/quaqua/src/memory.rs` (`TARGETS`, `check_text`) is the only place
they exist, and `tests/install_smoke.sh` greps to keep it that way. The one size
the mod reads is `$.fs.stat().size` beside `mtimeMs`, as a change stamp for the
review guard — not a length compared against a limit.

The engine on `PATH` must be a build that carries `memory hook pre`. An older
binary answers `unrecognized subcommand 'memory'` on stderr and exits 2, which
this mod reports as a refusal — loud, and never a silent allow. Verified against
the installed `quaqua 2.6.0`, which predates the subcommand:

```
$ echo '{"tool_name":"Write","tool_input":{"file_path":"/tmp/ws/memory/rules.md","content":"x"}}' \
    | quaqua memory hook pre ; echo "exit=$?"
error: unrecognized subcommand 'memory'
exit=2
```

## The review fork

`$.model.fork` cannot be given a restricted toolset (`ModelForkRequest` is
`{ prompt, effort? }`), so the restriction is enforced in code: the prompt asks
for a JSON array of `{op, file, …}` and no tool calls, and `src/review.js` drops
every element that is not one of the three operations on one of the two capped
files. What survives goes through the same gate as the tools.

It fires on a main-loop `turn.complete` (`reason: 'answer'`, no `agentId`) when two things hold at once:

- **due** — 10 turn ends or 15 tool calls since the last review;
- **spaced** — at least 10 user turns (`prompt.submit`, meta prompts excluded)
  since the last review. This is R12's "at most once per 10 turns", and it is
  what stops a burst of tool calls in one turn from pulling a second review
  forward.

It fires only while neither
`memory/rules.md` nor today's `memory/<date>.md` changed since the last review.

That last guard is a window, not a latch: when a review is due but memory moved,
the mod treats the agent's own write as the work the review would have asked for,
advances the baseline and restarts the window. Without that, one write would
silence every later review for the life of the store.

Counters reset only when the fork actually answered with an array; a fork that
does not answer leaves them alone so a later turn retries, and a review that
throws releases its marker. A review that never reports back at all (the process
died mid-fork) stops blocking after five minutes.

## Tests

```
node --test "mods/cos-memory/tests/**/*.test.js"   # no network, no engine
bash mods/cos-memory/tests/install_smoke.sh
```

The unit tests drive the mod through a fake engine (`tests/fake.js`) and a stub
`quaqua` is never spawned: the gate is a canned exit code and stdout, so the
payload the engine would see is asserted directly.
