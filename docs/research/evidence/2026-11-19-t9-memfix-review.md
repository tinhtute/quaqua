# T9 push door + memfix review (hermes-cos ship 3)

- Reviewer: suhuynh-t9rev (read-only, non-writer). Base: `quaqua/hermes3-integ` at a2e81f9.
- Units:
  - **memfix**: red 4659dc2, fix b362b9b, merged 08f7daf. It answers `2026-11-19-memwrite-T3-review.md` (M1–M3, minors 4–9, nits 10–13) and gate m1 (`2026-11-19-gate-T5-review.md:41-44`: "class an unmerged delete as High (ask)").
  - **push door**: red 9f5819f, impl a2d874b, merged a2e81f9 (`push.cos`, `crates/qq-core/src/daemon/push_door.rs`).
- Probes ran in a scratch copy of the tree (`git archive HEAD`), in a scratch `HOME` and workspace, and against local bare remotes only. Gate probes used `target/debug/quaqua gate --json` with no daemon, so a gate hit returns rc=2 and a miss returns rc=0. Door probes used a probe test file in the scratch copy that reuses the `cos_push_door.rs` rig, which has a real daemon, the peer gate, and FakeHerdr.

## Verdict: CHANGES REQUESTED

- **Push door:** the caller check and the force and refspec refusals hold. The remote default branch is not reliably refused. The door reads only the local `refs/remotes/<remote>/HEAD`. That ref is unset after `git remote add`, the caller can drop it, and a second remote name sidesteps it. In each case the push to the remote default lands.
- **memfix:** M1, M2 and M3 are fixed, and m1 is fixed for the spellings the gate sees. Two problems remain:
  - The fix for minor 5 adds `sed`, `uniq` and `git grep` to the read-only allow list without their write modes. As a result, a worker can now truncate `USER.md` or `memory/rules.md` with no gate hit. Each of these was verified on disk.
  - The M1 matcher sends every Write and Edit through whatever `quaqua` is on PATH. The installed binary predates the fix and blocks them all, which blocked this review's own Write.

## Phase check (non-writer rerun on a2e81f9)

| Command | Result |
|---|---|
| `cargo test --locked -p qq-core --test cos_push_door` | `test result: ok. 6 passed; 0 failed` |
| `cargo test --locked -p qq-core --test memory_write` | `test result: ok. 23 passed; 0 failed` (includes the `memory_fix_*` tests in that file) |
| `tests/qq-test.sh gate ask_tier memory_path_gate push_refuses_main` | exit 0, 102 passed and 0 failed across all binaries (it includes 4 `branch_delete_class_gate_*` tests) |

The named filters do not reach several of the memfix red tests: `tests/memory_fix.rs` (`memory_fix_home_spellings_denied`), `cos_fence.rs::memory_fix_cos_fence_denies_direct_memory_file_writes`, `gate_fix.rs:288 branch_delete_class_unmerged_is_high_and_asks_at_every_tier`, and `authority.rs::tier_table_delete_unmerged_branch`. They were not run here, per the brief. The probes below cover the behavior they assert.

The impl commit a2d874b changes `tests/cos_push_door.rs` only by rustfmt line wraps (`git show a2d874b -- crates/qq-core/tests/cos_push_door.rs`), so no red assertion was weakened.

Scope: `git diff 6194630 08f7daf --stat` touches 12 files. Every one maps to a T3 item or m1: `.claude/settings.json` (M1), `gate.rs` (M2, M3, minors 5 and 6, m1), `tier.rs` (m1), `memory_write.rs` (minor 7), `fence.rs` and the template `AGENTS.md` (M3), `herdr-gate.sh` (M1 fallback), and tests. Nothing outside the items changed.

## memfix: item table

