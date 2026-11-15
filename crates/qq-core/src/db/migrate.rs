//! Versioned migrations over `PRAGMA user_version`.
//!
//! The ladder is additive: a later task adds `sql/NNNN_<name>.sql` and one
//! line here. The journal tables `events` and `snapshots` belong to the
//! engine journal and are not created by any step.

use rusqlite::Connection;

use super::DbError;

/// `(version, sql)`, strictly ascending from 1.
pub const LADDER: &[(u32, &str)] = &[
    (1, include_str!("sql/0001_config.sql")),
    (2, include_str!("sql/0002_entities.sql")),
    (3, include_str!("sql/0003_run_capture.sql")),
];

pub fn latest_version() -> u32 {
    LADDER.last().map_or(0, |(v, _)| *v)
}

fn read_version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

/// Brings the database to the latest version. Safe to repeat and safe when
/// two processes run it together: the version is re-read inside an immediate
/// transaction, and a failed step rolls back with its version bump.
pub fn migrate(conn: &mut Connection) -> Result<(), DbError> {
    if read_version(conn)? == latest_version() {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let found = read_version(&tx)?;
    if found > latest_version() {
        return Err(DbError::TooNew {
            found,
            known: latest_version(),
        });
    }
    for (version, sql) in LADDER.iter().filter(|(v, _)| *v > found) {
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", *version)?;
    }
    tx.commit()?;
    Ok(())
}
