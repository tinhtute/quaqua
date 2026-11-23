//! T40: the daily-use verbs over the config tables: `quaqua config
//! get|set|import|validate` and `quaqua roster`. They open `runs/qq.db` directly
//! and go through the T35 readers and validators, so a kind written here is
//! the one the next spawn reads ([`resolve_spawn`]). Every write is one
//! transaction and converges: repeating a command leaves the same rows.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Map, Value};

use super::{parse, setting, validate, ConfigError, Kind, Parsed, RawKind, RawOverride};
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
        help: "config get [key] | set <key> <value> | import <team.toml> | validate [team.toml] | export | diff [team.toml]",
        run: config_cmd,
    })?;
    registry.register(Command {
        name: "roster",
        help: "list kinds with harness, model, use_for and avoid",
        run: roster_cmd,
    })?;
    daemon.register_verb(
        "config",
        Box::new(|ctx, req| serve(ctx, req, exec_config, true)),
    )?;
    daemon.register_verb(
        "roster",
        Box::new(|ctx, req| serve(ctx, req, |c, _, p| exec_roster(c, p), false)),
    )
}

/// The third argument is the profile the caller's `QUAQUA_PROFILE` named.
type Exec = fn(&mut Connection, &[String], Option<&str>) -> Result<String, CliError>;

/// `guarded`: every subcommand but `get` and `validate` is a write (so a new
/// one is guarded by default) and needs a caller that may run an orchestration
/// call: a Peer and a caller the daemon could not name never write a setting.
fn serve(ctx: &Ctx, req: &Request, exec: Exec, guarded: bool) -> Result<Value, RpcError> {
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
    if guarded
        && !matches!(
            argv.first().map(String::as_str),
            Some("get" | "validate" | "export" | "diff")
        )
    {
        let Some(caller) = req.caller.as_deref() else {
            return Err(RpcError::new(
                "refused",
                "a config write needs an identified caller",
            ));
        };
        let d = crate::authority::check_orchestration(
            &conn,
            caller,
            crate::authority::Op::ConfigWrite,
            crate::authority::Fence::Fenced,
        )
        .map_err(|e| RpcError::new("internal", e.0))?;
        if let crate::authority::Decision::Refused(r) = d {
            return Err(RpcError::new(
                "refused",
                format!("{} -> {}: {}", r.caller, r.target, r.reason),
            ));
        }
    }
    let profile = req
        .args
        .get("profile")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match exec(&mut conn, &argv, profile) {
        Ok(out) => Ok(json!({ "out": out })),
        Err(e) if e.code == 2 => Err(RpcError::new(BAD_REQUEST, e.message)),
        Err(e) => Err(RpcError::new("config_refused", e.message)),
    }
}