| Item | Status | Evidence |
|---|---|---|
| M1: Write/Edit not gated in engine worktrees | **Fixed, with a rollout break** | `.claude/settings.json:13-18` adds `Write\|Edit\|MultiEdit\|NotebookEdit` to the gate. The branch binary returns rc=2 for Write, Edit and MultiEdit on `memory/rules.md` and for NotebookEdit on `USER.md`. The fallback `herdr-gate.sh` with `QUAQUA_BIN=/nonexistent-qq` returns rc=2 for both files. The installed `quaqua` blocks every Write (major, below). On the T3 sub-question, non-engine worker worktrees get `settings.local.json` from `driver/claude.rs:17-18` (`^(Bash\|Write\|Edit\|MultiEdit)$`), which lacks NotebookEdit (trivial, below). |
| M2: `~`, `$HOME`, `$PWD` spellings | **Fixed** | All four T3 spellings now return rc=2: `echo x >> ~/ws/USER.md`, `sed -i s/u/v/ ~/ws/memory/rules.md`, `cp /dev/null $HOME/ws/USER.md`, `echo x >> "$PWD/USER.md"`. So do `${HOME}/…`, `"$HOME"/…`, `${PWD}/…`, `~/ws/./memory//rules.md` and `cd ~/ws && cp /dev/null USER.md`. Variable indirection (`D=~/ws; cp /dev/null $D/USER.md`) returns rc=0, the tripwire limit the gate documents. |
| M3: CoS direct writes unjournaled | **Fixed** | Bash: `gate.rs:292` blocks with `COS_MEMORY_MESSAGE` (test `memory_fix_cos_direct_write_denied_with_tool_hint`, ran green). Write and Edit: `quaqua memory hook pre` with cwd set to the scratch workspace root (the CoS shape) returns `"permissionDecision":"deny"` with the cos-memory tool hint for both Write and Edit on `USER.md`; a daily note under `memory/` passes. `templates/workspace/AGENTS.md:33` names the tools. Side effect: non-writing git commands are now denied to the CoS (minor, below). |
| minor 4: concurrent `memory.write` lost update | **Not fixed** | `memory_write.rs:124` still runs the drift check outside the transaction with no lock. `grep -n Mutex memory_write.rs` finds nothing. |
| minor 5: read false positives | **Fixed, with a regression** | All six T3 reads now return rc=0 (`sed -n 1,5p`, `git diff --`, `git log -p`, `awk 1`, `jq -R .`, `sort`). The new `read_only_cmd` (`gate.rs:994`) also allows write forms (major, below). |
| minor 6: `dd of=`, interpreter strings | **Fixed** | `dd if=/dev/zero of=USER.md count=0` returns rc=2, and so does `python3 -c "open('USER.md','a')"`. |
| minor 7: rollback | **Partly fixed** | A failed second `put` now restores or removes files (`memory_fix_rollback_*`, ran green). The commit-failure path is still untested, and the crash window between rename and `tx.commit()` is unaddressed. |
| minor 8: unreadable file counts as empty | **Not fixed** | `memory_write.rs:87` still uses `read_to_string(path).unwrap_or_default()`. |
| minor 9: tools.js drift baseline | **Not fixed, not documented** | `git diff 6194630 08f7daf --stat -- mods/` is empty. No R4 or report note was found. |
| nits 10–13 | 10 and 12 not addressed; 11 and 13 need no change | `.bak` is still written with `put_raw` (`memory_write.rs`). No test uses the worker role: `memory_write.rs` uses only `Who::Lead`. |
| gate m1: unmerged delete is High | **Fixed for the gated spellings** | `tier.rs:46-52` puts `BranchDeleteUnmerged` at High, so it asks at every tier. The full grant is the only exception (`gate.rs` judge, `known` target only). `git branch -D`, `--delete --force`, `-M main feat`, `push origin :feat` and `push origin --delete feat` all hit the gate (rc=2). The class can be forged, and other ways to drop a branch are not gated (minors, below). |

## Push door: spec check

