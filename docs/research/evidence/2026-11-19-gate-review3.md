# Gate review 3: gatefix3 + pushfix on hermes3-integ (6be07e6)

Reviewer: suhuynh-gate3 (read-only). Base: `quaqua/hermes3-integ` at 6be07e6.
Earlier reviews: `2026-11-19-gate-rereview.md` (N1–N3, P1, m-new-1..3) and
`2026-11-19-t9-memfix-review.md` (T9 majors and minors).

**Summary.** Most named items are fixed: N1, N2, m-new-2, the old-binary hook, push case
variants, and the m-new-3 false hits. The fail-closed rules add about 10 false
asks across 4,934 CoS commands (0.2%). The hook probe adds about 4 ms per Write.
Neither opens a new hole. Three majors remain:

1. Clustered short wrapper flags (`env -iC`, `sudo -Eu`) skip every hard block.
   This is the P1 class.
2. The push door's remote-default check reads the fetch URL, but the push goes
   to the push URL.
3. Read-path writers remain: a sed `r` backslash continuation, `xxd`, and
   `rg --pre`.

## Method

- **Gate probes.**
  - Binaries: `target/debug/quaqua gate`, with no daemon and `QUAQUA_HOME` set to a scratch workspace.
  - Daemon-backed probes use a scratch copy of the tree with two extra test files:
    - `tests/probe3.rs`: CoS and worker roles in the `gate_fix.rs` rig, with a full grant on repo A; repo B is unlisted.
    - `tests/probe_push3.rs`: local bare remotes, using the `cos_push_door.rs` rig.
  - Neither file is committed.
- **On-disk checks.** Writer probes ran in a scratch directory under BSD sed and macOS tools.
- **Hook probes.** `herdr-gate.sh` ran with `QUAQUA_BIN` set three ways: the branch binary, an e723967 build, and `/nonexistent-qq`.
- **Replay.** I took the 4,934 Bash commands from 55 CoS sessions (`~/.claude/projects/-Users-tinhtute-quaqua/*.jsonl`). Each went through `gate_evaluate_in` as the CoS, behind a daemon, on two trees: b94e630 (pre-gatefix3) and 6be07e6. Paths under `~/quaqua` were rewritten to the scratch workspace.

## Tests (foreground, one filter per run)

```
cargo test --locked -p qq-core --test gate_fix gate_fix3
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 54 filtered out

cargo test --locked -p qq-core --test cos_push_door
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

tests/qq-test.sh memory_path_gate hook_settings_route
8 passed (memory_path_gate 3, hook_settings_route 5), 0 failed; exit=0
```

## Earlier findings

| Item | Status | Probe (rc: 0 allow, 2 block or ask; verdicts are from the rig) |
|---|---|---|
| N1 cd-spelling tier escape | FIXED | All 21 spellings ASK in the CoS rig (cwd A, then cd to B, then `git tag v1`): `&`, builtin, command, eval, `\cd`, `"cd"`, `c\d`, `set --`/`cd "$1"`, `cd -`, CDPATH, popd, pushd, alias, function, `x=cd; $x`, `. <(…)`, `;`, `\|\|`. Residual: `source /dev/stdin <<< 'cd {B}'; git tag v1` → ALLOW (finding 6). |
| N2 `git -C .` after cd | FIXED | `cd {B} && git -C . tag v1` → ASK; `-C sub` → ASK. |
| N3 env-chdir / GIT_DIR | FIXED for the named forms | `env -C`, `--chdir=`, `--chdir`, `sudo -D`, `export GIT_DIR`, `declare -x`, `typeset -x`, `export GH_REPO` → ASK. Residual: clustered `env -iC {B} git tag v1` and `sudo -nD {B} git tag v1` → ALLOW (finding 1). |
| P1 wrapper flags skip hard blocks | PARTLY FIXED | All 6 named long-option spellings → rc=2. Clustered short flags still give rc=0 on base and branch alike (finding 1). |
| m-new-1 memory/instruction writers | FIXED for the named spellings | Worker BLOCK for: ln -sf, rsync, perl -pi, python open, git checkout --, tar -C, unzip, git restore, patch, and case variants (agents.md, user.md, Memory/Rules.md). Reads stay free: cat, cp out, diff, grep, wc, less, head, git diff. Residual: directory-destination copies (finding 5). |
| m-new-2 fallback parity | FIXED for the named cases | All six cases named in the re-review give bin=2 and fb=2 through `herdr-gate.sh`, with `QUAQUA_BIN=/nonexistent-qq` for the fallback: `git branch -C feat main`, `git tag --sort=refname v9`, `echo x >` AGENTS.md / USER.md, `>>` memory/rules.md, `sed -i … SOUL.md`, and the socat `grant.full` call. New drift: `gh pr -R o/B merge 5` and `gh pr --repo=o/B merge 5` give fb=0 (finding 7). The fallback's Write arm misses case-variant and symlink paths (finding 9). |
| m-new-3 grant tripwire false hits | FIXED | Mail body, `git commit -m`, `python --verb`, `node --test` → ALLOW. `"$(cat …)"` call and `$Q call` → ASK. Residual: shell-on-stdin (finding 6). |
| T9 push to remote default | PARTLY FIXED | The plain cases are refused, and these tests are green: `cos_push_door_remote_default_refused_without_local_head`, `…_after_set_head_delete`, `…_via_second_remote`, `cos_push_door_unreadable_remote_refused`. Bypass: when the push URL differs from the fetch URL, three probe_push3 setups still push to the remote default `trunk`. All three end `ALLOWED {"branch":"trunk","pushed":true}` and the origin's trunk moves (finding 2). |
| T9 read-path writers | PARTLY FIXED | Named spellings (`sed w`/`I`, `uniq` output, `git grep -O`) → rc=2. The sed allow-list parser fails closed on most forms. All 21 legit sed reads, uniq reads and git reads give rc=0. New holes: finding 3. |
| T9 old-binary hook | FIXED | An e723967 binary fails the Write probe, so the hook falls back. Ordinary Write rc=0. USER/AGENTS/rules Write and NotebookEdit rc=2. `/nonexistent-qq` gives the same results. |
| T9 push case variants | FIXED | `cos_push_door_case_variants_refused` is green; `same_branch` lowercases both sides. |

