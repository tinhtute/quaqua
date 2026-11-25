# Quaqua 🍑 — SLP orchestration on Herdr

A personal multi-agent setup following SLP (Supervisor – Lead – Peer) on [Herdr](https://herdr.dev).
The owner is the **Principal**, outside the system, and talks only to Quả Quả (Claude Code),
the **CoS** that fills SLP's Supervisor slot and owns the full SDLC; each project gets an
internal **Lead**; **Peers** are task workers in Herdr panes.
A read-only **Auditor** runs at 07:00 and 19:00 local time via systemd (Linux) or launchd
(macOS). Diagram: `docs/slp.png`.

This repo is the live workspace: clone it to `~/quaqua`. Per-workspace personal state
(`USER.md`, `MEMORY.md`, `memory/`, `projects/`, `raw/`, `runs/`, `reports/`) is gitignored and stays local; start
`USER.md` from the seed `quaqua init` writes.

## Architecture

![SLP architecture](docs/slp.png)

```
 Principal (owner) — goal, priorities, trade-offs; approves push / merge to main
        │ goal                ▲ digest
        ▼                     │
 CoS (Quả Quả) — full SDLC: intake, sizing, brief, verify, release  ◄── report ── Auditor
        │ small task (<15 min, 1 file, or needs live feedback) ──► one worker, no SLP   (systemd/launchd
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

### Who may call the daemon

The daemon names every caller from the socket peer, never from the request: the peer's pid
walks its ancestry to the Herdr pane whose shell or foreground process it is, and that pane's
agent is the caller. A same-uid process in **no** pane is the **Principal** (caller `owner`),
and it **reads only** (owner decision 2026-11-25): `ping`, `status`, `roster`, `caller.lookup`
(whose handler still names no agent, so the hook gate keeps reading `unmapped_peer`) and
`config` limited to `get`, `export`, `diff` and `validate`. Everything that changes the farm
runs from a Herdr pane: `stop`, `config set`, `config import`, and every agent verb (`task.*`,
`mail.*`, `push`, `merge`, `pipeline`, `memory.write`, `signal`, `watch.*`, `workflow.start`,
`pr.merge`). An allowlist means a verb or subcommand added later is closed until someone opens
it, and a refused write says so: run it from a Herdr pane.

Because nothing writable is reachable outside a pane, a process that leaves its pane's
ancestry (`(cmd &)`, `nohup … & disown`, a double fork, `setsid`, `systemd-run`, or a socket
handed on by a connector that exited) cannot stop the daemon or rewrite the topology even when
it is named the Principal; the same holds for a pane herdr cannot describe (its `process-info`
fails, or its entry has no `pane_id`), which is why one broken pane cannot turn the owner's
`ping` into a refusal. A process in a pane that hosts no agent is refused, and so is another
uid (`foreign_uid`).

Exit codes, so a launcher can branch: `quaqua ping` exits `69` when the daemon is not running
(start it), `77` when the daemon answered and refused this caller — an agent-only verb, or a
Herdr outage that leaves the daemon unable to ask which panes exist (`unmapped_peer`) while the
daemon itself is up — and `0` when it answered.

## Bring the farm up

`quaqua up` is the one command a launcher runs: it starts the Herdr session if it is
stopped, makes sure the farm daemon answers, finds — or creates — the workspace labelled
after the farm root and puts the CoS in that workspace's root pane, then focuses it and,
outside Herdr, attaches the TUI. Inside Herdr it focuses and returns.

The CoS is launched from `[roles.cos]`, and nothing is defaulted:

```toml
[roles.cos]
harness = "claude"
model = "opus-5.5"
effort = "low"
```

A missing key makes `up` exit non-zero with the missing key list and one
`quaqua config set roles.cos.<key> <value>` line per key. Those writes run from a Herdr pane
only, so `up` starts the session *before* it reads the launch: the pane it names is the pane
you then use.

When the workspace exists and the CoS has exited, `up` resumes the `agent_session` recorded
in `runs/cos.identity.json` through that harness's own resume flag (`claude --resume <id>`);
no record, a harness without a verified flag, or a refused resume prints one line and starts
a fresh CoS, so `up` never leaves the farm without one.

Outside Herdr, `up` passes the session and socket of the session it started or found to every
Herdr call and exports them, so installing the daemon unit (it asks `y/N` when there is no
`quaqua-daemon.service`) works from a plain terminal. The start it waits for is bounded by
`notify.daemon_start_sec` (default 90 s).

## Files

| File | Role |
|---|---|
| `templates/workspace/AGENTS.md` | Workspace entrypoint: SLP roles, memory policy, worker protocol (the root `AGENTS.md` is an engine contributor stub) |
| `templates/workspace/identity.md` | CoS persona and voice (injected at session start) |
| `templates/workspace/.claude/rules/brief.md` | Always-loaded brief shape for the CoS |
| `.claude/settings.json` | Engine worktree gate: denies the built-in `Agent` tool and routes Bash through the template hook; `templates/workspace/.claude/settings.json` is the workspace copy |
| `templates/workspace/.claude/skills/herdr-orchestration/` | Worker flow + consolidated command, recovery, and assignment/acceptance references |
| `quaqua auditor run` | Auditor: read-only drift + pipeline-run audit → `reports/audit-*.md` |
| `quaqua init`, `templates/workspace/`, seeds embedded from `crates/qq-core/src/init/seeds.rs` | Bootstraps a fresh clone's gitignored personal state (dirs + `memory/rules.md`, `projects/INDEX.md`, `_template.md`, `USER.md`); idempotent, never overwrites |
| `quaqua memory check`, `quaqua memory hook pre\|batch` | CoS memory gate: caps `memory/rules.md` at 2200 and `USER.md` at 1375 Unicode chars, scans both for secrets; `quaqua init` installs the `PreToolUse` (`Write\|Edit\|MultiEdit`) deny and the `PostToolBatch` flag |
| `tests/` | `hermetic.sh` (strace guard against live-DB opens), `e2e/`, `manual/`; the Rust suite lives in `crates/qq-core/tests/` |
| `SETUP-PROMPT.md` | Original prompt used to bootstrap this setup |
| `docs/slp.png` | Diagram: Principal → CoS → Lead → Peers |

## New machine

A fresh clone has no personal state: `memory/`, `projects/`, `raw/`, `runs/`,
`reports/` and `USER.md` are gitignored and stay on the machine that created
them. Bootstrap the skeleton — the dirs plus `memory/rules.md`,
`projects/INDEX.md`, `projects/_template.md` and `USER.md` — with:

```bash
git clone git@github.com:tinhtute/quaqua.git ~/quaqua && cd ~/quaqua
cargo install --path crates/qq-core --locked   # installs the quaqua binary to ~/.cargo/bin
quaqua init
```

`quaqua init` is idempotent and never overwrites an existing file, so it
is safe to re-run after a pull. The one exception is `.claude/settings.json`,
which it merges into: the two CoS memory hooks are added only when absent and
every other hook in the file is left alone. Then edit `USER.md` for the workspace, add one
row per project to `projects/INDEX.md` (copy `projects/_template.md` for the
card). The seeds are embedded in the binary (`crates/qq-core/src/init/seeds.rs`).

The installed hooks enforce AGENTS.md '## Memory'. A `Write`/`Edit`/`MultiEdit`
that would push `memory/rules.md` past 2200 chars or `USER.md` past 1375, or
that would store a secret, is denied before it runs. After any batch that could
have written, the check re-runs and hands a violation back to the agent, so a
`sed`/`Bash` write is caught too. Run it by hand with:

```bash
quaqua memory check [--workspace DIR]   # exit 1, one line per violation
```

The caps are constants in the binary — no config key raises them.

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
cargo install --path crates/qq-core --locked   # installs the quaqua binary to ~/.cargo/bin
~/.cargo/bin/quaqua --version   # verify; an interactive `quaqua` shell function can shadow the binary, so call it by path
quaqua init     # dirs + rules.md, INDEX.md, _template.md, USER.md
```

Schedule `quaqua auditor run` (07:00 and 19:00) with your own user systemd timer or LaunchAgent; the repo ships no installer. `quaqua` and `claude` must be on the unit's PATH, and the unit needs `QUAQUA_HOME` (or `QUAQUA_RUNS_DIR` for the runs dir alone) only when the workspace is not `$HOME/quaqua`; the root does not depend on the working directory.

Tests: `cargo test --locked -p qq-core`. `tests/hermetic.sh` (needs `strace`) runs the suite under a tracer and fails if any test opens the live workspace database.

CI: `.github/workflows/test.yml` runs `cargo test --locked` and actionlint on every PR
and push to `main`; `.github/workflows/release.yml` runs on a `v*` tag and builds and
publishes the release.

Run CoS (Quả Quả) from `~/quaqua` inside Herdr (`HERDR_ENV=1`); it spawns project Leads with
`quaqua lead spawn <slug> --task "<kickoff>"` (without `--task` the command prints the
`herdr agent prompt` line to send, because a Lead left at `ready` starts nothing).