| Spec point | Status | Probe |
|---|---|---|
| Only the CoS can call `push.cos` | **Holds** | A worker gets `refused`. A worker that sends `caller=cos, caller_pane=c` gets `refused w-1 -> push.cos`, because the peer gate overwrites the claim. A daemon with no `require_peer` that gets the same claim answers `refused unknown -> push.cos`. Production always installs the peer gate (`identity/mod.rs:268`). |
| Refuses `main` and `master` | **Holds for exact names** | Test `cos_push_door_cos_main_refuse`. Case variants reach `git push` (minor, below). |
| Refuses the remote default branch | **Does not hold** | See the major finding below. |
| Refspec spellings | **Holds** | `HEAD:main`, `+feat:main`, `feat:refs/heads/main`, `--force`, `-f`, `feat main`, `feat\nmain`, `HEAD`, `@{-1}` and `main.` as the branch are all refused. `refs/heads/main`, `heads/main`, `origin/main` and `@` are prefixed to `refs/heads/refs/heads/main` and so on, and git fails them (`src refspec … does not match any`). `--force`, `origin main`, `+origin`, `../origin.git` and `origin:main` as the remote are refused. A `remote.origin.push=+refs/heads/feat:refs/heads/main` config does not move main, because the explicit refspec wins. |
| Refuses force | **Holds** | `force` set to `true`, `"false"`, `0` or `""` is refused. Unknown keys (`force_with_lease`, `refspec`) are ignored and the plain push runs. A non-fast-forward to `feat` fails in git (`push_failed`). |
| No other path to `git push` | **Holds** | The door builds one argv, `git push -- <configured remote> refs/heads/<b>:refs/heads/<b>` (`push_door.rs:96-99`). |

## Findings

### Major

- [major] **`push.cos` does not refuse the remote default branch unless a local `refs/remotes/<remote>/HEAD` names it, and the caller controls that ref.**
  - Where: `crates/qq-core/src/authority/push.rs:75-91` (`protected_branch`). It reads only `git symbolic-ref refs/remotes/<remote>/HEAD` and the repo's `init.defaultBranch`, and never asks the remote.
  - The red test sets that ref by hand (`cos_push_door.rs:133-140`), so it cannot see the gap.
  - Probe (scratch copy, `probe_push.rs`; the bare remote has `HEAD -> refs/heads/trunk`):
    ```
    PROBE local origin/HEAD: None
    PROBE push trunk (remote HEAD=trunk, no local origin/HEAD): ALLOWED {"branch":"trunk","pushed":true,"remote":"origin"}
    PROBE remote trunk moved: true
    PROBE trunk with origin/HEAD set: refused cos -> trunk: branch is the repo's main or default branch
    PROBE trunk via o2 (same url): ALLOWED {"branch":"trunk","pushed":true,"remote":"o2"}
    PROBE   remote trunk moved: true
    PROBE trunk after `git remote set-head origin -d`: ALLOWED {"branch":"trunk","pushed":true,"remote":"origin"}
    PROBE   remote trunk moved: true
    ```
  - The gate does not see the commands that clear or sidestep the ref (rc=0 with no daemon): `git remote set-head origin -d`, `git remote add o2 /x/origin.git` and `git symbolic-ref -d refs/remotes/origin/HEAD`.
  - Failure: a repo whose remote default is not `main`/`master` (for example `trunk` or `develop`), with a remote added rather than cloned, or with a second remote name, takes a CoS fast-forward push to its default branch with no ask. That push should be a hard deny.
  - Fix: before the push, ask the remote for its default (`git ls-remote --symref <remote> HEAD`) and fail closed when that read fails. Add a test with a bare remote whose HEAD is not main and that has no local `origin/HEAD`.

