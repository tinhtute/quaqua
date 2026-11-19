# memwrite T3 review (hermes-cos ship 3, R4)

- Reviewer: suhuynh-memwrite (read-only, non-writer). Base: quaqua/hermes3-integ 1c0b33b.
- Scope: `crates/qq-core/src/daemon/memory_write.rs`, `crates/qq-core/src/db/sql/0007_memory_journal.sql`, `crates/qq-core/src/authority/gate.rs` (d02a2e6), `crates/qq-core/tests/memory_write.rs`, `mods/cos-memory/src/{memory-gate,tools,review}.js`, `mods/cos-memory/tests/`.
- Writer report: `~/quaqua/runs/sude-memwrite.md`.

## Verdict: CHANGES REQUESTED

The verb, journal, and per-tool refusal are built well, and the phase check passes when a non-writer reruns it. Three findings block approval:

- The path deny does not reach Write or Edit in an engine worktree session.
- The Bash deny misses the `~/` spelling of the live workspace.
- The CoS can still write both files directly with no journal row.

## Phase check (non-writer rerun on 1c0b33b)

| Command | Result |
|---|---|
| `tests/qq-test.sh memory_write_owner memory_write_drift memory_journal; echo exit=$?` | 11 + 4 + 3 passed, 0 failed, `exit=0` |
| `node --test mods/cos-memory/tests/` | tests 57, pass 57, fail 0 |
| `cargo clippy --locked --all-targets -- -D warnings` | `Finished dev profile` with no warnings |

## Findings

### Major

