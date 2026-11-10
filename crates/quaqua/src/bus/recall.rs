//! `quaqua recall <id>`: one trigger, its thread, and its ref — at most two hops.
//!
//! Spec §8: print the full trigger (including `payload_json`), the thread's owner,
//! state and 5 most recent triggers, and the `ref`: a workspace file's path plus its
//! first 40 lines, and for a `runs/<name>.md` report the commit SHA written in it.
//! A scheme ref (`mail:`, `project:`, `http(s)://`) is printed as written and never
//! looked up; a path that resolves outside the workspace is reported and never read.
//! No external link is followed, and recall never writes: a repeated call returns
//! the same answer and leaves every cursor untouched. A database shared with other
//! farms answers from the caller's farm only, and it is opened read-only.

use super::events::TriggerRecord;
use super::FarmBus;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
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

/// How a ref was interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    /// A workspace path; `exists`, `head`, `truncated` and `commit` describe it.
    Path,
    /// A scheme ref (`mail:`, `project:`, `http(s)://`, `;`-joined list) printed
    /// as written; no path lookup was made.
    Scheme,
    /// A path that resolves outside the workspace; never read.
    Outside,
}

/// The trigger's `ref`, resolved inside the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallRef {
    /// Resolved path for a workspace path, or the ref as written for a scheme
    /// ref or an outside path.
    pub path: String,
    /// How the ref was interpreted.
    pub kind: RefKind,
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
/// `workspace_root` (the directory holding `runs/`); any ref whose path resolves
/// outside the canonical workspace root is reported as [`RefKind::Outside`] and
/// never read. Scheme refs are printed as written. Read-only: no cursor is read
/// or advanced.
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

/// A shared database holds every farm's rows; recall answers from the caller's
/// farm only. A row written before the farm column existed is unattributed and
/// belongs to the local farm, as every other bus reader treats it.
const FARM_SCOPE: &str = "(farm = ?2 OR farm IS NULL)";

fn load_trigger(bus: &FarmBus, id: u64) -> rusqlite::Result<Option<TriggerRecord>> {
    let conn = bus.conn_for_test();
    crate::engine::sqlite::retry_while_busy(|| {
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {TRIGGER_COLUMNS} FROM triggers WHERE id = ?1 AND {FARM_SCOPE}"
        ))?;
        let mut rows = stmt.query(rusqlite::params![id as i64, bus.farm()])?;
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
            "SELECT {TRIGGER_COLUMNS} FROM triggers WHERE thread_id = ?1 AND {FARM_SCOPE} ORDER BY id DESC LIMIT {RECENT_LIMIT}"
        ))?;
        let rows = stmt.query_map(rusqlite::params![thread_id, bus.farm()], row_to_trigger)?;
        let mut res = Vec::new();
        for r in rows {
            res.push(r?);
        }
        Ok(res)
    })
}

fn resolve_ref(ref_val: &str, workspace_root: &Path) -> Option<RecallRef> {
    let raw = ref_val.trim();
    if raw.is_empty() {
        return None;
    }

    if is_scheme_ref(raw) {
        return Some(RecallRef {
            path: raw.to_string(),
            kind: RefKind::Scheme,
            exists: false,
            head: Vec::new(),
            truncated: false,
            commit: None,
        });
    }

    let root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let raw_path = Path::new(raw);
    let joined = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        root.join(raw_path)
    };

    // Only a successful canonicalize proves a path is inside the workspace; a
    // path with no canonicalizable prefix is reported, never guessed at.
    let Some(resolved) = canonicalize_lenient(&joined) else {
        return Some(outside_ref(raw));
    };
    if !resolved.starts_with(&root) {
        return Some(outside_ref(raw));
    }

    let display = resolved.display().to_string();

    if !resolved.exists() {
        return Some(missing_ref(display));
    }

    if !resolved.is_file() {
        return Some(RecallRef {
            path: display,
            kind: RefKind::Path,
            exists: true,
            head: Vec::new(),
            truncated: false,
            commit: None,
        });
    }

    // Prove the exact path again before reading it: the check above proved the
    // existing prefix, and a component can be swapped for a symlink in between.
    let Ok(real) = resolved.canonicalize() else {
        return Some(missing_ref(display));
    };
    if !real.starts_with(&root) {
        return Some(outside_ref(raw));
    }

    // A ref can point at a binary file; lossy decoding keeps recall readable.
    let text = std::fs::read(&real)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    let total_lines = text.lines().count();
    let head: Vec<String> = text
        .lines()
        .take(REF_HEAD_LINES)
        .map(|l| l.to_string())
        .collect();
    let commit = if is_report(&real) {
        extract_commit(&text)
    } else {
        None
    };

    Some(RecallRef {
        path: display,
        kind: RefKind::Path,
        exists: true,
        head,
        truncated: total_lines > REF_HEAD_LINES,
        commit,
    })
}

