//! `quaqua recall <id>`: one trigger, its thread, and its ref — at most two hops.
//!
//! Spec §8: print the full trigger (including `payload_json`), the thread's owner,
//! state and 5 most recent triggers, and the `ref`: a workspace file's path plus its
//! first 40 lines, and for a `runs/<name>.md` report the commit SHA written in it.
//! No external link is followed, and recall never writes: a repeated call returns
//! the same answer and leaves every cursor untouched.

use super::events::TriggerRecord;
use super::FarmBus;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Number of most recent thread triggers recalled with the trigger.
pub const RECENT_LIMIT: usize = 5;
/// Number of leading ref lines printed.
pub const REF_HEAD_LINES: usize = 40;

/// The thread a recalled trigger belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallThread {
    pub id: String,
    pub owner: String,
    pub state: String,
    pub last_trigger_ms: u64,
    /// Newest first, at most [`RECENT_LIMIT`] rows.
    pub recent: Vec<TriggerRecord>,
}

/// The trigger's `ref`, resolved inside the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallRef {
    /// Resolved path as displayed.
    pub path: String,
    /// True when the path exists (a file or a directory).
    pub exists: bool,
    /// First [`REF_HEAD_LINES`] lines of a regular file; empty otherwise.
    pub head: Vec<String>,
    /// True when the file has more lines than [`REF_HEAD_LINES`].
    pub truncated: bool,
    /// Commit SHA written in a `runs/<name>.md` report.
    pub commit: Option<String>,
}

/// Everything `quaqua recall <id>` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recall {
    pub trigger: TriggerRecord,
    pub thread: RecallThread,
    #[serde(rename = "ref")]
    pub ref_detail: Option<RecallRef>,
}

/// Loads the trigger `id`, its thread and its ref from `bus`.
///
/// Returns `Ok(None)` when no trigger has that id. Relative refs resolve against
/// `workspace_root` (the directory holding `runs/`); an absolute ref is used as is.
/// Read-only: no cursor is read or advanced.
pub fn load_recall(
    bus: &FarmBus,
    id: u64,
    workspace_root: &Path,
) -> Result<Option<Recall>, String> {
    let trigger =
        match load_trigger(bus, id).map_err(|e| format!("cannot load trigger {id}: {e}"))? {
            Some(t) => t,
            None => return Ok(None),
        };

    let (owner, state, last_trigger_ms) = load_thread(bus, &trigger.thread_id)
        .map_err(|e| format!("cannot load thread {}: {e}", trigger.thread_id))?;
    let recent = load_recent(bus, &trigger.thread_id)
        .map_err(|e| format!("cannot load recent triggers for {}: {e}", trigger.thread_id))?;

    let ref_detail = trigger
        .ref_val
        .as_deref()
        .and_then(|r| resolve_ref(r, workspace_root));
    let thread_id = trigger.thread_id.clone();

    Ok(Some(Recall {
        trigger,
        thread: RecallThread {
            id: thread_id,
            owner,
            state,
            last_trigger_ms,
            recent,
        },
        ref_detail,
    }))
}

fn row_to_trigger(row: &rusqlite::Row<'_>) -> rusqlite::Result<TriggerRecord> {
    Ok(TriggerRecord {
        id: row.get::<_, i64>(0)? as u64,
        ts_ms: row.get::<_, i64>(1)? as u64,
        source: row.get(2)?,
        kind: row.get(3)?,
        actor: row.get(4)?,
        to_name: row.get(5)?,
        thread_id: row.get(6)?,
        summary: row.get(7)?,
        ref_val: row.get(8)?,
        payload_json: row
            .get::<_, Option<String>>(9)?
            .unwrap_or_else(|| "{}".to_string()),
        dedupe_key: row.get(10)?,
        farm: row.get::<_, Option<String>>(11)?.unwrap_or_default(),
    })
}

const TRIGGER_COLUMNS: &str = "id, ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm";

fn load_trigger(bus: &FarmBus, id: u64) -> rusqlite::Result<Option<TriggerRecord>> {
    let conn = bus.conn_for_test();
    crate::engine::sqlite::retry_while_busy(|| {
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {TRIGGER_COLUMNS} FROM triggers WHERE id = ?1"
        ))?;
        let mut rows = stmt.query(rusqlite::params![id as i64])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_trigger(row)?)),
            None => Ok(None),
        }
    })
}

fn load_thread(bus: &FarmBus, thread_id: &str) -> rusqlite::Result<(String, String, u64)> {
    let conn = bus.conn_for_test();
    crate::engine::sqlite::retry_while_busy(|| {
        let mut stmt =
            conn.prepare_cached("SELECT owner, state, last_trigger_ms FROM threads WHERE id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![thread_id])?;
        match rows.next()? {
            Some(row) => Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as u64,
            )),
            // A trigger row can outlive its thread row only if the database was
            // edited outside quaqua; recall still prints the trigger.
            None => Ok(("(missing)".to_string(), "unknown".to_string(), 0)),
        }
    })
}

fn load_recent(bus: &FarmBus, thread_id: &str) -> rusqlite::Result<Vec<TriggerRecord>> {
    let conn = bus.conn_for_test();
    crate::engine::sqlite::retry_while_busy(|| {
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {TRIGGER_COLUMNS} FROM triggers WHERE thread_id = ?1 ORDER BY id DESC LIMIT {RECENT_LIMIT}"
        ))?;
        let rows = stmt.query_map(rusqlite::params![thread_id], row_to_trigger)?;
        let mut res = Vec::new();
        for r in rows {
            res.push(r?);
        }
        Ok(res)
    })
}

