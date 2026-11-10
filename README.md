# Quaqua 🍑 — SLP orchestration on Herdr

A personal multi-agent setup following SLP (Supervisor – Lead – Peer) on [Herdr](https://herdr.dev).
The owner is the **Principal**, outside the system, and talks only to Quả Quả (Claude Code),
the **CoS** that fills SLP's Supervisor slot and owns the full SDLC; each project gets an
internal **Lead**; **Peers** are task workers in Herdr panes.
A read-only **Auditor** runs at 07:00 and 19:00 local time via systemd (Linux) or launchd
(macOS). Diagram: `docs/slp.png`.

This repo is the live workspace: clone it to `~/quaqua`. Per-workspace personal state
(`USER.md`, `MEMORY.md`, `memory/`, `projects/`, `raw/`, `runs/`, `reports/`) is gitignored and stays local; start
`USER.md` from `USER.example.md`.

## Architecture

![SLP architecture](docs/slp.png)

```
 Principal (owner) — goal, priorities, trade-offs; approves push / merge to main
        │ goal                ▲ digest
        ▼                     │
 CoS (Quả Quả) — full SDLC: intake, sizing, brief, verify, release  ◄── report ── Auditor
        │ small task (<15 min, 1 file, or needs live feedback) ──► one worker, no SLP   (scripts/auditor/systemd/launchd
        │ spawn Lead + brief                                                           2×/day)
        ▼
 Lead (<slug>-lead) — project state, plan approval, acceptance,
        │ task brief                   merges into the project integration branch
        ▼
 Peers — build (sude) ⇄ review (suhuynh, claude read-only), own worktree each
        │ result                     default 2 review rounds (--rounds overrides); kill after ≥3 stuck
        ▼
 runs/pipeline-<task>.md ──► Lead accepts / merges      runs/telemetry.jsonl (1 line per run)
```

Rules:
- Brief has 5 sections: Goal / Hard constraints / Current design / Unknowns / Evidence; it never pre-solves.
- Peers may challenge premises in a `## Challenge` section; the Lead resolves it before acceptance.
- CoS never acts as Lead on pipeline-size work (no self-review); every redirect flows through CoS
  and is recorded in Lead state.
- Telemetry decides which mechanisms stay: a reviewer that rarely changes outcomes gets turned off.

## Files

| File | Role |
|---|---|
| `templates/workspace/AGENTS.md` | Workspace entrypoint: SLP roles, memory policy, worker protocol (the root `AGENTS.md` is an engine contributor stub) |
| `templates/workspace/identity.md`, `templates/workspace/SOUL.md` | CoS persona and voice |
| `templates/workspace/PEER.md` | Worker contract: boundaries, contract-before-test, git commit gates, handback |
| `templates/workspace/rules/_all/` | Worker rules injected as the system prompt (brief shape, fanout grid, timebox) |
| `templates/workspace/models.md` | Worker roster: nickname → kind / model / effort |
| `.claude/settings.json` | Engine worktree gate: denies the built-in `Agent` tool and routes Bash through the template hook; `templates/workspace/.claude/settings.json` is the workspace copy |
| `templates/workspace/.claude/skills/herdr-orchestration/` | Worker flow + consolidated command, recovery, and assignment/acceptance references |
| `quaqua auditor run` | Auditor: read-only drift + pipeline-run audit → `reports/audit-*.md` |
| `templates/workspace/scripts/auditor/install-auditor.sh`, `templates/workspace/scripts/auditor/systemd/`, `templates/workspace/scripts/auditor/launchd/` | Auditor scheduling: systemd user units (Linux) or a rendered LaunchAgent plist (macOS), 2×/day |
| `quaqua source github-ci poll --repo <owner/name>` | GitHub CI source: one `ci_failed` trigger per failed run on a `quaqua/*` branch that has a thread; deduped by run id, read position persisted |
| `templates/workspace/scripts/github-ci-poll/install-github-ci-poll.sh`, `templates/workspace/scripts/github-ci-poll/systemd/`, `templates/workspace/scripts/github-ci-poll/launchd/` | GitHub CI poll scheduling: systemd user unit+timer (Linux) or a rendered LaunchAgent plist (macOS), every 5 minutes |
| `quaqua init`, `templates/workspace/`, `templates/seed/` | Bootstraps a fresh clone's gitignored personal state (dirs + `memory/rules.md`, `projects/INDEX.md`, `_template.md`, `USER.md`); idempotent, never overwrites |
| `tests/` | Sandboxed test suite: `tests/run.sh` runs every `tests/test_*.sh` (the 2 slow suites only when the change touches a script they cover, or with `--slow`) and exits non-zero on any failure |
| `templates/seed/USER.example.md` | Template for the per-workspace `USER.md` |
| `SETUP-PROMPT.md` | Original prompt used to bootstrap this setup |
| `docs/slp.png` | Diagram: Principal → CoS → Lead → Peers |

## New machine

A fresh clone has no personal state: `memory/`, `projects/`, `raw/`, `runs/`,
`reports/` and `USER.md` are gitignored and stay on the machine that created
them. Bootstrap the skeleton — the dirs plus `memory/rules.md`,
`projects/INDEX.md`, `projects/_template.md` and `USER.md` — with:

```bash
git clone git@github.com:tinhtute/quaqua.git ~/quaqua && cd ~/quaqua
cargo install --path crates/quaqua --locked   # installs the quaqua binary to ~/.cargo/bin
quaqua init
```

`quaqua init` is idempotent and never overwrites an existing file, so it
is safe to re-run after a pull. Then edit `USER.md` for the workspace, add one
row per project to `projects/INDEX.md` (copy `projects/_template.md` for the
card), and continue with the auditor install below. Templates live in
`templates/seed/` under depth-safe names because `.gitignore`'s `memory/` and
`projects/` patterns match at any depth.

The same portable-paths pre-commit can be installed into another checkout
without bootstrapping a workspace there:

```bash
quaqua init --git-hooks ~/Lab/quaqua
```

It writes only `<git-dir>/quaqua-hooks/pre-commit` and sets `core.hooksPath` in
that repo; a repo whose `core.hooksPath` already points somewhere else is
refused untouched, and re-running is a no-op.

## Install

### From a release (no Rust toolchain)

Each release carries one `tar.gz` per target with a `.sha256` next to it:
`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin` and `x86_64-apple-darwin`.
Pick the tag to install and the target of this machine:

```bash
tag=v0.1.0                                  # the release tag, e.g. from the releases page
target=x86_64-unknown-linux-gnu             # or aarch64-apple-darwin, x86_64-apple-darwin
base="https://github.com/tinhtute/quaqua/releases/download/${tag}"
curl -fsSLO "${base}/quaqua-${tag}-${target}.tar.gz"
curl -fsSLO "${base}/quaqua-${tag}-${target}.tar.gz.sha256"
sha256sum -c "quaqua-${tag}-${target}.tar.gz.sha256"   # macOS: shasum -a 256 -c "…"
tar -xzf "quaqua-${tag}-${target}.tar.gz"               # quaqua, LICENSE, README.md
install -m 755 quaqua ~/.local/bin/quaqua               # any directory on PATH
quaqua --version
```

A `.sha256` that does not verify means a truncated download or a corrupted
archive: delete both files and download again.

Releasing is one tag push: `.github/workflows/release.yml` builds the three
targets on a `v*` tag, checksums each archive, and publishes them to the GitHub
release. Re-running the job for the same tag overwrites the assets instead of
duplicating them, so a tag that failed halfway is repaired by re-running.

### From source

Requires a Rust toolchain (install `rustup` if `cargo --version` fails).

```bash
git clone git@github.com:tinhtute/quaqua.git ~/quaqua && cd ~/quaqua
cargo install --path crates/quaqua --locked   # installs the quaqua binary to ~/.cargo/bin
~/.cargo/bin/quaqua --version   # verify; an interactive `quaqua` shell function can shadow the binary, so call it by path
quaqua init     # dirs + rules.md, INDEX.md, _template.md, USER.md
bash templates/workspace/scripts/auditor/install-auditor.sh  # installs scheduling files; prints manual activation command
```

Linux: the installer symlinks the user service/timer and calls only
`systemctl --user daemon-reload`. Activate manually with
`systemctl --user enable --now quaqua-auditor.timer`.

macOS: install a current Bash (e.g. `brew install bash`), then put its `bin` directory
first on PATH (`export PATH="$(brew --prefix bash)/bin:$PATH"` for Homebrew).
Run `bash templates/workspace/scripts/auditor/install-auditor.sh` with `claude` on PATH. The installer requires
Bash >= 4.4 from `command -v bash` and refuses the macOS system Bash 3.2.
It renders `templates/workspace/scripts/auditor/launchd/com.quaqua.auditor.plist.in` into
`~/Library/LaunchAgents/com.quaqua.auditor.plist`, pinning the discovered Bash,
the resolved `quaqua auditor run` path, and the install-time PATH (including
claude's directory). Logs go to `~/Library/Logs/quaqua-auditor.log`; reports still go
in this checkout's `reports/`. It never calls launchctl; run the printed bootstrap
command yourself. Reinstall after moving the checkout or changing toolchain paths.
Calendar intervals run at 07:00/19:00; unlike systemd's `Persistent=true`, launchd does
not replay missed runs while logged out (sleep wakeups may coalesce missed intervals).

### GitHub CI poll timer

`quaqua source github-ci poll --repo <owner/name>` records a `ci_failed` trigger for
each failed run on a `quaqua/*` branch that already has a thread. Schedule it with:

```bash
bash templates/workspace/scripts/github-ci-poll/install-github-ci-poll.sh [--repo <owner/name>]
```

`--repo` defaults to the `origin` remote of the checkout the script lives in, so a
clone of any fork polls its own repository; the origin must be a clone URL
(`https://`, `ssh://`, `git@host:owner/name`), never a local path. `gh` and `quaqua`
must be on PATH.

Both units pin the install-time PATH and `QUAQUA_RUNS_DIR` (`$QUAQUA_RUNS_DIR`, else
`<checkout>/runs`): the managers start the job with a bare PATH and no shell profile,
and the poll must record into this workspace's database rather than `$HOME/quaqua/runs`.

Linux: renders `~/.config/systemd/user/quaqua-github-ci-poll.service`, symlinks the
timer beside it, and calls only `systemctl --user daemon-reload`. Activate with
`systemctl --user enable --now quaqua-github-ci-poll.timer`; `OnCalendar=*:0/5` with
`Persistent=true` runs it every 5 minutes and replays a missed run. `quaqua doctor`
checks the pair: an installed, active timer is fine; a dangling symlink or an inactive
timer fails the run; a timer that was never installed warns (installing it is a
per-workspace choice).

macOS: renders `~/Library/LaunchAgents/com.quaqua.github-ci-poll.plist` with
`StartInterval` 300, the resolved `quaqua` path, the install-time PATH and the runs
dir (launchd does not source shell profiles, so `gh` must be on that PATH). Logs go to
`~/Library/Logs/quaqua-github-ci-poll.log`. It never calls launchctl; run the printed
`launchctl bootstrap` command yourself.

`--dry-run` prints the rendered unit and writes nothing, which is the safe way to
inspect the result on a machine where the timer should not be installed.

Tests live in `tests/` and require Bash >= 4.4 (macOS system Bash 3.2 is insufficient),
git, jq, Perl, python3 (for plist validation), and ripgrep. They use temporary homes and
stub herdr/claude/managers; no network, real agents, or service activation is needed:

```bash
# On macOS, put your installed Bash's bin directory first on PATH as above.
tests/run.sh   # PASS/FAIL per test, then a count; exits non-zero on any failure
RUN_SLOW_TESTS=1 tests/run.sh   # also run the 2 slow suites (or: tests/run.sh --slow)
```

`tests/run.sh` skips the two slow suites (`test_prompt_worker.sh`, `test_pipeline.sh`)
unless the change touches a script they cover, and prints which it skipped and why.

CI: `.github/workflows/test.yml` runs `cargo test --locked` and actionlint on every PR
and push to `main`; `.github/workflows/release.yml` runs on a `v*` tag and builds and
publishes the release.

Run CoS (Quả Quả) from `~/quaqua` inside Herdr (`HERDR_ENV=1`); it spawns project Leads with
`quaqua lead spawn <slug> --task "<kickoff>"` (without `--task` the command prints the
`herdr agent prompt` line to send, because a Lead left at `ready` starts nothing).
