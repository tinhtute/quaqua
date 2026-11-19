//! The new database `runs/qq.db`: schema, migrations, path and live guard,
//! and a read-only reader for the old `runs/quaqua.db` and `runs/` rows.

pub mod migrate;
pub mod old;
pub mod path;

use std::path::{Path, PathBuf};

use rusqlite::Connection;

#[derive(Debug)]
pub enum DbError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// The path is a live database and the process is in test mode.
    LiveDb(PathBuf),
    /// The file was written by a newer schema than this build knows.
    TooNew {
        found: u32,
        known: u32,
    },
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Sqlite(e) => write!(f, "sqlite: {e}"),
            DbError::Io(e) => write!(f, "io: {e}"),
            DbError::LiveDb(p) => write!(f, "refused to open live database {}", p.display()),
            DbError::TooNew { found, known } => {
                write!(f, "database schema v{found} is newer than v{known}")
            }
        }
    }
}

impl std::error::Error for DbError {}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Sqlite(e)
    }
}

impl From<std::io::Error> for DbError {
    fn from(e: std::io::Error) -> Self {
        DbError::Io(e)
    }
}

const BUSY_TIMEOUT_MS: u64 = 5000;

fn is_lock_error(e: &rusqlite::Error) -> bool {
    let s = e.to_string();
    s.contains("database is locked") || s.contains("database is busy")
}

/// Retries while another connection holds the lock: `journal_mode` bypasses
/// the busy handler, so the retry lives here.
fn retry_while_busy<T>(mut op: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(BUSY_TIMEOUT_MS);
    loop {
        match op() {
            Err(e) if is_lock_error(&e) && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            r => return r,
        }
    }
}

/// The one opener: guard, create the directory, WAL, busy timeout, foreign
/// keys on (per connection), then migrate.
pub fn open(path: &Path) -> Result<Connection, DbError> {
    if path::live_db_open_refused(path) {
        return Err(DbError::LiveDb(path.to_path_buf()));
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "busy_timeout", BUSY_TIMEOUT_MS)?;
    retry_while_busy(|| conn.pragma_update(None, "journal_mode", "WAL"))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate::migrate(&mut conn)?;
    Ok(conn)
}

/// [`open`] at `<runs>/qq.db`.
pub fn open_default() -> Result<Connection, DbError> {
    open(&path::default_db_path())
}
