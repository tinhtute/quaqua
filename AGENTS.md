# AGENTS.md — quaqua engine

This repository is the **engine** (`crates/qq-core`), not a workspace. Worker spawn
lives in `crates/qq-core/src/task/spawn.rs`; the worker brief rule ships in `crates/qq-core/templates/workspace/.claude/rules/brief.md`.

- The CoS document for the consuming workspace is `templates/workspace/AGENTS.md`.
- Managed workspace files live under `templates/workspace/`; seed-once files come from `crates/qq-core/src/init/seeds.rs`.
- Engine-only: `crates/`, `tests/`, `plugins/`, `docs/`, `.github/`, `Cargo.*`, `README.md`, `SETUP-PROMPT.md`.
- Never write private workspace state (`memory/`, `projects/`, `runs/`, `raw/`, `reports/`, `USER.md`) here.
- The PreToolUse gate for engine worktrees is `.claude/settings.json` → `templates/workspace/scripts/hooks/herdr-gate.sh`.
- Work only in your worktree: no push, no merge to `main`, no branch deletes without anh iu's approval.
- Run only the targeted tests named in your brief, in the foreground.