## Hook probe latency (30 runs, median/p90)

| Binary | Hook Write (probe + gate) | Binary alone | Hook Bash (no probe) |
|---|---|---|---|
| branch | 15.1 / 15.7 ms | 3.6 ms | 10.9 / 11.2 ms |
| e723967 | 12.4 ms (falls back) | — | 9.7 ms |

The probe costs about 4 ms per Write/Edit and nothing on Bash. Its fallback
text rules add no new allow path: a stale binary only adds blocks.

## Replay: extra asks from the fail-closed change

4,934 CoS commands, run through the daemon-backed CoS rig.

| | ALLOW | ASK | BLOCK |
|---|---|---|---|
| b94e630 | 4,730 | 30 | 174 |
| 6be07e6 | 4,718 | 42 | 174 |

The only change is 12 commands moving from ALLOW to ASK, all under `approval list: the CoS's live instruction files`:

- **2 are real writes.** `quaqua init --update --accept AGENTS.md` (#3590, #4915).
- **1 is borderline.** `quaqua init --update` (#540) writes pending copies.
- **9 are false asks.** They only read or mention a live file name:
  - #2323, #4801: `for f in … AGENTS.md; do diff`/`wc`.
  - #4806, #4807: the same loop in a /tmp merge directory.
  - #4639: `git check-ignore`.
  - #4886: `md5 -q AGENTS.md`.
  - #2434, #2471, #2472: prompt text that quotes "AGENTS.md" to `herdr agent prompt` and `quaqua lead prompt`.

That is 9–10 false asks in 4,934 commands (about 0.2%): acceptable, but listed as finding 8.

A replay of the release binaries with no daemon (b94e630 build, sha f4db346a…, against the branch build) moves the same 12 commands, and nothing else. The replay rig grants only `repo-a`, so the base-to-branch change is what matters here, not the absolute ASK counts. In #540 the ask may come from its `for … AGENTS.md; do diff` loop rather than from `init --update`; I did not isolate which, so the count stays a 9–10 range.

## Findings

- [major] **Clustered short wrapper flags skip every hard block (P1 class still open).**
  - Code: `crates/qq-core/src/authority/gate.rs:941` (`single_letter_flag` accepts only `-X`) and `gate.rs:1887` (`wrap`). `-iC`, `-Eu`, `-tn` and `-vs` are neither forked nor read as value flags, so the value becomes the command word.
  - Probe (no daemon, any caller; rc=0 on both b94e630 and 6be07e6):
    - `env -iC /tmp git push origin feat`
    - `env -iu FOO git push …`
    - `sudo -Eu root git push …`
    - `sudo -nu root …`
    - `xargs -tn 1 git push …`
    - `timeout -vs KILL 5 git push …`
    - `env -iS 'git push origin feat'`
    - `sudo -Eu root herdr pane split p1`
  - The same class escapes the CoS tier (rig ALLOW): `env -iC {B} git tag v1`, `sudo -nD {B} git tag v1`.
  - Fix: split clustered short flags, treat the last letter as the value flag, and fork when unsure.
- [major] **Push door can still push to the remote default.**
  - Code: `crates/qq-core/src/authority/push.rs:115` (`remote_default` runs `git ls-remote --symref` on the fetch URL), `push.rs:127` (Empty → allow) and `push.rs:199-201`.
  - The push itself goes to the push URL. Probes (`probe_push3.rs`, local bare remotes):
    - (a) `remote.origin.url` = a decoy bare repo whose HEAD is `decoy`, plus `remote.origin.pushurl` = the real origin.
    - (b) An empty bare repo as the fetch URL, plus a pushurl.
    - (c) `url.<real>.pushInsteadOf=<decoy>`.
  - All three: `ALLOWED {"branch":"trunk","pushed":true,"remote":"origin"}`, and the real origin's `trunk` moved.
  - The setup commands give rc=0 at the gate: `git config remote.origin.pushurl …`, `git remote set-url --push …`, `git config url.X.pushInsteadOf …`.
  - Fix: resolve the effective push URL (`git remote get-url --push`, including pushInsteadOf), run ls-remote against that URL, and refuse on Empty.
- [major] **Read-path writers remain.** Each probe gave rc=0 and was checked on disk:
  - `gate.rs:1367`: `sed_script_reads` applies backslash continuation to `r`/`R` filenames, so the next line's `w` is hidden. `sed -n 'r /dev/null\⏎w USER.md' in.txt` wrote USER.md (BSD sed). The same works for `memory/rules.md` and `AGENTS.md`.
  - `gate.rs:1109`: `xxd in.txt USER.md` and `xxd -r in.hex memory/rules.md` write the second operand. This was pre-existing and T9 missed it.
  - `gate.rs:1098`: `rg --pre tee x USER.md` truncated USER.md to 0 bytes.
  - `gate.rs:1112`: `bat --pager='tee USER.md'` gives rc=0. bat is not installed here, so this is unverified.
  - Fix: end `r`/`R`/`w` filenames at the newline, and drop xxd from READ_ONLY. Treat `rg --pre`/`--pre=` and `bat --pager` as writers.
- [minor] **Unlisted exec wrappers.** Pre-existing; `gate.rs:1007` lists the known wrappers. `caffeinate -i git push …` (stock macOS), `script -q /dev/null git push …`, `doas`, `unbuffer`, `flock`, `watch` and `parallel` give rc=0 for hard-deny pushes.
- [minor] **Directory-destination copies onto live files.**
  - Code: `gate.rs:542` (`copy_like`) and `gate.rs:465` (`names_live_file`). Only the destination path is checked, not destination directory plus source basename.
  - Worker probes, each rc=0 or ALLOW:
    - `cp /tmp/USER.md WS`
    - `cp -t WS /tmp/USER.md`
    - `mv /tmp/USER.md WS/`
    - `cp /tmp/rules.md WS/memory/`
    - `cp /tmp/x/USER.md .` (cwd WS)
    - `install /tmp/USER.md WS`
    - `cp /tmp/AGENTS.md WS`
    - `mv /tmp/SOUL.md WS/`
- [minor] **Shells fed on stdin bypass the cd and grant tripwires.**
  - Code: `gate.rs:552` (`bash_memory` does not recurse into stdin scripts).
  - Probes:
    - `source /dev/stdin <<< 'cd {B}'; git tag v1` → ALLOW in the CoS rig.
    - `sh -s <<< 'quaqua call grant.full'` → ALLOW.
    - `echo 'quaqua call grant.full' \| bash` → ALLOW.
    - `… \| sh -s` → ALLOW.
- [minor] **`gh` repo flag after the subcommand group.**
  - `gate.rs:1572` matches `release create` only as adjacent words. On the branch binary, `gh release -R o/B create v1` and `gh release --repo o/B create v1` give rc=0, while `gh release create v1` gives rc=2. This is pre-existing: b94e630 has the same adjacent-word rule, and both spellings give rc=0 there too.
  - The fallback (`templates/workspace/scripts/hooks/herdr-gate.sh:52`) misses `gh pr -R o/B merge 5` and `gh pr --repo=o/B merge 5` (fb rc=0); the binary blocks both.
- [minor] **Over-blocks from fail-closed name matching.**
  - Code: `gate.rs:64` (`ASK_FILE`).
  - The replay found 9–10 false CoS asks in 4,934 commands. They are reads or prose that mention AGENTS.md: diff/wc loops, `md5`, `git check-ignore`, and quoted prompt text.
  - Workers also get BLOCK for `vim AGENTS.md` and `code AGENTS.md`, and for `git add AGENTS.md`.
  - CoS `git -C WS add USER.md` is still BLOCK; the T9 minor is still open.
- [trivial] **Fallback Write misses case variants and symlinks.** `herdr-gate.sh:91` matches basenames exactly. With a stale or missing binary, Write to `WS/agents.md`, or to a symlink pointing at USER.md, gives rc=0 (APFS is case-insensitive). The branch binary blocks both.
- [trivial] **`Write runs/qq.db`** gives rc=0 on the branch binary and in the fallback. Pre-existing.
  - Code: `herdr-gate.sh:91`; the Write arm in `gate.rs` (`names_live_file`, `gate.rs:465`) does not cover `qq.db`.
  - Probe: `hk.py`, Write to `WS/runs/qq.db`.
- [trivial] **`sort -oUSER.md`** (attached form) gives rc=0. `gate.rs:1142` checks only a detached `-o`. Pre-existing.

The fail-closed rules and the hook probe add no new allow path. The new holes
above sit in parser corners (clustered flags, sed continuation) and in
read-list entries, not in the both-readings fork itself. The gate probes in
findings 1, 3, 5 and 7 also give rc=0 on a verified b94e630 build (sha
f4db346a…). That includes the sed `r\` form, because base allowed sed `w`
outright. These holes were not introduced in this
round; this round left them open.

Verdict: 11 findings
