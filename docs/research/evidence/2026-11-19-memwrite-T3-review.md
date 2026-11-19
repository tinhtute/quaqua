# memwrite T3 review (hermes-cos ship 3, R4)

- Reviewer: suhuynh-memwrite (read-only, non-writer). Base: quaqua/hermes3-integ 1c0b33b.
- Scope: `crates/qq-core/src/daemon/memory_write.rs`, `crates/qq-core/src/db/sql/0007_memory_journal.sql`, `crates/qq-core/src/authority/gate.rs` (d02a2e6), `crates/qq-core/tests/memory_write.rs`, `mods/cos-memory/src/{memory-gate,tools,review}.js`, `mods/cos-memory/tests/`.
- Writer report: `~/quaqua/runs/sude-memwrite.md`.

## Verdict: CHANGES REQUESTED

The verb, journal, and per-tool refusal are built well, and the phase check passes when a non-writer reruns it. Three findings block approval. First, concurrent `memory.write` calls race each other. Second, the CoS can still write both files directly with no journal or scan. Third, the Bash deny misses the `~/` spelling that briefs use for the live workspace.

## Phase check (non-writer rerun on 1c0b33b)

| Command | Result |
|---|---|
| `tests/qq-test.sh memory_write_owner memory_write_drift memory_journal; echo exit=$?` | 11 + 4 + 3 passed, 0 failed, `exit=0` |
| `node --test mods/cos-memory/tests/` | tests 57, pass 57, fail 0 |
| `cargo clippy --locked --all-targets -- -D warnings` | `Finished dev profile` with no warnings |

## Findings

### Major

1. **Concurrent `memory.write` calls race: lost updates, a shared temp file, and journal rows that contradict each other.** `crates/qq-core/src/daemon/memory_write.rs:111-145`; `crates/qq-core/src/daemon/mod.rs:441` serves each connection on its own thread.
   - The drift check (`:112`) and the rename (`:94`) do not hold a lock.
   - Two CoS writes that load the same `before` both pass the check, and both land. The second overwrites the first. The journal then shows two rows with the same `before`, and the first row's `after` never matches the disk.
   - Both writers also share one temp path, `USER.md.tmp` (`:92`). One writer can rename the other's half-written temp file, or fail with ENOENT and roll back its other files.
   - A tool call during a review fork, or two tools issued in parallel, is enough to trigger this.
   - Fix: hold a process-wide `Mutex` from the drift check through the commit, and use a unique temp name.
2. **The CoS still writes both files directly through Write, Edit, or Bash, with no journal, threat scan, or drift guard.** `crates/qq-core/src/authority/gate.rs:228,240` (`Caller::Cos => {}`, `Pending::CosOnly => continue`); `crates/qq-core/tests/memory_write.rs:387` (`memory_write_owner_cos_direct_write_is_not_denied`).
   - R4 says each write is journaled and that writes go through the `memory.write` verb. The success_signal says every cos-memory write leaves a journal row. The gate's own header calls the verb "the only door that journals".
   - Yet `crates/qq-core/templates/workspace/AGENTS.md:32,34` tells the CoS to "write" the files, with caps enforced by the memory hook. It never says to use the cos-memory tools. So the CoS's normal path is a plain Write, which leaves no history.
   - Fix: deny Write, Edit, and Bash on the two files for the CoS too, so the verb is the only writer. Point the AGENTS.md `## Memory` lines at the cos-memory tools.
   - If the current behaviour is intended, record anh iu's decision against R4.
3. **The Bash deny misses `~/` and `$HOME` spellings of the live workspace.** `crates/qq-core/src/authority/gate.rs:289-299` (`names_memory_file`) resolves a word with `PathBuf::from(w)` and never expands `~`, `$HOME`, or `$PWD`.
   - Reproduced with `quaqua gate --json`, no daemon, and `cwd` outside the workspace. A hit should block with rc=2. These all returned allow (rc=0):
     - `echo x >> ~/ws/USER.md`
     - `sed -i s/u/v/ ~/ws/memory/rules.md`
     - `cp /dev/null $HOME/ws/USER.md`
     - `echo x >> "$PWD/USER.md"`
   - Workers run in worktrees and name the live workspace as `~/quaqua/...`, since briefs require `~/` paths. That makes this the most likely spelling for a worker's direct write, not an obscure bypass.
   - Fix: expand a leading `~` and `$HOME`/`${HOME}` (and `$PWD`, relative to `cwd`) before resolving. Add these to `memory_write_owner_worker_direct_write_denied`.

### Minor

