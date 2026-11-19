# Gate: allow only known spellings (sude-gateallow)

Base: `quaqua/hermes3-integ` at cdd727d. Scope: `crates/qq-core/src/authority/gate.rs`
(hook script `herdr-gate.sh` is covered below). Closes review-3 findings 1, 3, 4 and 10.

## Premise under test

> The gate can enumerate the unsafe shell spellings.

Three reviews found holes of one class each time: a wrapper flag the gate did not
list (`env --chdir`, then `env -iC`), a reader flag it did not list (`sed w`, then
`sed r\<nl>w`, then `rg --pre`), a wrapper it did not list (`caffeinate`, `watch`).
Each fix added the spellings the reviewer had just found. The next review found
the next ones.

### The test

1. Write at least 12 spellings in the same classes that no review has listed
   (other clustered flags, other wrappers, other reader flags that exec or
   write). Commit them red, on the unchanged gate.
2. Change the gate by structure only. Those spellings must go green with no
   case added for them.

The first half is a measurement: if the premise held, the held-out set would
already be refused. It is not (see the red commit). The second half is the
check on the new structure.

## Chosen structure: the fast path is a known spelling; everything else is the strict answer

The old shape: a command is a pass-through unless it matches a list of bad
spellings. The new shape: a command takes the cheap path only when it matches an
exactly known spelling; every other shape takes the fail-closed path.

### 1. Wrappers (`sudo env nice timeout xargs …`)

- Each wrapper has an exact table: no-value short flags, value short flags,
  optional-attached-value short flags, no-value long flags, value long flags.
- A short cluster (`-iC`, `-Eu`, `-tn`, `-vs`) is exploded letter by letter through
  the table. A value letter takes the rest of the token, or the next token.
- The leading flags are skipped only if every token is in the table. The first
  token that is not (an unknown flag, an unknown letter in a cluster) makes the
  command word unknown.
- Unknown command word means: every suffix of the remaining words is checked as
  a command through the ordinary path, shallow (`check_cmd`, no nested suffix
  scan; a wrapper word passes, its words are suffixes of the same scan), so the
  db, path-writer and grant checks still run (a superset of every reading of which word is the command), every
  word that holds a space is scanned as a command line (for an option
  also the text after `=` and after each cluster letter: `--command='…'`,
  `-qc'…'`; and the command word itself: `sudo -s '…'`), and the target is
  unknown (high tier). This replaces the both-readings fork and the six-fork cap:
  one linear scan covers all readings, so no table of value-taking options has to
  stay complete, and no fork budget can be exhausted.

### 2. Unlisted commands (`caffeinate script doas flock watch parallel ssh …`)

A command word that is not a known wrapper, git, gh, herdr, quaqua, a shell or a
provable reader gets the same scan as an unknown wrapper word: suffixes as
commands, spaced words as command lines, unknown target. A guarded pair that
appears as consecutive argv words, or inside one quoted word, is treated as that
guarded command. Provable readers (below) are exempt, so `echo git push` and
`grep -rn git push docs/` stay free.

### 3. READ_ONLY

- Readers that have a flag or program that can write or execute (`sed sort uniq
  awk rg bat diff file git`) carry a flag table or a grammar. `sort rg bat diff
  file awk` are exact: any flag or operand shape outside the table means not
  read-only. `sed` and `uniq` keep their own parsers and `git` its subcommand
  list; they are not exact tables, and an unknown flag on them is not refused
  by this change (`sort -oUSER.md`,
  `sort -ruoX`, `rg --pre=tee`, `bat --pager=…` all fall out of the table).
- Readers that have no such flag in any implementation (`cat ls stat head tail wc
  du hexdump od strings checksum tools cmp realpath readlink basename dirname nl
  cut tac jq echo printf test [`) stay on the list unconditionally. This is the
  one place the design still relies on a claim about a tool, made from the
  documented GNU and BSD (macOS) option lists, not re-verified against man pages
  on every platform. The tests name live files through `cat ls head wc grep
  hexdump od` and the armed readers; a writing flag found on an inert reader
  moves that reader to an exact table.
