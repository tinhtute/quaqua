//! `run show` and `run list`: one run printed whole, and runs by project. Reads
//! the database only.

use rusqlite::{Connection, OptionalExtension};

use crate::cli::{CliError, Env};

pub struct Full {
    pub head: String,
    pub events: Vec<(i64, u64, String, String)>,
    pub turns: Vec<(u64, String, String)>,
    pub tools: Vec<(String, String, Option<String>, Option<bool>)>,
    pub artifacts: Vec<(String, Option<String>, Option<String>)>,
}

fn cost_text(state: &str, usd: Option<f64>) -> String {
    match (state, usd) {
        ("reported", Some(c)) => format!("${c:.4}"),
        _ => "unknown".into(),
    }
}

fn count_text(v: Option<i64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into())
}

/// Loads one run by workflow id (`worker:w1`) or numeric run id.
pub fn load(conn: &Connection, key: &str) -> rusqlite::Result<Option<Full>> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM run WHERE workflow_id = ?1 OR CAST(id AS TEXT) = ?1 ORDER BY (workflow_id = ?1) DESC LIMIT 1",
            [key],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id else { return Ok(None) };
    let head = conn.query_row(
        "SELECT r.id, COALESCE(r.workflow_id, '-'), a.name, a.role, r.kind, r.harness, r.model, \
         COALESCE(r.effort, '-'), COALESCE(p.slug, '-'), r.task_id, r.started_ms, r.ended_ms, \
         COALESCE(r.outcome, 'running'), COALESCE(r.candidate_sha, 'none'), \
         r.tokens_in, r.tokens_out, r.cost_state, r.cost_usd \
         FROM run r JOIN agent a ON a.id = r.agent_id \
         LEFT JOIN task t ON t.id = r.task_id \
         LEFT JOIN project p ON p.id = COALESCE(t.project_id, a.project_id) \
         WHERE r.id = ?1",
        [id],
        |r| {
            let ended: Option<i64> = r.get(11)?;
            let task: Option<i64> = r.get(9)?;
            Ok(format!(
                "run {} (id {})\nagent {} ({}) kind {} harness {} model {} effort {}\nproject {} task {}\nstarted {} ended {}\noutcome {}\ncandidate_sha {}\ntokens in={} out={}\ncost {}",
                r.get::<_, String>(1)?,
                r.get::<_, i64>(0)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                task.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
                r.get::<_, i64>(10)?,
                ended.map(|e| e.to_string()).unwrap_or_else(|| "-".into()),
                r.get::<_, String>(12)?,
                r.get::<_, String>(13)?,
                count_text(r.get(14)?),
                count_text(r.get(15)?),
                cost_text(&r.get::<_, String>(16)?, r.get(17)?),
            ))
        },
    )?;
    let workflow: Option<String> =
        conn.query_row("SELECT workflow_id FROM run WHERE id = ?1", [id], |r| r.get(0))?;
    let has_events: bool = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'events'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    let events = match (&workflow, has_events) {
        (Some(w), true) => conn
            .prepare("SELECT seq, ts_ms, kind, payload_json FROM events WHERE workflow_id = ?1 \
             OR (?1 LIKE 'worker:%' AND workflow_id = 'lookup:' || substr(?1, 8)) \
             ORDER BY ts_ms, workflow_id, seq")?
            .query_map([w], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?,
        _ => Vec::new(),
    };
    let turns = conn
        .prepare("SELECT ts_ms, role, content FROM run_transcript WHERE run_id = ?1 ORDER BY seq")?
        .query_map([id], |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let tools = conn
        .prepare("SELECT tool, COALESCE(args_json, ''), result_json, ok FROM run_tool_call WHERE run_id = ?1 ORDER BY seq")?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    let artifacts = conn
        .prepare("SELECT kind, path, sha FROM run_artifact WHERE run_id = ?1 ORDER BY id")?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(Some(Full { head, events, turns, tools, artifacts }))
}

/// The whole run as text: header, artifacts, events, transcript and tool calls.
/// An empty section prints a zero count (`transcript (0)`), so a missing capture is visible.
pub fn render(f: &Full) -> String {
    let mut o = f.head.clone();
    o.push_str(&format!("\nartifacts ({})", f.artifacts.len()));
    for (k, p, s) in &f.artifacts {
        o.push_str(&format!(
            "\n  {k} {} {}",
            p.as_deref().unwrap_or("-"),
            s.as_deref().unwrap_or("-")
        ));
    }
    o.push_str(&format!("\nevents ({})", f.events.len()));
    for (seq, ts, kind, payload) in &f.events {
        o.push_str(&format!("\n  {seq} {ts} {kind} {payload}"));
    }
    o.push_str(&format!("\ntranscript ({})", f.turns.len()));
    for (ts, role, content) in &f.turns {
        o.push_str(&format!("\n  [{ts}] {role}: {content}"));
    }
    o.push_str(&format!("\ntool calls ({})", f.tools.len()));
    for (tool, args, result, ok) in &f.tools {
        let ok = match ok {
            Some(true) => "ok",
            Some(false) => "error",
            None => "no result",
        };
        o.push_str(&format!("\n  {tool} {ok} args={args}"));
        if let Some(r) = result {
            o.push_str(&format!(" result={r}"));
        }
    }
    o
}

/// One line per run, oldest first, optionally of one project.
pub fn list(conn: &Connection, project: Option<&str>) -> rusqlite::Result<Vec<String>> {
    let mut st = conn.prepare(
        "SELECT r.id, COALESCE(r.workflow_id, '-'), a.name, r.harness, r.model, \
         COALESCE(r.outcome, 'running'), r.tokens_in, r.tokens_out, r.cost_state, r.cost_usd, \
         COALESCE(r.candidate_sha, 'none') \
         FROM run r JOIN agent a ON a.id = r.agent_id \
         LEFT JOIN task t ON t.id = r.task_id \
         LEFT JOIN project p ON p.id = COALESCE(t.project_id, a.project_id) \
         WHERE (?1 IS NULL OR p.slug = ?1) ORDER BY r.id",
    )?;
    let rows = st.query_map([project], |r| {
        Ok(format!(
            "{} {} {} {} {} {} in={} out={} cost={} sha={}",
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            count_text(r.get(6)?),
            count_text(r.get(7)?),
            cost_text(&r.get::<_, String>(8)?, r.get(9)?),
            r.get::<_, String>(10)?,
        ))
    })?;
    rows.collect()
}

const USAGE: &str = "usage: quaqua run show <workflow|id> | run list [--project <slug>]";

pub fn run_command(env: &Env, args: &[String]) -> Result<String, CliError> {
    let usage = || CliError::new(2, USAGE);
    let err = |e: String| CliError::new(1, e);
    let conn = crate::db::open(&env.runs_dir.join(crate::db::path::DB_FILE))
        .map_err(|e| err(e.to_string()))?;
    match args {
        [sub, key] if sub == "show" => match load(&conn, key).map_err(|e| err(e.to_string()))? {
            Some(f) => Ok(render(&f)),
            None => Err(err(format!("run show: no run '{key}'"))),
        },
        [sub, rest @ ..] if sub == "list" => {
            let project = match rest {
                [] => None,
                [flag, slug] if flag == "--project" => Some(slug.as_str()),
                _ => return Err(usage()),
            };
            if let Some(slug) = project {
                let known: bool = conn
                    .query_row("SELECT count(*) FROM project WHERE slug = ?1", [slug], |r| r.get::<_, i64>(0))
                    .map_err(|e| err(e.to_string()))?
                    > 0;
                if !known {
                    return Err(err(format!("run list: no project '{slug}'")));
                }
            }
            Ok(list(&conn, project).map_err(|e| err(e.to_string()))?.join("\n"))
        }
        _ => Err(usage()),
    }
}
