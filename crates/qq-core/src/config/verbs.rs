//! T40: the daily-use verbs over the config tables: `qq config
//! get|set|import|validate` and `qq roster`. They open `runs/qq.db` directly
//! and go through the T35 readers and validators, so a kind written here is
//! the one the next spawn reads ([`resolve_spawn`]). Every write is one
//! transaction and converges: repeating a command leaves the same rows.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Map, Value};

use super::{parse, setting, validate, ConfigError, Kind, Parsed, RawKind, HARNESSES};
use crate::cli::{rpc_call, CliError, Command, Env, Install, Registry};
use crate::daemon::{Ctx, Daemon};
use crate::db::path::DB_FILE;
use crate::rpc::{Request, RpcError, BAD_REQUEST};

/// The entry `cli::MODULES` lists (the Lead adds it at wave end).
pub const INSTALL: Install = install;

/// The daemon owns `qq.db` (R7): the verbs `config` and `roster` run there,
/// and the CLI commands of the same names are thin clients of them.
fn install(registry: &mut Registry, daemon: &mut Daemon) -> Result<(), String> {
    registry.register(Command {
        name: "config",
        help: "config get [key] | set <key> <value> | import <team.toml> | validate [team.toml]",
        run: config_cmd,
    })?;
    registry.register(Command {
        name: "roster",
        help: "list kinds with harness, model, use_for and avoid",
        run: roster_cmd,
    })?;
    daemon.register_verb("config", Box::new(|ctx, req| serve(ctx, req, exec_config)))?;
    daemon.register_verb(
        "roster",
        Box::new(|ctx, req| serve(ctx, req, |c, _| exec_roster(c))),
    )
}

type Exec = fn(&mut Connection, &[String]) -> Result<String, CliError>;

fn serve(ctx: &Ctx, req: &Request, exec: Exec) -> Result<Value, RpcError> {
    let argv: Vec<String> = req
        .args
        .get("argv")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut conn = crate::db::open(&ctx.runs_dir.join(DB_FILE))
        .map_err(|e| RpcError::new("internal", e.to_string()))?;
    match exec(&mut conn, &argv) {
        Ok(out) => Ok(json!({ "out": out })),
        Err(e) if e.code == 2 => Err(RpcError::new(BAD_REQUEST, e.message)),
        Err(e) => Err(RpcError::new("config_refused", e.message)),
    }
}

fn client(env: &Env, verb: &str, args: &[String]) -> Result<String, CliError> {
    let v = rpc_call(env, verb, json!({ "argv": args }))?;
    Ok(v.get("out")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

pub fn config_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    client(env, "config", args)
}

pub fn roster_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    client(env, "roster", args)
}

const FIELDS: &[&str] = &[
    "harness",
    "model",
    "effort",
    "fallback",
    "can_review",
    "spawn",
    "on_cost_cap",
    "use_for",
    "avoid",
];
/// Fields a `kind.<name>.<field>` set may clear with an empty value.
const NULLABLE: &[&str] = &["effort", "fallback", "use_for", "avoid"];

fn usage(m: &str) -> CliError {
    CliError::new(2, format!("usage: qq config {m}"))
}

