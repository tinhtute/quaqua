//! GUI read contract (spec `docs/specs/herdr-gui.md`, R1).
//!
//! Stable, read-only SQL views over `runs/quaqua.db` for the Herdr GUI mods, plus
//! `gui_schema_version`. Mods query only these views; a mod that finds a version
//! it does not know shows "Quaqua GUI needs update". Bump [`GUI_SCHEMA_VERSION`]
//! whenever a view's column set changes.
//!
//! Views are created by [`ensure_gui_views`] from `FarmBus::open`. A view is
//! replaced only when its stored SQL differs from the wanted SQL, inside one
//! transaction, so an open of an up-to-date database writes nothing and a
//! concurrent `sqlite3 -readonly` reader never sees a missing view.
//!
//! `v_gui_inbox` is the unread set for consumer `cos` that `quaqua inbox` loads
//! from `triggers` (id past the `cos` cursor, addressed to `cos` or on a live
//! subscription). It exposes `farm` for the mod to filter on, and it does not
//! apply the Rust-side incident owner-group filter.

use rusqlite::Connection;

/// Value of `gui_schema_version.version`.
pub const GUI_SCHEMA_VERSION: i64 = 1;

pub const V_GUI_WORKFLOWS_COLUMNS: [&str; 6] = [
    "workflow_id",
    "phase",
    "snap_seq",
    "last_event_kind",
    "last_event_seq",
    "last_event_ts_ms",
];

pub const V_GUI_INBOX_COLUMNS: [&str; 11] = [
    "id",
    "ts_ms",
    "source",
    "kind",
    "from_name",
    "to_name",
    "summary",
    "thread_id",
    "ref",
    "payload_json",
    "farm",
];

pub const V_GUI_WORKERS_COLUMNS: [&str; 13] = [
    "name",
    "kind",
    "workflow_id",
    "budget_sec",
    "cost_budget",
    "start_ms",
    "agent_status",
    "hook_active",
    "errors",
    "lead_waiting",
    "snap_seq",
    "last_event_kind",
    "last_event_ts_ms",
];

pub const GUI_SCHEMA_VERSION_COLUMNS: [&str; 1] = ["version"];

/// Latest event and latest snapshot per workflow, shared by two views.
const LATEST: &str = "
    WITH last_ev AS (
        SELECT e.workflow_id, e.seq, e.ts_ms, e.kind
        FROM events e
        JOIN (SELECT workflow_id, MAX(seq) AS m FROM events GROUP BY workflow_id) x
          ON x.workflow_id = e.workflow_id AND x.m = e.seq
    ),
    last_snap AS (
        SELECT s.workflow_id, s.seq,
               CASE WHEN json_valid(s.state_json) THEN s.state_json END AS state_json
        FROM snapshots s
        JOIN (SELECT workflow_id, MAX(seq) AS m FROM snapshots GROUP BY workflow_id) x
          ON x.workflow_id = s.workflow_id AND x.m = s.seq
    ),
    ids AS (
        SELECT workflow_id FROM events UNION SELECT workflow_id FROM snapshots
    )";

fn views() -> [(&'static str, String); 4] {
    [
        (
            "v_gui_workflows",
            format!(
                "{LATEST}
                SELECT ids.workflow_id AS workflow_id,
                       CASE WHEN le.kind = 'workflow_cancelled' THEN 'cancelled'
                            WHEN ls.state_json IS NOT NULL
                                THEN json_extract(ls.state_json, '$.phase')
                       END AS phase,
                       ls.seq AS snap_seq,
                       le.kind AS last_event_kind,
                       le.seq AS last_event_seq,
                       le.ts_ms AS last_event_ts_ms
                FROM ids
                LEFT JOIN last_ev le ON le.workflow_id = ids.workflow_id
                LEFT JOIN last_snap ls ON ls.workflow_id = ids.workflow_id"
            ),
        ),
        (
            "v_gui_inbox",
            "SELECT t.id AS id, t.ts_ms AS ts_ms, t.source AS source, t.kind AS kind,
                    t.actor AS from_name, t.to_name AS to_name, t.summary AS summary,
                    t.thread_id AS thread_id, t.ref AS ref,
                    t.payload_json AS payload_json, t.farm AS farm
             FROM triggers t
             WHERE t.id > COALESCE((SELECT trigger_id FROM cursors WHERE consumer = 'cos'), 0)
               AND (t.to_name = 'cos'
                    OR EXISTS (
                        SELECT 1 FROM subscriptions s
                        WHERE s.thread_id = t.thread_id AND s.consumer = 'cos'
                          AND s.until_ms >= CAST(strftime('%s', 'now') AS INTEGER) * 1000))"
                .to_string(),
        ),
        (
            "v_gui_workers",
            format!(
                "{LATEST}
                SELECT w.name AS name, w.kind AS kind, w.workflow_id AS workflow_id,
                       w.budget_sec AS budget_sec, w.cost_budget AS cost_budget,
                       w.start_ms AS start_ms, w.agent_status AS agent_status,
                       w.hook_active AS hook_active, w.errors AS errors,
                       w.lead_waiting AS lead_waiting, w.snap_seq AS snap_seq,
                       le.kind AS last_event_kind, le.ts_ms AS last_event_ts_ms
                FROM (
                    SELECT ls.workflow_id AS workflow_id, ls.seq AS snap_seq,
                           json_extract(ls.state_json, '$.input.name') AS name,
                           json_extract(ls.state_json, '$.input.kind') AS kind,
                           json_extract(ls.state_json, '$.input.budget_sec') AS budget_sec,
                           json_extract(ls.state_json, '$.input.cost_budget') AS cost_budget,
                           json_extract(ls.state_json, '$.start_ms') AS start_ms,
                           json_extract(ls.state_json, '$.agent_status') AS agent_status,
                           json_extract(ls.state_json, '$.hook_active') AS hook_active,
                           json_extract(ls.state_json, '$.errors') AS errors,
                           json_extract(ls.state_json, '$.lead_waiting') AS lead_waiting
                    FROM last_snap ls
                    WHERE ls.state_json IS NOT NULL
                ) w
                LEFT JOIN last_ev le ON le.workflow_id = w.workflow_id
                WHERE w.name IS NOT NULL"
            ),
        ),
        (
            "gui_schema_version",
            format!("SELECT {GUI_SCHEMA_VERSION} AS version"),
        ),
    ]
}

/// Creates or refreshes the GUI views. Idempotent: a database whose views
/// already match is not written. Needs the bus and journal tables to exist.
pub fn ensure_gui_views(conn: &Connection) -> rusqlite::Result<()> {
    let wanted = views();
    let stale = |conn: &Connection| -> rusqlite::Result<bool> {
        for (name, body) in &wanted {
            let stored: Option<String> = conn
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type = 'view' AND name = ?1",
                    [name],
                    |r| r.get(0),
                )
                .ok();
            if stored.as_deref() != Some(&format!("CREATE VIEW {name} AS {body}")) {
                return Ok(true);
            }
        }
        Ok(false)
    };
    if !stale(conn)? {
        return Ok(());
    }
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    // Re-check under the write lock: another opener may have won the race.
    if stale(&tx)? {
        for (name, body) in &wanted {
            tx.execute_batch(&format!(
                "DROP VIEW IF EXISTS {name}; CREATE VIEW {name} AS {body};"
            ))?;
        }
    }
    tx.commit()
}
