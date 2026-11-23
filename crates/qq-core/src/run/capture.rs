//! Run capture (R19): reads a harness's own session file and stores the
//! transcript, tool calls and token totals of one run. Capture replaces what an
//! earlier capture of the same run stored, so a repeat converges.
//!
//! Session files (no real-pane measurement during the build):
//! - claude: `~/.claude/projects/<project>/<session>.jsonl` (documented); herdr
//!   reports the session id. Entries `user`/`assistant` carry `message.content`
//!   blocks (`text`, `tool_use`, `tool_result`) and `message.usage`. Claude
//!   reports no cost, so cost stays `unknown`.
//! - pi: the session jsonl whose path herdr reports (`agent_session.kind = "path"`,
//!   measured by T39). Messages are `user`, `assistant` and `toolResult`; an
//!   assistant message carries `usage` with `cost.total`.
//! - any other driver (generic, agy until T39): no session source. Only the
//!   engine events of the run exist; tokens and cost stay `unknown`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::config::Driver;
use crate::driver::{ErrorShape, HarnessDriver};

/// How a session file is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Claude,
    Pi,
}

impl Format {
    /// The session format of a driver; `None` for one with no session source
    /// (every generic driver, whatever error shape it declares).
    pub fn of(d: &dyn HarnessDriver) -> Option<Format> {
        if d.driver() != Driver::Native {
            return None;
        }
        match d.error_shape()? {
            ErrorShape::Claude => Some(Format::Claude),
            ErrorShape::Pi => Some(Format::Pi),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub ts_ms: u64,
    pub role: &'static str,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub ts_ms: u64,
    pub tool: String,
    pub args_json: String,
    pub result_json: Option<String>,
    pub ok: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Parsed {
    pub turns: Vec<Turn>,
    pub tools: Vec<ToolCall>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    /// `None`: some assistant message carried no cost, so the total is unknown.
    pub cost_usd: Option<f64>,
}

/// What one capture stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Captured {
    pub turns: usize,
    pub tool_calls: usize,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug)]
pub enum CaptureError {
    NoRun(String),
    /// A native driver was given a session reference that names no file.
    SessionNotFound(String),
    Io(std::io::Error),
    Sql(rusqlite::Error),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::NoRun(w) => write!(f, "no run for workflow {w}"),
            CaptureError::SessionNotFound(s) => write!(f, "no session file for {s}"),
            CaptureError::Io(e) => write!(f, "reading session: {e}"),
            CaptureError::Sql(e) => write!(f, "sqlite: {e}"),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<rusqlite::Error> for CaptureError {
    fn from(e: rusqlite::Error) -> Self {
        CaptureError::Sql(e)
    }
}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        CaptureError::Io(e)
    }
}

/// Finds the session file: `session` as a path when it is a file, else (claude)
/// `<projects>/*/<session>.jsonl`.
pub fn resolve_session(format: Format, session: &str, projects: Option<&Path>) -> Option<PathBuf> {
    let p = PathBuf::from(session);
    if p.is_file() {
        return Some(p);
    }
    if format != Format::Claude || session.contains('/') {
        return None;
    }
    let root = crate::transcripts::projects_root(projects);
    std::fs::read_dir(root).ok()?.flatten().find_map(|d| {
        let f = d.path().join(format!("{session}.jsonl"));
        f.is_file().then_some(f)
    })
}

/// Captures the session of the run of `workflow_id` through `driver`. A driver
/// with no session source, or no session reference, stores nothing and
/// returns `None`; the run keeps cost `unknown`. A native driver whose session
/// reference names no file is an error, so a lost transcript is never mistaken
/// for a driver that has none. `tokens_in` counts every input token, cache
/// reads and writes included.
pub fn capture(
    conn: &Connection,
    workflow_id: &str,
    driver: &dyn HarnessDriver,
    session: Option<&str>,
    projects: Option<&Path>,
) -> Result<Option<Captured>, CaptureError> {
    let (Some(format), Some(session)) = (Format::of(driver), session) else {
        return Ok(None);
    };
    let path = resolve_session(format, session, projects)
        .ok_or_else(|| CaptureError::SessionNotFound(session.to_string()))?;
    let text = std::fs::read_to_string(&path)?;
    store(conn, workflow_id, &parse(format, &text)).map(Some)
}

