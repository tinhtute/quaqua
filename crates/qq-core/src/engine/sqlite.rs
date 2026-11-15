//! Shared SQLite connection opener and retry helpers.

use std::path::Path;

/// How long a SQLite operation waits for a competing connection, in ms.
pub const BUSY_TIMEOUT_MS: u64 = 5000;

/// Resolves the busy timeout, respecting `QUAQUA_BUSY_TIMEOUT_MS` if set.
pub fn busy_timeout_ms() -> u64 {
    std::env::var("QUAQUA_BUSY_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(BUSY_TIMEOUT_MS)
}

/// True for SQLite's transient lock errors (`SQLITE_BUSY` / `SQLITE_LOCKED`).
pub fn is_lock_error(err: &rusqlite::Error) -> bool {
    if matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    ) {
        return true;
    }
    let s = err.to_string();
    s.contains("database is locked")
        || s.contains("database is busy")
        || s.contains("database table is locked")
}

/// Retries `op` while SQLite reports a transient lock, for up to [`BUSY_TIMEOUT_MS`].
///
/// `busy_timeout` covers the statements SQLite knows it can safely re-run, but
/// not `PRAGMA journal_mode`: switching a rollback-journal database to WAL
/// returns `SQLITE_BUSY` immediately when another connection holds the write
/// lock, even with a timeout set. Opening the database while another runner is
/// writing is normal, so the retry lives here rather than at each caller.
pub fn retry_while_busy<T>(mut op: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let timeout = busy_timeout_ms();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout);
    loop {
        match op() {
            Err(err) if is_lock_error(&err) && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

/// Opens a SQLite connection at `path`, sets `busy_timeout` to 5000 ms,
/// and enables WAL journal mode with retry while busy.
/// Creates parent directories if they do not exist.
pub fn open_conn(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }

    let conn = rusqlite::Connection::open(path)?;

    // Set the timeout first so everything after it waits for a competing
    // connection instead of failing immediately.
    let timeout = busy_timeout_ms();
    conn.pragma_update(None, "busy_timeout", timeout)?;

    // WAL mode is what makes concurrent readers and writers safe, but the
    // mode change bypasses the busy handler (see `retry_while_busy`).
    retry_while_busy(|| conn.pragma_update(None, "journal_mode", "WAL"))?;

    Ok(conn)
}
