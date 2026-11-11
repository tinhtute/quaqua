//! The transcript index: one FTS5 table of messages plus the file stamps that
//! make indexing incremental.
//!
//! The index is a cache — deleting `runs/transcripts.db` loses nothing that a
//! re-index does not rebuild — and it is never the bus database. It lives in
//! its own file so a transcript rebuild can never touch `runs/quaqua.db`.
//!
//! Incremental rule: a file whose `(mtime_ns, size)` still matches its `files`
//! row is skipped. A changed file is re-indexed by deleting its rows and
//! inserting the new ones in one transaction, so a crash mid-file leaves the
//! previous rows intact and the next run redoes that file from scratch.

use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection};

use super::parse::Entry;

/// Schema, applied idempotently on every open.
///
/// `path` and `line` are the `path:line` anchor `search` prints; the six
/// columns the requirement names are all here. `indexed_at` is epoch seconds.
pub const SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS messages USING fts5(
    text,
    session UNINDEXED, project UNINDEXED, cwd UNINDEXED,
    ts      UNINDEXED, role    UNINDEXED,
    path    UNINDEXED, line    UNINDEXED,
    tokenize = 'trigram'
);
CREATE TABLE IF NOT EXISTS files (
    path       TEXT PRIMARY KEY,
    mtime_ns   INTEGER NOT NULL,
    size       INTEGER NOT NULL,
    rows       INTEGER NOT NULL,
    indexed_at INTEGER NOT NULL
);
"#;

/// Opens (creating if needed) the index and applies the schema.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = crate::engine::sqlite::open_conn(path)?;
    crate::engine::sqlite::retry_while_busy(|| conn.execute_batch(SCHEMA))?;
    Ok(conn)
}

/// What the index remembers about a file so it can tell it has not changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub mtime_ns: i64,
    pub size: i64,
}

/// The stamp of a file on disk.
pub fn stamp_of(meta: &std::fs::Metadata) -> Stamp {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Stamp {
        mtime_ns,
        size: meta.len() as i64,
    }
}

/// The stamp recorded for `path`, if the index has one.
pub fn stored_stamp(conn: &Connection, path: &str) -> rusqlite::Result<Option<Stamp>> {
    let mut st = conn.prepare_cached("SELECT mtime_ns, size FROM files WHERE path = ?1")?;
    let mut rows = st.query([path])?;
    match rows.next()? {
        Some(r) => Ok(Some(Stamp {
            mtime_ns: r.get(0)?,
            size: r.get(1)?,
        })),
        None => Ok(None),
    }
}

/// Replaces every row of `path` with `entries` and records its stamp, in one
/// transaction. Returns the number of rows written.
pub fn index_file(
    conn: &mut Connection,
    path: &str,
    project: &str,
    entries: &[Entry],
    stamp: Stamp,
) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM messages WHERE path = ?1", [path])?;
    {
        let mut ins = tx.prepare_cached(
            "INSERT INTO messages(text, session, project, cwd, ts, role, path, line) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for e in entries {
            ins.execute(params![
                e.text,
                e.session,
                project,
                e.cwd,
                e.ts,
                e.role,
                path,
                e.line.to_string()
            ])?;
        }
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    tx.execute(
        "INSERT INTO files(path, mtime_ns, size, rows, indexed_at) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(path) DO UPDATE SET mtime_ns = excluded.mtime_ns, size = excluded.size, \
         rows = excluded.rows, indexed_at = excluded.indexed_at",
        params![path, stamp.mtime_ns, stamp.size, entries.len() as i64, now],
    )?;
    tx.commit()?;
    Ok(entries.len())
}

/// Drops the rows of every indexed file that is no longer in `keep`.
pub fn prune(conn: &mut Connection, keep: &HashSet<String>) -> rusqlite::Result<usize> {
    let gone: Vec<String> = {
        let mut st = conn.prepare("SELECT path FROM files")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(Result::ok)
            .filter(|p| !keep.contains(p))
            .collect()
    };
    if gone.is_empty() {
        return Ok(0);
    }
    let tx = conn.transaction()?;
    for p in &gone {
        tx.execute("DELETE FROM messages WHERE path = ?1", [p])?;
        tx.execute("DELETE FROM files WHERE path = ?1", [p])?;
    }
    tx.commit()?;
    Ok(gone.len())
}

/// One row `search` prints.
#[derive(Debug, Clone)]
pub struct Hit {
    pub path: String,
    pub line: String,
    pub ts: String,
    pub role: String,
    pub text: String,
}

/// A search request, already reduced to what the SQL needs.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// An FTS5 `MATCH` expression, or `None` for a plain scan.
    pub match_expr: Option<String>,
    /// Substrings that must all appear in `text` (trigram cannot match a term
    /// shorter than three characters, so those narrow by `LIKE` instead).
    pub like_terms: Vec<String>,
    /// Project directory name, or a cwd (exact, or a trailing path segment).
    pub project: Option<String>,
    /// `YYYY-MM-DD`; compared lexicographically against the ISO timestamp.
    pub since: Option<String>,
    pub limit: usize,
}