/// A ref that no canonicalize proved inside the workspace: reported, never read.
fn outside_ref(raw: &str) -> RecallRef {
    RecallRef {
        path: raw.to_string(),
        kind: RefKind::Outside,
        exists: false,
        head: Vec::new(),
        truncated: false,
        commit: None,
    }
}

/// A workspace path that does not exist.
fn missing_ref(path: String) -> RecallRef {
    RecallRef {
        path,
        kind: RefKind::Path,
        exists: false,
        head: Vec::new(),
        truncated: false,
        commit: None,
    }
}

/// Scheme prefixes a ref producer writes instead of a workspace path: `mail send`
/// writes `mail:<id>` / `project:<p>` joined with `;`, and the GitHub CI source
/// writes the run URL.
fn is_scheme_ref(ref_val: &str) -> bool {
    let s = ref_val.trim();
    if s.is_empty() {
        return false;
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return true;
    }
    s.split(';')
        .all(|part| part.starts_with("mail:") || part.starts_with("project:"))
}

/// Resolves `path` as far as the filesystem allows: canonicalizes the longest
/// existing prefix (following symlinks) and appends the rest, so a missing file
/// still gets a `..`-free, symlink-resolved answer.
///
/// Returns `None` when no prefix can be canonicalized. The walk-up stops at a
/// prefix ending in `..`, where `file_name()` is `None`; popping that `..`
/// lexically instead would resolve a path the filesystem never did, hiding the
/// symlink the rest of the path goes through.
fn canonicalize_lenient(path: &Path) -> Option<PathBuf> {
    let mut prefix = path.to_path_buf();
    let mut tail: Vec<OsString> = Vec::new();
    loop {
        if let Ok(canonical) = prefix.canonicalize() {
            let mut out = canonical;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return Some(out);
        }
        match (prefix.parent(), prefix.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                prefix = parent.to_path_buf();
            }
            _ => return None,
        }
    }
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
        match detail.kind {
            RefKind::Scheme => out.push_str("  (scheme ref, not followed)\n"),
            RefKind::Outside => out.push_str("  (outside the workspace, not read)\n"),
            RefKind::Path if !detail.exists => out.push_str("  (missing)\n"),
            RefKind::Path if detail.head.is_empty() => out.push_str("  (not a regular file)\n"),
            RefKind::Path => {
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
    }

    out
}

/// Runs `quaqua recall <id>` against the workspace bus.
///
/// Exit codes: `0` printed, `1` no trigger with that id. Bus errors are returned
/// as `Err` so the CLI can map a legacy database to exit 2. The bus is opened
/// read-only: recall never creates, migrates or writes the database.
pub fn run_recall(id: u64, json: bool) -> Result<i32, String> {
    let runs_dir = crate::worker::cleanup::default_runs_dir();
    let workspace_root = runs_dir.parent().unwrap_or(&runs_dir).to_path_buf();
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open_read_only(&db_path)
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
