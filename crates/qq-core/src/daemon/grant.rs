//! `grant.full`, `grant.tier`, `grant.lane` (R14, Q15): the only writers of the
//! per-project full grant, the repo tier and the plan lane the gate reads.
//! Only `cos` writes, checked as `Op::ConfigWrite`; every change is journaled
//! in `grant_note` and mirrored to anh iu as one herdr notification. A grant
//! has no expiry and no verb here revokes it: revoking is anh iu's.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::{now_ms, str_arg, Ctx, Daemon};
use crate::authority::tier::{repo_key, tier_word, Tier};
use crate::authority::{check_orchestration, Decision, Fence, Op};
use crate::db::path::DB_FILE;
use crate::rpc::{Request, RpcError, BAD_REQUEST};

fn refused(why: impl Into<String>) -> RpcError {
    RpcError::new("refused", why)
}

/// The caller of a write, or the refusal: `cos` only, and only when the
/// orchestration check lets it run a config write.
fn cos_caller(conn: &Connection, req: &Request) -> Result<String, RpcError> {
    let Some(caller) = req.caller.as_deref() else {
        return Err(refused("a grant write needs an identified caller"));
    };
    if caller != "cos" {
        return Err(refused(format!("{caller}: only cos writes a grant")));
    }
    match check_orchestration(conn, caller, Op::ConfigWrite, Fence::Fenced)
        .map_err(|e| RpcError::new("internal", e.0))?
    {
        Decision::Allowed => Ok(caller.to_string()),
        Decision::Refused(r) => Err(refused(format!(
            "{} -> {}: {}",
            r.caller, r.target, r.reason
        ))),
    }
}

fn tier_arg(args: &Value) -> Result<Tier, RpcError> {
    match str_arg(args, "tier")? {
        "low" => Ok(Tier::Low),
        "medium" => Ok(Tier::Medium),
        "high" => Ok(Tier::High),
        other => Err(RpcError::new(
            BAD_REQUEST,
            format!("tier must be low, medium or high, not '{other}'"),
        )),
    }
}

fn internal(e: impl ToString) -> RpcError {
    RpcError::new("internal", e.to_string())
}

/// Writes one change and its `grant_note` row in one transaction, then raises
/// the herdr notification and records whether it went out. The write stands
/// when the notification fails; `notified` says so.
fn change(
    ctx: &Ctx,
    caller: &str,
    kind: &str,
    subject: &str,
    value: &str,
    write: impl FnOnce(&rusqlite::Transaction, i64) -> rusqlite::Result<()>,
) -> Result<Value, RpcError> {
    let mut conn = crate::db::open(&ctx.runs_dir.join(DB_FILE)).map_err(internal)?;
    let now = now_ms() as i64;
    let tx = conn.transaction().map_err(internal)?;
    write(&tx, now).map_err(internal)?;
    tx.execute(
        "INSERT INTO grant_note (ts_ms, caller, kind, subject, value) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![now, caller, kind, subject, value],
    )
    .map_err(internal)?;
    let id = tx.last_insert_rowid();
    tx.commit().map_err(internal)?;
    let body = format!("{caller} set {kind} {subject} = {value}");
    let notified = matches!(
        ctx.herdr
            .notification_show("quaqua: grant change", &body, Some("request")),
        Ok(r) if r.ok
    );
    if notified {
        conn.execute("UPDATE grant_note SET notified = 1 WHERE id = ?1", [id])
            .map_err(internal)?;
    }
    Ok(json!({ "kind": kind, "subject": subject, "value": value, "notified": notified }))
}

pub fn register(d: &mut Daemon) {
    let _ = d.register_verb(
        "grant.full",
        Box::new(|ctx, req| {
            let repo = repo_key(std::path::Path::new(str_arg(&req.args, "repo")?));
            let conn = crate::db::open(&ctx.runs_dir.join(DB_FILE)).map_err(internal)?;
            let caller = cos_caller(&conn, req)?;
            drop(conn);
            change(ctx, &caller, "full-grant", &repo, "on", |tx, now| {
                tx.execute(
                    "INSERT INTO grant_full (repo, granted_ms, granted_by) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(repo) DO NOTHING",
                    params![repo, now, caller],
                )
                .map(|_| ())
            })
        }),
    );
    let _ = d.register_verb(
        "grant.tier",
        Box::new(|ctx, req| {
            let repo = repo_key(std::path::Path::new(str_arg(&req.args, "repo")?));
            let tier = tier_arg(&req.args)?;
            let conn = crate::db::open(&ctx.runs_dir.join(DB_FILE)).map_err(internal)?;
            let caller = cos_caller(&conn, req)?;
            drop(conn);
            change(
                ctx,
                &caller,
                "repo-tier",
                &repo,
                tier_word(tier),
                |tx, now| {
                    tx.execute(
                    "INSERT INTO repo_tier (repo, tier, set_ms, set_by) VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT(repo) DO UPDATE SET tier = ?2, set_ms = ?3, set_by = ?4",
                    params![repo, tier_word(tier), now, caller],
                )
                .map(|_| ())
                },
            )
        }),
    );
    let _ = d.register_verb(
        "grant.lane",
        Box::new(|ctx, req| {
            let lane = str_arg(&req.args, "lane")?.to_string();
            let tier = tier_arg(&req.args)?;
            let conn = crate::db::open(&ctx.runs_dir.join(DB_FILE)).map_err(internal)?;
            let caller = cos_caller(&conn, req)?;
            drop(conn);
            change(
                ctx,
                &caller,
                "plan-lane",
                &lane,
                tier_word(tier),
                |tx, now| {
                    tx.execute(
                    "INSERT INTO plan_lane (lane, tier, set_ms, set_by) VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT(lane) DO UPDATE SET tier = ?2, set_ms = ?3, set_by = ?4",
                    params![lane, tier_word(tier), now, caller],
                )
                .map(|_| ())
                },
            )
        }),
    );
}
