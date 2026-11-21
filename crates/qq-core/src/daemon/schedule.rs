//! Daemon schedule (R24): the daemon pulls loop sources on a schedule and
//! nowhere else. A schedule row names a source and its interval; `pull_due`
//! is the only caller of `Source::pull`. No socket, webhook or external-event
//! endpoint lives here.

use rusqlite::{params, Connection};

use crate::source::{file_once, Source};

/// Adds a schedule row, due at `now_ms`; an existing row is left as it is.
pub fn add(conn: &Connection, name: &str, interval_ms: i64, now_ms: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO schedule (name, interval_ms, next_due_ms) VALUES (?1, ?2, ?3)",
        params![name, interval_ms, now_ms],
    )?;
    Ok(())
}

/// Names of the schedules due at `now_ms`, oldest due first.
pub fn due(conn: &Connection, now_ms: i64) -> rusqlite::Result<Vec<String>> {
    let mut st = conn
        .prepare("SELECT name FROM schedule WHERE next_due_ms <= ?1 ORDER BY next_due_ms, name")?;
    let rows = st.query_map([now_ms], |r| r.get(0))?;
    rows.collect()
}

/// The earliest next-due time, for the daemon's wait; none with no schedule.
pub fn next_deadline(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    conn.query_row("SELECT MIN(next_due_ms) FROM schedule", [], |r| r.get(0))
}

/// Pulls every source whose schedule is due, files its new items once and moves
/// the schedule by one interval from `now_ms`. Returns the ids of the proposals
/// filed. The schedule moves before the pull, so one broken source cannot
/// spin; its error comes back after the other sources ran.
pub fn pull_due(
    conn: &mut Connection,
    sources: &[&dyn Source],
    now_ms: i64,
) -> Result<Vec<i64>, String> {
    let mut filed = Vec::new();
    let mut first_err = None;
    for name in due(conn, now_ms).map_err(|e| e.to_string())? {
        conn.execute(
            "UPDATE schedule SET next_due_ms = ?2 + interval_ms, last_run_ms = ?2 WHERE name = ?1",
            params![name, now_ms],
        )
        .map_err(|e| e.to_string())?;
        let Some(src) = sources.iter().find(|s| s.name() == name) else {
            continue;
        };
        match src.pull(conn) {
            Ok(items) => {
                for item in &items {
                    match file_once(conn, &name, item, now_ms) {
                        Ok(Some(id)) => filed.push(id),
                        Ok(None) => {}
                        Err(e) => {
                            first_err.get_or_insert(format!("source {name}: {e}"));
                        }
                    }
                }
            }
            Err(e) => {
                first_err.get_or_insert(format!("source {name}: {e}"));
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(filed),
    }
}
