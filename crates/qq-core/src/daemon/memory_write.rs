//! `memory.write` (R4): the only writer of `USER.md` and `memory/rules.md`.
//!
//! The caller is named from the socket peer and only `cos` may write. A call
//! carries, per file, the text the caller loaded (`before`) and the text it
//! wants (`after`). Each file must pass the cap and the strict threat scan on
//! its final state, and must still hold `before` (a drift guard: an outside
//! edit refuses the write and the edited file is kept as `<file>.bak`). Every
//! file written adds one row to the append-only `memory_journal`. A call is all
//! or nothing across its files.

use std::path::{Path, PathBuf};

use rusqlite::params;
use serde_json::{json, Value};

use super::{caller, now_ms, Ctx, Daemon};
use crate::db::path::DB_FILE;
use crate::memory::threat::{self, Scope};
use crate::memory::{check_text, TARGETS};
use crate::rpc::{Request, RpcError, BAD_REQUEST};

pub const VERB: &str = "memory.write";

fn internal_err(e: impl ToString) -> RpcError {
    RpcError::new("internal", e.to_string())
}

fn refused(why: impl Into<String>) -> RpcError {
    RpcError::new("refused", why)
}

struct Change {
    rel: &'static str,
    path: PathBuf,
    before: String,
    after: String,
    /// The file was on disk when the call began: a rollback restores it, else removes it.
    existed: bool,
}

/// The files of the call, validated: only a capped target, only strings.
fn parse(root: &Path, args: &Value) -> Result<Vec<Change>, RpcError> {
    let files = args
        .get("files")
        .and_then(Value::as_object)
        .filter(|f| !f.is_empty())
        .ok_or_else(|| RpcError::new(BAD_REQUEST, "files must be a non-empty object"))?;
    let mut out = Vec::new();
    for (name, spec) in files {
        let Some(t) = TARGETS.iter().find(|t| t.rel == name) else {
            return Err(refused(format!(
                "not a memory file this verb writes: {name}"
            )));
        };
        let text = |k: &str| {
            spec.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| RpcError::new(BAD_REQUEST, format!("{name}: missing string '{k}'")))
        };
        out.push(Change {
            rel: t.rel,
            path: root.join(t.rel),
            existed: root.join(t.rel).exists(),
            before: text("before")?,
            after: text("after")?,
        });
    }
    Ok(out)
}

/// The refusal for one file's final text, or `None` when it may land.
fn violation(c: &Change) -> Option<String> {
    let mut found: Vec<String> = check_text(c.rel, &c.after)
        .iter()
        .map(|v| v.line())
        .collect();
    // Only the lines this write adds: a line already there must not lock every
    // later write out of the file.
    for id in threat::scan(&threat::added_lines(&c.before, &c.after), Scope::Strict) {
        found.push(format!("{}: matches threat pattern {id}", c.rel));
    }
    (!found.is_empty()).then(|| found.join("; "))
}

fn disk_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Writes through a temp file and a rename, so a reader never sees half a file.
fn put(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Puts a file back as it was before the call: its old text, or gone.
fn restore(c: &Change) {
    if c.existed {
        let _ = put(&c.path, &c.before);
    } else {
        let _ = std::fs::remove_file(&c.path);
    }
}

fn handle(ctx: &Ctx, req: &Request) -> Result<Value, RpcError> {
    // The caller first, before any argument is read or any path touched.
    let who = match caller::identified(req) {
        Some("cos") => "cos".to_string(),
        Some(other) => return Err(refused(format!("{other}: only cos writes memory"))),
        None => return Err(refused("memory write needs a caller the daemon identified")),
    };
    let root = ctx
        .runs_dir
        .parent()
        .ok_or_else(|| internal_err("workspace root unknown".to_string()))?;
    let changes = parse(root, &req.args)?;

    // Drift and checks for every file before the first write.
    for c in &changes {
        let now = disk_text(&c.path);
        if now != c.before {
            let bak = PathBuf::from(format!("{}.bak", c.path.display()));
            let _ = put_raw(&bak, &now);
            return Err(refused(format!(
                "{}: changed outside since you loaded it; nothing written, the edited file is kept as {}.bak — reload and retry",
                c.rel, c.rel
            )));
        }
        if let Some(why) = violation(c) {
            return Err(refused(format!("{why} (AGENTS.md '## Memory')")));
        }
    }

    let mut conn = crate::db::open(&ctx.runs_dir.join(DB_FILE)).map_err(internal_err)?;
    let tx = conn.transaction().map_err(internal_err)?;
    let ts = now_ms() as i64;
    for c in &changes {
        tx.execute(
            "INSERT INTO memory_journal (ts_ms, caller, file, before, after) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ts, who, c.rel, c.before, c.after],
        )
        .map_err(internal_err)?;
    }
    let mut done: Vec<&Change> = Vec::new();
    for c in &changes {
        if let Err(e) = put(&c.path, &c.after) {
            for d in done {
                restore(d);
            }
            return Err(internal_err(format!("write failed, nothing kept: {e}")));
        }
        done.push(c);
    }
    if let Err(e) = tx.commit() {
        for d in done {
            restore(d);
        }
        return Err(internal_err(e));
    }
    Ok(json!({ "caller": who, "written": changes.iter().map(|c| c.rel).collect::<Vec<_>>() }))
}

fn put_raw(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)
}

pub fn register(d: &mut Daemon) {
    let _ = d.register_verb(VERB, Box::new(handle));
}
