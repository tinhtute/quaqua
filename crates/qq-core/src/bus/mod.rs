//! Mail through the daemon: `mail.send`, `mail.inbox`, `mail.ack`.
//!
//! A send is stored once (a repeat with the same `dedupe_key` from the same
//! sender returns the first mail), journaled once under `mail:<id>`, and
//! pushed as one `mail` Signal to the workflow named like the addressee, if
//! that workflow runs. Nothing polls. Delivery reads the rows after the
//! recipient's cursor and advances the cursor in the same transaction, so a
//! re-read never redelivers; `peek` plus `ack` is the form that survives a
//! lost response.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

use crate::cli::Registry;
use crate::daemon::{Ctx, Daemon};
use crate::db::path::DB_FILE;
use crate::engine::journal::{EventRecord, Journal};
use crate::rpc::{self, Request, RpcError};

/// Registers the mail verbs. Called from `cli::MODULES`.
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

    let (id, created) = bus.with(&ctx.runs_dir, |c| {
        let n = c.execute(
            "INSERT OR IGNORE INTO mail (sender, recipient, kind, body, dedupe_key, created_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![from, to, kind, body, key, now_ms() as i64],
        )?;
        let id = if n == 1 {
            c.last_insert_rowid()
        } else {
            c.query_row(
                "SELECT id FROM mail WHERE sender = ?1 AND dedupe_key = ?2",
                params![from, key],
                |r| r.get(0),
            )?
        };
        Ok((id, n == 1))
    })?;

    // Journal once per mail, also when a retry finds the row but a crash
    // came before its journal line.
    let jid = format!("mail:{id}");
    let mut journal = ctx.store.journal();
    let seen = journal.load_events(&jid, 1).map_err(internal)?;
    if seen.is_empty() {
        let payload = json!({ "id": id, "from": from, "to": to, "kind": kind });
        journal
            .append_event(
                &jid,
                &EventRecord::new(0, now_ms(), "mail_sent", payload.to_string()),
            )
            .map_err(internal)?;
    }

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
    let peek = req
        .args
        .get("peek")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mail = bus.with(&ctx.runs_dir, |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cursor = cursor_of(&tx, who)?;
        let rows: Vec<Value> = {
            let mut st = tx.prepare(
                "SELECT id, sender, kind, body, created_ms FROM mail \
                 WHERE recipient = ?1 AND id > ?2 ORDER BY id",
            )?;
            let it = st.query_map(params![who, cursor], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "from": r.get::<_, String>(1)?,
                    "kind": r.get::<_, String>(2)?, "body": r.get::<_, String>(3)?,
                    "created_ms": r.get::<_, i64>(4)?,
                }))
            })?;
            it.collect::<Result<_, _>>()?
        };
        if !peek {
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
