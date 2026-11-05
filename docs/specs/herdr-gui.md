---
title: Quaqua Herdr GUI (Shepherdr-style mods)
status: draft
lane: high-risk
created: 2026-11-05
---

# Quaqua Herdr GUI

## Goal

- **outcome:** The Principal sees and steers every Quaqua pipeline from the Herdr app window: what needs them, what is in review, what is working, and what finished. They can act on it (jump, ack, stop, approve merge) without typing a `quaqua` command.
- **success_signal:** For one full day of normal work, the Principal answers every "needs you" item and every merge approval from the GUI, and runs no `quaqua status` or `quaqua inbox` command by hand.
- **actors:** Principal (the person using the GUI), CoS (Quả Quả), Leads, Peers (they only produce the data).
- **authority:** Read access comes from `runs/quaqua.db` and `runs/*`. Every write goes through the `quaqua` CLI, never straight to the DB or files.
- **lane:** high-risk. The merge approval moves a Principal gate into the UI, and the mods read an internal schema.

## Prior art

- Shepherdr 3.1.9 (`~/Lab/shepherdr`): `tasks`, `spaces-chat`, `workspace-status-chat`, `toasts` and `accept-card` mods. Patterns: group by what the task needs, a pie dot with % done and time left, a one-line "now doing" text, cards answered with one click, a status bar showing the waiting count.
- Engine features already landed under `audit:shepherdr`: #176 hook liveness, #177 retry/backoff, #178 severity, #179 short reasons for "needs you", #180 time-left estimates, #195/#196 omp follow-ups.
- Herdr app contract: `~/.config/herdr-app/AGENTS.md`. Mods are gpui-kit JS with `exec(cmd, args, {cwd, on})` and `watchFolder(dir)`, and have no Node APIs. Herdr's shipped layouts are read-only, so we clone `main`.

## Decisions

| Decision | Choice | Note |
|---|---|---|
| Where the mods live | In this repo, `templates/herdr-app/{layouts,mods}/`, installed by `quaqua init --herdr` | Versioned together with the DB contract |
| Data source | Read `runs/quaqua.db` directly with `exec("sqlite3", ["-readonly", "-json", db, sql])`, refresh on `watchFolder(runs/)` with a 5 s timer fallback | **adr candidate:** this ties the mods to the internal schema; mitigated by R1 |
| Writes | Only through the `quaqua` CLI (`mail ack`, `worker stop/cleanup`, `merge …`) | The mods never write to the DB |
| v1 actions | Jump to pane, ack mail, stop/cleanup a worker, approve a merge | Stop and approve always show a confirm dialog |
| v1 scope | board, inbox, lead sidebar, layout + status bar | One issue each, plus R1 |

## Requirements

- **R1 · Read contract for the DB.** (#214) The engine publishes stable read-only SQL views (`v_gui_workflows`, `v_gui_inbox`, `v_gui_workers`) and a `gui_schema_version` value. The mods query only those views.
  - acceptance: a Rust test opens a fresh DB, runs the migrations, and asserts the column sets of the 3 views and the version. A mod that finds an unknown version shows "Quaqua GUI needs update" instead of empty or wrong data.
- **R2 · `quaqua-board` panel.** (#215) Right-side panel grouped `Needs you` / `Review` / `Working` / `Queued` / `Done (24h)`. Each row shows the worker name, kind, pipeline round, the #179 short reason, the #180 time left, and how long ago the last event was.
  - acceptance: with a fixture DB that holds one workflow per phase, each one shows under the right group. Clicking a row focuses its Herdr pane (`panes()` matched by agent name). A row whose pane is gone shows "no pane" and nothing else.
- **R3 · `quaqua-inbox`.** (#216) Unacked mail and incident digests for `cos` show as cards. A new item raises a toast. An "Ack" button runs `quaqua mail ack <id>`.
  - acceptance: with a fixture DB that has 2 unacked items, 2 cards show. After Ack, the CLI exits 0 and the card is gone on the next refresh. If the CLI fails, the card stays and the error shows on it.
- **R4 · `quaqua-lead` sidebar.** (#217) Tree of projects (`projects/INDEX.md`) → `<slug>-lead` → Peers, with a status dot per agent from hook liveness (#176).
  - acceptance: the fixture tree renders, clicking a node jumps to its pane, and a worker counted as stuck (≥3) shows a warning dot.
- **R5 · Worker controls.** (#218) Stop and cleanup from a row's menu run `quaqua worker stop|cleanup <name>` after a confirm dialog.
  - acceptance: cancel runs nothing; confirm runs the exact command; a non-zero exit shows a toast with the stderr.
- **R6 · Merge approval.** (#219) Rows in the Lead merge queue that wait for the Principal show "Approve merge" with the diffstat and review verdicts. A confirm dialog runs the CLI approval.
  - acceptance: the button appears only when the reviews are clean (severity under the #178 must-fix threshold). The confirm text names the branch and target. The action is written to `memory/<today>.md` by the CLI, never by the mod.
- **R7 · Layout + status bar.** (#220) `layouts/quaqua.json` (cloned from `main`) wires R2–R4. The status bar shows the needs-you count, the `quaqua cap` slots in use, and today's OpenCode usage.
  - acceptance: `herdr-app check mods/<each>` passes, `layout.switch {"name":"quaqua"}` returns ok, and `ctrl-shift-l` returns to `main`.
- **R8 · Installer.** (#221) `quaqua init --herdr` copies the layout and mods into `~/.config/herdr-app`. It never overwrites; a name clash gets a suffix, as AGENTS.md says.
  - acceptance: running it twice is a no-op, and an existing `quaqua` mod folder is left as it is.

## Non-goals

- Chat with the CoS inside Herdr (keep using the CoS pane).
- Web or mobile app, voice, artifacts.
- Running Shepherdr side by side, or reusing its code.
- Writing to `runs/quaqua.db` from any mod.

## Risks

- **Schema coupling:** R1 is a hard prerequisite; R2–R7 are blocked until it lands.
- **Read-only open:** `sqlite3 -readonly` worked from inside `runs/`, but opening `runs/quaqua.db` by a relative path from the repo root failed with "unable to open database file". The mods must pass an absolute path; verify this in R1's test.
- **Merge gate in the UI:** a misclick merges to an integration branch. Mitigated by the R6 confirm dialog and the clean-review condition.
- **Polling cost:** `sqlite3` is spawned on every refresh. Watching the folder and debouncing to 1 refresh per second keeps it cheap; measure it in R2.

## Open questions

- Does `quaqua merge` already have a Principal-approve subcommand, or is one needed? | owner: Principal
- Does `Done (24h)` read from `telemetry.jsonl` or from DB events? | owner: decide in R1
