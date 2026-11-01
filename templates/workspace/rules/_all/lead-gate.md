# Lead gate

AGENTS.md '## Roles (SLP)' requires a Lead when 3 or more task workers run in parallel, or when
the work spans more than one day. `quaqua worker spawn` now enforces it instead of trusting prose:
on a project the CoS owns, two or more topology signals refuse the spawn.

## When the gate runs

Only when the spawn's repo maps to a `projects/INDEX.md` row whose owner is `cos` (the first such
row). A repo with no row, or a project already running under `<slug>-lead`, passes untouched — the
Lead decides its own Peers.

## Signals

- **Worker count** — this spawn would be the 3rd or later live build worker on the same repo.
  Which kinds hold a build slot is `team.toml`'s business, not this file's: `strategy.build_kinds`
  (`quaqua config get strategy.build_kinds` prints it), and a kind the list does not name — a
  reviewer kind, a `--review` worker — holds none; with the key absent, every configured kind
  holds one. A running pipeline counts as `limits.weights.pipeline` slots (2) together.
  Attribution is `runs/<name>.repo`, written at spawn: a worktree path is
  `<repo-parent>/.wt/<name>`, so it cannot name the repo when several repos share that parent,
  and a worker spawned before this metadata existed is not counted.
- **Plan phases** — the project's active plan declares `phases_left: N` with N > 1. The plan is
  the `docs/plans/active/*.md` the project card names, else `docs/plans/active/<slug>.md`. Frontmatter
  only: no checkbox counting.
- **Memory days** — `memory/YYYY-MM-DD.md` has `- [<slug>]` lines on two or more consecutive days:
  the work already spans days.
- **Brief budget** — `Time budget` over 240 min or `Cost budget` over $10. Only the brief carries
  these, so `quaqua worker prompt --check-brief` raises this one as a warning; `worker spawn` has
  no brief to read.

## Verdict

- 2 or more signals → exit non-zero, each signal listed, plus `quaqua lead spawn <slug>`.
- Exactly 1 → a warning on stderr; the spawn proceeds.
- 0 → silent.

## `--solo "<reason>"`

Bypasses a block. The reason needs at least 10 characters, is printed, and is appended to
`memory/<today>.md` as `- [<slug>] solo override: <reason>` — so a Lead-less spawn leaves a trail
the CoS reads at cleanup. `--solo` with fewer than 2 signals records nothing: there was no block.

## Briefs declare their topology

Every brief carries `Topology: solo|lead — <reason>` as its first header; `--check-brief` exits 1
without it (`rules/_all/brief.md`).

## Spawning the Lead

`quaqua lead spawn <slug> --task "<kickoff>"` starts the Lead, sends its brief with a verified
submit, waits for it to become ready, then sends the kickoff. Without `--task` it prints
`NEXT: herdr agent prompt <slug>-lead "<kickoff>"` on stderr: a Lead left at `ready` starts nothing
by itself (2026-10-23 llmhub-lead idled that way).