/// Runs `q`, best match first. `bm25` orders when there is a `MATCH`; ties (and
/// the scan path) fall back to newest-first so output is deterministic.
pub fn search(conn: &Connection, q: &Query) -> rusqlite::Result<Vec<Hit>> {
    let mut sql = String::from("SELECT path, line, ts, role, text FROM messages WHERE 1=1");
    let mut args: Vec<SqlValue> = Vec::new();
    let ranked = q.match_expr.is_some();
    if let Some(expr) = &q.match_expr {
        sql.push_str(" AND messages MATCH ?");
        args.push(SqlValue::Text(expr.clone()));
    }
    for term in &q.like_terms {
        sql.push_str(" AND text LIKE ? ESCAPE '\\'");
        args.push(SqlValue::Text(format!("%{}%", escape_like(term))));
    }
    if let Some(p) = &q.project {
        sql.push_str(" AND (project = ? OR cwd = ? OR cwd LIKE ? ESCAPE '\\')");
        args.push(SqlValue::Text(p.clone()));
        args.push(SqlValue::Text(p.clone()));
        args.push(SqlValue::Text(format!("%/{}", escape_like(p))));
    }
    if let Some(s) = &q.since {
        sql.push_str(" AND ts >= ?");
        args.push(SqlValue::Text(s.clone()));
    }
    sql.push_str(if ranked {
        " ORDER BY bm25(messages) ASC, ts DESC, path ASC, line ASC"
    } else {
        " ORDER BY ts DESC, path ASC, line ASC"
    });
    sql.push_str(" LIMIT ?");
    args.push(SqlValue::Integer(q.limit as i64));

    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args), |r| {
        Ok(Hit {
            path: r.get(0)?,
            line: r.get(1)?,
            ts: r.get(2)?,
            role: r.get(3)?,
            text: r.get(4)?,
        })
    })?;
    rows.collect()
}

/// Escapes the `LIKE` wildcards so a literal `%` or `_` in a query stays literal.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(line: u64, role: &'static str, ts: &str, text: &str) -> Entry {
        Entry {
            line,
            role,
            ts: ts.to_string(),
            cwd: "/w".to_string(),
            session: "s1".to_string(),
            text: text.to_string(),
            sidechain: false,
        }
    }

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    /// The whole design rests on the bundled SQLite having FTS5 *and* the
    /// trigram tokenizer; this fails the moment the pinned dependency loses
    /// either, instead of silently degrading search to a full scan.
    #[test]
    fn the_bundled_sqlite_has_fts5_trigram() {
        let conn = Connection::open_in_memory().unwrap();
        let version: String = conn
            .query_row("SELECT sqlite_version()", [], |r| r.get(0))
            .unwrap();
        assert!(
            conn.execute_batch(
                "CREATE VIRTUAL TABLE probe USING fts5(body, tokenize = 'trigram');"
            )
            .is_ok(),
            "SQLite {version} has no FTS5 trigram tokenizer"
        );
        conn.execute("INSERT INTO probe(body) VALUES ('memory caps')", [])
            .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM probe WHERE probe MATCH '\"caps\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "trigram MATCH found nothing on SQLite {version}");
    }

    #[test]
    fn a_reindex_replaces_the_files_rows() {
        let mut conn = mem();
        let s = Stamp {
            mtime_ns: 1,
            size: 10,
        };
        index_file(
            &mut conn,
            "/a.jsonl",
            "-w",
            &[entry(1, "user", "", "first")],
            s,
        )
        .unwrap();
        index_file(
            &mut conn,
            "/a.jsonl",
            "-w",
            &[entry(1, "user", "", "second")],
            s,
        )
        .unwrap();
        let hits = search(
            &conn,
            &Query {
                match_expr: Some("\"second\"".to_string()),
                limit: 10,
                ..Query::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "second");
        assert_eq!(stored_stamp(&conn, "/a.jsonl").unwrap(), Some(s));
    }

    #[test]
    fn prune_drops_a_vanished_file() {
        let mut conn = mem();
        let s = Stamp {
            mtime_ns: 1,
            size: 10,
        };
        index_file(
            &mut conn,
            "/a.jsonl",
            "-w",
            &[entry(1, "user", "", "keep")],
            s,
        )
        .unwrap();
        index_file(
            &mut conn,
            "/b.jsonl",
            "-w",
            &[entry(1, "user", "", "gone")],
            s,
        )
        .unwrap();
        let keep: HashSet<String> = ["/a.jsonl".to_string()].into_iter().collect();
        assert_eq!(prune(&mut conn, &keep).unwrap(), 1);
        let hits = search(
            &conn,
            &Query {
                like_terms: vec!["gone".to_string()],
                limit: 10,
                ..Query::default()
            },
        )
        .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn a_like_term_narrows_a_match() {
        let mut conn = mem();
        let s = Stamp {
            mtime_ns: 1,
            size: 10,
        };
        index_file(
            &mut conn,
            "/a.jsonl",
            "-w",
            &[
                entry(1, "user", "", "memory caps are fine"),
                entry(2, "user", "", "memory caps, no ok"),
            ],
            s,
        )
        .unwrap();
        let hits = search(
            &conn,
            &Query {
                match_expr: Some("\"memory\"".to_string()),
                like_terms: vec!["ok".to_string()],
                limit: 10,
                ..Query::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, "2");
    }
}
