//! Seed files: written once when absent, never overwritten (they hold the
//! Principal's own notes after the first edit).

/// `(workspace path, content)`.
pub(super) const SEEDS: &[(&str, &str)] = &[
    (
        "memory/rules.md",
        r#"# CoS rules (always loaded)

Durable lessons from anh iu. One rule per bullet: rule — why (date).
Pane layout, herdr mechanics and the watchdog live in `rules/_all/`.

<!-- Add rules here. Cap: 2200 chars (AGENTS.md '## Memory'). Over cap, merge
     or drop the oldest entry first — never raise the cap. -->

- Advisor workers (suhuynh) only advise, never edit code; their findings go to a build worker (2026-09-30).
- Clean up each worker (pane + worktree) right after its run is verified and its tests pass; keep the branch — branch delete/push/merge need approval (2026-10-05).
- Ownership — orchestrators work only in their own repo; this workspace is CoS-only. Check `git worktree list` before spawning; one file, one branch (2026-10-01).
- Gardener — a bug class found twice becomes a `rules/<slug>/` rule or test; a workaround used twice becomes a script (2026-10-06).
- Context pointers — prompts point to spec §, rules, runs, commits; never restate them. Keep verification output short (`| tail -3`).
- Paths in committed files: `~/` or repo-relative, never `/home/<user>/` (2026-10-08).
"#,
    ),
    (
        "projects/INDEX.md",
        r#"# Projects

| slug | repo | owner | status | one-line goal |
|------|------|-------|--------|---------------|
| _example_ | ~/code/example | cos | idle | copy `_template.md` to add a project |
"#,
    ),
    (
        "projects/_template.md",
        r#"# <project name>

repo: ~/code/<repo>
test: <command>

## Vision
<why this exists, what done looks like>

## Now
- [ ] <next task>

## Decisions
- <date> <decision + why>
"#,
    ),
    (
        "USER.md",
        r#"# <Owner name>

- Role, stack, languages.
- Communication preferences (e.g. direct, verdict first, verify before reporting).
- Project roots; exact paths live in `projects/INDEX.md`.
- Git authority (e.g. commit on branch OK; push/PR/merge → ask).
- Dislikes (e.g. extra sections, bloated files, changing worker model/effort).
"#,
    ),
];
