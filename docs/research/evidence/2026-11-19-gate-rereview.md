# Gate re-review: hermes-cos ship 3 fix round (quaqua/hermes3-integ ea08e65)

Reviewer: suhuynh-gate2 (read-only, non-writer). First review: `docs/research/evidence/2026-11-19-gate-T5-review.md` at ef60fd9 (branch quaqua/suhuynh-gate, 4a9a901). Fix round: e232fed..c945fec (sude-gatefix), merged as ea08e65. Out of scope here: m5 (R12 launcher, moved to phase `live`), the m1 verdict (anh iu's call) and memwrite M1/M3 (later memfix unit).

## Verdict: CHANGES REQUESTED

M1 is fixed, and so are m2, m3, m4 and m6 for every spelling the first review named. M2 is fixed for the named spellings, but the fix round's lone-`cd` carve-out (d9aba79) added a new way for one repo's grant to cover another: **N1**. Before d9aba79 every `cd` was judged High. Now `cd <granted> & git tag v1`, or `cd <granted> && builtin|command|eval cd <other> && git tag v1`, is allowed against the other repo. The carve-out buys almost nothing (1 of 51 replayed CoS approval-list hits, see "Unknown"), so dropping it, or tightening it as below, closes N1.

Separately, a **pre-existing** full bypass of the hard blocks (P1) is not from this fix round. It dates from 1e53ed3 and is present at 4a9a901. It should be fixed before ship 3 lands: `env --chdir /tmp git push origin feat` returns Allow for every caller.

## Phase check (non-writer rerun at ea08e65)

```
tests/qq-test.sh gate ask_tier tier_table caller_lookup gate_bypass instruction_file_gate pane_input_gate hook_settings_route grant_write tier_source tier_source_wired fence_path_tier rerun_isolation memory_path_gate push_refuses_main; echo exit=$?
  -> exit=0, no "matched zero tests" line, 174 passed in total, 0 failed
cargo test --locked -p qq-core --test orchestration_verbs pr_merge | tail -3
  -> test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 15 filtered out
cargo test --locked -p qq-core --test docs | tail -3
  -> test result: ok. 5 passed; 0 failed
cargo clippy --locked --all-targets -- -D warnings | tail -3
  -> Finished `dev` profile (no warnings), exit 0
```

Probes ran two ways. The first is a scratch copy of the `tests/gate_fix.rs` rig (live daemon, FakeHerdr, a CoS or worker peer, a full grant on repo A, repo B unlisted). That copy is not committed. The second is `target/debug/quaqua gate` with no daemon, plus `herdr-gate.sh` with `QUAQUA_BIN` pointed at a missing path.

## First-review items

| Item | Status | Evidence |
|---|---|---|
| M1 grant consent | **Fixed** | `ask_tier_grant_write_asks_cos_even_under_a_full_grant` and `ask_tier_grant_write_denies_everyone_else` (11 spellings, socat included). Probe: `quaqua call $V '{}'` and `printf grant.full \| xargs -I{} quaqua call {}` give ASK. Residuals in m-new-3. |
| M2 target-repo tier | **Fixed for the named spellings; new hole N1** | `tier_source_wired_target_repo_beats_the_hook_cwd`: `-C`, `-C a -C b`, `--git-dir`, `--work-tree`, `GIT_DIR=`, `env GIT_DIR=`, `cd`, `pushd`, `( … )`, `bash -c`, `gh -R/--repo`, `GH_REPO=`. Probe: `gh --repo=owner/B release create`, `git -C $B branch -D feat` and `cd $B\ngit tag v1` (from A) give ASK. |
| m1 merge-base class | **Not fixed (verdict deferred to anh iu)** | The class is now judged on the deleted ref (`ask_tier_branch_delete_class_follows_the_deleted_ref`, tier.rs:229-266). `ask_tier_branch_delete_unmerged_is_medium_and_still_cos_alone_at_low` pins Allow at Low and Medium, so the class still never changes a verdict. |
| m2 parse misses | **Fixed** | `ask_tier_tag_create_spellings_with_options`, `ask_tier_branch_copy_onto_trunk` and `pane_input_gate_qq_db_writes_by_redirect_and_interpreter`. Probe: `git tag --sort refname v9`, `git tag -e v9`, `git branch --copy --force feat main`, `cp/ln -sf/rsync … runs/qq.db` and `sqlite3 runs/qq.db-wal` give ASK. Read forms (`git tag -l/--list/--sort=…/--contains`, `sqlite3 -readonly`, `cat qq.db`) give ALLOW. |
| m3 fallback parity | **Fixed for the three named cases** | `hook_settings_route_fallback_global_flags_before_the_gh_subcommand`. Probe (bin/fb, no daemon): `gh -R o/r release create v1` 2/2, `gh -R o/r pr merge 5` 2/2, `rm …/qq.db` 2/2, `cat x > …/qq.db` 2/2, python sqlite 2/2. New drift: m-new-2. |
| m4 fence list | **Fixed** | `fence_path_tier_covers_matcher_workflows_and_grant_sql`. pr_merge.rs:31-44 now holds `.github/`, `crates/qq-core/src/db/sql/` and `crates/qq-core/src/driver/claude.rs`. |
| m6 Bash writes to instruction files | **Fixed for the named spellings; residual m-new-1** | `instruction_file_gate_bash_writes`. Probe as a worker: `echo >`, `>>`, `sed -i`, `cp`, `tee`, `dd of=`, `mv` and `cd $WS && echo x > AGENTS.md` give BLOCK. A template or repo-relative `AGENTS.md` gives ALLOW. |

## New in the fix round

### N1 (major, regression from d9aba79): the lone-`cd` carve-out lets a grant on A cover a command that acts on B
- **Where:** `crates/qq-core/src/authority/gate.rs:565-576`. `lone` accepts a command whose first segment is `cd <literal>`, that has exactly one segment starting with the word `cd`, and that has no `;` `|` `(` `` ` `` `$(`. Every `Here` hit is then retargeted to that directory.
- **Probe:** CoS, hook cwd B (unlisted, High), full grant on A. Each of these returns **ALLOW**, while the command acts on B:
  - `cd $A & git tag v1`: `&` backgrounds the `cd` in a subshell, so `git tag` runs in B.
  - `cd $A && builtin cd $B && git tag v1`
  - `cd $A && command cd $B && git tag v1`
  - `cd $A && eval cd $B && git tag v1`, and `eval 'cd $B'`
  - `cd $A && env -C $B git tag v1` (P2 combined with the carve-out)
- **Before d9aba79:** every `cd` made the hits Unknown, so each of these asked.
- **Failure:** under a grant on one repo, a tag, release or branch delete on an unlisted repo goes through with no ask. That is the M2 failure again, through the fix for it.
- **Fix:** either drop the carve-out (the replay below shows it saves 1 ask in 51), or keep it only when every separator in the command is `&&` (reject a lone `&`), no segment's command word is `eval`, `builtin`, `command`, `source` or `.`, and no wrapper carries a chdir flag. Add one `tier_source_wired_*` case per spelling above.

### N2 (minor): a relative `git -C` after a `cd` resolves against the hook cwd
- `cd $B && git -C . tag v1` and `cd $B && git -C sub tag v1`, run from A, return **ALLOW**.
- `retarget` skips hits that already carry `Target::Dir`, and `judge` resolves `Dir` against the hook cwd (`gate.rs:304`, `target_dir`), not against the `cd` target.
- This predates d9aba79 (from 7d9e344), and the M2 test asserts `git -C . tag v1` from A, which is correct on its own.
- **Fix:** when a `cd` precedes, join a relative `-C` onto the `cd` target, or judge it Unknown.

### N3 (minor): M2 spellings still not read
These return **ALLOW** from A with the target in B:
- `env -C $B git tag v1`, `env --chdir=$B …` and `env --chdir $B …`. `wrapper` skips `-C` as a value flag without retargeting.
- `sudo -D $B git tag v1`
- `export GIT_DIR=$B/.git && git tag v1`, also with a newline instead of `&&`. `retargets` only reads assignments that prefix the same segment.
- **Fix:** treat `env -C/--chdir`, `sudo -D/--chdir` and any `export GIT_DIR|GIT_WORK_TREE|GH_REPO` earlier in the command as Unknown.

### m-new-1 (minor): the instruction-file Bash rule is narrower than the memory-file rule
- As a worker, these return **ALLOW** onto the live `AGENTS.md`: `ln -sf /tmp/x $WS/AGENTS.md`, `rsync /tmp/x $WS/AGENTS.md`, `perl -pi -e … $WS/AGENTS.md`, `python3 -c "open('$WS/AGENTS.md','w')…"` and `git -C $WS checkout -- AGENTS.md`.
- `perl -pi … $WS/memory/rules.md` **BLOCKS**: `bash_memory` (gate.rs:442-476) flags any non-read-only command naming the file, while instruction files go through `write_targets` (gate.rs:859-885), which lists only `tee`, `mv`, `truncate`, `rm`, `unlink`, `shred`, `sed -i`, `cp`, `install` and `dd`.
- **Fix:** route the three instruction files through the `bash_memory` rule, keeping the Ask verdict for the CoS.

### m-new-2 (minor): fallback drift on the spellings the fix round added to the binary
- With the binary missing and no daemon, these give bin=2, fb=0: `git branch -C feat main`, `git tag --sort=refname v9`, `echo x > ~/quaqua/AGENTS.md`, `sed -i … ~/quaqua/SOUL.md`, `echo x > ~/quaqua/USER.md` and `echo '{"verb":"grant.full"}' | socat - UNIX-CONNECT:…`.
- The fallback's Write/Edit arm gained `USER.md`. Its Bash arm has no instruction-file, memory-file or `grant.` pattern.
- `quaqua call grant.*` with the binary missing is moot, but the socat form is not.

### m-new-3 (minor): M1 tripwire edges
- **Misses, ALLOW:** `quaqua call "$(cat /tmp/v)" '{}'`, because the tokenizer resolves the substitution, so `opaque` never sees `$`. `Q=quaqua; $Q call grant.full '{}'`, because the command word is `$Q`, not an RPC tool.
- **False hits:**
  - `quaqua mail send --to x --body 'grant.full consent landed'` (`check_quaqua` matches any argument) and `git commit -m 'grant.tier' && quaqua status` (the text-wide scan at gate.rs:538-546) are approval-list hits. A worker is **blocked** from mailing the CoS about this very feature.
  - `python3 scripts/report.py --verb grant.full` and `node --test …/grant.full.test.js` ASK for the CoS.
- **Fix:** match only the verb position (`call <verb>`) for `quaqua`, and add an opaque-command-word case (`$X call`).

## Pre-existing, found on this pass

### P1 (major, pre-existing since 1e53ed3): wrapper long options with a separate value skip every hard block
- **Where:** `gate.rs:1406-1414`, `wrapper`. A `--long` flag is skipped as one word, so its separate value becomes the command word, and the real command is never checked.
- **Probe:** `target/debug/quaqua gate`, no daemon, every caller. Each of these exits **0**, while `git push origin feat` exits 2:
  - `env --chdir /tmp git push origin feat`
  - `nice --adjustment 5 git push origin feat`
  - `timeout --signal KILL 5 git push origin feat`
  - `sudo --user x git push origin feat`
  - `xargs --max-args 1 git push origin feat`
  - `env --chdir /tmp herdr pane split p1`
- The `=` forms (`env --chdir=/tmp …`) are caught. The regex fallback catches all of these.
- **Failure:** a worker pushes (or `HEAD:main`) and spawns panes past the binary gate. The daemon push guard and GitHub branch protection remain the fences, so the push to main is still refused remotely where protection is on.
- **Fix:** a per-wrapper table of long options that take a value (`--chdir`, `--split-string`, `--user`, `--group`, `--adjustment`, `--signal`, `--kill-after`, `--max-args`, `--max-lines`, `--max-procs`, `--replace`, `--arg-file`, `--delimiter`, …). When an unknown `--long` is followed by a non-flag word, scan both readings and keep the strictest. Add one `gate_bypass_*` case per wrapper.

### Notes (not regressions)
- `git checkout -B main …` and `git switch -C main …` force-move local main with ALLOW for every caller. R7's bounded list names `git branch -m/-M/-f` only. Raise with anh iu if the list should grow.
- `python3 -c "…connect('runs/q'+'q.db')"` gives ALLOW: a run-time-built path gets past any text tripwire, as the R4 comment at gate.rs:439-441 says.
- R13 fence: `mods/cos-memory/` (the `memory.write` routing) and `crates/qq-core/src/memory/` are not fenced. Under a grant, a PR that unroutes cos-memory writes is Low. The list matches R13 plus the m4 additions, so this is a requirement question for anh iu.

## Unknown: does fail-closed High on a compound `cd` over-ask for normal CoS work?

Only on approval-list hits. Ordinary commands never reach the tier code, and the daemon is asked only on a hit (gate.rs:284-286).

**Replay:** every Bash command from 45 CoS sessions (8649 commands) went through the binary gate with no daemon.
- 6207 of them (72%) contain a `cd`.
- 51 of those are approval-list hits, about one per session.
- Only 1 of the 51 matches the lone-`cd` form.

So with the carve-out, about 50 hits still ask, and without it all 51 ask. The 51 include always-ask classes (`qq.db`, instruction files, `grant.*`) and cos-only memory writes, which ask or pass whatever the `cd`. So 50 is an upper bound on extra asks, and those happen only once a grant or a Low/Medium row exists, since today every repo is unlisted and High. The over-ask cost is small. It does not justify the N1 exposure.

**Fail-closed rows confirmed (ASK):** `cd $A && cargo test 2>&1 | tail -3 && git tag v1` and `cd $A && git fetch -q; git tag v1`, both with the grant on A.

**Carve-out rows confirmed (ALLOW):** `cd sub && git tag v1`, `cd $A/sub && git tag v1` and `cd $A && git branch -d merged-x`. The repo key is the git common dir, so a subdirectory keeps the grant.

## Checks that still hold
- **Non-cos and no-daemon deny:** `judge` (gate.rs:288-293) denies before any tier read, and a worker probe of `git tag v1` gives BLOCK.
- **Memory files:** `memory_path_gate_*` is green. A worker `echo x > $WS/USER.md` and `perl -pi … memory/rules.md` give BLOCK. `cat`, `grep`, and `git -C $WS log/diff` naming them give ALLOW.
- **Always-ask classes:** `GrantWrite` joins `Floor | TrunkMove | Bypass | DbWrite | InstructionFile` in `tier::decide` (tier.rs:69), so a full grant never lowers it.
