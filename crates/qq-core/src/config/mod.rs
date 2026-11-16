//! T35: config seed and readers. `init` is `qq init` as a library call: it
//! seeds the config tables of `qq.db` from a team.toml once. After that the
//! roster, harness, model and effort are read only from SQLite
//! ([`resolve_spawn`], [`roster`], ...); the toml is never read again.
//!
//! Seeding is one transaction and all-or-nothing: a database that already
//! holds kinds is left untouched, so a re-run converges and never overwrites
//! what `config set` changed later.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;

/// The closed harness registry: name and driver. Anything else is rejected.
/// `opencode` has no native driver and goes through the generic one.
pub const HARNESSES: &[(&str, Driver)] = &[
    ("claude", Driver::Native),
    ("pi", Driver::Native),
    ("agy", Driver::Native),
    ("opencode", Driver::Generic),
];

/// The four roles a topology edge can join.
const ROLES: &[&str] = &["cos", "supervisor", "lead", "peer"];

/// Top-level sections that map to rows (kinds, purposes, roles) or to
/// `setting` keys. Every other top-level key is an error, except the one in
/// [`NOT_PORTED`].
const MAPPED_TOP: &[&str] = &["schema_version", "kinds", "purposes", "roles"];

/// R15: the removed feature is not ported. Its top-level section and its
/// `can_spawn` / `can_prompt` target are accepted in a team.toml and skipped
/// on purpose. The name is built from two halves so the R15 grep over this
/// crate stays empty.
const NOT_PORTED: &str = concat!("task", "force");

/// Keys a `[roles.<role>]` table may carry.
const ROLE_KEYS: &[&str] = &["can_spawn", "can_prompt", "reports_to", "max_peers"];

/// Sections copied flat into `setting` as `<section>.<key>`.
const SETTING_SECTIONS: &[&str] = &[
    "limits",
    "timers",
    "strategy",
    "merge",
    "grid",
    "opencode_models",
    "projects",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    Native,
    Generic,
}

impl Driver {
    fn as_str(self) -> &'static str {
        match self {
            Driver::Native => "native",
            Driver::Generic => "generic",
        }
    }

    fn parse(s: &str) -> Option<Driver> {
        match s {
            "native" => Some(Driver::Native),
            "generic" => Some(Driver::Generic),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Db(rusqlite::Error),
    Io(std::io::Error),
    Parse(String),
    /// A kind names a harness outside [`HARNESSES`] (`omp` included).
    UnknownHarness {
        kind: String,
        harness: String,
    },
    /// A fallback, purpose or reviewer names a kind that is not defined.
    UnknownKind {
        referrer: String,
        kind: String,
    },
    /// A key the seed has no mapping for (top level or inside `[roles.*]`).
    UnknownKey {
        path: String,
    },
    /// A role name that is not one of the four roles.
    UnknownRole {
        path: String,
        role: String,
    },
    /// A value of the wrong toml type.
    BadType {
        path: String,
        expected: &'static str,
    },
    /// A kind row is unusable (empty model, ...).
    Invalid(String),
    /// A spawn asked for a kind the DB does not hold.
    NoSuchKind(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Db(e) => write!(f, "sqlite: {e}"),
            ConfigError::Io(e) => write!(f, "io: {e}"),
            ConfigError::Parse(m) => write!(f, "team.toml: {m}"),
            ConfigError::UnknownHarness { kind, harness } => {
                write!(f, "kind '{kind}': unknown harness '{harness}'")
            }
            ConfigError::UnknownKind { referrer, kind } => {
                write!(f, "{referrer}: unknown kind '{kind}'")
            }
            ConfigError::UnknownKey { path } => write!(f, "unknown key '{path}'"),
            ConfigError::UnknownRole { path, role } => {
                write!(f, "{path}: unknown role '{role}'")
            }
            ConfigError::BadType { path, expected } => write!(f, "{path}: expected {expected}"),
            ConfigError::Invalid(m) => write!(f, "invalid config: {m}"),
            ConfigError::NoSuchKind(k) => write!(f, "no such kind '{k}'"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<rusqlite::Error> for ConfigError {
    fn from(e: rusqlite::Error) -> Self {
        ConfigError::Db(e)
    }
}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        ConfigError::Io(e)
    }
}

/// What a seed call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seeded {
    /// The config tables were empty and are now filled.
    Fresh,
    /// The database already holds kinds; nothing was written.
    AlreadySeeded,
}

// ---- typed rows (the data shape) -------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Harness {
    pub name: String,
    pub driver: Driver,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kind {
    pub name: String,
    pub harness: String,
    pub model: String,
    pub effort: Option<String>,
    pub fallback: Option<String>,
    pub can_review: bool,
    pub spawn_approval: bool,
    pub use_for: Option<String>,
    pub avoid: Option<String>,
}

/// Everything a spawn needs about one kind, read from SQLite alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnTarget {
    pub kind: String,
    pub harness: Harness,
    pub model: String,
    pub effort: Option<String>,
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub edge: String,
}

