# Gate T5 review: hermes-cos ship 3 (quaqua/hermes3-integ 4a9a901 vs 7491347)

Reviewer: suhuynh-gate (read-only). Units: T3 sude-gate, T4 sude-hooks, T6 sude-grant, T7 sude-prmerge, T8 sude-tierwire. Files from the origin/main 2ecc892 merge are ignored.

## Verdict: CHANGES REQUESTED

Two majors. (1) R14's PreToolUse `ask` before a grant write is missing, so the CoS can grant itself a full grant with no consent. (2) The tier is read from the hook's cwd, so a grant or a low row on one repo covers commands aimed at another repo. The phase check is green because no named test covers either case.

## Phase check (non-writer rerun at 4a9a901)

```
tests/qq-test.sh gate ask_tier tier_table caller_lookup gate_bypass instruction_file_gate pane_input_gate hook_settings_route grant_write tier_source tier_source_wired fence_path_tier rerun_isolation push_refuses_main; echo exit=$?
  -> exit=0, no "matched zero tests" line, 148 passed in total, 0 failed
cargo test --locked -p qq-core --test orchestration_verbs pr_merge | tail -3
  -> test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 15 filtered out
cargo clippy --locked --all-targets -- -D warnings | tail -3
  -> Finished `dev` profile [unoptimized + debuginfo] target(s) in 16.36s  (no warnings)
```

## Major

### M1. R14 consent `ask` on grant/tier/lane writes is not implemented
- Where: `crates/qq-core/src/authority/gate.rs:421-439`. `check_words` has no arm for `quaqua call grant.*`, so it reaches `names_db` and gets `Allow`. `crates/qq-core/src/daemon/grant.rs:79-117` only notifies.
- R14 and Q17 say a grant is set "only through `ask` plus an out-of-band herdr notification". The T6 brief left out the ask, and no other unit picked it up.
- Probe: `quaqua gate --json '{"tool_name":"Bash","tool_input":{"command":"quaqua call grant.full --args {\"repo\":\"/x\"}"}}'` exits 0 with the daemon down. Any approval-list hit would deny in that state, so grant writes are not on the list.
- Failure: in auto mode the CoS runs `quaqua call grant.full '{"repo":"/abs/repo"}'`. The herdr notification fires, which does not wait for anh iu, and the row commits. From then on every non-floor action in that repo is `Low`, including tags, releases, unmerged-branch deletes and `pr.merge` on non-fence PRs (`tier.rs:192-206`). anh iu never gave consent.
- Fix: add `quaqua … call grant.full|grant.tier|grant.lane` to the approval list as an always-ask action, the same way `InstructionFile` works. Add a `grant_write_*` or `ask_tier_*` test that asserts `Ask` for cos and `Block` for a worker. Note that a direct socket call skips the hook entirely (see m5), so the ask is a tripwire, like the rest of the gate.

### M2. Tier is judged by the hook cwd, not the command's target: a grant on one repo works on another
- Where: `crates/qq-core/src/authority/gate.rs:210-211`, where `judge` computes `ctx` from `cwd` only. `gate.rs:739-740` (`check_git` skips `-C <dir>`) and `gate.rs:745-747` (`--git-dir`/`--work-tree`) also skip the target. `gate.rs:216` passes `branch_delete_action(dir, …)` the cwd. `check_gh` ignores `-R/--repo`, and the scanner does not follow `cd`.
- Failure: a full grant (or `repo_tier=low` plus `plan_lane=low`) on repo A, and a CoS session with its cwd in A. Each of these is judged at A's `Low` and allowed, while B (unlisted, so `High`) should `ask`:
  - `git -C ~/B tag v1`
  - `cd ~/B && git tag v1`
  - `gh -R owner/B release create v1`
  - `git -C ~/B push origin --delete feat`. Merge-base also runs against A, so feat reads as unmerged/medium, which is still allowed.
- The reverse also holds: a session whose cwd is outside A cannot use A's grant (fails closed, nit). Q17 decided the grant is per project, and this breaks that scope. `tier_source_wired_*` "grant on another repo" moves the cwd, so it does not test this case.
- Fix: when a hit's segment carries `-C`, `--git-dir`, `--work-tree`, `GIT_DIR=`, `-R/--repo`, or the command has `cd`/`pushd` before it, judge at `Tier::High` (or at the strictest of cwd and the resolved target). Add one test per spelling.

## Minor

