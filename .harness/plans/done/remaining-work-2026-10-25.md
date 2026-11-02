# Remaining work after pstack MVP + farm P0–P7 (2026-10-25)

Owner: CoS. Source of truth for what is left; each row ends in a check.

## Wave 1 (running, disjoint file areas)
| Unit | Team | Area | Done when |
|---|---|---|---|
| farm P6 consumers (absorbs pstack `watch`/drain trigger) | bus-lead | `bus/`, watchdog | DONE fa5144f on bus-integ (CoS re-ran 29 ok) |
| Test hermeticity: tests write live `runs/quaqua.db` (342/344 `would_deny` = `baro-lead` fixtures); QUAQUA_OWNER leak | gate-lead | test harness, `lead/prompt.rs` | live DB row count unchanged by full suite; genuine would_deny judged |
| Verdict level vocabulary (45/67 `inconclusive`) | tieude-verdictlvl | `engine/journal_sqlite.rs`, `worker/prompt`, rules doc | DONE 3e09c7e (verdict 32 + vocabulary 2 ok); no backfill |
| Lead per-Peer routing rule | tieude-leadroute | template AGENTS.md | DONE 5951d54 (tests 29+11 ok) |

## Wave 2 (gated)
- S0 baseline: DROPPED (anh iu 2026-10-28) — no measurement window; no G1–G3 delta to compute.
- P4 gate enforce: SKIPPED (anh iu 2026-10-28) — no shadow window, no enforce flip.
- P8 user layouts + council: DONE — gate (P4) skipped, so CP4 no longer gated it.
- #138 report frontmatter: DONE 453cf64 (#160).

## Needs anh iu (irreversible / preference)
- Push + PR: DONE 2026-10-28 — commits are in main (3d04669, 5951d54, 3e09c7e, fa5144f).
- Delete stale remotes: DONE — `pstack-integ` and `quaqua/gate-integ` are gone from the remote.
- Clean the fixture rows from live runs/quaqua.db: DONE 2026-10-28 — 110 fixture rows deleted, backup at `raw/2026-10-28-quaqua.db.bak`.
- `.quaqua/pending/models.md`, `team.toml` conflicts: DONE — `.quaqua/pending` no longer exists.

## Closed 2026-11-02

Closed by CoS (anh iu decision). Open row "Test hermeticity" carried into `.harness/plans/active/superagent-bus.md` as R0 / phase `hermetic`.
