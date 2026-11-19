# cos-memory in worker sessions: tools seen, and how it is enabled

Question (hermes-cos ship 3, memwrite T1): which `mcp__cos-memory__*` tools does a
worker session see, how does the plugin get enabled there, and does scoping it out
of workers need a change outside this repo?

## Verdict

- A worker sees **all four** write tools. It has no read-only subset.
- Enablement is **user-global**, not per-project and not set by the engine.
- Scoping it out of workers **by configuration needs a change outside this repo**
  (`~/.claude/settings.json`). An in-repo route exists only inside the plugin's
  own code, and it needs a worker signal this research did not find.

## Tools this session sees

Read from this session's tool list; no write tool was called.

- `mcp__cos-memory__memory_add`
- `mcp__cos-memory__memory_replace`
- `mcp__cos-memory__memory_remove`
- `mcp__cos-memory__memory_batch`

The plugin defines exactly these four (`mods/cos-memory/src/tools.js:11`, `:24`,
`:38`, `:51`) and registers them on `session.start`
(`mods/cos-memory/hooks/register.js:28-30`). Each tool writes only the two capped
files `memory/rules.md` and `USER.md` (`mods/cos-memory/src/memory-gate.js:7`),
resolved against `$.session.root()`, so in a worktree they target that worktree's
own paths.

## How it is enabled

| Layer | Setting | Cite |
|---|---|---|
| User settings | `enabledPlugins["cos-memory@quaqua"] = true` | `~/.claude/settings.json:116-119` |
| Install record | `cos-memory@quaqua`, `"scope": "user"` | `~/.claude/plugins/installed_plugins.json:14`, `:16` |
| Marketplace | `cos-memory` served from `./cos-memory` | `mods/.claude-plugin/marketplace.json:8-9` |
| Session-only alternative | `--plugin-dir` or `CLAUDE_CODE_PLUGIN_DIRS` (which currently lists only ninja-kit and cache-guard, not cos-memory) | `mods/cos-memory/README.md:31`, `:35`; `~/.claude/settings.json:10` |

No repo layer enables or disables it:

- Worktree `.claude/settings.json` has `permissions` and a `PreToolUse` Bash hook
  only; no `enabledPlugins` (`.claude/settings.json:2`).
- `.claude/settings.local.json` holds only the `quaqua gate` hook, written by the
  driver (`crates/qq-core/src/driver/claude.rs:10`, `:56`).
- `crates/qq-core/templates/workspace/.claude/settings.json` has no
  `enabledPlugins` either (the grep count is 0).
- Spawn passes only `--model` and `--effort`
  (`crates/qq-core/src/driver/claude.rs:40-46`, called at
  `crates/qq-core/src/task/spawn.rs:222`). It sets no plugin flag and no
  `--setting-sources`, so a worker inherits the user's settings chain.
- The one place the engine blocks the chain is the headless auditor, with
  `--setting-sources ""` (`crates/qq-core/src/auditor/headless.rs:25-26`). That is
  not the worker path.

So any claude session on this machine, including every worker, loads cos-memory
the same way the CoS does.

## Scoping it out of workers

1. **Configuration, outside the repo.** Disable it in user settings
   (`~/.claude/settings.json:119`), which also removes it from the CoS. A
   per-project disable is not possible there either: the install scope is `user`.
2. **Project override, in the repo, unverified.** A tracked
   `.claude/settings.json` could set `enabledPlugins["cos-memory@quaqua"] = false`
   for worker repos. Claude Code settings precedence says project outranks user,
   but nobody has tested it for plugins. memwrite T2 should run a probe first. The
   CoS workspace template (`crates/qq-core/templates/workspace/.claude/settings.json`)
   would have to stay unset so the CoS keeps the tools.
3. **In the plugin, in the repo.** `register.js` could skip `$.tool.register` for
   workers. No worker signal (env var or role) was found in `mods/cos-memory`, so
   the engine would need to export one first. That is still a repo change.

## Not verified

- Whether a project-level `enabledPlugins: false` overrides a user-level `true`
  (route 2). This is the cheapest path to keep everything in the repo, so test it.
- Whether the `quaqua memory hook pre` gate also refuses worker writes by path; it
  was not read in this unit.