/// Stores a parsed session on the run of `workflow_id`, replacing an earlier capture.
pub fn store(
    conn: &Connection,
    workflow_id: &str,
    p: &Parsed,
) -> Result<Captured, CaptureError> {
    let tx = conn.unchecked_transaction()?;
    let run_id: i64 = tx
        .query_row("SELECT id FROM run WHERE workflow_id = ?1", [workflow_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| CaptureError::NoRun(workflow_id.to_string()))?;
    tx.execute("DELETE FROM run_transcript WHERE run_id = ?1", [run_id])?;
    tx.execute("DELETE FROM run_tool_call WHERE run_id = ?1", [run_id])?;
    for (i, t) in p.turns.iter().enumerate() {
        tx.execute(
            "INSERT INTO run_transcript (run_id, seq, ts_ms, role, content) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, i as i64 + 1, t.ts_ms as i64, t.role, t.content],
        )?;
    }
    for (i, c) in p.tools.iter().enumerate() {
        tx.execute(
            "INSERT INTO run_tool_call (run_id, seq, ts_ms, tool, args_json, result_json, ok) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![run_id, i as i64 + 1, c.ts_ms as i64, c.tool, c.args_json, c.result_json, c.ok],
        )?;
    }
    let state = if p.cost_usd.is_some() { "reported" } else { "unknown" };
    tx.execute(
        "UPDATE run SET tokens_in = ?2, tokens_out = ?3, cost_usd = ?4, cost_state = ?5 WHERE id = ?1",
        params![
            run_id,
            p.tokens_in.map(|v| v as i64),
            p.tokens_out.map(|v| v as i64),
            p.cost_usd,
            state
        ],
    )?;
    tx.commit()?;
    Ok(Captured {
        turns: p.turns.len(),
        tool_calls: p.tools.len(),
        tokens_in: p.tokens_in,
        tokens_out: p.tokens_out,
        cost_usd: p.cost_usd,
    })
}

/// The agent session herdr reports for a pane (`pane get`).
fn pane_session(herdr: &dyn crate::herdr::HerdrClient, pane: &str) -> Option<String> {
    let reply = herdr.pane_get(pane).ok().filter(|r| r.ok)?;
    let v: Value = serde_json::from_str(&reply.body).ok()?;
    v.pointer("/result/pane/agent_session/value")
        .or_else(|| v.pointer("/result/pane/agent_session"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Captures the run of `workflow_id` when its worker flow ends: resolves the
/// run's driver, reads the agent session from the agent's pane and stores it.
/// A driver with no session source captures nothing. A failure (no pane, no
/// session, a session file that is gone, a database error) is appended to the
/// workflow's journal as `run_capture_failed` and never stops the caller.
pub fn capture_closed(
    journal: &mut dyn crate::engine::journal::Journal,
    herdr: &dyn crate::herdr::HerdrClient,
    drivers: &crate::driver::Registry,
    db_path: &Path,
    workflow_id: &str,
) {
    let fail = |journal: &mut dyn crate::engine::journal::Journal, reason: String| {
        let ts = crate::engine::clock::SystemClock.now_ms();
        let payload = serde_json::json!({ "reason": reason }).to_string();
        let ev = crate::engine::journal::EventRecord::new(0, ts, "run_capture_failed", payload);
        if let Err(e) = journal.append_event(workflow_id, &ev) {
            eprintln!("warning: cannot journal run_capture_failed for {workflow_id}: {e}");
        }
    };
    let go = || -> Result<Option<()>, String> {
        let conn = crate::db::open(db_path).map_err(|e| e.to_string())?;
        capture_run(&conn, herdr, drivers, workflow_id)
    };
    if let Err(reason) = go() {
        fail(journal, reason);
    }
}

/// Reads the agent session of the run's pane and stores it on the run.
fn capture_run(
    conn: &Connection,
    herdr: &dyn crate::herdr::HerdrClient,
    drivers: &crate::driver::Registry,
    workflow_id: &str,
) -> Result<Option<()>, String> {
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT r.harness, a.pane_id FROM run r JOIN agent a ON a.id = r.agent_id \
             WHERE r.workflow_id = ?1",
            [workflow_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((harness, pane)) = row else {
        return Err("no run record".into());
    };
    let h = crate::config::harness(conn, &harness)
        .map_err(|e| format!("{e:?}"))?
        .ok_or_else(|| format!("unknown harness {harness}"))?;
    let driver = drivers.resolve(&h).ok_or_else(|| format!("no driver for {harness}"))?;
    if Format::of(&*driver).is_none() {
        return Ok(None);
    }
    let pane = pane.ok_or("the agent has no recorded pane")?;
    let session = pane_session(herdr, &pane).ok_or("herdr reports no agent session")?;
    capture(conn, workflow_id, &*driver, Some(&session), None).map_err(|e| e.to_string())?;
    Ok(Some(()))
}

/// Opens the run of an open agent that has none (a `worker spawn` peer; only
/// `task.start` opens runs otherwise) and captures its spend while its pane
/// still exists. Returns the run's workflow id, or `None` when the agent is
/// unknown or a task flow already owns its run. A capture that fails leaves
/// cost `unknown`. A repeat (a cleanup that was refused) converges. The workflow
/// row is born `finished`: no flow runs, so the daemon never resumes it.
pub fn open_spawned(
    conn: &Connection,
    herdr: &dyn crate::herdr::HerdrClient,
    name: &str,
    now_ms: u64,
) -> Result<Option<String>, String> {
    let e = |e: rusqlite::Error| e.to_string();
    let agent: Option<(i64, Option<String>, Option<i64>)> = conn
        .query_row(
            "SELECT id, kind, project_id FROM agent WHERE name = ?1 AND ended_ms IS NULL",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(e)?;
    let Some((agent_id, Some(kind), project_id)) = agent else {
        return Ok(None);
    };
    let wf = format!("spawn:{name}:{agent_id}");
    let foreign: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM run WHERE agent_id = ?1 AND workflow_id IS NOT ?2",
            params![agent_id, wf],
            |r| r.get(0),
        )
        .map_err(e)?;
    if foreign > 0 {
        return Ok(None);
    }
    let t = crate::config::resolve_spawn(conn, &kind).map_err(|e| format!("{e:?}"))?;
    conn.execute(
        "INSERT OR IGNORE INTO workflow (id, kind, state, project_id, started_ms, updated_ms, finished_ms) \
         VALUES (?1, 'spawn', 'finished', ?2, ?3, ?3, ?3)",
        params![wf, project_id, now_ms as i64],
    )
    .map_err(e)?;
    crate::run::open(
        conn,
        &crate::run::NewRun {
            workflow_id: &wf,
            agent_id,
            task_id: None,
            kind: &kind,
            harness: &t.harness.name,
            model: &t.model,
            effort: t.effort.as_deref(),
            base: None,
            now_ms,
        },
    )
    .map_err(e)?;
    let harnesses: Vec<_> = crate::config::roster(conn)
        .map_err(|e| format!("{e:?}"))?
        .iter()
        .filter_map(|k| crate::config::harness(conn, &k.harness).ok().flatten())
        .collect();
    let drivers = crate::driver::Registry::from_config(&harnesses);
    let _ = capture_run(conn, herdr, &drivers, &wf);
    Ok(Some(wf))
}


/// Records one artifact of a run; the same kind, path and sha converge.
pub fn add_artifact(
    conn: &Connection,
    run_id: i64,
    kind: &str,
    path: Option<&str>,
    sha: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO run_artifact (run_id, kind, path, sha) \
         SELECT ?1, ?2, ?3, ?4 WHERE NOT EXISTS (SELECT 1 FROM run_artifact \
         WHERE run_id = ?1 AND kind = ?2 AND path IS ?3 AND sha IS ?4)",
        params![run_id, kind, path, sha],
    )?;
    Ok(())
}

/// Parses session text; a half-written or foreign line is skipped.
pub fn parse(format: Format, text: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut usage = Usage::default();
    let mut seen_msgs: HashSet<String> = HashSet::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(msg) = v.get("message") else { continue };
        let ts = v.get("timestamp").and_then(Value::as_str).map(iso_ms).unwrap_or(0);
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        match format {
            Format::Claude => claude_entry(&mut out, &mut usage, &mut seen_msgs, msg, role, ts),
            Format::Pi => pi_entry(&mut out, &mut usage, msg, role, ts),
        }
    }
    if usage.seen {
        out.tokens_in = Some(usage.input);
        out.tokens_out = Some(usage.output);
        out.cost_usd = usage.cost;
    }
    out
}

struct Usage {
    seen: bool,
    input: u64,
    output: u64,
    cost: Option<f64>,
    /// Tool call id -> index into `Parsed::tools`.
    ids: Vec<(String, usize)>,
}

impl Default for Usage {
    fn default() -> Self {
        Usage { seen: false, input: 0, output: 0, cost: Some(0.0), ids: Vec::new() }
    }
}

impl Usage {
    fn call(&mut self, id: &str, idx: usize) {
        self.ids.push((id.to_string(), idx));
    }

    fn find(&self, id: &str) -> Option<usize> {
        self.ids.iter().find(|(i, _)| i == id).map(|(_, x)| *x)
    }
}

/// Days from 1970-01-01 of a civil date.
fn days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// `2026-11-21T13:52:02.391Z` (UTC) in epoch milliseconds; 0 when it does not parse.
fn iso_ms(s: &str) -> u64 {
    let go = || -> Option<i64> {
        let (date, time) = s.split_once('T')?;
        let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
        let (y, m, dd) = (d.next()??, d.next()??, d.next()??);
        let time = time.trim_end_matches('Z');
        let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
        let mut t = hms.split(':').map(|p| p.parse::<i64>().ok());
        let (h, mi, se) = (t.next()??, t.next()??, t.next()??);
        let ms: i64 = format!("{:0<3}", frac).get(..3)?.parse().ok()?;
        Some(((days(y, m, dd) * 24 + h) * 60 + mi) * 60_000 + se * 1000 + ms)
    };
    go().filter(|v| *v > 0).unwrap_or(0) as u64
}

fn blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(s) => vec![serde_json::json!({"type": "text", "text": s})],
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    }
}

fn text_of(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn turn(out: &mut Parsed, ts_ms: u64, role: &'static str, blocks: &[Value]) {
    let content = text_of(blocks);
    if !content.trim().is_empty() {
        out.turns.push(Turn { ts_ms, role, content });
    }
}

fn open_call(out: &mut Parsed, ts_ms: u64, tool: &str, args: Option<&Value>) {
    out.tools.push(ToolCall {
        ts_ms,
        tool: tool.to_string(),
        args_json: args.map(Value::to_string).unwrap_or_else(|| "{}".into()),
        result_json: None,
        ok: None,
    });
}

fn claude_entry(
    out: &mut Parsed,
    usage: &mut Usage,
    seen: &mut HashSet<String>,
    msg: &Value,
    role: &str,
    ts: u64,
) {
    let bl = blocks(msg.get("content").unwrap_or(&Value::Null));
    match role {
        "assistant" => {
            turn(out, ts, "assistant", &bl);
            for b in bl.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use")) {
                let id = b.get("id").and_then(Value::as_str).unwrap_or("");
                if !id.is_empty() && !seen.insert(format!("call:{id}")) {
                    continue;
                }
                open_call(out, ts, b.get("name").and_then(Value::as_str).unwrap_or("?"), b.get("input"));
                usage.call(id, out.tools.len() - 1);
            }
            // One streamed message is several lines with the same usage.
            let fresh = msg
                .get("id")
                .and_then(Value::as_str)
                .map(|id| seen.insert(format!("msg:{id}")))
                .unwrap_or(true);
            if let (true, Some(u)) = (fresh, msg.get("usage")) {
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                usage.seen = true;
                usage.input +=
                    n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens");
                usage.output += n("output_tokens");
                usage.cost = None; // claude reports no cost
            }
        }
        "user" => {
            let mut result = false;
            for b in bl.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")) {
                result = true;
                let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                let idx = usage.find(id);
                if let Some(c) = idx.and_then(|i| out.tools.get_mut(i)) {
                    c.result_json = Some(b.get("content").cloned().unwrap_or(Value::Null).to_string());
                    c.ok = Some(!b.get("is_error").and_then(Value::as_bool).unwrap_or(false));
                }
            }
            if !result {
                turn(out, ts, "user", &bl);
            }
        }
        _ => {}
    }
}