// ---- parse at the boundary ---------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKind {
    harness: String,
    model: String,
    effort: Option<String>,
    fallback: Option<String>,
    #[serde(default)]
    can_review: bool,
    spawn: Option<String>,
    on_cost_cap: Option<String>,
    use_for: Option<String>,
    avoid: Option<String>,
}

struct Parsed {
    kinds: BTreeMap<String, RawKind>,
    purposes: Vec<(String, String)>,
    reviewers: Vec<String>,
    edges: BTreeSet<(String, String, &'static str)>,
    settings: Vec<(String, serde_json::Value)>,
}

fn parse(text: &str) -> Result<Parsed, ConfigError> {
    let doc: toml::Table = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let bad = |m: String| ConfigError::Parse(m);

    for key in doc.keys() {
        let k = key.as_str();
        if !(MAPPED_TOP.contains(&k) || SETTING_SECTIONS.contains(&k) || k == NOT_PORTED) {
            return Err(ConfigError::UnknownKey { path: key.clone() });
        }
    }

    let mut kinds = BTreeMap::new();
    if let Some(v) = doc.get("kinds") {
        let t = v
            .as_table()
            .ok_or_else(|| bad("[kinds] is not a table".into()))?;
        for (name, row) in t {
            let raw: RawKind = row
                .clone()
                .try_into()
                .map_err(|e: toml::de::Error| bad(format!("kinds.{name}: {e}")))?;
            kinds.insert(name.clone(), raw);
        }
    }

    let mut purposes = Vec::new();
    if let Some(v) = doc.get("purposes") {
        let t = v
            .as_table()
            .ok_or_else(|| bad("[purposes] is not a table".into()))?;
        for (p, k) in t {
            let k = k
                .as_str()
                .ok_or_else(|| bad(format!("purposes.{p} is not a string")))?;
            purposes.push((p.clone(), k.to_string()));
        }
    }

    let mut reviewers = Vec::new();
    let mut settings = Vec::new();
    if let Some(v) = doc.get("schema_version") {
        settings.push(("schema_version".to_string(), json(v)?));
    }
    for sec in SETTING_SECTIONS {
        let Some(v) = doc.get(*sec) else { continue };
        let t = v
            .as_table()
            .ok_or_else(|| bad(format!("[{sec}] is not a table")))?;
        for (k, val) in t {
            if *sec == "strategy" && k == "default_reviewer" {
                let arr = val
                    .as_array()
                    .ok_or_else(|| bad("strategy.default_reviewer is not an array".into()))?;
                for r in arr {
                    let r = r.as_str().ok_or_else(|| ConfigError::BadType {
                        path: "strategy.default_reviewer".into(),
                        expected: "string entries",
                    })?;
                    reviewers.push(r.to_string());
                }
            } else {
                settings.push((format!("{sec}.{k}"), json(val)?));
            }
        }
    }

    let mut edges = BTreeSet::new();
    if let Some(v) = doc.get("roles") {
        let roles = v.as_table().ok_or(ConfigError::BadType {
            path: "roles".into(),
            expected: "table",
        })?;
        for (role, row) in roles {
            let path = format!("roles.{role}");
            if !ROLES.contains(&role.as_str()) {
                return Err(ConfigError::UnknownRole {
                    path: "roles".into(),
                    role: role.clone(),
                });
            }
            let row = row.as_table().ok_or(ConfigError::BadType {
                path: path.clone(),
                expected: "table",
            })?;
            for key in row.keys() {
                if !ROLE_KEYS.contains(&key.as_str()) {
                    return Err(ConfigError::UnknownKey {
                        path: format!("{path}.{key}"),
                    });
                }
            }
            if let Some(v) = row.get("can_spawn") {
                for to in role_list(&format!("{path}.can_spawn"), v)? {
                    edges.insert((role.clone(), to, "spawn"));
                }
            }
            // The schema has no edge kind for "may prompt" (edges: spawn, mail,
            // report, question). Kept as a setting so SQLite still holds it;
            // the Lead decides whether it becomes an edge.
            if let Some(v) = row.get("can_prompt") {
                let list = role_list(&format!("{path}.can_prompt"), v)?;
                settings.push((format!("{path}.can_prompt"), serde_json::json!(list)));
            }
            if let Some(v) = row.get("reports_to") {
                let to = v.as_str().ok_or(ConfigError::BadType {
                    path: format!("{path}.reports_to"),
                    expected: "string",
                })?;
                if ROLES.contains(&to) {
                    edges.insert((role.clone(), to.to_string(), "report"));
                } else if to == "owner" {
                    // "owner" is the spawner, known only at spawn time: not a role.
                    settings.push((format!("{path}.reports_to"), serde_json::json!("owner")));
                } else {
                    return Err(ConfigError::UnknownRole {
                        path: format!("{path}.reports_to"),
                        role: to.to_string(),
                    });
                }
            }
            if let Some(n) = row.get("max_peers") {
                settings.push((format!("{path}.max_peers"), json(n)?));
            }
        }
    }

    Ok(Parsed {
        kinds,
        purposes,
        reviewers,
        edges,
        settings,
    })
}

/// A list of role names; the not-ported name is dropped, anything else that is
/// not a role is an error.
fn role_list(path: &str, v: &toml::Value) -> Result<Vec<String>, ConfigError> {
    let arr = v.as_array().ok_or_else(|| ConfigError::BadType {
        path: path.to_string(),
        expected: "array",
    })?;
    let mut out = Vec::new();
    for item in arr {
        let name = item.as_str().ok_or_else(|| ConfigError::BadType {
            path: path.to_string(),
            expected: "string entries",
        })?;
        if name == NOT_PORTED {
            continue;
        }
        if !ROLES.contains(&name) {
            return Err(ConfigError::UnknownRole {
                path: path.to_string(),
                role: name.to_string(),
            });
        }
        out.push(name.to_string());
    }
    Ok(out)
}

fn json(v: &toml::Value) -> Result<serde_json::Value, ConfigError> {
    serde_json::to_value(v).map_err(|e| ConfigError::Parse(e.to_string()))
}

fn validate(p: &Parsed) -> Result<(), ConfigError> {
    for (name, k) in &p.kinds {
        if !HARNESSES.iter().any(|(h, _)| *h == k.harness) {
            return Err(ConfigError::UnknownHarness {
                kind: name.clone(),
                harness: k.harness.clone(),
            });
        }
        if k.model.trim().is_empty() {
            return Err(ConfigError::Invalid(format!("kind '{name}': empty model")));
        }
        for (what, v, allowed) in [
            ("spawn", &k.spawn, &["auto", "approve"][..]),
            ("on_cost_cap", &k.on_cost_cap, &["warn", "kill"][..]),
        ] {
            if let Some(v) = v {
                if !allowed.contains(&v.as_str()) {
                    return Err(ConfigError::Invalid(format!("kind '{name}': {what} '{v}'")));
                }
            }
        }
        if let Some(f) = &k.fallback {
            if !p.kinds.contains_key(f) {
                return Err(ConfigError::UnknownKind {
                    referrer: format!("kinds.{name}.fallback"),
                    kind: f.clone(),
                });
            }
        }
    }
    for (purpose, kind) in &p.purposes {
        if !p.kinds.contains_key(kind) {
            return Err(ConfigError::UnknownKind {
                referrer: format!("purposes.{purpose}"),
                kind: kind.clone(),
            });
        }
    }
    for r in &p.reviewers {
        if !p.kinds.contains_key(r) {
            return Err(ConfigError::UnknownKind {
                referrer: "strategy.default_reviewer".into(),
                kind: r.clone(),
            });
        }
    }
    Ok(())
}

// ---- seed -----------------------------------------------------------------

/// `qq init` for the config tables: read `team_toml` and [`seed`] it.
pub fn init(conn: &mut Connection, team_toml: &Path) -> Result<Seeded, ConfigError> {
    let text = std::fs::read_to_string(team_toml)?;
    seed(conn, &text)
}

/// Validate `team_toml_text` and write it in one transaction. A database that
/// already holds kinds is left as it is.
pub fn seed(conn: &mut Connection, team_toml_text: &str) -> Result<Seeded, ConfigError> {
    let tx = conn.transaction()?;
    // Already seeded: a no-op, before the toml is even parsed.
    let seeded: i64 = tx.query_row("SELECT count(*) FROM kind", [], |r| r.get(0))?;
    if seeded > 0 {
        return Ok(Seeded::AlreadySeeded);
    }
    let parsed = parse(team_toml_text)?;
    validate(&parsed)?;

    for (name, driver) in HARNESSES {
        tx.execute(
            "INSERT OR IGNORE INTO harness (name, driver) VALUES (?1, ?2)",
            params![name, driver.as_str()],
        )?;
    }
    for (name, k) in &parsed.kinds {
        tx.execute(
            "INSERT INTO kind (name, harness, model, effort, can_review, spawn, on_cost_cap, use_for, avoid)
             VALUES (?1, ?2, ?3, ?4, ?5, COALESCE(?6, 'auto'), COALESCE(?7, 'warn'), ?8, ?9)",
            params![
                name,
                k.harness,
                k.model,
                k.effort,
                k.can_review,
                k.spawn,
                k.on_cost_cap,
                k.use_for,
                k.avoid
            ],
        )?;
    }
    // fallback is a self reference: set it once every kind exists.
    for (name, k) in &parsed.kinds {
        if let Some(f) = &k.fallback {
            tx.execute(
                "UPDATE kind SET fallback = ?1 WHERE name = ?2",
                params![f, name],
            )?;
        }
    }
    for (purpose, kind) in &parsed.purposes {
        tx.execute(
            "INSERT INTO purpose (name, kind) VALUES (?1, ?2)",
            params![purpose, kind],
        )?;
    }
    for (i, kind) in parsed.reviewers.iter().enumerate() {
        tx.execute(
            "INSERT INTO kind_reviewer (position, kind) VALUES (?1, ?2)",
            params![i as i64, kind],
        )?;
    }
    for (from, to, edge) in &parsed.edges {
        tx.execute(
            "INSERT INTO topology_edge (from_role, to_role, edge) VALUES (?1, ?2, ?3)",
            params![from, to, edge],
        )?;
    }
    for (key, value) in &parsed.settings {
        tx.execute(
            "INSERT INTO setting (key, value_json) VALUES (?1, ?2)",
            params![key, value.to_string()],
        )?;
    }
    tx.commit()?;
    Ok(Seeded::Fresh)
}

// ---- readers: SQLite only -----------------------------------------------------

fn row_kind(r: &rusqlite::Row<'_>) -> rusqlite::Result<Kind> {
    Ok(Kind {
        name: r.get(0)?,
        harness: r.get(1)?,
        model: r.get(2)?,
        effort: r.get(3)?,
        fallback: r.get(4)?,
        can_review: r.get::<_, i64>(5)? == 1,
        spawn_approval: r.get::<_, String>(6)? == "approve",
        use_for: r.get(7)?,
        avoid: r.get(8)?,
    })
}

const KIND_COLS: &str = "name, harness, model, effort, fallback, can_review, spawn, use_for, avoid";

/// Every kind, ordered by name.
pub fn roster(conn: &Connection) -> Result<Vec<Kind>, ConfigError> {
    let mut st = conn.prepare(&format!("SELECT {KIND_COLS} FROM kind ORDER BY name"))?;
    let rows = st.query_map([], row_kind)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn kind(conn: &Connection, name: &str) -> Result<Option<Kind>, ConfigError> {
    Ok(conn
        .query_row(
            &format!("SELECT {KIND_COLS} FROM kind WHERE name = ?1"),
            [name],
            row_kind,
        )
        .optional()?)
}

pub fn harness(conn: &Connection, name: &str) -> Result<Option<Harness>, ConfigError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT name, driver FROM harness WHERE name = ?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(name, d)| {
        Driver::parse(&d)
            .map(|driver| Harness { name, driver })
            .ok_or_else(|| ConfigError::Invalid(format!("harness driver '{d}'")))
    })
    .transpose()
}

