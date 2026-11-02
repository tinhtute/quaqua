<!-- audience: orchestrator -->
# Lead → CoS inbox

- A Lead reports to the CoS via `quaqua mail send --to cos --kind <kind> [--body-file -]`, never by `herdr agent prompt` to the CoS pane: a prompt lands in anh iu's main chat and is rejected while the CoS waits on a question — 2026-10-19, arch-lead E-side report.
- `mail send` writes to SQLite and dual-writes the entry to `~/quaqua/runs/<slug>-lead.inbox.md`. If the database is missing or corrupt, it falls back to the markdown append automatically with a warning.
- Manual fallback (when quaqua is not on PATH or database is offline): append in a single write (`cat >> ~/quaqua/runs/<slug>-lead.inbox.md <<'EOF'`), first line `## <ISO-8601 time> <lead-name> <kind>`, kind = `handoff | accept | challenge | escalate | blocked`, then ≤10 lines: verdict, SHA, verify output, cost, ask.
- Append only; never rewrite or truncate the file. The CoS answers by prompting the Lead.

## Farm scoping

A workspace is a *farm*: its id is the last component of the canonicalized root, lowercased and sanitized to `[a-z0-9_-]` (`~/quaqua` → `quaqua`, `~/Work/TTDVKH` → `ttdvkh`), and `runs/quaqua.db` belongs to the farm of the directory holding `runs/`.

- Mail you send is stamped with the farm of the database you write into, not with the farm of your pane. Rows written before the farm column existed (`farm IS NULL`) stay readable by every consumer.
- Every read is farm-scoped: unacked mail, `quaqua mail inbox`, `quaqua mail ls` and `quaqua mail ack` return only rows of the local farm (plus those pre-migration `NULL` rows), so mail of another workspace sharing the same database cannot reach this inbox.
- Incidents: the CoS sees every incident of the local farm; a Lead sees only incidents whose `owner` equals its own name, and an incident with `owner` unset never reaches a Lead.
- Planned contract (U5, not yet in `quaqua/sude-farmfix`): `mail send` refuses when the writer's pane farm differs from the DB farm and names `QUAQUA_HOME` in the error. Until that lands, a `mail send` typed inside another farm's pane writes into *this* farm's database — point `QUAQUA_HOME`/`QUAQUA_RUNS_DIR` at the intended workspace first — 2026-10-29, runs/tieude-farmdoc.md