fn pi_entry(out: &mut Parsed, usage: &mut Usage, msg: &Value, role: &str, ts: u64) {
    let bl = blocks(msg.get("content").unwrap_or(&Value::Null));
    match role {
        "user" => turn(out, ts, "user", &bl),
        "assistant" => {
            turn(out, ts, "assistant", &bl);
            for b in bl.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("toolCall")) {
                open_call(out, ts, b.get("name").and_then(Value::as_str).unwrap_or("?"), b.get("arguments"));
                usage.call(b.get("id").and_then(Value::as_str).unwrap_or(""), out.tools.len() - 1);
            }
            if let Some(u) = msg.get("usage") {
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                usage.seen = true;
                usage.input += n("input") + n("cacheRead") + n("cacheWrite");
                usage.output += n("output");
                usage.cost = match (usage.cost, u.pointer("/cost/total").and_then(Value::as_f64)) {
                    (Some(t), Some(c)) => Some(t + c),
                    _ => None,
                };
            }
        }
        "toolResult" => {
            let id = msg.get("toolCallId").and_then(Value::as_str).unwrap_or("");
            let idx = usage.find(id);
            if let Some(c) = idx.and_then(|i| out.tools.get_mut(i)) {
                c.result_json = Some(msg.get("content").cloned().unwrap_or(Value::Null).to_string());
                c.ok = Some(!msg.get("isError").and_then(Value::as_bool).unwrap_or(false));
            }
        }
        _ => {}
    }
}
