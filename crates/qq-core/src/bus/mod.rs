//! Mail through the daemon: `mail.send`, `mail.inbox`, `mail.ack`.
//!
//! A send is stored once (a repeat with the same `dedupe_key` and content from
//! the same sender returns the first mail; other content is a `conflict`),
//! journaled in the same transaction under `mail:<id>`, and
//! pushed as a `mail` Signal to the addressee's workflows (see [`workflows_of`]),
//! if they run. Nothing polls. Delivery reads the rows after the
//! recipient's cursor; with `advance: true` the cursor moves in the same
//! transaction, so a re-read never redelivers. `ack` is the form that
//! survives a lost response.

use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

use crate::authority::{self, Decision};
use crate::cli::{Install, Registry};
use crate::daemon::store::Store;
use crate::daemon::{Ctx, Daemon};
use crate::rpc::{self, Request, RpcError};

pub mod cli;
mod inbox_md;

/// Registers the mail verbs. Called from `cli::MODULES`.
///
/// Contract: a `mail` Signal is a push to a workflow running at send time. A
/// workflow that goes live later (a start, or a recovery at daemon start) gets
/// a `mail` Signal if unacked mail waits for it, so a restart signals again
/// until the mail is acked. Delivery is at-least-once: a send racing a start
/// can signal one mail twice, so a consumer dedupes on the payload's mail `id`. `mail.inbox` only peeks unless `advance: true`;
/// `mail.ack` is the retry-safe way to advance.
pub fn install(registry: &mut Registry, daemon: &mut Daemon) -> Result<(), String> {
    registry.register(cli::command())?;
    daemon.on_workflow_live(Arc::new(|sup, store, id| {
        for who in recipients_of(id) {
            if let Ok(Some(p)) = pending(store, who) {
                let _ = sup.signal(id, "mail", "mail", p);
            }
        }
    }));
    let bus = Arc::new(Bus);
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

/// The entry `cli::MODULES` lists.
pub const INSTALL: Install = install;

type Op = fn(&Bus, &Ctx, &Request) -> Result<Value, RpcError>;

/// The bus holds no state of its own: every call runs on the daemon store's
/// connection ([`crate::daemon::store::Store::with_conn`]).
struct Bus;

impl Bus {
    fn with<T>(
        &self,
        ctx: &Ctx,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, RpcError> {
        ctx.store.with_conn(f).map_err(internal)
    }
}

/// Workflow-id prefix of a worker's flow (`worker:<agent name>`).
const WORKER_PREFIX: &str = "worker:";

/// The one name→workflow mapping: the workflows that hear mail for agent
/// `to` are the worker flow `worker:<to>` and a flow named `<to>` itself.
fn workflows_of(to: &str) -> [String; 2] {
    [format!("{WORKER_PREFIX}{to}"), to.to_string()]
}

/// The inverse: the mailboxes a workflow `id` listens on.
fn recipients_of(id: &str) -> Vec<&str> {
    let mut who = vec![id];
    if let Some(name) = id.strip_prefix(WORKER_PREFIX) {
        who.insert(0, name);
    }
    who
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

/// The mailbox owner: always the caller. `to` may name the caller (the CLI
/// passes it for `--for`); naming anyone else is refused and journaled as an
/// authority refusal on the edge `op`, target = that mailbox.
fn owner<'a>(ctx: &Ctx, req: &'a Request, op: &str) -> Result<&'a str, RpcError> {
    let caller = req
        .caller
        .as_deref()
        .ok_or_else(|| bad("no mailbox: a caller is needed"))?;
    match req.args.get("to").and_then(Value::as_str) {
        Some(to) if !to.is_empty() && to != caller => {
            ctx.store
                .with_conn(|c| {
                    authority::refuse(
                        c,
                        caller,
                        op,
                        to,
                        "a mailbox is read and acked by its owner only",
                    )
                })
                .map_err(internal)?;
            Err(RpcError::new(
                "refused",
                format!("{caller} -> {to}: {op} reaches only the caller's own mailbox"),
            ))
        }
        _ => Ok(caller),
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
    if body.chars().count() > 4096 {
        return Err(bad("body over 4096 characters"));
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

    // The edge check runs on the same connection, outside the transaction, so
    // a refusal is journaled and never rolled back with a send.
    let refused = ctx
        .store
        .with_conn(|c| authority::check_mail(c, from, to))
        .map_err(internal)?;
    if let Decision::Refused(r) = refused {
        return Err(RpcError::new(
            "refused",
            format!("{} -> {}: {}", r.caller, r.target, r.reason),
        ));
    }

    deliver(bus, ctx, from, to, kind, body, key)
}

/// A mail the daemon itself sends: no caller, so no edge check.
pub(crate) fn daemon_mail(
    ctx: &Ctx,
    to: &str,
    kind: &str,
    body: &str,
    key: Option<&str>,
) -> Result<Value, RpcError> {
    deliver(&Bus, ctx, DAEMON, to, kind, body, key)
}

/// Sender of the mail the daemon writes on its own.
pub const DAEMON: &str = "daemon";

/// A state a watched agent moved into that the CoS hears about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentState {
    Stalled,
    Blocked,
    Done,
    Exited,
}

impl AgentState {
    fn word(self) -> &'static str {
        match self {
            Self::Stalled => "stalled",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Exited => "exited",
        }
    }

    /// Done is news for the digest, not for anh iu's screen.
    fn notifies(self) -> bool {
        self != Self::Done
    }
}

/// One `kind=state` mail to the CoS for `agent`'s move into `state`, and a
/// herdr notification for the states that need a person. Best effort.
pub fn state_mail(ctx: &Ctx, agent: &str, role: &str, state: AgentState, detail: &str) {
    let line = format!("{agent} ({role}) {}{detail}", state.word());
    let created = daemon_mail(ctx, "cos", "state", &line, None)
        .ok()
        .and_then(|r| r["created"].as_bool())
        .unwrap_or(false);
    if created && state.notifies() {
        let _ = ctx.herdr.notification_show(
            &format!("quaqua: {agent} {}", state.word()),
            &line,
            Some("request"),
        );
    }
}

fn deliver(
    bus: &Bus,
    ctx: &Ctx,
    from: &str,
    to: &str,
    kind: &str,
    body: &str,
    key: Option<&str>,
) -> Result<Value, RpcError> {
    // Row and journal line commit together, so a mail is never delivered
    // unjournaled; a retry finds both and writes neither.
    let outcome = bus.with(ctx, |c| {
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

    // The markdown copy of a Lead -> CoS mail. Written after the commit and
    // on a retry too, so a send that died between the two is repaired by the
    // client's next attempt.
    inbox_md::mirror(&ctx.runs_dir, id, from, to, kind, body, now_ms()).map_err(internal)?;

    // The push: only for a new mail, only to the addressee's running
    // workflows. The inbox is the record, so a missing workflow is not an error.
    let mut signalled = false;
    if created {
        for wf in workflows_of(to) {
            if ctx.supervisor.is_running(&wf) {
                signalled |= ctx
                    .supervisor
                    .signal(
                        &wf,
                        "mail",
                        "mail",
                        json!({ "id": id, "from": from, "kind": kind }),
                    )
                    .is_ok();
            }
        }
    }
    if created {
        // Best effort: the mail is stored, so a herdr that is down or does not
        // support a verb never fails the send.
        if kind == "escalate" || kind == "blocked" {
            let _ = ctx.herdr.notification_show(
                &format!("quaqua mail: {kind}"),
                &format!("{from} -> {to}: {kind}"),
                Some("request"),
            );
        }
        if !signalled {
            wake_idle(ctx, to, from, kind);
        }
    }
    Ok(json!({ "id": id, "created": created, "signalled": signalled }))
}

/// Nudges `to` about new mail when it sits idle in herdr right now. The CoS is
/// never nudged (its inbox is read on its own turn); a busy agent reads the
/// mail at its next inbox read.
fn wake_idle(ctx: &Ctx, to: &str, from: &str, kind: &str) {
    if to == "cos" {
        return;
    }
    let Ok(reply) = ctx.herdr.agent_get(to) else {
        return;
    };
    let status = serde_json::from_str::<Value>(&reply.body)
        .ok()
        .and_then(|v| {
            v.pointer("/result/agent/agent_status")?
                .as_str()
                .map(String::from)
        });
    if !reply.ok || !matches!(status.as_deref(), Some("idle" | "done")) {
        return;
    }
    let line = format!("quaqua: new {kind} mail from {from}; read your inbox");
    let _ = ctx
        .herdr
        .agent_prompt(to, &line.replace(['\n', '\r'], " "), None);
}

/// The Signal payload for mail waiting past `who`'s cursor: the newest item's
/// `{id, from, kind}` plus the `pending` count; `None` when nothing waits.
fn pending(store: &Store, who: &str) -> rusqlite::Result<Option<Value>> {
    store.with_conn(|c| {
        let cursor = cursor_of(c, who)?;
        let n: i64 = c.query_row(
            "SELECT COUNT(*) FROM mail WHERE recipient = ?1 AND id > ?2",
            params![who, cursor],
            |r| r.get(0),
        )?;
        if n == 0 {
            return Ok(None);
        }
        c.query_row(
            "SELECT id, sender, kind FROM mail WHERE recipient = ?1 AND id > ?2 \
             ORDER BY id DESC LIMIT 1",
            params![who, cursor],
            |r| {
                Ok(Some(json!({
                    "id": r.get::<_, i64>(0)?, "from": r.get::<_, String>(1)?,
                    "kind": r.get::<_, String>(2)?, "pending": n,
                })))
            },
        )
    })
}

fn inbox(bus: &Bus, ctx: &Ctx, req: &Request) -> Result<Value, RpcError> {
    let who = owner(ctx, req, "mail.inbox")?;
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
    let mail = bus.with(ctx, |c| {
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
    let who = owner(ctx, req, "mail.ack")?;
    let upto = req
        .args
        .get("upto")
        .and_then(Value::as_i64)
        .ok_or_else(|| bad("missing integer argument 'upto'"))?;
    let cursor = bus.with(ctx, |c| {
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
