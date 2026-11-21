//! T52: `config export` and `config diff`. Export writes the stored base
//! config (the active profile never leaks into it) as a team.toml that
//! [`super::seed`] and `config import` read back to the same rows. Diff
//! compares the stored config with an engine default text and writes nothing.

use std::collections::BTreeMap;

use rusqlite::Connection;
use toml::{Table, Value};

use super::{
    base_roster, reviewers, setting, stored_harnesses, stored_profiles, topology, ConfigError,
    ACTIVE_PROFILE, SETTING_SECTIONS,
};

/// The team.toml the engine ships; `config diff` compares the stored config to it.
pub const ENGINE_DEFAULT: &str = include_str!("../../templates/workspace/team.toml");

fn strs(v: &[String]) -> Value {
    Value::Array(v.iter().cloned().map(Value::String).collect())
}

fn sub<'a>(t: &'a mut Table, key: &str) -> &'a mut Table {
    t.entry(key)
        .or_insert_with(|| Value::Table(Table::new()))
        .as_table_mut()
        .expect("sub-table")
}

fn bad(m: String) -> ConfigError {
    ConfigError::Invalid(format!("export: {m}"))
}

fn export_table(conn: &Connection) -> Result<Table, ConfigError> {
    let mut doc = Table::new();

    for (name, spec) in stored_harnesses(conn)? {
        let s = spec.ok_or_else(|| bad(format!("harness '{name}' has no declaration")))?;
        let t = sub(sub(&mut doc, "harnesses"), &name);
        t.insert("herdr_kind".into(), Value::String(s.herdr_kind));
        t.insert("args".into(), strs(&s.args));
        if !s.efforts.is_empty() {
            t.insert("efforts".into(), strs(&s.efforts));
        }
        if !s.stop_keys.is_empty() {
            t.insert("stop_keys".into(), strs(&s.stop_keys));
        }
        if let Some(e) = s.error_shape {
            t.insert("error_shape".into(), Value::String(e));
        }
    }

    for k in base_roster(conn)? {
        let cap: String = conn.query_row(
            "SELECT on_cost_cap FROM kind WHERE name = ?1",
            [&k.name],
            |r| r.get(0),
        )?;
        let t = sub(sub(&mut doc, "kinds"), &k.name);
        t.insert("harness".into(), Value::String(k.harness));
        t.insert("model".into(), Value::String(k.model));
        for (key, v) in [
            ("effort", k.effort),
            ("fallback", k.fallback),
            ("use_for", k.use_for),
            ("avoid", k.avoid),
        ] {
            if let Some(v) = v {
                t.insert(key.into(), Value::String(v));
            }
        }
        t.insert("can_review".into(), Value::Boolean(k.can_review));
        let spawn = if k.spawn_approval { "approve" } else { "auto" };
        t.insert("spawn".into(), Value::String(spawn.into()));
        t.insert("on_cost_cap".into(), Value::String(cap));
    }

    let mut st = conn.prepare("SELECT name, kind FROM purpose ORDER BY name")?;
    let purposes = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (p, k) in purposes {
        sub(&mut doc, "purposes").insert(p, Value::String(k));
    }

    let rs = reviewers(conn)?;
    if !rs.is_empty() {
        sub(&mut doc, "strategy").insert("default_reviewer".into(), strs(&rs));
    }

    for e in topology(conn)? {
        let t = sub(sub(&mut doc, "roles"), &e.from);
        match e.edge.as_str() {
            "spawn" => {
                t.entry("can_spawn")
                    .or_insert_with(|| Value::Array(vec![]))
                    .as_array_mut()
                    .expect("array")
                    .push(Value::String(e.to));
            }
            "report" => {
                if t.insert("reports_to".into(), Value::String(e.to)).is_some() {
                    return Err(bad(format!("role '{}' has two report edges", e.from)));
                }
            }
            other => {
                return Err(bad(format!(
                    "edge '{other}' ({} -> {}) has no team.toml form",
                    e.from, e.to
                )))
            }
        }
    }

    let mut st = conn.prepare("SELECT key FROM setting ORDER BY key")?;
    let keys = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for key in keys {
        let json = setting(conn, &key)?.ok_or_else(|| bad(format!("setting '{key}' vanished")))?;
        let value = Value::try_from(&json)
            .map_err(|e| bad(format!("setting '{key}' has no team.toml form: {e}")))?;
        let parts: Vec<&str> = key.split('.').collect();
        match parts.as_slice() {
            ["schema_version"] => {
                doc.insert(key.clone(), value);
            }
            [ACTIVE_PROFILE] => {
                if !value.is_str() {
                    return Err(bad("setting 'profile' is not a string".into()));
                }
                doc.insert(key.clone(), value);
            }
            ["roles", role, field] => {
                let t = sub(sub(&mut doc, "roles"), role);
                if t.insert((*field).to_string(), value).is_some() {
                    return Err(bad(format!("{key} is set twice (edge and setting)")));
                }
            }
            [sec, ..] if SETTING_SECTIONS.contains(sec) && parts.len() > 1 => {
                let rest = &key[sec.len() + 1..];
                sub(&mut doc, sec).insert(rest.to_string(), value);
            }
            _ => return Err(bad(format!("setting '{key}' has no team.toml form"))),
        }
    }

    for (name, per_kind) in stored_profiles(conn)? {
        let t = sub(sub(&mut doc, "profiles"), &name);
        for (kind, o) in per_kind {
            let row = sub(t, &kind);
            for (k, v) in [
                ("harness", o.harness),
                ("model", o.model),
                ("effort", o.effort),
            ] {
                if let Some(v) = v {
                    row.insert(k.into(), Value::String(v));
                }
            }
        }
    }
    Ok(doc)
}

/// The stored base config as team.toml text, whatever profile is active.
pub fn export_toml(conn: &Connection) -> Result<String, ConfigError> {
    toml::to_string(&export_table(conn)?).map_err(|e| bad(e.to_string()))
}

fn flatten(prefix: &str, t: &Table, out: &mut BTreeMap<String, String>) {
    for (k, v) in t {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            Value::Table(sub) => flatten(&path, sub, out),
            v => {
                out.insert(path, v.to_string());
            }
        }
    }
}

/// Compare the stored config with `default_text`: `+ key` only in the default,
/// `- key` only stored, `~ key` differs. Reads only; the default is seeded into
/// a scratch in-memory database, so both sides go through the same export.
pub fn diff(conn: &Connection, default_text: &str) -> Result<String, ConfigError> {
    let mut scratch = Connection::open_in_memory()?;
    scratch.pragma_update(None, "foreign_keys", "ON")?;
    crate::db::migrate::migrate(&mut scratch).map_err(|e| ConfigError::Invalid(e.to_string()))?;
    super::seed(&mut scratch, default_text)?;
    let (mut want, mut have) = (BTreeMap::new(), BTreeMap::new());
    flatten("", &export_table(&scratch)?, &mut want);
    flatten("", &export_table(conn)?, &mut have);

    let keys: std::collections::BTreeSet<&String> = want.keys().chain(have.keys()).collect();
    let mut lines = Vec::new();
    for k in keys {
        match (have.get(k), want.get(k)) {
            (None, Some(d)) => lines.push(format!("+ {k} = {d}")),
            (Some(s), None) => lines.push(format!("- {k} = {s}")),
            (Some(s), Some(d)) if s != d => lines.push(format!("~ {k}: stored {s} -> default {d}")),
            _ => {}
        }
    }
    Ok(if lines.is_empty() {
        "no differences".to_string()
    } else {
        lines.join("\n")
    })
}
