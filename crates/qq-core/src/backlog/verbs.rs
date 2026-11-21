//! The backlog commands: `quaqua backlog list` and `quaqua backlog decide`.
//! The daemon owns `qq.db`, so both are thin clients of the verbs
//! `backlog.list` and `backlog.decide`, which call [`decide`] and
//! [`task_from_proposal`]. Only the CoS records a verdict, and only on low
//! risk; a high-risk proposal stays open for anh iu.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::{decide, task_from_proposal, BacklogError, Decider};
use crate::cli::{rpc_call, CliError, Command, Env, Registry};
use crate::daemon::{caller, Ctx, Daemon};
use crate::db::path::DB_FILE;
use crate::rpc::{Request, RpcError, BAD_REQUEST};
use crate::task::graph::{DEFAULT_DISPOSITION, DISPOSITIONS};

pub const LIST: &str = "backlog.list";
pub const DECIDE: &str = "backlog.decide";

const USAGE: &str = "usage: quaqua backlog list [open|approved|rejected] | decide <id> approve|reject [--note TEXT] [--title TEXT] [--kind K] [--disposition D]";

pub fn install(registry: &mut Registry, daemon: &mut Daemon) -> Result<(), String> {
    registry.register(Command {
        name: "backlog",
        help: "backlog list [status] | decide <id> approve|reject [--note T] [--title T] [--kind K] [--disposition D]",
        run: backlog_cmd,
    })?;
    daemon.register_verb(LIST, Box::new(serve_list))?;
    daemon.register_verb(DECIDE, Box::new(serve_decide))
}

fn bad(m: impl Into<String>) -> RpcError {
    RpcError::new(BAD_REQUEST, m.into())
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::new("internal", e.to_string())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn open(ctx: &Ctx) -> Result<Connection, RpcError> {
    crate::db::open(&ctx.runs_dir.join(DB_FILE)).map_err(internal)
}

fn serve_list(ctx: &Ctx, req: &Request) -> Result<Value, RpcError> {
    let status = req.args.get("status").and_then(Value::as_str);
    if let Some(s) = status {
        if !["open", "approved", "rejected"].contains(&s) {
            return Err(bad("status must be open, approved or rejected"));
        }
    }
    let conn = open(ctx)?;
    // A proposal has no kind of its own; the kind shown is its task's.
    let mut stmt = conn
        .prepare(
            "SELECT p.id, p.loop, t.kind, p.risk, p.status, t.id \
             FROM proposal p LEFT JOIN task t ON t.id = \
               (SELECT MIN(id) FROM task WHERE proposal_id = p.id) \
             WHERE (?1 IS NULL OR p.status = ?1) ORDER BY p.id",
        )
        .map_err(internal)?;
    let rows = stmt
        .query_map(params![status], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "loop": r.get::<_, i64>(1)?,
                "kind": r.get::<_, Option<String>>(2)?,
                "risk": r.get::<_, String>(3)?,
                "status": r.get::<_, String>(4)?,
                "task_id": r.get::<_, Option<i64>>(5)?,
            }))
        })
        .map_err(internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?;
    Ok(json!({ "proposals": rows }))
}

