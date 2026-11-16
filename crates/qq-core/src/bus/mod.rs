//! Mail through the daemon: `mail.send`, `mail.inbox`, `mail.ack`.
//!
//! A send is stored once (a repeat with the same `dedupe_key` and content from
//! the same sender returns the first mail; other content is a `conflict`),
//! journaled in the same transaction under `mail:<id>`, and
//! pushed as one `mail` Signal to the workflow named like the addressee, if
//! that workflow runs. Nothing polls. Delivery reads the rows after the
//! recipient's cursor; with `advance: true` the cursor moves in the same
//! transaction, so a re-read never redelivers. `ack` is the form that
//! survives a lost response.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

use crate::cli::Registry;
use crate::daemon::{Ctx, Daemon};
use crate::db::path::DB_FILE;
use crate::rpc::{self, Request, RpcError};

/// Registers the mail verbs. Called from `cli::MODULES`.
///
/// Contract: a `mail` Signal is a push to a workflow running at send time. A
/// workflow that starts later, or after a daemon restart, is not signalled for
/// mail already stored, so it reads `mail.inbox` at start. `mail.inbox` only
/// peeks unless `advance: true`; `mail.ack` is the retry-safe way to advance.
pub fn install(_: &mut Registry, daemon: &mut Daemon) -> Result<(), String> {
    let bus = Arc::new(Bus::default());
    for (name, f) in [
        ("mail.send", send as Op),
        ("mail.inbox", inbox as Op),
        ("mail.ack", ack as Op),
    ] {
        let bus = Arc::clone(&bus);
        daemon.register_verb(name, Box::new(move |ctx, req| f(&bus, ctx, req)))?;
    }
    Ok(())
}

/// Most mail one `mail.inbox` call returns.
const MAX_PAGE: i64 = 100;

type Op = fn(&Bus, &Ctx, &Request) -> Result<Value, RpcError>;

/// The bus's own connection to `runs/qq.db`, opened on first use (the daemon
/// owns the directory, and the opener migrates to the mail schema).
#[derive(Default)]
struct Bus {
    conn: Mutex<Option<Connection>>,
}

impl Bus {
    fn with<T>(
        &self,
        runs_dir: &Path,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, RpcError> {
        let mut slot = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            let c = crate::db::open(&runs_dir.join(DB_FILE)).map_err(internal)?;
            // The journal tables are the engine's; the idempotent DDL keeps the
            // send transaction independent of who opened the file first.
            c.execute_batch(crate::engine::journal_sqlite::JOURNAL_SCHEMA_SQL)
                .map_err(internal)?;
            *slot = Some(c);
        }
        f(slot.as_mut().expect("opened above")).map_err(internal)
    }
}

fn internal(e: impl ToString) -> RpcError {
    RpcError::new("internal", e.to_string())
}

fn bad(msg: impl Into<String>) -> RpcError {
    RpcError::new(rpc::BAD_REQUEST, msg)
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    match args.get(key).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(bad(format!("missing string argument '{key}'"))),
    }
}