- [major] **The minor 5 fix allows memory-file writes with no gate hit (regression).**
  - Where: `crates/qq-core/src/authority/gate.rs:976-1030` (`GIT_READ`, `read_only_cmd`). When `read_only_cmd` returns true, `bash_memory` skips the segment (`gate.rs:506`).
  - Before b362b9b, `sed`, `uniq` and `git` were not on `READ_ONLY` (`git show 6194630:…/gate.rs`). Any of them naming a memory file was denied: T3 reproduced `sed -n 1,5p USER.md` as rc=2.
  - Probe, with a scratch workspace and cwd inside it:
    ```
    rc=0  sed -n 'w USER.md' /dev/null
    rc=0  sed 's/a/b/w memory/rules.md' /dev/null
    rc=0  sed -n 'w <ws>/USER.md' /dev/null          (absolute, cwd outside)
    rc=0  sed -I '' s/u/v/ USER.md                   (BSD in-place is capital -I; the check looks only for 'i')
    rc=0  uniq /dev/null USER.md                     (uniq's 2nd operand is its output file)
    rc=0  uniq /dev/null ~/ws/memory/rules.md
    rc=0  git grep -O'tee USER.md' x                 (grep is in GIT_READ; -O runs a command)
    ```
  - Real effect in the scratch workspace:
    - `sed -n 'w USER.md' /dev/null` leaves `bytes=0`.
    - `uniq /dev/null memory/rules.md` leaves `bytes=0`.
    - `sed -I '' s/u/v/ USER.md` changes the content to `v`.
    - In a scratch repo, `git grep -O'tee g.out' hit` created `g.out`.
  - Failure: a worker, a Lead or the CoS writes or truncates `USER.md` or `memory/rules.md` with no deny and no journal row. That reopens the gap M2 and M3 closed.
  - Fix:
    - Drop `uniq` from the always-read list, or allow it only with fewer than two operands.
    - Treat `sed` as a writer when any `-i` or `-I` flag is present, or when the script contains a `w` or `W` command or an `s///w` flag.
    - Remove `grep` from `GIT_READ`, or refuse `-O` and `--open-files-in-pager`.
    - Add each spelling to `memory_path_gate_bash_writes`.

- [major] **The M1 matcher blocks every Write and Edit in an engine worktree while the installed `quaqua` is older than this branch.**
  - Where: `.claude/settings.json:13-18` sends Write, Edit, MultiEdit and NotebookEdit to `herdr-gate.sh`, which runs `exec "$bin" gate` with `quaqua` taken from PATH and no version check (`herdr-gate.sh:24-27`).
  - Here, `quaqua` resolves to the mise shim, and that binary answers any Write with the pane-spawn block.
  - Probe (the same hook JSON for a Write to a scratch path):
    ```
    $(command -v quaqua) gate                        -> "quaqua: blocked: spawn/move panes and workspaces only through the quaqua daemon …", rc=2
    target/debug/quaqua gate (branch build)          -> rc=0
    ```
  - Live effect: this reviewer's own Write tool call, even for a file in the scratchpad, was refused with that message. The review doc had to be written through Bash.
  - Failure: once 08f7daf reaches any engine worktree, every worker and Lead there loses Write and Edit until the installed binary is rebuilt.
  - Fix: before this lands on main, either install the new binary as part of the rollout, or make the gate pass Write and Edit through when the binary does not know the tool.

### Minor

- [minor] **`push.cos` does not refuse case variants of `main`, `master` or the default branch.** `push.rs:84` uses an exact `matches!`, and `:88` compares with `==`.
  - Probe: `Main` and `MAIN` reach `git push`. Against a loose remote `main`, git fails with `cannot lock ref 'refs/heads/Main': reference already exists`.
  - Against a packed remote `main` on APFS (case-insensitive), the door pushes:
    ```
    PROBE Main vs packed main: ALLOWED {"branch":"Main","pushed":true,"remote":"origin"}
    PROBE remote refs: refs/heads/Main b2ad584 | refs/heads/main d1c0e19
    ```
  - The packed `main` is unchanged, but `rev-parse refs/heads/main` on that remote now resolves to the loose `Main` (`d1c0e19 -> b2ad584`).
  - On a case-sensitive remote this just creates a new branch `Main`.
  - Fix: compare the branch name to the protected names case-insensitively.
- [minor] **The m1 class can be forged with ungated commands.** `tier.rs:234-264` (`branch_delete_class`) takes the default from the local `refs/remotes/origin/HEAD`.
  - Probe: `git fetch . wip:refs/remotes/origin/x && git remote set-head origin x` (both rc=0 in the gate). After that, `branch_delete_action(work, "wip")` returns `BranchDeleteMerged` for a branch whose commit exists only locally (`PROBE m1 wip … -> merged: true`). The honest class was `Unmerged`.
  - Failure: a later `git branch -D wip` is Low and is allowed at Low or Medium tier with no ask.
  - Fix: check merge state against the remote's real default, read live with `ls-remote`, or require the tip to exist on the remote.