- Dropped from the list: `xxd` (the second operand is the output; the operand
  count is not provable once `-r`, `-p`, `-i` and `-s` change it), `less` and
  `more` (`-o`, `--log-file`, `+cmd`, `!cmd`). A pager has no use in a hook-run
  Bash call.
- `sed`: the existing allow-list parser stays. Fix: `r` and `R` file names end at
  the newline (no backslash continuation), so a following `w` line is parsed and
  rejected. A script that does not parse completely is not read-only.
- `awk`: read-only only when the program text has none of `>`, `|`, `system`,
  `getline`, `@`, and no flag but `-F`, `-v`, `--`.

### 4. Kept from gatefix3

The both-readings idea (now subsumed by the suffix scan), the fail-closed rules,
the cd and `GIT_DIR` retargeting, the grant tripwires, and the Write/Edit gate
are unchanged.

## Hook fallback (`herdr-gate.sh`)

The fallback only adds blocks: it runs when the binary is missing or stale, and
`exec`s the binary otherwise, so it can never allow what the binary refuses. It
has no wrapper parser to widen: its regexes use `git[^"]*[[:space:]]push`, which
matches a push after any wrapper words, quoted or not. Measured: all 48
push/herdr probes of the gate_fix4 tests (review-3 and held-out) exit 2 through
`herdr-gate.sh` with `QUAQUA_BIN` unset to a missing binary; the same probes exit
0 on the cdd727d binary in 40 of 48. The script is unchanged. Known and
pre-existing: the fallback's live-file writer list is a tool list (`tee mv cp …`),
so reader-writers such as `xxd in USER.md` are only caught by the binary.

## Results

- Red (5dddbd1, f3f0f7d): 7 of 8 gate_fix4 tests fail at 5dddbd1; at f3f0f7d 8
  of 9 (the worker-tier test joins); the ordinary-commands guard passes. Of the 48 push/herdr probes, 40 exit 0
  (allowed) on the cdd727d binary; on the fix build, 0.
- Mutation check: removing the letters `E` (sudo), `C` (env), digits (nice),
  `v` (timeout) and `t` (xargs) from the tables leaves every block and ask test
  green; only the free-path guard fails (`xargs -tn 1 echo git push` becomes a
  block). A missing table entry costs a false ask, never a hole.
- Replay (4,934 CoS commands): ALLOW 4,718→4,717, BLOCK 174→175, ASK 42→42.
  One move, ALLOW→BLOCK, a false block: a shell function `g` that feeds the
  string `herdr pane split …` to the hook through `jq`, with the herdr text as
  a quoted argument of an unlisted command. ALLOW→ASK: 0. False moves
  1 of 4,934 (0.02%).

- Second round (167575b red, 7588352 fix), after review of the first fix: the
  first `check_flat` was a second dispatch table without the default arm's db,
  path-writer and grant checks, so `env -Z rm runs/qq.db` went from block (base)
  to allow. Now `check_flat` runs `check_cmd` shallow. Both red tests fail at
  167575b and pass at 7588352; the replay is unchanged (one move, #166).

## Rejected alternatives

- **Add the review's spellings** (cluster letters `C u t n s S`, wrappers
  `caffeinate script doas …`, `xxd`, `rg --pre`, `bat --pager`, `-o` attached).
  This is rounds 1-3 again. Refused by the premise test.
- **Block any unknown flag on a wrapper, outright.** Closes the class but turns
  every `env --some-new-flag ls` into a block. The suffix scan keeps the strict
  answer only where a guarded command is actually present.
- **Reconstruct the real argv grammar of each wrapper** (a full getopt per tool).
  Tables of known spellings plus a strict fallback give the same safety without
  the grammar having to be complete or version-correct.