/// The mailbox owner: the `to` argument, else the caller.
fn owner(req: &Request) -> Result<&str, RpcError> {
    match req.args.get("to").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => req
            .caller
            .as_deref()
            .ok_or_else(|| bad("no mailbox: pass 'to' or a caller")),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

enum Sent {
    New(i64),
    Same(i64),
    Conflict(i64),
}

fn send(bus: &Bus, ctx: &Ctx, req: &Request) -> Result<Value, RpcError> {
    let to = str_arg(&req.args, "to")?;
    let body = str_arg(&req.args, "body")?;
    if body.len() > 4096 {
        return Err(bad("body over 4096 bytes"));
    }
    let kind = req
        .args
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("note");
    let key = req.args.get("dedupe_key").and_then(Value::as_str);
    let from = req
        .caller
        .as_deref()
        .ok_or_else(|| bad("mail needs a caller to name the sender"))?;

    // Row and journal line commit together, so a mail is never delivered
    // unjournaled; a retry finds both and writes neither.
    let outcome = bus.with(&ctx.runs_dir, |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms() as i64;
        let n = tx.execute(
            "INSERT INTO mail (sender, recipient, kind, body, dedupe_key, created_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(sender, dedupe_key) DO NOTHING",
            params![from, to, kind, body, key, now],
        )?;
        if n == 1 {
            let id = tx.last_insert_rowid();
            let payload = json!({ "id": id, "from": from, "to": to, "kind": kind });
            tx.execute(
                "INSERT INTO events (workflow_id, seq, ts_ms, kind, payload_json) \
                 VALUES (?1, 1, ?2, 'mail_sent', ?3)",
                params![format!("mail:{id}"), now, payload.to_string()],
            )?;
            tx.commit()?;
            return Ok(Sent::New(id));
        }
        let (id, r, k, b): (i64, String, String, String) = tx.query_row(
            "SELECT id, recipient, kind, body FROM mail WHERE sender = ?1 AND dedupe_key = ?2",
            params![from, key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        Ok(
            if (r.as_str(), k.as_str(), b.as_str()) == (to, kind, body) {
                Sent::Same(id)
            } else {
                Sent::Conflict(id)
            },
        )
    })?;
    let (id, created) = match outcome {
        Sent::New(id) => (id, true),
        Sent::Same(id) => (id, false),
        Sent::Conflict(id) => {
            return Err(RpcError::new(
                "conflict",
                format!("dedupe_key already used for a different mail (id {id})"),
            ))
        }
    };

    // The push: only for a new mail, only if the addressee's workflow runs.
    // The inbox is the record, so a missing workflow is not an error.
    let mut signalled = false;
    if created && ctx.supervisor.is_running(to) {
        signalled = ctx
            .supervisor
            .signal(
                to,
                "mail",
                "mail",
                json!({ "id": id, "from": from, "kind": kind }),
            )
            .is_ok();
    }
    Ok(json!({ "id": id, "created": created, "signalled": signalled }))
}

fn inbox(bus: &Bus, ctx: &Ctx, req: &Request) -> Result<Value, RpcError> {
    let who = owner(req)?;
    let advance_cursor = req
        .args
        .get("advance")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = req
        .args
        .get("limit")
        .and_then(Value::as_i64)
        .map_or(MAX_PAGE, |l| l.clamp(1, MAX_PAGE));
    let mail = bus.with(&ctx.runs_dir, |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cursor = cursor_of(&tx, who)?;
        let rows: Vec<Value> = {
            let mut st = tx.prepare(
                "SELECT id, sender, kind, body, created_ms FROM mail \
                 WHERE recipient = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
            )?;
            let it = st.query_map(params![who, cursor, limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "from": r.get::<_, String>(1)?,
                    "kind": r.get::<_, String>(2)?, "body": r.get::<_, String>(3)?,
                    "created_ms": r.get::<_, i64>(4)?,
                }))
            })?;
            it.collect::<Result<_, _>>()?
        };
        if advance_cursor {
            if let Some(last) = rows.last().and_then(|m| m["id"].as_i64()) {
                advance(&tx, who, last)?;
            }
        }
        tx.commit()?;
        Ok(rows)
    })?;
    Ok(json!({ "to": who, "mail": mail }))
}

fn ack(bus: &Bus, ctx: &Ctx, req: &Request) -> Result<Value, RpcError> {
    let who = owner(req)?;
    let upto = req
        .args
        .get("upto")
        .and_then(Value::as_i64)
        .ok_or_else(|| bad("missing integer argument 'upto'"))?;
    let cursor = bus.with(&ctx.runs_dir, |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Clamped to what exists, so a wild `upto` cannot swallow later mail.
        let newest: i64 = tx.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM mail WHERE recipient = ?1",
            params![who],
            |r| r.get(0),
        )?;
        advance(&tx, who, upto.min(newest))?;
        let cur = cursor_of(&tx, who)?;
        tx.commit()?;
        Ok(cur)
    })?;
    Ok(json!({ "to": who, "cursor": cursor }))
}

fn cursor_of(c: &Connection, who: &str) -> rusqlite::Result<i64> {
    Ok(c.query_row(
        "SELECT mail_id FROM mail_cursor WHERE recipient = ?1",
        params![who],
        |r| r.get(0),
    )
    .optional()?
    .unwrap_or(0))
}

/// Monotonic: a repeat or an older ack changes nothing.
fn advance(c: &Connection, who: &str, upto: i64) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO mail_cursor (recipient, mail_id) VALUES (?1, ?2) \
         ON CONFLICT(recipient) DO UPDATE SET mail_id = MAX(mail_id, excluded.mail_id)",
        params![who, upto],
    )?;
    Ok(())
}
