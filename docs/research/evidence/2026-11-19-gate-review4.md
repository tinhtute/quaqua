# Gate review 4: sude-pushurl + sude-gateallow on hermes3-integ (1164df7)

Reviewer: suhuynh-gate4 (read-only). Base of the delta: cdd727d (review 3).
Delta: sude-pushurl 65e91e8..ea1bbeb, sude-gateallow 6dcf489..9026d5e.

**Summary.**
- Review-3 major 1 (clustered wrapper flags) is closed. Every review-3 spelling and every held-out wrapper spelling here now refuses.
- Review-3 major 3 (read-path writers) is closed for every named form.
- Review-3 major 2 (push URL) is closed for static configs, but a pushurl flip between the check and the push still lands on the remote default. That is new finding A (major).
- Separately, git's own option and config surface still lets a hard-deny push through (finding B, major). It was open before this delta, and the allowlist change did not reach it.
- Normal CoS work is not hurt. The replay reproduces the worker's figure exactly: 1 false block in 4,934 commands.

## Method

- **Builds.** Each commit is built from `git archive` in scratch, with its own target dir: `tgt-base` for cdd727d, `tgt-head` for 1164df7, `tgt-wt` for the worktree tests.
- **Gate probes.** `quaqua gate` with no daemon, base and head side by side. rc=0 means allowed for any caller. rc=2 means a hit (block, or ask behind the no-caller refusal).
- **Push door.** `probe_push4.rs` is the `cos_push_door.rs` rig plus 4 probes, run in the scratch copy against local bare remotes.
- **Replay.** The review-3 `probe_replay.rs` and `corpus.jsonl` are copied unchanged from the read-only gate3-rig. They ran in the daemon-backed CoS rig on both trees.
- **On-disk.** The git mechanisms were checked with harmless payloads (`touch`, `rev-parse`). This session's own gate refuses a literal `git push`, so no push payload was run outside the door rig.

## Tests (foreground, one filter per run, worktree at 1164df7)

```
cargo test --locked -p qq-core --test gate_fix
test result: ok. 72 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 17.85s
cargo test --locked -p qq-core --test cos_push_door
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.35s
cargo test --locked -p qq-core --test orchestration_verbs
test result: ok. 30 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.34s
```

## Replay (4,934 CoS commands, daemon-backed CoS rig)

| Build | ALLOW | ASK | BLOCK |
|---|---|---|---|
| cdd727d (base) | 4,718 | 42 | 174 |
| 1164df7 (fix) | 4,717 | 42 | 175 |

- **One move, ALLOW→BLOCK: #166.** A shell function feeds `herdr pane split …` as a quoted argument of `jq`, piped into the hook. The block message is the pane message.
- **No ALLOW→ASK moves.** The worker figure (1 false block, 0.02%) reproduces exactly.

## Review-3 findings

