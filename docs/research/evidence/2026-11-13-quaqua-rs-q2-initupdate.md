Outcome:         COMPLETE
Snapshot:        read-only; no files written in the worktree /Users/tinhtute/Lab/.wt/tieude-initupd (main 43a60bd, clean)
Scope:           read: crates/quaqua/src/init.rs, crates/quaqua/src/main.rs, crates/quaqua/tests/init.rs,
                 crates/quaqua/build.rs, docs/research/plans/quaqua-rs-split.md, templates/workspace/,
                 ~/quaqua/.quaqua/{template.lock,pending/,backup-20261028212407/}
                 wrote (outside any repo): /tmp/q2-initupd-experiment.sh and 4 mktemp -d sandboxes
Verification:    cargo test --locked -p quaqua --test init (CARGO_TARGET_DIR=~/Lab/quaqua/target)
                 -> "test result: ok. 30 passed; 0 failed; 0 ignored" in 1.56s
                 installed binary quaqua 2.6.0 (engine id 2.6.0+43a60bd, same commit as main) driven
                 against fixture templates via QUAQUA_TEMPLATE_DIR in mktemp -d sandboxes; every row
                 below is quoted from real output. Deliberately skipped: `bash tests/run.sh --slow`
                 (Lead's gate), any run inside ~/quaqua (read-only inspection only), a cargo build in
                 this worktree (no target/; reused ~/Lab/quaqua/target).
Unknown / risk: the installed binary is 2.6.0+43a60bd, i.e. the same commit as main, so no drift;
                 but the experiments drive it through QUAQUA_TEMPLATE_DIR (init.rs:453-468), the
                 test-only fixture path. The embedded table takes the identical Entry list and the
                 identical run_update code, so behaviour is the same; the only untested difference is
                 which files are in the table.
                 No test in crates/quaqua/tests/init.rs covers a Managed->Seed kind move or a stale
                 lock entry — those two answers rest on code reading plus my measurements (D, D2, G2,
                 G3, E, N).
Ownership:       released; nothing held, nothing to merge

## Playbook
1: done
2: done
3: done

## Investigation: Q2 — `init --update` vs a Managed file the user edited

### Overview

`quaqua init --update` is a five-row reconcile table over the template entries embedded in the
binary. It is keyed by workspace-relative path and compares three md5s (`init.rs:297-430`):

- `L` — the hash recorded in `.quaqua/template.lock` for that path (what quaqua last wrote),
- `W` — the hash of the file in the workspace now,
- `T` — the hash of the file in the new binary's template.

It never merges, never deletes, and never overwrites a file whose `W != L`. The only way a file
stops being reported is for `W` to become `T` — that is, the user takes the pending copy (or the
edit is landed in `templates/` upstream and the binary rebuilt). Everything else about the
question follows from that one sentence.

### Key Concepts

- **Managed** (`init.rs:152`) — mirrors the workspace root, reconciled by `--update`.
- **Seed** (`init.rs:154`) — copied only when missing, never updated. Source names live in
  `templates/seed/` and are remapped to workspace paths by `SEED_TARGETS` (`init.rs:41-46`) and
  `target_path` (`init.rs:435-441`); a seed file whose name is not in that list keeps its own path
  (so `seed/team.toml` targets `team.toml`, which is how I could test a kind move without a rebuild).
- **Lock entry** (`init.rs:170-174`) — `{ md5, mode }` only. **There is no `kind` field**, so a kind
  change is invisible in the lock; only the md5 comparison decides what happens next.
- **`.quaqua/pending/<path>`** — an outbox of the *upstream* version (`T`), not a merge base and not
  a backup of the user's edit. Written on row 4 only, overwritten in place, never removed.

### How It Works — the rows, each with measured output

Plain `init` (no `--update`) writes only what is missing; an existing divergent file is left alone
**and left out of the lock** (`init.rs:262-278`), so the first `--update` sees "no lock entry and
`W != T`" and reports a conflict instead of silently overwriting. Pinned by
`init_never_clobbers_an_existing_file` (`tests/init.rs:202-224`).

`--update`, per entry (`init.rs:297-430`):

| Row | Condition | Action | Where |
|---|---|---|---|
| 1 | file missing in workspace | write `T`, record | `init.rs:348-363` |
| 2 | `W == T` | record only (+ restore mode) | `init.rs:364-377` |
| 3 | `W == L`, `T != L` | overwrite with `T`, record | `init.rs:378-393` |
| 4 | `W != L`, or no lock entry and `W != T` | **never overwrite**: write `T` to `.quaqua/pending/<rel>`, `diff -u`, keep `L`, count a conflict | `init.rs:394-409` |
| 5 | path in lock, gone from the template | print `removed upstream <rel> (kept)`, delete nothing | `init.rs:411-415` |
| seed | kind == Seed | missing → create; `W == T` → record silently; else `keep <rel> (seed, never updated)` | `init.rs:313-341` |

Exit `0` = nothing pending, `3` = at least one conflict, `1` = hard error (corrupt lock).

**Upstream unchanged, user edited** — the case the map log calls "row 3 untouched → overwrite" is
wrong in one respect: upstream unchanged is *not* a separate row. `T == L` and `W != L` still lands
on row 4, because row 3 requires `W == L`:

```
### A. user edits team.toml, upstream UNCHANGED -> update
  W=f5eebc51… L=62ee7cb8… T=62ee7cb8…
record AGENTS.md
record rules/_all/core.md
conflict team.toml -> .quaqua/pending/team.toml
--- ws/team.toml
+++ ws/.quaqua/pending/team.toml
@@ -1 +1 @@
-kinds.a.model = "USER-EDIT"
+kinds.a.model = "m1"
1 conflicts — see .quaqua/pending/
  -> exit=3
  ws/team.toml now: kinds.a.model = "USER-EDIT"
  lock[team.toml] = md5=62ee7cb8…        (L kept, unchanged)
```

So a Managed file the user edits conflicts **on every subsequent update**, forever, whether or not
upstream ever changes. The diff prints the user's edit as the `-` side, i.e. it reads as "here is
what you would lose", not "here is what changed upstream".

**Upstream changed while the edit stands** — same row, pending copy replaced in place:

```
### B. upstream CHANGES (v2) while the user edit stands -> update x2
conflict team.toml -> .quaqua/pending/team.toml
  pending content: kinds.a.model = "m2"
  ws content:      kinds.a.model = "USER-EDIT"
  lock[team.toml] = md5=62ee7cb8…
  --- second update, same state:
  pending content: kinds.a.model = "m2"
  pending file count under .quaqua/pending: 1
```

`.quaqua/pending/` does **not** accumulate versions: one file per path, overwritten with the newest
`T` on each update. But it is **never cleaned up** — not even after the conflict is resolved:

```
### C. user resolves by taking the pending copy -> update
record team.toml
  -> exit=0
  pending still on disk? YES  content=kinds.a.model = "m2"
```

Measured in the live workspace too: `~/quaqua/.quaqua/pending/models.md` is `e54a65bc…` while
`~/quaqua/models.md` and `templates/workspace/models.md` are both `d6cb529d…` — a stale pending copy
from an earlier conflict that no longer exists. `ls .quaqua/pending/` is therefore **not** a
reliable "you have conflicts" signal; only re-running `--update` is.

Live workspace state right now (measured hashes, `inferred` verdict from the code):

```
team.toml    L=9dc4ccb5… W=9dc4ccb5… T=9dc4ccb5…  W==L:True W==T:True  -> record
models.md    L=d6cb529d… W=d6cb529d… T=d6cb529d…  W==L:True W==T:True  -> record
AGENTS.md    L=16e0745e… W=5e77d0c7… T=27a3f7cf…  W==L:False W==T:False -> conflict
```

and `templates/workspace/team.toml` is byte-identical to `~/quaqua/team.toml` (same md5) — the
shipped template carries the personal roster, which is the PR #271 shape.

### Managed → Seed in a newer binary (existing workspace)

Measured by moving `team.toml` from the fixture's `workspace/` to `seed/` (same target path):

```
### D. kind moves Managed -> Seed, user file edited
keep team.toml (seed, never updated)
  -> exit=0
  ws/team.toml now: kinds.a.model = "USER-EDIT-2"      (user file untouched)
  lock[team.toml] = md5=ce5c9cdf…                      (old L retained)
  pending after move: YES                              (stale, from before)

### D2. same move, file UNTOUCHED (W == L)
record AGENTS.md
record rules/_all/core.md
  -> exit=0                                            (no line for team.toml at all)
  lock[team.toml] = md5=ce5c9cdf…                      (unchanged)
```

Answers to the two unknowns in the map:

- **The user file is kept, never overwritten, never deleted.** No pending copy is produced by the
  move itself (the `YES` in D is the leftover from the earlier conflict).
- **The lock entry survives with its old md5.** The lock has no `kind` field (`init.rs:170-174`) and
  the seed branch only writes a lock entry when it creates the file or when `W == T`
  (`init.rs:316-334`); otherwise it prints `keep` and leaves `L` alone. Nothing prunes it.
- **No `removed upstream` line**, because the path is still in the template set (`targets`,
  `init.rs:304-308`).
- Output differs by state: `keep … (seed, never updated)` when `W != T`, one silent record when
  `W == T`.

The stale `L` only matters if the file later moves **back** to Managed:

```
### G2. seed -> managed, file UNTOUCHED, template changed
overwrite team.toml                    -> exit=0, ws=managed v2

### G3. managed -> seed, user edits while seed, then back to managed
keep team.toml (seed, never updated)   (while seed; lock stays at the pre-seed md5)
conflict team.toml -> .quaqua/pending/team.toml   (after moving back; exit=3)
```

So `W == L` (untouched since the last recorded write) → silent overwrite; `W != L` → conflict. A
Seed-era edit is protected exactly like a Managed-era edit, by the same stale `L`.

### Removed from the template set entirely

```
### E. file removed from the template set entirely -> update x2
removed upstream team.toml (kept)      -> exit=0, ws/team.toml still there? YES
  --- second update:
removed upstream team.toml (kept)      -> exit=0
  lock[team.toml] = md5=ce5c9cdf…      (entry still present after both runs)
```

The file is kept, the lock entry is kept, and the line prints on **every** future update — a
permanent, harmless-looking nag. Nothing removes the entry (`Lock::save`, `init.rs:191-206`, writes
`self.files` verbatim).

Re-adding the path later re-enters the normal table against the stale `L`:

```
### M. removed upstream, user edited it while it was gone, then re-added
conflict team.toml -> .quaqua/pending/team.toml   -> exit=3, user edit kept

### N. removed upstream, user NEVER edited, then re-added with new content
overwrite team.toml                               -> exit=0, ws/team.toml = v9
```

### Edge cases measured

```
### H. Managed file DELETED by the user, still in template
write team.toml        -> exit=0, recreated (row 1). A user cannot opt out by deleting.

### J. no lock at all (workspace from an older binary), files differ
record AGENTS.md
conflict team.toml -> .quaqua/pending/team.toml   -> exit=3
   (every divergent file becomes row 4, one pending copy each)

### K. corrupt lock
init: corrupt …/.quaqua/template.lock: expected ident at line 1 column 2 (delete it to re-baseline)
  -> exit=1        (whole command fails, nothing written)

### L. dry-run on a conflict
conflict team.toml -> .quaqua/pending/team.toml
dry run: nothing written                          -> exit=3, .quaqua/ not created
```

### Where Things Live

- `crates/quaqua/src/init.rs:297-430` — `run_update`, the whole table.
- `crates/quaqua/src/init.rs:150-155` — `Kind`; `:164-206` — `Lock`/`LockEntry`/load/save;
  `:41-46` + `:435-441` — seed source→target mapping.
- `crates/quaqua/src/init.rs:895-910` — the one Managed file with a merge path: `.claude/settings.json`
  gets its hooks merged, then its lock entry is re-baselined to the merged hash so it never conflicts.
- `crates/quaqua/build.rs:62-83` — the embedded `ENTRIES` table and `ENGINE` id (`<version>+<sha>`).
- `crates/quaqua/tests/init.rs:275-460` — rows 1, 2, 3, 4(a), 4(b), the pending-copy resolution,
  row 5, and "seed never updated". No kind-move test.
- `docs/research/plans/quaqua-rs-split.md:66-79` — the written rule table, and the design intent:
  "Local edits drift until upstreamed: a CoS lesson promoted into `rules/_all/` is also landed in
  `templates/workspace/…` **the same day**, so the next `--update` sees `W == T`." That same-day
  upstreaming is the mechanism PR #271 broke for a personal roster: the file cannot be landed
  upstream without shipping the user's private config in the template.
- `crates/quaqua/src/main.rs:91-104` — the CLI surface (`init [dir] --update --dry-run`).
- `git log`: the table arrived whole in `8db0665` ("feat(init): embedded template + init [dir]
  --update --dry-run"); no later commit changed a row.

### Gotchas

1. **"Upstream unchanged" is not a row.** A local edit is row 4 either way, so a Managed file the
   user owns conflicts on every update for as long as the edit stands. The map log's "row 3
   untouched → overwrite" is only true when the user has not touched the file (`W == L`).
2. **`pending/` is not a conflict indicator and is never cleaned.** Stale copies outlive resolution
   (measured C, and live `pending/models.md`).
3. **The lock has no `kind`.** A Managed→Seed move is invisible; the stale md5 only bites if the
   file moves back (`G2`/`G3`).
4. **Row 5 never prunes**, so `removed upstream` prints forever and the entry survives; re-adding
   the path silently overwrites an untouched file (`N`).
5. **Deleting a Managed file does not opt out** — row 1 recreates it (`H`).
6. **A corrupt lock fails the whole run** with a "delete it to re-baseline" hint (`K`), which then
   turns every divergent file into a conflict (`J`).
7. The engine already has one Managed file that is exempt from this table by re-baselining its lock
   entry after a merge: `.claude/settings.json` (`init.rs:903-911`).

### Implications for one user-owned toml (facts only)

- A Managed file that the user edits can never stay edited: it is reported as a conflict on every
  `init --update`, and the only exit is `W == T`, i.e. the user's content must be what the binary
  ships. This is exactly the PR #271 loop for `team.toml`.
- Upstream cannot help by changing the file: any `T != L` with a local edit is still row 4, and the
  pending copy is the *upstream* version, so it never carries the user's edit anywhere.
- Making the file Seed does stop the overwrite (measured D/D2: kept, exit 0), but the lock keeps a
  stale entry, the move is invisible in the lock, and a later move back to Managed compares against
  that stale md5.
- Removing the file from the template set keeps it (row 5) but prints `removed upstream` on every
  update forever and keeps the lock entry; re-adding it silently overwrites an untouched file.
- Today, on this machine, `team.toml` and `models.md` would reconcile as `record` (they are
  byte-identical to the template), and `AGENTS.md` is the only live conflict — the roster only
  conflicts because the template and the workspace happen to agree right now, not because the
  mechanism protects the edit.
- Deleting the file is not an opt-out (row 1 recreates it); a corrupt or absent lock turns every
  divergent file into a conflict.
- The only existing precedent for a Managed file that must not conflict is the post-merge hash
  re-baseline used for `.claude/settings.json` (`init.rs:903-911`).

Verified: unit-test-verified 43a60bd

## Blockers
none

## Lessons
- `init --update` has no "user-owned" concept: any Managed file with `W != L` conflicts forever, and
  `pending/` is never cleaned, so it is not a conflict signal — 2026-11-13, runs/tieude-initupd.md
- A kind move (Managed↔Seed) is invisible in `.quaqua/template.lock` (no `kind` field), so the
  transition is only safe while the file never moves back — 2026-11-13, runs/tieude-initupd.md