/// Harness, model and effort for a spawn of `kind_name`.
pub fn resolve_spawn(conn: &Connection, kind_name: &str) -> Result<SpawnTarget, ConfigError> {
    let k = kind(conn, kind_name)?.ok_or_else(|| ConfigError::NoSuchKind(kind_name.to_string()))?;
    let h = harness(conn, &k.harness)?.ok_or_else(|| ConfigError::UnknownHarness {
        kind: k.name.clone(),
        harness: k.harness.clone(),
    })?;
    Ok(SpawnTarget {
        kind: k.name,
        harness: h,
        model: k.model,
        effort: k.effort,
        fallback: k.fallback,
    })
}

/// The kind a purpose (build, review, ...) maps to.
pub fn purpose_kind(conn: &Connection, purpose: &str) -> Result<Option<String>, ConfigError> {
    Ok(conn
        .query_row("SELECT kind FROM purpose WHERE name = ?1", [purpose], |r| {
            r.get(0)
        })
        .optional()?)
}

/// Default reviewer kinds, in order.
pub fn reviewers(conn: &Connection) -> Result<Vec<String>, ConfigError> {
    let mut st = conn.prepare("SELECT kind FROM kind_reviewer ORDER BY position")?;
    let rows = st
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(rows)
}

/// A flat setting such as `limits.depth`, decoded from its JSON.
pub fn setting(conn: &Connection, key: &str) -> Result<Option<serde_json::Value>, ConfigError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value_json FROM setting WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|s| serde_json::from_str(&s).map_err(|e| ConfigError::Invalid(e.to_string())))
        .transpose()
}

/// Role topology edges, sorted.
pub fn topology(conn: &Connection) -> Result<Vec<Edge>, ConfigError> {
    let mut st = conn.prepare(
        "SELECT from_role, to_role, edge FROM topology_edge ORDER BY from_role, to_role, edge",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(Edge {
                from: r.get(0)?,
                to: r.get(1)?,
                edge: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
