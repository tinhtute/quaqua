# SLP team: roles, authority, integration, challenges

**Verdict: PARTIAL, 3/5.** Spawn and prompt authority, caps, worktrees, the integration branch and challenges are in code. "Coordinators never write code" and "one writer per file area" are prose only.

Audited at `41a5327`. `src/` means `crates/quaqua/src/`. `tpl/` means `templates/workspace/`.

## Scope

- "The Principal (owner) sets goals and approves; the Chief of Staff coordinates the full lifecycle; Leads own projects; Peers own one task each."
- "Coordinators never write code; they brief, verify, and merge."
- "Each Peer works in its own isolated worktree and branch; each file area has one writer."
- "Leads integrate Peer work into a project integration branch before it goes up to the Chief of Staff."
- "Peers may challenge a premise with evidence, and the challenge is resolved before acceptance."
- "Short-lived taskforces (reviewers, advisors) are read-only and report to their caller."

## Findings

| Requirement | Status | Evidence | Gap |
|---|---|---|---|
| Principal, CoS, Lead and Peer roles with distinct authority | met | Roles and edges in `tpl/team.toml:20-33` (`can_spawn`, `can_prompt`, `reports_to`). The layout gate checks identity, role, edge, depth and cap (`src/layout/gate.rs:560-561`). Layout validation rejects cycles and over-depth roles (`src/layout/gate.rs:209-300`). A Lead is registered per project and takes ownership in `projects/INDEX.md` (`src/lead/spawn.rs` usage text, `quaqua lead spawn -h`). | Identity is derived from pane tokens and environment (`src/identity.rs`, `QUAQUA_OWNER` in `src/pipeline/mod.rs:121`). INFERENCE: an agent that sets `QUAQUA_OWNER` itself could claim another role; I did not find a signed identity. |
| Coordinators never write code | missing | The rule is stated (`tpl/AGENTS.md:60`, `:16`). | No hook denies `Write`/`Edit` for the CoS or a Lead outside the two memory files. The only `Write\|Edit\|MultiEdit` hook is the memory gate (`tpl/.claude/settings.json:28-33`). The CoS runs with bypassed permissions (`tpl/.claude/skills/poteto-mode/SKILL.md:10`). Whether the rule is followed in practice is unverified: commit authors are personas, not roles, and the workspace was not available. |
| Each Peer in its own worktree and branch (first half of the bullet) | met | `quaqua worker spawn` creates `<repo-parent>/.wt/<name>` on `quaqua/<name>` (`src/worker/worktree.rs:21-25`, `:95-96`). Ownership is proven before cleanup (`CHANGELOG.md:55`). | None. |
| Each file area has one writer (second half of the bullet; the bullet as a whole is partial) | partial | The rule is stated (`tpl/AGENTS.md:43`, `tpl/PEER.md` "One moving scope has one writer"). A file-ownership table was used for one wave (`src/engine/OWNERS.md:1-9`), checked by an ignored test (`src/engine/OWNERS.md:5`). | No engine check compares the paths two live workers may write. Overlap is found at merge time as a conflict (`src/merge/mod.rs:22`). |
| Leads integrate into a project integration branch | met | `quaqua merge <slug> <branch>...` merges one branch at a time into `quaqua/<slug>-integ` and never pushes (`src/merge/mod.rs:11-15`). The branch name comes from config (`tpl/team.toml:95-96`). The integration branch is created at Lead spawn (`quaqua lead spawn -h`). Lead cap of 4 Peers (`tpl/team.toml:28`, `src/cap.rs:56-67`). | Nothing stops a CoS from merging a Peer branch elsewhere by hand with plain `git merge`; the Bash gate only watches push and `gh` (`src/gate/policy.rs:675-677`). |
| Peers may challenge; resolved before acceptance | partial | A `## Challenge` section in any worker report stops the pipeline as `disputed` (`src/pipeline/mod.rs:54-56`, `:2269`, `:2401`). The auditor pre-checks challenges (`src/auditor.rs:4`). | For a solo worker outside a pipeline, resolution is prose (`tpl/rules/_all/brief.md:36-37`). The engine records that a dispute happened but not how it was resolved. |
| Taskforces are read-only and report to their caller | met | Advisor, dual-lens and council shapes (`src/taskforce/mod.rs:1`, `tpl/rules/_all/taskforce.md:4-11`). Members are read-only in config (`tpl/team.toml:87-93`). Read-only is verified by comparing the worktree baseline at acceptance (`src/worker/prompt/readonly.rs:1-4`). Peers and taskforce members cannot spawn or prompt (`tpl/team.toml:31-33`). Council reports are sealed until quorum (`tpl/rules/_all/taskforce.md:11`). Taskforce weight counts against the caller's cap (`src/taskforce/mod.rs:341`). | Read-only is by detection after the fact. A taskforce member on agy runs with `--dangerously-skip-permissions` (`src/launch.rs:409`). |

## Refs

- Role table: `tpl/AGENTS.md:31-46`.
- Caps: `src/cap.rs:56-92`; pipeline weight 2 (`tpl/team.toml:7`, `src/pipeline/mod.rs:673`).
- `quaqua lead spawn -h` output, captured during this audit, documents the Lead workspace, the integration branch and the ownership row.
- The live workspace `~/quaqua` was not available in this environment, so I could not check actual Lead state, project cards or who edited which file.

## Fixes

1. Enforce "coordinators never write code": a `PreToolUse` hook on `Write|Edit|MultiEdit` and on Bash write patterns that denies edits under a project repo when the caller role is `cos` or `lead`, with lesson promotion as the one allowed path.
2. Give every worker a declared write scope in the brief, store it at spawn, refuse a second live worker whose scope overlaps, and check the diff against it at acceptance.
3. Record challenge resolution as an event (who decided, which way, evidence ref) and make the pipeline refuse to resume a `disputed` run without it.
4. Bind caller identity to something an agent cannot set for itself, such as a per-spawn token the engine issues and checks.
5. Route integration-branch merges only through `quaqua merge` by denying raw `git merge` into `*-integ` in the gate.