- [minor] **Other ways to drop an unmerged branch skip m1 (pre-existing).**
  - Probe (scratch repo, rc=0): `git update-ref -d refs/heads/feat`, `git branch -f feat main`, `git checkout -B feat main`.
  - `gate.rs:1355` gates `update-ref` only for `refs/remotes/`, `refs/tags/` and trunk refs.
  - Fix: class `update-ref -d refs/heads/<b>` like `branch -D`, and a `-f`/`-B` reset of an existing unmerged branch like `HistoryRewrite`.
- [minor] **The M3 deny now catches non-writing git commands for the CoS.** `gate.rs:292`.
  - `git add USER.md` and `git commit -m x -- USER.md` both hit as memory writes (rc=2), because `add` and `commit` are not in `GIT_READ`. Since M3, the CoS is blocked too, not only workers.
  - Fix: allow `git add` and `git commit` naming the files, or record that the CoS never versions them.
- [minor] **T3 minor 4 (lost update under concurrent `memory.write`) is not fixed.** `memory_write.rs:124` runs the drift check with no lock before the transaction, as before. No probe was run; this is from reading the code, like T3.
- [minor] **T3 minor 7 is partly fixed.** A failed `put` is rolled back and tested. A failed `tx.commit()` is untested, and a crash between the renames and the commit still leaves files with no journal row (`memory_write.rs:150-162`).
- [minor] **T3 minor 8 is not fixed.** `memory_write.rs:87` still counts a permission error or non-UTF-8 content as `""`, so a caller that sends `before: ""` overwrites a file it never read.

### Trivial

- [trivial] **T3 minor 9 is neither fixed nor recorded.** The `mods/cos-memory` load-time baseline is unchanged (`git diff 6194630 08f7daf -- mods/` is empty), and neither R4 nor a report records it as accepted.
- [trivial] **T3 nits 10 and 12 are not addressed.** The `.bak` is still written with `put_raw`, not through a temp file and rename. No owner test uses the worker role (`tests/memory_write.rs` uses `Who::Lead` only).
- [trivial] **The door follows a symref on the remote side.** If the remote has `refs/heads/alias -> refs/heads/main`, pushing `alias` moves main (`PROBE   remote main moved: true`). This needs write access to the remote's refs, so it is out of the CoS's reach on a hosted remote.
- [trivial] **The worker worktree hook matcher omits NotebookEdit** (`driver/claude.rs:17-18`, `^(Bash|Write|Edit|MultiEdit)$`). The engine settings and the template include it. This is pre-existing. NotebookEdit on a `.md` file is not a practical write path.
- [trivial] **Some tripwire misses predate memfix.** `sort -m /dev/null -oUSER.md` returns rc=0, because the attached `-o` value is never resolved. `D=~/ws; cp /dev/null $D/USER.md` also returns rc=0.

## What was checked and holds

- **Caller check:** the push door caller check holds in three cases, a worker, a worker claiming `cos` with a pane, and a daemon with no peer gate. A daemon that is down fails closed (`cos_push_door_daemon_down_refuse`).
- **Every refusal is journaled:** three refusals give three rows (`cos_push_door_cos_main_refuse`).
- **The memory deny resolves expanded spellings.** It resolves `..`, `.`, `//`, `~`, `$HOME`, `${HOME}`, `$PWD`, `${PWD}`, `key=path` and interpreter strings, and a CoS Write or Edit is denied with the tool hint.
- **The m1 tier change holds for every gated delete spelling,** and only a full grant, on a target the scanner can read, gets past it.

## Open risk

- The door runs git with `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1` and `GIT_TERMINAL_PROMPT=0` (`push.rs:43-50`). An HTTPS remote that depends on a credential helper set in global or system config (for example osxkeychain) is likely to fail authentication. This was not probed, because the brief forbids real remotes. Check it in the `live` phase.

Verdict: 15 findings