fn fail(e: impl std::fmt::Display) -> CliError {
    CliError::new(1, e.to_string())
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

// ---- commands ----------------------------------------------------------------

fn exec_config(conn: &mut Connection, args: &[String]) -> Result<String, CliError> {
    let a = |i: usize| args.get(i).map(String::as_str);
    match a(0) {
        Some("get") => get(conn, a(1)).map(|v| match v {
            Value::String(s) => s,
            v => pretty(&v),
        }),
        Some("set") => {
            let (Some(key), Some(val)) = (a(1), a(2)) else {
                return Err(usage("set <key> <value>"));
            };
            set(conn, key, val)?;
            Ok(format!("set {key}"))
        }
        Some("import") => {
            let Some(path) = a(1) else {
                return Err(usage("import <team.toml>"));
            };
            let text = std::fs::read_to_string(path).map_err(fail)?;
            let n = import(conn, &text).map_err(fail)?;
            Ok(format!("imported {n} kinds"))
        }
        Some("validate") => {
            match a(1) {
                Some(path) => {
                    let text = std::fs::read_to_string(path).map_err(fail)?;
                    check_text(conn, &text).map_err(fail)?;
                }
                None => check_db(conn).map_err(fail)?,
            }
            Ok("ok".to_string())
        }
        _ => Err(usage(
            "get [key] | set <key> <value> | import <file> | validate [file]",
        )),
    }
}

fn exec_roster(conn: &mut Connection) -> Result<String, CliError> {
    let kinds = super::roster(conn).map_err(fail)?;
    Ok(kinds
        .iter()
        .map(|k| {
            format!(
                "{}\t{}/{}{}\tuse: {}\tavoid: {}",
                k.name,
                k.harness,
                k.model,
                k.effort
                    .as_ref()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default(),
                k.use_for.as_deref().unwrap_or("-"),
                k.avoid.as_deref().unwrap_or("-"),
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

// ---- get -----------------------------------------------------------------------

fn kind_json(k: &Kind, on_cost_cap: &str) -> Value {
    json!({
        "name": k.name, "harness": k.harness, "model": k.model,
        "effort": k.effort, "fallback": k.fallback, "can_review": k.can_review,
        "spawn": if k.spawn_approval { "approve" } else { "auto" },
        "on_cost_cap": on_cost_cap, "use_for": k.use_for, "avoid": k.avoid,
    })
}

fn on_cost_cap(conn: &Connection, kind: &str) -> Result<String, ConfigError> {
    Ok(conn.query_row(
        "SELECT on_cost_cap FROM kind WHERE name = ?1",
        [kind],
        |r| r.get(0),
    )?)
}

fn get(conn: &Connection, key: Option<&str>) -> Result<Value, CliError> {
    let no = |k: &str| CliError::new(1, format!("no such key '{k}'"));
    let Some(key) = key else {
        let mut kinds = Map::new();
        for k in super::roster(conn).map_err(fail)? {
            let cap = on_cost_cap(conn, &k.name).map_err(fail)?;
            kinds.insert(k.name.clone(), kind_json(&k, &cap));
        }
        let mut st = conn
            .prepare("SELECT key, value_json FROM setting ORDER BY key")
            .map_err(fail)?;
        let settings = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(fail)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(fail)?
            .into_iter()
            .map(|(k, v)| (k, serde_json::from_str(&v).unwrap_or(Value::Null)))
            .collect::<Map<_, _>>();
        let mut purposes = Map::new();
        let mut st = conn
            .prepare("SELECT name, kind FROM purpose ORDER BY name")
            .map_err(fail)?;
        for row in st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(fail)?
        {
            let (p, k) = row.map_err(fail)?;
            purposes.insert(p, Value::String(k));
        }
        return Ok(json!({
            "kinds": kinds, "purposes": purposes,
            "reviewers": super::reviewers(conn).map_err(fail)?,
            "settings": settings,
        }));
    };
    if key == "reviewers" || key == "strategy.default_reviewer" {
        return Ok(json!(super::reviewers(conn).map_err(fail)?));
    }
    if let Some(rest) = key.strip_prefix("kind.") {
        let (name, field) = match rest.split_once('.') {
            Some((n, f)) => (n, Some(f)),
            None => (rest, None),
        };
        let k = super::kind(conn, name)
            .map_err(fail)?
            .ok_or_else(|| no(key))?;
        let v = kind_json(&k, &on_cost_cap(conn, name).map_err(fail)?);
        return match field {
            None => Ok(v),
            Some(f) => v.get(f).cloned().ok_or_else(|| no(key)),
        };
    }
    if let Some(p) = key.strip_prefix("purpose.") {
        return super::purpose_kind(conn, p)
            .map_err(fail)?
            .map(Value::String)
            .ok_or_else(|| no(key));
    }
    setting(conn, key).map_err(fail)?.ok_or_else(|| no(key))
}

// ---- set -----------------------------------------------------------------------

/// A value from the command line: JSON when it parses, else a plain string.
fn loose(v: &str) -> Value {
    serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.to_string()))
}

fn ensure_harnesses(tx: &Transaction<'_>) -> Result<(), ConfigError> {
    for (name, driver) in HARNESSES {
        tx.execute(
            "INSERT OR IGNORE INTO harness (name, driver) VALUES (?1, ?2)",
            params![name, driver.as_str()],
        )?;
    }
    Ok(())
}

fn set(conn: &mut Connection, key: &str, raw: &str) -> Result<(), CliError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(fail)?;
    ensure_harnesses(&tx).map_err(fail)?;
    if let Some(rest) = key.strip_prefix("kind.") {
        match rest.split_once('.') {
            None => {
                let Value::Object(obj) = loose(raw) else {
                    return Err(CliError::new(
                        2,
                        format!("kind.{rest}: value must be a JSON object"),
                    ));
                };
                upsert_kind(&tx, rest, obj)?;
            }
            Some((name, field)) => {
                if !FIELDS.contains(&field) {
                    return Err(CliError::new(2, format!("unknown kind field '{field}'")));
                }
                let v = if NULLABLE.contains(&field) && (raw.is_empty() || raw == "null") {
                    Value::Null
                } else if field == "can_review" {
                    match raw {
                        "true" | "1" => json!(true),
                        "false" | "0" => json!(false),
                        _ => return Err(CliError::new(2, "can_review is true or false")),
                    }
                } else {
                    Value::String(raw.to_string())
                };
                if super::kind(&tx, name).map_err(fail)?.is_none() {
                    return Err(fail(ConfigError::NoSuchKind(name.to_string())));
                }
                upsert_kind(&tx, name, Map::from_iter([(field.to_string(), v)]))?;
            }
        }
    } else if let Some(p) = key.strip_prefix("purpose.") {
        require_kind(&tx, &format!("purpose.{p}"), raw)?;
        tx.execute(
            "INSERT INTO purpose (name, kind) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET kind = excluded.kind",
            params![p, raw],
        )
        .map_err(fail)?;
    } else if key == "reviewers" || key == "strategy.default_reviewer" {
        let Value::Array(items) = loose(raw) else {
            return Err(CliError::new(2, "reviewers must be a JSON array of kinds"));
        };
        let mut names = Vec::new();
        for i in &items {
            let n = i
                .as_str()
                .ok_or_else(|| CliError::new(2, "reviewers must be strings"))?;
            require_kind(&tx, "reviewers", n)?;
            names.push(n);
        }
        tx.execute("DELETE FROM kind_reviewer", []).map_err(fail)?;
        for (i, n) in names.iter().enumerate() {
            tx.execute(
                "INSERT INTO kind_reviewer (position, kind) VALUES (?1, ?2)",
                params![i as i64, n],
            )
            .map_err(fail)?;
        }
    } else {
        if !setting_key(key) {
            return Err(CliError::new(
                2,
                format!("unknown key '{key}': use kind.<name>[.field], purpose.<p>, reviewers or a team.toml setting"),
            ));
        }
        tx.execute(
            "INSERT INTO setting (key, value_json) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
            params![key, loose(raw).to_string()],
        )
        .map_err(fail)?;
    }
    tx.commit().map_err(fail)
}

/// A key the toml seed maps to a `setting` row: a flat section key, or a role
/// setting. Anything else would be stored and never read.
fn setting_key(key: &str) -> bool {
    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["schema_version"] => true,
        [sec, rest @ ..] if super::SETTING_SECTIONS.contains(sec) && !rest.is_empty() => {
            !rest.iter().any(|p| p.is_empty())
        }
        ["roles", role, k] => {
            super::ROLES.contains(role) && matches!(*k, "can_prompt" | "reports_to" | "max_peers")
        }
        _ => false,
    }
}

fn require_kind(conn: &Connection, referrer: &str, kind: &str) -> Result<(), CliError> {
    match super::kind(conn, kind).map_err(fail)? {
        Some(_) => Ok(()),
        None => Err(fail(ConfigError::UnknownKind {
            referrer: referrer.to_string(),
            kind: kind.to_string(),
        })),
    }
}

/// Merge `patch` onto the kind `name` (or a new one) and write it. The merged
/// row goes through the same checks as an import.
fn upsert_kind(
    tx: &Transaction<'_>,
    name: &str,
    patch: Map<String, Value>,
) -> Result<(), CliError> {
    if name.is_empty() || name != name.to_lowercase() {
        return Err(CliError::new(
            2,
            format!("kind name '{name}' must be lowercase"),
        ));
    }
    let mut row: Map<String, Value> = tx
        .query_row(
            "SELECT harness, model, effort, fallback, can_review, spawn, on_cost_cap, use_for, avoid
             FROM kind WHERE name = ?1",
            [name],
            |r| {
                let mut m = Map::new();
                for (i, f) in FIELDS.iter().enumerate() {
                    let v: Value = match *f {
                        "can_review" => json!(r.get::<_, i64>(i)? == 1),
                        _ => r
                            .get::<_, Option<String>>(i)?
                            .map_or(Value::Null, Value::String),
                    };
                    m.insert((*f).to_string(), v);
                }
                Ok(m)
            },
        )
        .optional()
        .map_err(fail)?
        .unwrap_or_default();
    for (f, v) in patch {
        if !FIELDS.contains(&f.as_str()) {
            return Err(CliError::new(2, format!("unknown kind field '{f}'")));
        }
        row.insert(f, v);
    }
    let s = |f: &str| -> Result<Option<String>, CliError> {
        match row.get(f) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(CliError::new(
                2,
                format!("kind.{name}.{f} must be a string"),
            )),
        }
    };
    let need = |f: &str| {
        s(f).and_then(|v| v.ok_or_else(|| CliError::new(2, format!("kind '{name}' needs '{f}'"))))
    };
    let (harness, model) = (need("harness")?, need("model")?);
    let (effort, fallback) = (s("effort")?, s("fallback")?);
    let (spawn, cap) = (s("spawn")?, s("on_cost_cap")?);
    let can_review = match row.get("can_review") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(CliError::new(2, "can_review is true or false")),
    };
    let raw = RawKind {
        harness: harness.clone(),
        model: model.clone(),
        effort: effort.clone(),
        fallback: fallback.clone(),
        can_review,
        spawn: spawn.clone(),
        on_cost_cap: cap.clone(),
        use_for: s("use_for")?,
        avoid: s("avoid")?,
    };
    let mut p = Parsed {
        kinds: Default::default(),
        purposes: vec![],
        reviewers: vec![],
        edges: Default::default(),
        settings: vec![],
    };
    // The fallback may be a kind already stored: stand it in for the check.
    if let Some(f) = &fallback {
        if f != name {
            if let Some(k) = super::kind(tx, f).map_err(fail)? {
                p.kinds.insert(f.clone(), stand_in(&k));
            }
        }
    }
    p.kinds.insert(name.to_string(), raw);
    validate(&p).map_err(fail)?;
    write_kind(tx, name, &p.kinds[name]).map_err(fail)?;
    if let Some(f) = &fallback {
        tx.execute(
            "UPDATE kind SET fallback = ?1 WHERE name = ?2",
            params![f, name],
        )
        .map_err(fail)?;
    } else {
        tx.execute("UPDATE kind SET fallback = NULL WHERE name = ?1", [name])
            .map_err(fail)?;
    }
    Ok(())
}