| # | Finding | Status | Probe (base rc → head rc) |
|---|---|---|---|
| 1 | Clustered wrapper flags | CLOSED | All 8 review-3 spellings 0→2: `env -iC`, `env -iu`, `sudo -Eu`, `sudo -nu`, `xargs -tn`, `timeout -vs`, `env -iS '…'`, `sudo -Eu … herdr pane split`. Held out: `caffeinate env -S '…'`, `caffeinate sudo -u root -- env -i git push`, `env -S'git push …'`, `script -q /dev/null git push`, `su root -c '…'`, `ssh host '…'`, `watch -n1 '…'`, `parallel ::: '…'` all 0→2. `nohup --`, `stdbuf -oL`, `time -p`, `command -p`, `exec -a`, `xargs -I{}`, `nice -n5 -- timeout 5 sudo`: 2→2. |
| 2 | Push door reads the fetch URL | CLOSED for static config; OPEN under a flip (finding A) | Six new `cos_push_door` tests are green: pushurl decoy, empty fetch URL, pushInsteadOf, multiple pushurls, chained insteadOf, URL as remote. A hung transport (`core.sshCommand='sleep 120 #'`) is refused at 20.07 s. An empty origin with unborn HEAD `trunk` is refused. A pushurl flip during the call moves the real `trunk` (A). |
| 3 | Read-path writers | CLOSED for the named forms | `sed -n 'r /dev/null\⏎w USER.md'`, `xxd in USER.md`, `rg --pre tee x USER.md`, `bat --pager='tee USER.md'` all 0→2. Held out: `sort -oUSER.md` and `sort -ruoUSER.md` 0→2. `sed --posix 'w …'`, `sed '…;0~2w …'`, `uniq -c/-D --/--group/-f1 in USER.md`, `git log --output=`, `git diff --ou=`, `git grep -O'tee USER.md'`, `git -c core.pager=… -p log`, `git -c diff.external=… diff` all 2→2. Reads stay free: `cat`, `grep -rn git push`, `echo git push`, `sed -n 1,5p`, `sort -u`, `rg -n --hidden`, `git diff --stat`, `git log`, `awk -F:`, `uniq -c`, `diff -u` all 0→0. |
| 4 | Unlisted exec wrappers | CLOSED | `caffeinate …`, `script -q /dev/null git push`, `watch`, `parallel`, `ssh`, `su -c`: 0→2. |
| 5 | Directory-destination copies | OPEN, unchanged | `cp /tmp/USER.md WS`, `cp -t WS …`, `mv … WS/`, `cp … WS/memory/`, `install … WS`, `mv /tmp/SOUL.md WS/`: 0→0. |
| 6 | Shells fed on stdin (cd/grant) | OPEN for grant | `sh -s <<< 'quaqua call grant.full'` and `echo '…grant.full' \| bash`: 0→0. The cd form needs the tier rig, which was not re-run; no delta code touches it. |
| 7 | `gh -R` after the subcommand group | OPEN, unchanged | `gh release -R o/B create v1`, `gh release --repo o/B create v1`: 0→0. Fallback script unchanged. |
| 8 | Over-blocks from name matching | OPEN, unchanged | Replay ASK 42→42. `for … AGENTS.md; do diff` and `md5 -q AGENTS.md`: 2→2. |
| 9 | Fallback Write misses case variants | OPEN, unchanged | `herdr-gate.sh` has no diff since cdd727d. The binary still blocks `WS/agents.md` (Write rc 2→2). |
| 10 | Write `runs/qq.db` | OPEN, unchanged | Write `WS/runs/qq.db`: rc 0→0. |
| 11 | `sort -oUSER.md` | CLOSED | 0→2 (see row 3). |

## Findings