- **m1. The merge-base class never changes a verdict.**
  - Where: `crates/qq-core/src/authority/tier.rs:46-47,70-77`. `BranchDeleteMerged` is Low and `BranchDeleteUnmerged` is Medium. Both give `Allow` at a Low/Medium context and both `Ask` at High, so `branch_delete_action` (`tier.rs:223-257`) is inert. R7/Q14 list only the merged delete as low and never class an unmerged one.
  - Failure: CoS at `repo_tier=low` runs `git push origin --delete wip-unmerged` and gets Allow, losing unpushed-elsewhere work with no ask.
  - Fix: class an unmerged delete as High (ask), or drop the git calls.
- **m2. Gate parse misses (binary, verified).**
  - `git tag --sort=refname v9` creates tag `v9` (checked in a scratch repo) but the gate returns Allow for everyone, because `reads` beats `writes` in `gate.rs:868-904`.
  - `git branch -C feat main` force-copies feat over local main and gets Allow (`gate.rs:857-858` checks only `-m/-M/--move/-f/--force`).
  - `cat x > ~/quaqua/runs/qq.db` gets Allow: `cat` is READ_ONLY and the redirect target is not seen.
  - `python3 -c "…sqlite3.connect('runs/qq.db')"` gets Allow: `names_db` matches only a word whose basename is `qq.db` (`gate.rs:476`).
  - Failure: a worker writes the tier tables directly with the python form (R14 says the gate "denies Bash that names runs/qq.db"), or forges a local tag.
- **m3. Fallback parity (`herdr-gate.sh`, binary missing), verified against the binary with the daemon down.**
  - `gh -R o/r pr merge 5` passes the fallback (exit 0) but the binary hard-blocks it, because `herdr-gate.sh:34` needs `gh[[:space:]]+pr`.
  - `gh -R o/r release create v1` passes the fallback (`herdr-gate.sh:37`).
  - `rm ~/quaqua/runs/qq.db` passes the fallback, which only knows `sqlite3`.
  - Agreed on: `git -C … tag`, `herdr pane send-keys`, `git push`, `git branch -D`. The fallback is stricter for cos on pane input and on instruction files matched by basename, which is acceptable.
  - Fix: allow global flags between `gh` and the subcommand (`gh([[:space:]]+-[^[:space:]]+([[:space:]]+[^-][^[:space:]]*)?)*[[:space:]]+pr…`), and add a generic `qq\.db` arm.
- **m4. The R13 fence list misses files that weaken the fence.**
  - Where: `crates/qq-core/src/daemon/pr_merge.rs:31-42`.
  - `crates/qq-core/src/driver/claude.rs` is not fenced. Its `GATE_MATCHER`/`install_hook` (`claude.rs:17,76`) decides which tools reach the gate in every worker.
  - `.github/` is not fenced: the workflows behind the "green" checks `pr.merge` trusts, and CODEOWNERS itself.
  - `crates/qq-core/src/db/sql/` (the grant tables) is not fenced.
  - Failure: under a grant, a non-fence PR that narrows `GATE_MATCHER` to `^Bash$`, or deletes a required workflow, is tier Low and merges through `pr.merge` with no anh iu review.
  - The list matches R13 word for word, so this is a requirement gap: raise it with anh iu.
- **m5. R12 is test-green but unenforced.**
  - `rerun_isolation_*` (`tests/rerun_isolation.rs:102-130`) proves only that peer resolution maps a child to its nearest pane. The engine has no rerun launcher.
  - Any process in the CoS pane tree resolves as cos (`daemon/caller.rs:51-59`) and can call `grant.*` and `pr.merge` straight on the socket, with no hook. For example, the CoS runs a worker branch's `cargo test`, or a script from a worker report.
  - Failure: a worker-authored test calls `grant.full` on its own repo during a CoS-run check, then `pr.merge`.
  - R12 needs a launcher (verifier pane or daemon-run, no socket) before any rerun counts as merge evidence. Until then, the CoS must not run worker code in its own pane.
- **m6. R5 covers Write/Edit only.**
  - `echo x > ~/quaqua/AGENTS.md` (and `sed -i`, `cp`) from a worker gets Allow (probe exit 0).
  - This is within the R5 text, but it is the obvious route around it. The `memwrite` phase plans Bash denies for USER.md and rules.md; extend that to the three instruction files.

## Nit

- **n1. `set_test_hooks` is `pub #[doc(hidden)]` with no cfg or feature gate** (`pr_merge.rs:110-116`).
  - The workspace has one crate (`Cargo.toml` members = qq-core) and plugins are JS, so only code compiled into qq-core or its integration tests can reach it. No RPC path reaches it.
  - Risk is low. Still, put it behind `#[cfg(any(test, feature = "test-hooks"))]` with the feature enabled only in dev-deps.