/// A stored kind as a [`RawKind`], so [`validate`] can see it.
fn stand_in(k: &Kind) -> RawKind {
    RawKind {
        harness: k.harness.clone(),
        model: k.model.clone(),
        effort: k.effort.clone(),
        fallback: None,
        can_review: k.can_review,
        spawn: None,
        on_cost_cap: None,
        use_for: k.use_for.clone(),
        avoid: k.avoid.clone(),
    }
}

/// Insert or update everything but `fallback` (a self reference, set after).
fn write_kind(tx: &Transaction<'_>, name: &str, k: &RawKind) -> Result<(), ConfigError> {
    tx.execute(
        "INSERT INTO kind (name, harness, model, effort, can_review, spawn, on_cost_cap, use_for, avoid)
         VALUES (?1, ?2, ?3, ?4, ?5, COALESCE(?6, 'auto'), COALESCE(?7, 'warn'), ?8, ?9)
         ON CONFLICT(name) DO UPDATE SET
           harness = excluded.harness, model = excluded.model, effort = excluded.effort,
           can_review = excluded.can_review, spawn = excluded.spawn,
           on_cost_cap = excluded.on_cost_cap, use_for = excluded.use_for,
           avoid = excluded.avoid",
        params![
            name, k.harness, k.model, k.effort, k.can_review, k.spawn, k.on_cost_cap,
            k.use_for, k.avoid
        ],
    )?;
    Ok(())
}