1. **In an engine worktree, Write and Edit onto the live memory files are not gated.** `.claude/settings.json:8` (the engine repo's own settings, which every engine worktree session loads) has one PreToolUse matcher, `"Bash"`.
   - The `Write|Edit|MultiEdit` matchers exist only in the workspace template, `crates/qq-core/templates/workspace/.claude/settings.json:47,53`.
   - So a worker or Lead in an engine worktree can run `Write` on `~/quaqua/USER.md` or `~/quaqua/memory/rules.md` without `quaqua gate` ever running. `edit_found` (`gate.rs:271-276`) would deny it, but it is never called.
   - R4 says "every session template, worker and Lead worktrees included, carries a PreToolUse deny on Write, Edit and Bash". The gate tests call `evaluate_in` directly, so they cannot catch this.
   - Fix: add `Write|Edit|MultiEdit|NotebookEdit` to the engine `.claude/settings.json` gate matcher.
   - Also check which settings a worker in a non-engine project worktree carries. This review did not check that.
2. **The Bash deny misses `~/` and `$HOME` spellings of the live workspace.** `crates/qq-core/src/authority/gate.rs:289-299` (`names_memory_file`) resolves a word with `PathBuf::from(w)` and never expands `~`, `$HOME`, or `$PWD`.
   - Reproduced with `quaqua gate --json`, `QUAQUA_RUNS_DIR` set to a scratch workspace, no daemon, and `cwd` outside the workspace. A hit should block with rc=2. These all returned allow (rc=0):
     - `echo x >> ~/ws/USER.md`
     - `sed -i s/u/v/ ~/ws/memory/rules.md`
     - `cp /dev/null $HOME/ws/USER.md`
     - `echo x >> "$PWD/USER.md"`
   - Workers run in worktrees and name the live workspace as `~/quaqua/...`, since briefs require `~/` paths. That makes this the most likely spelling for a worker's direct write, not an obscure bypass.
   - Fix: expand a leading `~` and `$HOME`/`${HOME}` (and `$PWD`, relative to `cwd`) before resolving. Add these to `memory_write_owner_worker_direct_write_denied`.
3. **A direct Write, Edit, or Bash by the CoS lands with no journal row and no drift guard.** `crates/qq-core/src/authority/gate.rs:228,240` (`Caller::Cos => {}`, `Pending::CosOnly => continue`); `crates/qq-core/tests/memory_write.rs:387` (`memory_write_owner_cos_direct_write_is_not_denied`).
   - The memory hook still applies the cap and the strict threat scan to a direct Write or Edit (`crates/qq-core/src/memory/caps.rs:142-165`, `pre_decision`). It writes no `memory_journal` row, though, and it has no drift guard or `.bak`.
   - R4 says "Each write … appends one row" and "every session template … carries a PreToolUse deny on Write, Edit and Bash to the two paths", with no exception for the CoS. The gate's own header calls the verb "the only door that journals".
   - `crates/qq-core/templates/workspace/AGENTS.md` never names the cos-memory tools; a grep for `cos-memory`, `memory_add` and `memory_batch` finds nothing. Line 32 tells the CoS to "write" the files, so a plain Write is the CoS's normal path, and it leaves no history.
   - Fix: deny Write, Edit, and Bash on the two files for the CoS too, so the verb is the only writer. Point the AGENTS.md `## Memory` lines at the cos-memory tools.
   - If the current behaviour is intended, record anh iu's decision against R4.

### Minor

4. **Concurrent `memory.write` calls can lose an update.** This is from reading the code; it was not reproduced. `crates/qq-core/src/daemon/memory_write.rs:111-128`; `daemon/mod.rs:441` serves each connection on its own thread.
   - The drift check runs before the transaction and holds no lock.
   - Two calls that loaded the same `before` can both pass the check. SQLite then runs them one after the other: the first INSERT takes the write lock, and `busy_timeout` (`db/mod.rs:80`) makes the second wait. So the second write lands on a stale `before` and silently drops the first.
   - The window is milliseconds, for example a tool call while the review fork writes.
   - Fix: take a process-wide `Mutex` before the drift check, or re-read the disk text inside the transaction.
5. **Reads that are not on the list are blocked (false positives).** `crates/qq-core/src/authority/gate.rs:564` (`READ_ONLY`).
   - These are blocked for a non-cos caller when the path resolves to the live workspace: `sed -n 1,5p USER.md`, `git diff -- USER.md`, `git log -p memory/rules.md`, `awk 1 USER.md`, `jq -R . USER.md`, `sort USER.md`. All were reproduced (rc=2).
   - They are worker-legal reads, and the deny message tells the worker to escalate.
   - Fix: allow `git diff/log/show/blame`, `awk`/`jq`/`sort`/`nl`, and `sed` without `-i`, or list only the commands that write.
6. **Other spellings slip past the Bash deny.** `crates/qq-core/src/authority/gate.rs:340-374`. `dd of=USER.md` and `python3 -c "open('USER.md','a')"` both allow (rc=0). The code already admits it is a tripwire, so at least add `of=` and the `key=value` word shape.
7. **The rollback after a write has started is untested and imperfect.** `crates/qq-core/src/daemon/memory_write.rs:136-151`.
   - `memory_write_owner_batch_is_all_or_nothing` only covers a cap refusal before any write. No test fails the second `put`, or the commit, after the first file has landed.
   - A file that did not exist before (`disk_text` returns `""`) is rolled back to an empty file, not removed.
   - If the daemon crashes between the renames and `tx.commit()`, the files are written with no journal row.
8. **An unreadable file counts as empty.** `crates/qq-core/src/daemon/memory_write.rs:83-85`: `unwrap_or_default()` treats a permission error or non-UTF-8 content as `""`. A caller that sends `before: ""` then passes the drift check and overwrites a file it never saw. Fix: refuse on any read error other than NotFound.
9. **The tools' drift baseline is when the tool reads the file, not when the session loaded it.** `mods/cos-memory/src/tools.js:139-143,166`.
   - `apply` and `applyBatch` without `loaded` read `before` at call time. An outside edit made after session-start injection is built upon, not refused.
   - R4 says "an outside edit since load". Only the review fork (`review.js:160-161`) uses a load-time baseline.
   - Each op is applied to the current text, so this may be acceptable, but say so in R4 or the report.

### Nit

10. `.bak` is written in place with `put_raw` (`memory_write.rs:115,155-160`), not through a temp file and rename, and a later drift overwrites the earlier `.bak`.
11. The journal triggers block UPDATE and DELETE but not `DROP TABLE` or `DROP TRIGGER` (`0007_memory_journal.sql`). Acceptable, because the gate already denies Bash naming `runs/qq.db` (R14).
12. R4 asks for "a worker-caller case (verb refused)". `memory_write_owner_worker_refused` uses a Lead (`Who::Lead`, `tests/memory_write.rs:164-165`). Any non-cos caller is refused the same way, but there is no case with the worker role.
13. R4 asks for "a fork-remove case whose journal row holds the before and after text". This is covered across two tests, not one: `review.test.js:356` checks the verb arguments, and the Rust `memory_journal_row_holds_caller_file_before_and_after` checks the row.

## What was checked and holds

- **Verb owner check:** `memory_write.rs:99-103` reads the caller before any argument or path. An unidentified peer (`caller_pane` unset) is refused, and only `cos` passes. Tests cover a Lead and an unidentified caller (`tests/memory_write.rs:164,174`).
- **Per-tool non-cos refusal:** `mods/cos-memory/tests/owner.test.js` has one refusal test and one cos-through-the-verb test for each of `memory_add`, `memory_replace`, `memory_remove`, and `memory_batch`. Each asserts zero direct writes. The daemon's reply comes from a fake engine, so these tests show that the mod relays the refusal. The Rust tests check that the daemon refuses.
- **The mod never writes the files:** neither `tools.js` nor `memory-gate.js` writes them. A daemon that cannot answer is treated as a refusal.
- **Targets:** only `TARGETS` (`USER.md`, `memory/rules.md`). Any other key is refused, with `AGENTS.md` tested (`memory_write.rs:48-52`; test `:185`).
- **Cap and threat scan:** the cap and secrets are checked on the final text. The strict threat scan covers only the added lines, so a line already in the file does not lock out later writes. Over-cap and threat refusals are tested with no journal row (`:205,216`).
- **Drift:** the call is refused before any write, and `<file>.bak` holds the edited disk text. The file itself is left alone. Tests cover both files, a clean write with no `.bak`, and a missing file (`:251-284`). The review fork refuses an edit made while it ran (`review.test.js:374`).
- **Journal:** one row per file is inserted in the same transaction. Caller, file, before, and after are asserted (`:293`), and the UPDATE and DELETE triggers abort (`:326`).
- **Gate, when it runs:** Write, Edit, and MultiEdit, redirects, `sed -i`, `tee`, `cp`, nested `bash -c`, and `cd`-relative paths are all denied for a Lead or an unidentified caller. `cat`, `grep`, and `wc` and writes to other files pass. The engine copies under `templates/` are not gated. `root` comes from `workspace_root()` (`QUAQUA_HOME`/`QUAQUA_ROOT`/`HOME`), not from the cwd, so a gate in a worktree still checks against the live workspace.

## Open risk

Live peer identity is still unknown. The mod's `quaqua call` child must resolve to `cos` through the pid ancestry from the plugin host to the herdr pane. Only the test rig shows this. Verify it in the `live` phase on the canonical machine.