fn serve_decide(ctx: &Ctx, req: &Request) -> Result<Value, RpcError> {
    if !caller::is_cos(req) {
        let who = caller::identified(req).unwrap_or("unknown");
        let why = "only the CoS records a backlog verdict";
        let _ = ctx
            .store
            .with_conn(|c| crate::authority::refuse(c, who, DECIDE, DECIDE, why));
        return Err(RpcError::new(
            "refused",
            format!("{who} -> {DECIDE}: {why}"),
        ));
    }
    let a = &req.args;
    let id = a
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| bad("missing integer argument 'id'"))?;
    let approve = match a.get("verdict").and_then(Value::as_str) {
        Some("approve") => true,
        Some("reject") => false,
        _ => return Err(bad("verdict must be approve or reject")),
    };
    let s = |k: &str| {
        a.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
    };
    let disposition = s("disposition").unwrap_or(DEFAULT_DISPOSITION);
    if !DISPOSITIONS.contains(&disposition) {
        return Err(bad(format!("disposition must be one of {DISPOSITIONS:?}")));
    }
    let mut conn = open(ctx)?;
    if let Some(k) = s("kind") {
        let known: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM kind WHERE name = ?1)",
                [k],
                |r| r.get(0),
            )
            .map_err(internal)?;
        if !known {
            return Err(bad(format!("no kind '{k}'")));
        }
    }
    let now = now_ms();
    decide(&mut conn, id, Decider::Cos, approve, s("note"), now).map_err(refusal)?;
    let task_id = if approve {
        let title = match s("title") {
            Some(t) => t.to_string(),
            None => default_title(&conn, id).map_err(internal)?,
        };
        Some(
            task_from_proposal(&mut conn, id, &title, disposition, s("kind"), now)
                .map_err(refusal)?,
        )
    } else {
        None
    };
    Ok(json!({
        "id": id,
        "status": if approve { "approved" } else { "rejected" },
        "task_id": task_id,
    }))
}

fn default_title(conn: &Connection, id: i64) -> Result<String, rusqlite::Error> {
    let obs: Option<String> = conn.query_row(
        "SELECT observation FROM proposal WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    let line = obs
        .as_deref()
        .and_then(|o| o.lines().next())
        .unwrap_or("")
        .trim();
    Ok(if line.is_empty() {
        format!("proposal {id}")
    } else {
        line.chars().take(120).collect()
    })
}

fn refusal(e: BacklogError) -> RpcError {
    match e {
        BacklogError::Sqlite(m) => internal(m),
        BacklogError::NoSuchProposal(_) => bad(e.to_string()),
        e => RpcError::new("refused", e.to_string()),
    }
}

fn usage() -> CliError {
    CliError::new(2, USAGE)
}

fn backlog_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    match args.first().map(String::as_str) {
        Some("list") => {
            if args.len() > 2 {
                return Err(usage());
            }
            let body = match args.get(1) {
                Some(s) => json!({ "status": s }),
                None => json!({}),
            };
            let v = rpc_call(env, LIST, body)?;
            Ok(render(&v))
        }
        Some("decide") => {
            let (id, verdict) = match (args.get(1), args.get(2)) {
                (Some(i), Some(v)) => (i.parse::<i64>().map_err(|_| usage())?, v.clone()),
                _ => return Err(usage()),
            };
            let mut body = json!({ "id": id, "verdict": verdict });
            let mut rest = args[3..].iter();
            while let Some(flag) = rest.next() {
                let key = match flag.as_str() {
                    "--note" => "note",
                    "--title" => "title",
                    "--kind" => "kind",
                    "--disposition" => "disposition",
                    _ => return Err(usage()),
                };
                body[key] = json!(rest.next().ok_or_else(usage)?);
            }
            let v = rpc_call(env, DECIDE, body)?;
            let status = v["status"].as_str().unwrap_or_default();
            Ok(match v["task_id"].as_i64() {
                Some(t) => format!("proposal {id} {status}, task {t}"),
                None => format!("proposal {id} {status}"),
            })
        }
        _ => Err(usage()),
    }
}

fn render(v: &Value) -> String {
    let mut out = String::from("id\tloop\tkind\trisk\tstatus\ttask");
    for p in v["proposals"].as_array().into_iter().flatten() {
        let num = |k: &str| {
            p[k].as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".into())
        };
        let txt = |k: &str| p[k].as_str().unwrap_or("-").to_string();
        out.push_str(&format!(
            "\n{}\t{}\t{}\t{}\t{}\t{}",
            num("id"),
            num("loop"),
            txt("kind"),
            txt("risk"),
            txt("status"),
            num("task_id")
        ));
    }
    out
}