// ---- import / validate ---------------------------------------------------------

/// Parse `text` and check it against what the database already holds: a
/// fallback, purpose or reviewer may name a stored kind.
fn parse_checked(conn: &Connection, text: &str) -> Result<Parsed, ConfigError> {
    let mut p = parse(text)?;
    let mut stored = Parsed {
        kinds: Default::default(),
        purposes: vec![],
        reviewers: vec![],
        edges: Default::default(),
        settings: vec![],
    };
    for k in super::roster(conn)? {
        if !p.kinds.contains_key(&k.name) {
            stored.kinds.insert(k.name.clone(), stand_in(&k));
        }
    }
    // Validate the file's kinds with the stored ones visible.
    let file_kinds = std::mem::take(&mut p.kinds);
    p.kinds = stored.kinds;
    p.kinds.extend(file_kinds);
    validate(&p)?;
    Ok(p)
}

fn check_text(conn: &Connection, text: &str) -> Result<(), ConfigError> {
    parse_checked(conn, text).map(|_| ())
}

/// Merge a team.toml into the database in one transaction. Kinds, purposes and
/// settings are upserted, edges added; the reviewer list is replaced when the
/// file names one. Nothing the file omits is deleted. Returns the kinds named.
fn import(conn: &mut Connection, text: &str) -> Result<usize, ConfigError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let p = parse_checked(&tx, text)?;
    ensure_harnesses(&tx)?;
    // Only the file's own kinds are written; the stored stand-ins are skipped.
    let doc: toml::Table = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let named: Vec<String> = doc
        .get("kinds")
        .and_then(|v| v.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    for name in &named {
        write_kind(&tx, name, &p.kinds[name])?;
    }
    for name in &named {
        let f = p.kinds[name].fallback.as_deref();
        tx.execute(
            "UPDATE kind SET fallback = ?1 WHERE name = ?2",
            params![f, name],
        )?;
    }
    for (purpose, kind) in &p.purposes {
        tx.execute(
            "INSERT INTO purpose (name, kind) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET kind = excluded.kind",
            params![purpose, kind],
        )?;
    }
    if !p.reviewers.is_empty() {
        tx.execute("DELETE FROM kind_reviewer", [])?;
        for (i, kind) in p.reviewers.iter().enumerate() {
            tx.execute(
                "INSERT INTO kind_reviewer (position, kind) VALUES (?1, ?2)",
                params![i as i64, kind],
            )?;
        }
    }
    for (from, to, edge) in &p.edges {
        tx.execute(
            "INSERT OR IGNORE INTO topology_edge (from_role, to_role, edge) VALUES (?1, ?2, ?3)",
            params![from, to, edge],
        )?;
    }
    for (key, value) in &p.settings {
        tx.execute(
            "INSERT INTO setting (key, value_json) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
            params![key, value.to_string()],
        )?;
    }
    tx.commit()?;
    Ok(named.len())
}

/// The stored config must satisfy the checks an import applies.
fn check_db(conn: &Connection) -> Result<(), ConfigError> {
    let mut p = Parsed {
        kinds: Default::default(),
        purposes: vec![],
        reviewers: super::reviewers(conn)?,
        edges: Default::default(),
        settings: vec![],
    };
    for k in super::roster(conn)? {
        let mut raw = stand_in(&k);
        raw.fallback = k.fallback.clone();
        p.kinds.insert(k.name.clone(), raw);
    }
    let mut st = conn.prepare("SELECT name, kind FROM purpose ORDER BY name")?;
    p.purposes = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    validate(&p)
}