fn client(env: &Env, verb: &str, args: &[String]) -> Result<String, CliError> {
    let mut body = json!({ "argv": args });
    if let Some(p) = super::env_profile() {
        body["profile"] = json!(p);
    }
    let v = rpc_call(env, verb, body)?;
    Ok(v.get("out")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

/// Usage and the caller's own paths are settled here, before the call: the
/// daemon runs in another cwd, so a file argument goes over as an absolute path.
pub fn config_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    let need = |n: usize, m: &str| {
        if args.len() > n {
            Ok(())
        } else {
            Err(usage(m))
        }
    };
    let mut argv = args.to_vec();
    match args.first().map(String::as_str) {
        Some("get" | "validate" | "export" | "diff") => {}
        Some("set") => need(2, "set <key> <value>")?,
        Some("import") => need(1, "import <team.toml> [--yes]")?,
        _ => {
            return Err(usage(
                "get [key] | set <key> <value> | import <file> | validate [file] | export | diff [file]",
            ))
        }
    }
    if matches!(args[0].as_str(), "import" | "validate" | "diff") {
        if let Some(i) = (1..args.len()).find(|&i| args[i] != "--yes") {
            let abs = std::path::absolute(&args[i]).map_err(fail)?;
            argv[i] = abs.to_string_lossy().into_owned();
        }
    }
    client(env, "config", &argv)
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
    CliError::new(2, format!("usage: quaqua config {m}"))
}

fn fail(e: impl std::fmt::Display) -> CliError {
    CliError::new(1, e.to_string())
}

fn drift_lines(d: &[(String, String, String)], file: &str) -> String {
    d.iter()
        .map(|(k, stored, f)| format!("  ~ {k}: stored {stored}, {file} {f}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

// ---- commands ----------------------------------------------------------------

fn exec_config(
    conn: &mut Connection,
    args: &[String],
    _profile: Option<&str>,
) -> Result<String, CliError> {
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
            let yes = args[1..].iter().any(|s| s == "--yes");
            let paths: Vec<&String> = args[1..].iter().filter(|s| *s != "--yes").collect();
            let [path] = paths.as_slice() else {
                return Err(usage("import <team.toml> [--yes]"));
            };
            let text = std::fs::read_to_string(path).map_err(fail)?;
            let (n, changed) = import(conn, &text, yes).map_err(fail)?;
            let mut out = format!("imported {n} kinds");
            if !changed.is_empty() {
                out.push_str(&format!("\noverwrote:\n{}", drift_lines(&changed, "import writes")));
            }
            Ok(out)
        }
        Some("export") => super::export_toml(conn).map_err(fail),
        Some("diff") => {
            let text = match a(1) {
                Some(path) => std::fs::read_to_string(path).map_err(fail)?,
                None => super::ENGINE_DEFAULT.to_string(),
            };
            super::diff(conn, &text).map_err(fail)
        }
        Some("validate") => {
            match a(1) {
                Some(path) => {
                    let text = std::fs::read_to_string(path).map_err(fail)?;
                    check_text(conn, &text).map_err(fail)?;
                }
                None => check_db(conn).map_err(fail)?,
            }
            let mut out = "ok".to_string();
            // `config set` writes the DB only, so a value can drift from the
            // workspace team.toml, which the next `config import` would reset.
            if a(1).is_none() {
                let team = conn
                    .path()
                    .and_then(|p| std::path::Path::new(p).parent()?.parent().map(|r| r.join("team.toml")));
                if let Some(text) = team.and_then(|t| std::fs::read_to_string(t).ok()) {
                    let drift = super::conflicts(conn, &text).map_err(fail)?;
                    if !drift.is_empty() {
                        out.push_str(&format!(
                            "\ndrift: {} stored value(s) differ from team.toml (a plain import refuses them; --yes resets them to the file):\n{}",
                            drift.len(),
                            drift_lines(&drift, "team.toml")
                        ));
                    }
                }
            }
            Ok(out)
        }
        _ => Err(usage(
            "get [key] | set <key> <value> | import <file> | validate [file] | export | diff [file]",
        )),
    }
}

fn exec_roster(conn: &mut Connection, profile: Option<&str>) -> Result<String, CliError> {
    let kinds = super::roster_as(conn, profile).map_err(fail)?;
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
        for k in super::base_roster(conn).map_err(fail)? {
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
        let k = super::base_kind(conn, name)
            .map_err(fail)?
            .ok_or_else(|| no(key))?;
        let v = kind_json(&k, &on_cost_cap(conn, name).map_err(fail)?);
        return match field {
            None => Ok(v),
            Some(f) => v.get(f).cloned().ok_or_else(|| no(key)),
        };
    }
    if let Some(p) = key.strip_prefix("profile.") {
        let mut all = super::stored_profiles(conn).map_err(fail)?;
        let per_kind = all.remove(p).ok_or_else(|| no(key))?;
        return Ok(Value::Object(
            per_kind
                .into_iter()
                .map(|(k, o)| {
                    (
                        k,
                        json!({ "harness": o.harness, "model": o.model, "effort": o.effort }),
                    )
                })
                .collect(),
        ));
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
    super::write_harnesses(tx, std::iter::empty())
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
                if super::base_kind(&tx, name).map_err(fail)?.is_none() {
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
    } else if key == super::ACTIVE_PROFILE {
        set_active_profile(&tx, raw)?;
    } else if let Some(rest) = key.strip_prefix("profile.") {
        set_profile(&tx, rest, raw)?;
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
        set_setting(&tx, key, loose(raw))?;
    }
    tx.commit().map_err(fail)
}

/// `config set profile <name>` switches the active profile; `none` (or an
/// empty value) clears it. Only the setting row changes: no worker is touched.
fn set_active_profile(tx: &Transaction<'_>, raw: &str) -> Result<(), CliError> {
    let name = raw.trim();
    if name.is_empty() || name == "none" || name == "null" {
        tx.execute(
            "DELETE FROM setting WHERE key = ?1",
            [super::ACTIVE_PROFILE],
        )
        .map_err(fail)?;
        return Ok(());
    }
    let mut p = stored_parsed(tx).map_err(fail)?;
    p.settings.retain(|(k, _)| k != super::ACTIVE_PROFILE);
    p.settings
        .push((super::ACTIVE_PROFILE.to_string(), json!(name)));
    validate(&p).map_err(fail)?;
    put_setting(tx, super::ACTIVE_PROFILE, &json!(name))
}

/// `profile.<name>` takes a JSON object of kind -> override and replaces the
/// profile; `profile.<name>.<kind>` takes one override object (`null` removes
/// it). An override names any of harness, model, effort.
fn set_profile(tx: &Transaction<'_>, rest: &str, raw: &str) -> Result<(), CliError> {
    let (name, kind) = match rest.split_once('.') {
        Some((n, k)) => (n, Some(k)),
        None => (rest, None),
    };
    if name.is_empty() || name != name.to_lowercase() || kind.is_some_and(str::is_empty) {
        return Err(CliError::new(
            2,
            format!("profile.{rest}: names must be lowercase and non-empty"),
        ));
    }
    let parse_one = |v: Value| -> Result<RawOverride, CliError> {
        serde_json::from_value(v).map_err(|e| CliError::new(2, format!("profile.{rest}: {e}")))
    };
    let mut p = stored_parsed(tx).map_err(fail)?;
    match kind {
        None => {
            let Value::Object(obj) = loose(raw) else {
                return Err(CliError::new(
                    2,
                    format!("profile.{name}: value must be a JSON object of kind -> override"),
                ));
            };
            let mut per_kind = std::collections::BTreeMap::new();
            for (k, o) in obj {
                per_kind.insert(k, parse_one(o)?);
            }
            p.profiles.insert(name.to_string(), per_kind);
        }
        Some(k) => match loose(raw) {
            Value::Null => {
                p.profiles.entry(name.to_string()).or_default().remove(k);
            }
            o => {
                let o = parse_one(o)?;
                p.profiles
                    .entry(name.to_string())
                    .or_default()
                    .insert(k.to_string(), o);
            }
        },
    }
    validate(&p).map_err(fail)?;
    tx.execute("DELETE FROM profile_kind WHERE profile = ?1", [name])
        .map_err(fail)?;
    super::write_profiles(
        tx,
        &p.profiles
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(n, m)| (n.clone(), m.clone()))
            .collect(),
    )
    .map_err(fail)
}

fn put_setting(tx: &Transaction<'_>, key: &str, v: &Value) -> Result<(), CliError> {
    tx.execute(
        "INSERT INTO setting (key, value_json) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        params![key, v.to_string()],
    )
    .map_err(fail)?;
    Ok(())
}

/// Store a setting the way the toml seed does: role settings are typed, and a
/// role-valued `reports_to` is a topology edge, not a setting row.
fn set_setting(tx: &Transaction<'_>, key: &str, v: Value) -> Result<(), CliError> {
    let parts: Vec<&str> = key.split('.').collect();
    let ["roles", role, field] = parts.as_slice() else {
        return put_setting(tx, key, &v);
    };
    match *field {
        "can_prompt" => {
            let tv: toml::Value = serde_json::from_value(v).map_err(fail)?;
            let list = super::role_list(key, &tv).map_err(fail)?;
            put_setting(tx, key, &json!(list))
        }
        "max_peers" => match v.as_u64() {
            Some(_) => put_setting(tx, key, &v),
            None => Err(fail(ConfigError::BadType {
                path: key.to_string(),
                expected: "integer",
            })),
        },
        "reports_to" => {
            let to = v.as_str().ok_or_else(|| {
                fail(ConfigError::BadType {
                    path: key.to_string(),
                    expected: "string",
                })
            })?;
            let is_role = super::ROLES.contains(&to);
            if !is_role && to != "owner" {
                return Err(fail(ConfigError::UnknownRole {
                    path: key.to_string(),
                    role: to.to_string(),
                }));
            }
            tx.execute(
                "DELETE FROM topology_edge WHERE from_role = ?1 AND edge = 'report'",
                [role],
            )
            .map_err(fail)?;
            if is_role {
                tx.execute("DELETE FROM setting WHERE key = ?1", [key])
                    .map_err(fail)?;
                tx.execute(
                    "INSERT INTO topology_edge (from_role, to_role, edge) VALUES (?1, ?2, 'report')",
                    params![role, to],
                )
                .map_err(fail)?;
                Ok(())
            } else {
                put_setting(tx, key, &v)
            }
        }
        _ => put_setting(tx, key, &v),
    }
}

fn kind_name_ok(name: &str) -> bool {
    !name.is_empty() && name == name.to_lowercase()
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
    match super::base_kind(conn, kind).map_err(fail)? {
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
    if !kind_name_ok(name) {
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
        harnesses: super::stored_harnesses(tx).map_err(fail)?,
        kinds: Default::default(),
        profiles: super::stored_profiles(tx).map_err(fail)?,
        purposes: vec![],
        reviewers: vec![],
        edges: Default::default(),
        settings: vec![],
    };
    // The fallback may be a kind already stored: stand it in for the check.
    if let Some(f) = &fallback {
        if f != name {
            if let Some(k) = super::base_kind(tx, f).map_err(fail)? {
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
        harnesses: Default::default(),
        kinds: Default::default(),
        profiles: Default::default(),
        purposes: vec![],
        reviewers: vec![],
        edges: Default::default(),
        settings: vec![],
    };
    for k in super::base_roster(conn)? {
        if !p.kinds.contains_key(&k.name) {
            stored.kinds.insert(k.name.clone(), stand_in(&k));
        }
    }
    // Validate the file's kinds with the stored ones visible; the file's own
    // declarations win over stored harnesses of the same name.
    let file_harnesses = std::mem::take(&mut p.harnesses);
    p.harnesses = super::stored_harnesses(conn)?;
    p.harnesses.extend(file_harnesses);
    let file_profiles = std::mem::take(&mut p.profiles);
    p.profiles = super::stored_profiles(conn)?;
    for (name, per_kind) in file_profiles {
        p.profiles.entry(name).or_default().extend(per_kind);
    }
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
///
/// What the import changes is computed from what it writes: the export before
/// and after, inside the transaction. Without `yes` a changed or removed
/// stored key rolls the transaction back and refuses, naming every key.
fn import(
    conn: &mut Connection,
    text: &str,
    yes: bool,
) -> Result<(usize, Vec<(String, String, String)>), ConfigError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let p = parse_checked(&tx, text)?;
    let before = super::export::export_table(&tx)?;
    // Only the file's own kinds and harnesses are written; the stored
    // stand-ins are skipped.
    let doc: toml::Table = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let declared: Vec<String> = doc
        .get("harnesses")
        .and_then(|v| v.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    super::write_harnesses(
        &tx,
        p.harnesses.iter().filter(|(n, _)| declared.contains(n)),
    )?;
    let doc_profiles: Vec<String> = doc
        .get("profiles")
        .and_then(|v| v.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    let named: Vec<String> = doc
        .get("kinds")
        .and_then(|v| v.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    if let Some(bad) = named.iter().find(|n| !kind_name_ok(n)) {
        return Err(ConfigError::Invalid(format!(
            "kind name '{bad}' must be lowercase"
        )));
    }
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
    super::write_profiles(
        &tx,
        &p.profiles
            .iter()
            .filter(|(n, _)| doc_profiles.contains(n))
            .map(|(n, m)| (n.clone(), m.clone()))
            .collect(),
    )?;
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
    let changed = super::changed_keys(&before, &super::export::export_table(&tx)?);
    if !changed.is_empty() && !yes {
        drop(tx); // rolls back
        return Err(ConfigError::Invalid(format!(
            "import would overwrite {} stored value(s); nothing written, rerun with --yes to overwrite:\n{}",
            changed.len(),
            drift_lines(&changed, "import writes")
        )));
    }
    tx.commit()?;
    Ok((named.len(), changed))
}

/// The stored config as a [`Parsed`] (edges and the file-only settings are
/// left out; the active profile is kept), for [`validate`].
fn stored_parsed(conn: &Connection) -> Result<Parsed, ConfigError> {
    let mut p = Parsed {
        harnesses: super::stored_harnesses(conn)?,
        kinds: Default::default(),
        profiles: super::stored_profiles(conn)?,
        purposes: vec![],
        reviewers: super::reviewers(conn)?,
        edges: Default::default(),
        settings: vec![],
    };
    if let Some(v) = setting(conn, super::ACTIVE_PROFILE)? {
        p.settings.push((super::ACTIVE_PROFILE.to_string(), v));
    }
    for k in super::base_roster(conn)? {
        let mut raw = stand_in(&k);
        raw.fallback = k.fallback.clone();
        p.kinds.insert(k.name.clone(), raw);
    }
    let mut st = conn.prepare("SELECT name, kind FROM purpose ORDER BY name")?;
    p.purposes = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(p)
}

/// The stored config must satisfy the checks an import applies.
fn check_db(conn: &Connection) -> Result<(), ConfigError> {
    validate(&stored_parsed(conn)?)
}