4. **Reads that are not on the list are blocked (false positives).** `crates/qq-core/src/authority/gate.rs:564` (`READ_ONLY`).
   - These are blocked for a non-cos caller when the path resolves to the live workspace: `sed -n 1,5p USER.md`, `git diff -- USER.md`, `git log -p memory/rules.md`, `awk 1 USER.md`, `jq -R . USER.md`, `sort USER.md`. All were reproduced (rc=2).
   - They are worker-legal reads, and the deny message tells the worker to escalate.
   - Fix: allow `git diff/log/show/blame`, `awk`/`jq`/`sort`/`nl`/`bat`, and `sed` without `-i`, or list only the commands that write.
5. **Other spellings slip past the Bash deny.** `crates/qq-core/src/authority/gate.rs:340-374`. `dd of=USER.md` and `python3 -c "open('USER.md','a')"` both allow (rc=0). The code already admits it is a tripwire, so at least add `of=` and the `key=value` word shape.
6. **The rollback after a write has started is untested and imperfect.** `crates/qq-core/src/daemon/memory_write.rs:136-151`.
   - `memory_write_owner_batch_is_all_or_nothing` only covers a refusal before any write. No test fails the second `put`, or the commit, after the first file has landed.
   - A file that did not exist before (`disk_text` returns `""`) is rolled back to an empty file, not removed.
   - If the daemon crashes between the renames and `tx.commit()`, the files are written with no journal row.
7. **An unreadable file counts as empty.** `crates/qq-core/src/daemon/memory_write.rs:83-85`: `unwrap_or_default()` treats a permission error or non-UTF-8 content as `""`. A caller that sends `before: ""` then passes the drift check and overwrites a file it never saw. Fix: refuse on any read error other than NotFound.
8. **The tools' drift baseline is when the tool reads the file, not when the session loaded it.** `mods/cos-memory/src/tools.js:139-143,166`.
   - `apply` and `applyBatch` without `loaded` read `before` at call time. An outside edit made after session-start injection is built upon, not refused.
   - R4 says "an outside edit since load". Only the review fork (`review.js:160-161`) uses a load-time baseline.
   - Each op is applied to the current text, so this may be acceptable, but say so in R4 or the report.

### Nit

9. `.bak` is written in place with `put_raw` (`memory_write.rs:115,155-160`), not through a temp file and rename, and a later drift overwrites the earlier `.bak`.
10. The journal triggers block UPDATE and DELETE but not `DROP TABLE` or `DROP TRIGGER` (`0007_memory_journal.sql`). Acceptable, because the gate already denies Bash naming `runs/qq.db` (R14).
11. R4 asks for "a fork-remove case whose journal row holds the before and after text". This is covered across two tests, not one: `review.test.js:356` checks the verb arguments, and the Rust `memory_journal_row_holds_caller_file_before_and_after` checks the row.

## What was checked and holds

- **Verb owner check:** `memory_write.rs:99-103` reads the caller before any argument or path. An unidentified peer (`caller_pane` unset) is refused, and only `cos` passes. Tests cover a worker, a Lead, and an unidentified caller.
- **Per-tool non-cos refusal:** `mods/cos-memory/tests/owner.test.js` has one refusal test and one cos-through-the-verb test for each of `memory_add`, `memory_replace`, `memory_remove`, and `memory_batch`. Each asserts zero direct writes. The mod never writes the files (`tools.js`, `memory-gate.js`). A daemon that cannot answer is treated as a refusal.
- **Targets:** only `TARGETS` (`USER.md`, `memory/rules.md`). Any other key is refused (`memory_write.rs:48-52`).
- **Cap and threat scan:** the cap and secrets are checked on the final text. The strict threat scan covers only the added lines, so a line already in the file does not lock out later writes. Tests cover over-cap and threat cases.
- **Drift:** the call is refused before any write, and `<file>.bak` holds the edited disk text. The file itself is left alone. Tests cover the external edit, `.bak` placement, and a missing file.
- **Journal:** one row per file is inserted in the same transaction. The UPDATE and DELETE triggers abort, which is tested.
- **Gate:** Write, Edit, and MultiEdit, redirects, `sed -i`, `tee`, `cp`, nested `bash -c`, and `cd`-relative paths are all denied for a Lead or an unidentified caller. `cat`, `grep`, and `wc` on the files and writes to other files pass. The engine copies under `templates/` are not gated.

## Open risk

Live peer identity is still unknown. The mod's `quaqua call` child must resolve to `cos` through the pid ancestry from the plugin host to the herdr pane. Only the test rig shows this. Verify it in the `live` phase on the canonical machine.