- **Exact allowlist for every reader, with no inert group.** More typing, more
  false asks on `ls`/`cat` flag drift, no safety gain for tools that cannot write
  or exec. Revisit if a held-out probe finds a writing flag on an inert reader.
- **Scan the whole command text for `git` + `push`.** Already used for
  stdin-fed shells; as a general rule it blocks prose (commit messages, prompts).

## Measurement plan

Replay `corpus.jsonl` (4,934 real CoS Bash commands) through the CoS rig on
cdd727d and on the fix build. Report ALLOW→ASK and ALLOW→BLOCK, each move
classified as a real write or a false ask. Target: false asks at most 0.5% of
commands (about 24). Not met means reporting the number and the cause, not
loosening the gate.

## Round 3: git's own option, config and env surface (review 4 finding B)

The allowlist premise now covers git itself (`gate.rs`, `check_git_at`).

- **Global options.** An exact table (`GIT_NOVAL`, `GIT_VAL`, `GIT_GLUED`). A value-taking option consumes its value (`--attr-source HEAD`, `--super-prefix x`); an option not in the table makes the call unknown: ask for the CoS, block for workers, and a hard block when a `push` word follows.
- **`-c` / `--config-env`.** Keys outside a short safe list (`user.name`, `user.email`, `commit.gpgsign`, `tag.gpgsign`, `core.quotepath`, `core.abbrev`, `core.autocrlf`, `init.defaultBranch`, `color.ui`, `safe.directory`, `protocol.version`, `gc.auto`, `advice.*`) are unknown. A plain alias (`alias.st=status`) stays free; `!…` aliases and aliases naming a push do not. `--config-env` values are env var names, so any alias key there is unknown.
- **Env.** `NAME=v` and `NAME+=v` words anywhere in a command (`export` and bare assignments included). `GIT_CONFIG_KEY_n` and `GIT_CONFIG_PARAMETERS` keys are checked like `-c`; `GIT_CONFIG`/`_GLOBAL`/`_SYSTEM` and `GIT_EXEC_PATH` ask unless empty or `/dev/null`. Program-valued env (`GIT_EXTERNAL_DIFF`, `GIT_EDITOR`, `GIT_SEQUENCE_EDITOR`, `GIT_SSH(_COMMAND)`, `GIT_PAGER`, `GIT_ASKPASS`, `GIT_PROXY_COMMAND`, `EDITOR`, `VISUAL`, `PAGER`, `SSH_ASKPASS`) is free only for exact plain values (`true`, `:`, `cat`, `less`, `vim`, `ssh`, `code --wait`, …); anything else asks, and a command line in it is scanned, so a push there is a hard deny. `git --exec-path=<dir>` is unknown too.
- **`git config`.** A write (two positionals, no `--get*`/`--unset`/`--list`; the values of `-f`/`--file`/`-t`/`--type`/`--blob` are skipped) of a key outside the safe list asks. A literal push in the value of a non-alias key is a hard deny. Reads and safe keys (`user.name`, `pull.rebase`, …) stay free.
- **Exec verbs.** `rebase`, `bisect`, `submodule`, `fetch`, `pull`, `ls-remote`, `clone`, `difftool`, `mergetool`, `remote` run the suffix scan on their words (`rebase -x '…'`, `bisect run …`, `submodule foreach '…'`, `--upload-pack=…`), top-level only (`deep`), so the scan cannot nest.
- **Corpus measure (4,934 CoS commands).** git globals: `-C` ×395; `-c`: `user.name` ×1 (plus `user.email`); env: `GIT_CONFIG_GLOBAL=/dev/null` ×11, `GIT_EDITOR=true` ×2.
- **Replay, base 9e5c7b6 vs fix.** ALLOW 4,717 / ASK 42 / BLOCK 175 on both; no command moved.
- **Not covered.** Config already stored in the repo (a stored alias or `core.fsmonitor`), a script path in a program env or option (its content is not read), and unknown subcommands outside the exec-verb list, which still pass.