- **n2. No `.claude` or `install_hook` matcher routes the `NotebookEdit` arm** (`gate.rs:168`, `herdr-gate.sh:63`). Matchers are `^(Bash|Write|Edit|MultiEdit)$` (`claude.rs:17`) and `Write|Edit|MultiEdit` (template `settings.json:47`).
- **n3. (g) No drift today, but no test guards it.**
  - The 3 MCP regexes are byte-identical in `gate.rs:59-63`, `claude.rs:17-22`, template `settings.json:29,35,41` and `herdr-gate.sh:38-42` (mechanical `cmp`).
  - `hook_routes.rs` checks sample names, not text. Add one test that extracts and compares all four.
- **n4. Floor omissions are deliberate** (`docs/research/hermes-cos-memory/mcp-outward-tools.md` "Borderline, excluded"). They include TinyFish monitors and browser sessions, `Claude_Docs__*`, chrome. Revisit once billing is confirmed, since they are allowed without an ask today.
- **n5. `plan_lane` is keyed by repo key** (`tier.rs:203-205`), so the "plan lane" is a second per-repo row. `grant.lane` takes a raw string, so a key typo silently has no effect (fails closed to High).
- **n6. `grant.rs:89-115` holds the SQLite write transaction open across the herdr `notification_show` call.** A slow herdr blocks every other qq.db writer for that time.
- **n7. A successful `pr.merge` is not journaled** (`pr_merge.rs:302`). Only refusals are.
- **n8. `install_hook` change** (`claude.rs:76`): if the gate binary is missing, the worker's `|| exit 2` now blocks every Write/Edit, not just Bash. The writer flagged this.
- **n9. The engine repo's own `.claude/settings.json` still routes only Bash.** Engine sessions that are not spawned workers get no Write/Edit or MCP gate.

## Checks that hold

- **(a)** Non-cos callers are denied. `judge` blocks `Caller::Other` and `Caller::Unknown` before any tier read (`gate.rs:205-209`). `caller::lookup` maps any RPC failure to `Unknown` (`caller.rs:62-78`), so a down daemon means deny. Probes with the daemon down gave exit 2 on every hit.
- **(b)** The R7 table holds. `decide` (`tier.rs:60-80`):
  - push main and force-push → Deny always.
  - Floor, TrunkMove, Bypass, DbWrite and InstructionFile → Ask at every tier. A grant lowers only `ctx`, so the floor and these guards stay ask under a full grant.
  - HistoryRewrite carries its own High class, so it always asks.
  - High allows only PushNonMain and OpenPr. Raw `git push` stays a hard block for everyone (`gate.rs:772-780`).
- **(e) `pr.merge`** checks, in order:
  1. cos from the peer gate.
  2. pr is digits and sha is 40-hex.
  3. sha == the journaled clean reviewed sha.
  4. A High base is refused before gh runs.
  5. head == sha.
  6. The full-diff fence forces High after the grant (`pr_tier` runs on the granted base, `pr_merge.rs:275`).
  7. Checks are non-empty and all pass/skipping.
  8. `--match-head-commit <sha>`.

  gh runs as argv with cwd taken from the journal, never a shell (`pr_merge.rs:135-142,194-303`). Residual race: a check can turn red between `checks` and `merge`, and branch protection covers that.
- **(f)** Grant writes require a peer-identified caller (`caller_pane`), `cos`, and `check_orchestration(Op::ConfigWrite)` before any argument is read (`grant.rs:22-42`). There is a `grant_note` row per change. A failed notification rolls back, and the test asserts `repo_tier`/`grant_note` counts are 0 (`tests/grant.rs:224-236`).
- **(j)** A plain Write/Edit through `quaqua gate` takes 5-6 ms with exit 0 and no daemon call. The daemon is asked only on an approval-list hit (`gate.rs:202-205`).

## Probe corpus (binary exit / fallback exit, daemon down)

```
bin=2 fb=0  gh -R o/r release create v1
bin=2 fb=0  gh -R o/r pr merge 5
bin=2 fb=0  rm ~/quaqua/runs/qq.db
bin=0 fb=0  cat x > ~/quaqua/runs/qq.db
bin=0 fb=2  python3 -c "import sqlite3;sqlite3.connect('runs/qq.db')"
bin=0 fb=0  git branch -C feat main
bin=0 fb=0  git tag --sort=refname v9
bin=2 fb=2  git -C /other tag v1
bin=2 fb=2  herdr pane send-keys p1 hi
bin=2 fb=2  git push origin feat
bin=2 fb=2  git branch -D feat
```