- [major] **A. Push door TOCTOU: a pushurl flip between the check and the push lands on the remote default.** (new in this unit's design; hard deny)
  - Code: `crates/qq-core/src/authority/push.rs:236-258`. `remote_default` resolves the push URLs and checks each one.
  - Then `crates/qq-core/src/daemon/push_door.rs:128-129` pushes by remote *name* (`git push -- origin refs/heads/b:refs/heads/b`), so git re-reads `remote.origin.pushurl` and `insteadOf` at push time.
  - The Lead verb has the same shape: `push.rs:331-333`.
  - Probe `probe_push4.rs::p4_pushurl_flip_race`:
    - Setup: origin is a bare repo with HEAD `trunk`; the decoy is a bare repo with HEAD `other`.
    - A thread alternates `git config remote.origin.pushurl <decoy|origin>` while the CoS calls `push.cos {branch: trunk}`.
    - Run 1: the real origin's `trunk` moved on call 3 (ok=2 refused=1, 91 flips).
    - Run 2: it moved on call 4 (ok=3 refused=1, 123 flips). Both runs took under 1 s.
  - The gate lets the flip loop run: review 3 measured `git config remote.origin.pushurl …` at rc=0.
  - Fix: push to the URLs that were checked (`git push -- <checked-url> <refspec>`, one per URL; `url_is_stable` already proves each one is free of rewrites), never by remote name.
- [major] **B. git's own option and config surface still lets a hard-deny push through.** (pre-existing: base rc=0 too; not opened by this delta, but outside the allowlist premise it claims)
  - Code: `crates/qq-core/src/authority/gate.rs:1793-1835` (`check_git_at`).
    - It skips any unknown `-x`/`--x` global option as one word.
    - It reads `-c` and the env only for `alias.` keys.
    - An unknown subcommand passes (`_ => Found::pass()`, `gate.rs:1863`).
    - Assignment words before the command word are not scanned (`check_words_in`, `gate.rs:987`).
  - Probes, rc=0 on base and head:
    - `git --attr-source HEAD push origin main`
    - `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=alias.ship GIT_CONFIG_VALUE_0=push git ship origin main`
    - `GIT_CONFIG_PARAMETERS="'alias.ship'='push'" git ship origin main`
    - the same with an `env` prefix
    - `git -c core.fsmonitor='git push origin HEAD:main;:' status`
    - `GIT_EXTERNAL_DIFF='git push origin HEAD:main;:' git diff`
    - `GIT_EDITOR='…' git commit`
    - `GIT_SSH_COMMAND='…' git fetch`
    - `git config core.fsmonitor '…git push…'`
    - `git config diff.external '…'`
  - On disk (git 2.56.1), each mechanism ran its payload:
    - `--attr-source HEAD rev-parse --abbrev-ref HEAD` → `main`: the value is consumed and the next word is the subcommand.
    - The env alias ran `rev-parse`.
    - `core.fsmonitor` and `GIT_EXTERNAL_DIFF` each ran `touch`.
  - Fix, in the same shape as the wrapper tables:
    - an exact table of git global options;
    - a guarded scan of `-c` values, `GIT_CONFIG_*` and the program-valued env (`GIT_EXTERNAL_DIFF`, `GIT_EDITOR`, `GIT_SSH_COMMAND`, `GIT_PAGER`);
    - program-valued keys in `git config` (`core.fsmonitor`, `core.sshCommand`, `diff.external`, `*.textconv`, `core.hooksPath`) on the approval list;
    - an unknown subcommand gets the suffix scan.
- [minor] **C. The Lead `push` verb still treats an empty remote as safe.**
  - Code: `crates/qq-core/src/authority/push.rs:324` (`RemoteDefault::Empty => false`). The CoS door refuses Empty (`push_door.rs:96`).
  - `git ls-remote --symref` on `git init --bare -b trunk` prints nothing, so the remote's unborn default is invisible.
  - A Lead's first push of `trunk` to such a remote creates the default branch, unless the local `protected_branch` (main, master, local `origin/HEAD`, `init.defaultBranch`) names it.
  - Confirmed from the code and the ls-remote probe. Not run through the Lead rig.
  - This answers the brief's unknown: no worker path reaches a protected branch through Empty, but the Lead path can, for a first push only.
- [minor] **D. Command lines inside interpreter code and stdin-fed xargs.** (pre-existing, rc=0 on base and head; the worker named this risk)
  - Probes:
    - `python3 -c 'import os; os.system("git push origin main")'`
    - `perl -e 'system("git push …")'`
    - `awk 'BEGIN{system("git push …")}'`
    - `echo 'git push origin main' | xargs -0 sh -c`
    - GNU `sed -z 'e git push origin main'` (gsed is not installed here; BSD sed has no `e`)
  - The suffix scan reads a spaced word only when its first word is a known command (`spaced_word`, `gate.rs:2202`).
- [minor] **E. New false-block shape: prose in a quoted argument of an unlisted command.**
  - Probes:
    - `notify-send 'git push finished'` → 0→2
    - `python3 tools/note.py 'git push origin feat done'` → 0→2
    - `osascript -e 'display notification "git push done"'` → 0→0, because the line's first word is `display`
  - Code: `gate.rs:1020-1031` (`scan_suffixes`/`spaced_word` on every unlisted command).
  - Measured cost: 1 in 4,934 (#166). Not a blocker for normal CoS work.

## Answers to the brief

- **Majors 1 and 3:** closed.
- **Major 2:** closed for static configs, reopened as a race (A).
- **New major holes from the two units:** one, A. It comes from the push door's check-then-push-by-name design. B was open before this delta; this unit left it open.
- **Blocks normal CoS work badly:** no. 0 new asks, 1 false block in 4,934.

Verdict: 5 findings