fn resolve_ref(ref_val: &str, workspace_root: &Path) -> Option<RecallRef> {
    let raw = Path::new(ref_val);
    let resolved: PathBuf = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workspace_root.join(raw)
    };
    let display = resolved.display().to_string();

    if !resolved.exists() {
        return Some(RecallRef {
            path: display,
            exists: false,
            head: Vec::new(),
            truncated: false,
            commit: None,
        });
    }

    if !resolved.is_file() {
        return Some(RecallRef {
            path: display,
            exists: true,
            head: Vec::new(),
            truncated: false,
            commit: None,
        });
    }

    // A ref can point at a binary file; lossy decoding keeps recall readable.
    let text = std::fs::read(&resolved)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    let total_lines = text.lines().count();
    let head: Vec<String> = text
        .lines()
        .take(REF_HEAD_LINES)
        .map(|l| l.to_string())
        .collect();
    let commit = if is_report(&resolved) {
        extract_commit(&text)
    } else {
        None
    };

    Some(RecallRef {
        path: display,
        exists: true,
        head,
        truncated: total_lines > REF_HEAD_LINES,
        commit,
    })
}

/// A report is a markdown file directly under a `runs/` directory.
fn is_report(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("md")
        && path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|name| name.to_str())
            == Some("runs")
}

/// Extracts the commit SHA a report records: the first SHA token on its
/// `Snapshot:` line, else on its `Verified:` line.
pub fn extract_commit(text: &str) -> Option<String> {
    for prefix in ["Snapshot:", "Verified:"] {
        for line in text.lines() {
            if let Some(rest) = line.trim_start().strip_prefix(prefix) {
                if let Some(sha) = first_sha_token(rest) {
                    return Some(sha);
                }
            }
        }
    }
    None
}

fn first_sha_token(s: &str) -> Option<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|tok| (7..=40).contains(&tok.len()) && tok.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|tok| tok.to_string())
}

/// Renders a recall as the human-readable text `quaqua recall <id>` prints.
pub fn render_text(recall: &Recall) -> String {
    let t = &recall.trigger;
    let mut out = String::new();

    out.push_str(&format!("trigger #{}\n", t.id));
    out.push_str(&format!(
        "  ts:      {}\n",
        crate::bus::mail::format_iso_timestamp(t.ts_ms)
    ));
    out.push_str(&format!("  source:  {}\n", t.source));
    out.push_str(&format!("  kind:    {}\n", t.kind));
    out.push_str(&format!("  actor:   {}\n", t.actor));
    if let Some(to) = &t.to_name {
        out.push_str(&format!("  to:      {to}\n"));
    }
    out.push_str(&format!("  thread:  {}\n", t.thread_id));
    out.push_str(&format!("  summary: {}\n", t.summary));
    if let Some(r) = &t.ref_val {
        out.push_str(&format!("  ref:     {r}\n"));
    }
    if let Some(key) = &t.dedupe_key {
        out.push_str(&format!("  dedupe:  {key}\n"));
    }
    out.push_str(&format!("  payload: {}\n", t.payload_json));

    out.push('\n');
    out.push_str(&format!("thread {}\n", recall.thread.id));
    out.push_str(&format!("  owner: {}\n", recall.thread.owner));
    out.push_str(&format!("  state: {}\n", recall.thread.state));
    out.push_str(&format!(
        "  last trigger: {}\n",
        crate::bus::mail::format_iso_timestamp(recall.thread.last_trigger_ms)
    ));
    if recall.thread.recent.is_empty() {
        out.push_str("  recent (newest first): none\n");
    } else {
        out.push_str("  recent (newest first):\n");
        for r in &recall.thread.recent {
            out.push_str(&format!(
                "    #{} {} {}: {}\n",
                r.id, r.kind, r.actor, r.summary
            ));
        }
    }

    if let Some(detail) = &recall.ref_detail {
        out.push('\n');
        out.push_str(&format!("ref {}\n", detail.path));
        if !detail.exists {
            out.push_str("  (missing)\n");
        } else if detail.head.is_empty() {
            out.push_str("  (not a regular file)\n");
        } else {
            if let Some(sha) = &detail.commit {
                out.push_str(&format!("  commit: {sha}\n"));
            }
            out.push_str(&format!("  first {REF_HEAD_LINES} lines:\n"));
            for line in &detail.head {
                out.push_str(&format!("    {line}\n"));
            }
            if detail.truncated {
                out.push_str("  … (truncated)\n");
            }
        }
    }

    out
}

/// Runs `quaqua recall <id>` against the workspace bus.
///
/// Exit codes: `0` printed, `1` no trigger with that id. Bus errors are returned
/// as `Err` so the CLI can map a legacy database to exit 2.
pub fn run_recall(id: u64, json: bool) -> Result<i32, String> {
    let runs_dir = crate::worker::cleanup::default_runs_dir();
    let workspace_root = runs_dir.parent().unwrap_or(&runs_dir).to_path_buf();
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path)
        .map_err(|e| format!("cannot open bus at {}: {e}", db_path.display()))?;

    let Some(recall) = load_recall(&bus, id, &workspace_root)? else {
        eprintln!("no trigger with id {id}");
        return Ok(1);
    };

    if json {
        let rendered = serde_json::to_string_pretty(&recall)
            .map_err(|e| format!("cannot render recall as json: {e}"))?;
        println!("{rendered}");
    } else {
        print!("{}", render_text(&recall));
    }
    Ok(0)
}
