//! T35: config seed and readers. `init` is `quaqua init` as a library call: it
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
use serde::{Deserialize, Serialize};

mod export;
mod verbs;
pub use export::{diff, export_toml, ENGINE_DEFAULT};
pub use verbs::{config_cmd, roster_cmd, INSTALL};

/// The four roles a topology edge can join.
const ROLES: &[&str] = &["cos", "supervisor", "lead", "peer"];

/// Top-level sections that map to rows (kinds, purposes, roles) or to
/// `setting` keys. Every other top-level key is an error, except the one in
/// [`NOT_PORTED`].
const MAPPED_TOP: &[&str] = &[
    "schema_version",
    "harnesses",
    "kinds",
    "purposes",
    "roles",
    "profile",
    "profiles",
];

/// The `setting` key that names the active profile (`profile = "name"`).
const ACTIVE_PROFILE: &str = "profile";

/// The env var that overrides the stored active profile for this process.
const PROFILE_ENV: &str = "QUAQUA_PROFILE";

/// R15: the removed feature is not ported. Its top-level section and its
/// `can_spawn` / `can_prompt` target are accepted in a team.toml and skipped
/// on purpose. The next line is the one documented R15 grep exemption (plan,
/// `constraints`); the grep filter matches that line alone.
const NOT_PORTED: &str = "taskforce";

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

/// A harness a team.toml declares: everything the generic driver needs.
/// Stored in the `harness` row: `herdr_agent_kind` holds `herdr_kind`, and
/// `args_template` holds the rest as one JSON object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSpec {
    /// The herdr agent kind `herdr agent start --kind` is given.
    pub herdr_kind: String,
    /// Launch args after herdr's `--`; one element each, `{model}` required,
    /// `{effort}` optional.
    pub args: Vec<String>,
    /// Efforts a kind may name; empty: not checked.
    pub efforts: Vec<String>,
    /// The verified interrupt key sequence (never ctrl+c); empty: none.
    pub stop_keys: Vec<String>,
    /// Pane error-line shape (`claude`, `pi`, `agy`, `opencode`); none: try all.
    pub error_shape: Option<String>,
}

/// The JSON kept in `harness.args_template`.
#[derive(Debug, Serialize, Deserialize)]
struct StoredSpec {
    args: Vec<String>,
    #[serde(default)]
    efforts: Vec<String>,
    #[serde(default)]
    stop_keys: Vec<String>,
    error_shape: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Harness {
    pub name: String,
    pub driver: Driver,
    /// `Some` for a harness declared in config; natives and rows stored
    /// before declarations existed carry none.
    pub spec: Option<HarnessSpec>,
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

/// What a profile changes on one kind: only the fields it names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOverride {
    harness: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHarness {
    herdr_kind: String,
    args: Vec<String>,
    #[serde(default)]
    efforts: Vec<String>,
    #[serde(default)]
    stop_keys: Vec<String>,
    error_shape: Option<String>,
}

struct Parsed {
    /// Declared harnesses by name; `None` is a stored row with no spec.
    harnesses: BTreeMap<String, Option<HarnessSpec>>,
    kinds: BTreeMap<String, RawKind>,
    /// Profile name -> kind name -> override.
    profiles: BTreeMap<String, BTreeMap<String, RawOverride>>,
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

    let mut harnesses = BTreeMap::new();
    if let Some(v) = doc.get("harnesses") {
        let t = v
            .as_table()
            .ok_or_else(|| bad("[harnesses] is not a table".into()))?;
        for (name, row) in t {
            let raw: RawHarness = row
                .clone()
                .try_into()
                .map_err(|e: toml::de::Error| bad(format!("harnesses.{name}: {e}")))?;
            harnesses.insert(
                name.clone(),
                Some(HarnessSpec {
                    herdr_kind: raw.herdr_kind,
                    args: raw.args,
                    efforts: raw.efforts,
                    stop_keys: raw.stop_keys,
                    error_shape: raw.error_shape,
                }),
            );
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

    let mut profiles = BTreeMap::new();
    if let Some(v) = doc.get("profiles") {
        let t = v
            .as_table()
            .ok_or_else(|| bad("[profiles] is not a table".into()))?;
        for (pname, row) in t {
            let row = row
                .as_table()
                .ok_or_else(|| bad(format!("profiles.{pname} is not a table")))?;
            let mut per_kind = BTreeMap::new();
            for (kname, o) in row {
                let raw: RawOverride = o
                    .clone()
                    .try_into()
                    .map_err(|e: toml::de::Error| bad(format!("profiles.{pname}.{kname}: {e}")))?;
                per_kind.insert(kname.clone(), raw);
            }
            profiles.insert(pname.clone(), per_kind);
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
    if let Some(v) = doc.get(ACTIVE_PROFILE) {
        let name = v
            .as_str()
            .ok_or_else(|| bad("profile is not a string".into()))?;
        settings.push((ACTIVE_PROFILE.to_string(), serde_json::json!(name)));
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
        harnesses,
        kinds,
        profiles,
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

/// The placeholders an args template may carry. `{rules_file}` and `{prompt}`
/// (plan Q11, Q43) need the one-shot run, which is deferred (NG13).
fn check_template(name: &str, args: &[String]) -> Result<(), ConfigError> {
    let bad = |m: String| ConfigError::Invalid(format!("harnesses.{name}: {m}"));
    for t in args {
        let mut rest = t.as_str();
        while let Some(i) = rest.find('{') {
            let tail = &rest[i + 1..];
            let Some(j) = tail.find('}') else {
                return Err(bad(format!("unclosed '{{' in arg '{t}'")));
            };
            match &tail[..j] {
                "model" | "effort" => {}
                p @ ("rules_file" | "prompt") => {
                    return Err(bad(format!("placeholder {{{p}}} is not wired yet")))
                }
                p => return Err(bad(format!("unknown placeholder {{{p}}} in arg '{t}'"))),
            }
            rest = &tail[j + 1..];
        }
    }
    if !args.iter().any(|t| t.contains("{model}")) {
        return Err(bad("args must contain {model}".into()));
    }
    Ok(())
}

fn check_harness(name: &str, spec: &HarnessSpec) -> Result<(), ConfigError> {
    let bad = |m: &str| ConfigError::Invalid(format!("harnesses.{name}: {m}"));
    if name != name.to_lowercase() || name.trim().is_empty() {
        return Err(bad("name must be lowercase"));
    }
    if name == "omp" {
        return Err(ConfigError::UnknownHarness {
            kind: format!("harnesses.{name}"),
            harness: name.to_string(),
        });
    }
    if crate::driver::Registry::native().get(name).is_some() {
        return Err(bad("a native harness cannot be redeclared"));
    }
    if spec.herdr_kind.trim().is_empty() {
        return Err(bad("empty herdr_kind"));
    }
    check_template(name, &spec.args)?;
    if spec.stop_keys.iter().any(|k| k.contains("ctrl+c")) {
        return Err(bad("stop_keys must never send ctrl+c"));
    }
    if let Some(s) = &spec.error_shape {
        if crate::driver::ErrorShape::parse(s).is_none() {
            return Err(bad(&format!("unknown error_shape '{s}'")));
        }
    }
    Ok(())
}

fn validate(p: &Parsed) -> Result<(), ConfigError> {
    for (name, spec) in &p.harnesses {
        if let Some(spec) = spec {
            check_harness(name, spec)?;
        }
    }
    let natives = crate::driver::Registry::native();
    for (name, k) in &p.kinds {
        if natives.get(&k.harness).is_none() && !p.harnesses.contains_key(&k.harness) {
            return Err(ConfigError::UnknownHarness {
                kind: name.clone(),
                harness: k.harness.clone(),
            });
        }
        if let (Some(e), Some(Some(spec))) = (&k.effort, p.harnesses.get(&k.harness)) {
            if !spec.efforts.is_empty() && !spec.efforts.contains(e) {
                return Err(ConfigError::Invalid(format!(
                    "kind '{name}': effort '{e}' is not one of harnesses.{}.efforts {:?}",
                    k.harness, spec.efforts
                )));
            }
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
    for (profile, per_kind) in &p.profiles {
        if matches!(profile.as_str(), "none" | "null" | "") {
            return Err(ConfigError::Invalid(format!(
                "profile name '{profile}' is reserved (`config set profile none` clears the switch)"
            )));
        }
        for (kname, o) in per_kind {
            let at = format!("profiles.{profile}.{kname}");
            let Some(k) = p.kinds.get(kname) else {
                return Err(ConfigError::UnknownKind {
                    referrer: at,
                    kind: kname.clone(),
                });
            };
            let harness = o.harness.as_ref().unwrap_or(&k.harness);
            if natives.get(harness).is_none() && !p.harnesses.contains_key(harness) {
                return Err(ConfigError::UnknownHarness {
                    kind: at,
                    harness: harness.clone(),
                });
            }
            if o.model.as_deref().is_some_and(|m| m.trim().is_empty()) {
                return Err(ConfigError::Invalid(format!("{at}: empty model")));
            }
            if let (Some(e), Some(Some(spec))) = (
                o.effort.as_ref().or(k.effort.as_ref()),
                p.harnesses.get(harness),
            ) {
                if !spec.efforts.is_empty() && !spec.efforts.contains(e) {
                    return Err(ConfigError::Invalid(format!(
                        "{at}: effort '{e}' is not one of harnesses.{harness}.efforts {:?}",
                        spec.efforts
                    )));
                }
            }
        }
    }
    if let Some((_, v)) = p.settings.iter().find(|(k, _)| k == ACTIVE_PROFILE) {
        let name = v.as_str().unwrap_or_default();
        if !p.profiles.contains_key(name) {
            return Err(ConfigError::Invalid(format!("unknown profile '{name}'")));
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

/// `quaqua init` for the config tables: read `team_toml` and [`seed`] it.
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

    write_harnesses(&tx, parsed.harnesses.iter())?;
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
    write_profiles(&tx, &parsed.profiles)?;
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

/// Upserts every profile and its per-kind overrides (kinds must exist).
fn write_profiles(
    tx: &rusqlite::Transaction<'_>,
    profiles: &BTreeMap<String, BTreeMap<String, RawOverride>>,
) -> Result<(), ConfigError> {
    for (name, per_kind) in profiles {
        tx.execute("INSERT OR IGNORE INTO profile (name) VALUES (?1)", [name])?;
        for (kind, o) in per_kind {
            tx.execute(
                "INSERT INTO profile_kind (profile, kind, harness, model, effort)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(profile, kind) DO UPDATE SET harness = excluded.harness,
                   model = excluded.model, effort = excluded.effort",
                params![name, kind, o.harness, o.model, o.effort],
            )?;
        }
    }
    Ok(())
}

/// Every stored profile with its overrides.
fn stored_profiles(
    conn: &Connection,
) -> Result<BTreeMap<String, BTreeMap<String, RawOverride>>, ConfigError> {
    let mut out: BTreeMap<String, BTreeMap<String, RawOverride>> = BTreeMap::new();
    let mut st = conn.prepare("SELECT name FROM profile ORDER BY name")?;
    for n in st.query_map([], |r| r.get::<_, String>(0))? {
        out.entry(n?).or_default();
    }
    let mut st = conn.prepare("SELECT profile, kind, harness, model, effort FROM profile_kind")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            RawOverride {
                harness: r.get(2)?,
                model: r.get(3)?,
                effort: r.get(4)?,
            },
        ))
    })?;
    for row in rows {
        let (p, k, o) = row?;
        out.entry(p).or_default().insert(k, o);
    }
    Ok(out)
}

/// The profile spawns resolve through: `QUAQUA_PROFILE` of this process when
/// set, else the stored `profile` setting. Read at every call, so a switch
/// reaches the next spawn and no running worker (a worker's harness, model
/// and effort were fixed when it was launched).
pub fn active_profile(conn: &Connection) -> Result<Option<String>, ConfigError> {
    if let Some(v) = env_profile() {
        return Ok(Some(v));
    }
    Ok(setting(conn, ACTIVE_PROFILE)?.and_then(|v| v.as_str().map(String::from)))
}

/// This process's `QUAQUA_PROFILE`, if set.
pub fn env_profile() -> Option<String> {
    std::env::var(PROFILE_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The roster as profile `requested` makes it; with none, the stored
/// `profile` setting. The process env is not read: the daemon answers the
/// `roster` verb with the profile its client's env named.
pub fn roster_as(conn: &Connection, requested: Option<&str>) -> Result<Vec<Kind>, ConfigError> {
    let name = match requested {
        Some(r) => Some(r.to_string()),
        None => setting(conn, ACTIVE_PROFILE)?.and_then(|v| v.as_str().map(String::from)),
    };
    let o = overrides_for(conn, name)?;
    let mut kinds = base_roster(conn)?;
    for k in &mut kinds {
        if let Some(x) = o.get(&k.name) {
            apply(k, x);
        }
    }
    Ok(kinds)
}

/// The active profile's overrides by kind; none when no profile is active.
fn active_overrides(conn: &Connection) -> Result<BTreeMap<String, RawOverride>, ConfigError> {
    overrides_for(conn, active_profile(conn)?)
}

fn overrides_for(
    conn: &Connection,
    name: Option<String>,
) -> Result<BTreeMap<String, RawOverride>, ConfigError> {
    let Some(name) = name else {
        return Ok(BTreeMap::new());
    };
    let known: Option<i64> = conn
        .query_row("SELECT 1 FROM profile WHERE name = ?1", [&name], |r| {
            r.get(0)
        })
        .optional()?;
    if known.is_none() {
        return Err(ConfigError::Invalid(format!("unknown profile '{name}'")));
    }
    Ok(stored_profiles(conn)?.remove(&name).unwrap_or_default())
}

fn apply(k: &mut Kind, o: &RawOverride) {
    if let Some(h) = &o.harness {
        k.harness = h.clone();
    }
    if let Some(m) = &o.model {
        k.model = m.clone();
    }
    if let Some(e) = &o.effort {
        k.effort = Some(e.clone());
    }
}

/// Writes the native harness rows (kinds reference them) and every declared
/// harness, replacing a declaration of the same name.
fn write_harnesses<'a>(
    tx: &rusqlite::Transaction<'_>,
    declared: impl Iterator<Item = (&'a String, &'a Option<HarnessSpec>)>,
) -> Result<(), ConfigError> {
    for name in crate::driver::Registry::native().names() {
        tx.execute(
            "INSERT OR IGNORE INTO harness (name, driver) VALUES (?1, 'native')",
            [name],
        )?;
    }
    for (name, spec) in declared {
        let Some(spec) = spec else { continue };
        let stored = serde_json::to_string(&StoredSpec {
            args: spec.args.clone(),
            efforts: spec.efforts.clone(),
            stop_keys: spec.stop_keys.clone(),
            error_shape: spec.error_shape.clone(),
        })
        .map_err(|e| ConfigError::Invalid(e.to_string()))?;
        tx.execute(
            "INSERT INTO harness (name, driver, args_template, herdr_agent_kind)
             VALUES (?1, 'generic', ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET driver = 'generic',
               args_template = excluded.args_template,
               herdr_agent_kind = excluded.herdr_agent_kind",
            params![name, stored, spec.herdr_kind],
        )?;
    }
    Ok(())
}

/// Every stored generic harness: the names a kind may still use.
fn stored_harnesses(
    conn: &Connection,
) -> Result<BTreeMap<String, Option<HarnessSpec>>, ConfigError> {
    let mut st = conn.prepare("SELECT name FROM harness WHERE driver = 'generic'")?;
    let names = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = BTreeMap::new();
    for n in names {
        let spec = harness(conn, &n)?.and_then(|h| h.spec);
        out.insert(n, spec);
    }
    Ok(out)
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

/// Every kind as the active profile makes it (harness, model, effort),
/// ordered by name.
pub fn roster(conn: &Connection) -> Result<Vec<Kind>, ConfigError> {
    let o = active_overrides(conn)?;
    let mut kinds = base_roster(conn)?;
    for k in &mut kinds {
        if let Some(x) = o.get(&k.name) {
            apply(k, x);
        }
    }
    Ok(kinds)
}

/// The stored kind as the active profile makes it.
pub fn kind(conn: &Connection, name: &str) -> Result<Option<Kind>, ConfigError> {
    let Some(mut k) = base_kind(conn, name)? else {
        return Ok(None);
    };
    if let Some(x) = active_overrides(conn)?.get(name) {
        apply(&mut k, x);
    }
    Ok(Some(k))
}

/// Every stored kind, profile ignored: what export, diff and the edit verbs see.
fn base_roster(conn: &Connection) -> Result<Vec<Kind>, ConfigError> {
    let mut st = conn.prepare(&format!("SELECT {KIND_COLS} FROM kind ORDER BY name"))?;
    let rows = st.query_map([], row_kind)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn base_kind(conn: &Connection, name: &str) -> Result<Option<Kind>, ConfigError> {
    Ok(conn
        .query_row(
            &format!("SELECT {KIND_COLS} FROM kind WHERE name = ?1"),
            [name],
            row_kind,
        )
        .optional()?)
}

pub fn harness(conn: &Connection, name: &str) -> Result<Option<Harness>, ConfigError> {
    type Row = (String, String, Option<String>, Option<String>);
    let row: Option<Row> = conn
        .query_row(
            "SELECT name, driver, args_template, herdr_agent_kind FROM harness WHERE name = ?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    row.map(|(name, d, template, kind)| {
        let driver = Driver::parse(&d)
            .ok_or_else(|| ConfigError::Invalid(format!("harness driver '{d}'")))?;
        let spec = match (template, kind) {
            (Some(t), Some(herdr_kind)) => {
                let s: StoredSpec = serde_json::from_str(&t)
                    .map_err(|e| ConfigError::Invalid(format!("harness '{name}': {e}")))?;
                Some(HarnessSpec {
                    herdr_kind,
                    args: s.args,
                    efforts: s.efforts,
                    stop_keys: s.stop_keys,
                    error_shape: s.error_shape,
                })
            }
            _ => None,
        };
        Ok(Harness { name, driver, spec })
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

/// `notify.stall_sec`: seconds a worker may go without a change before it is
/// stalled. One key for the daemon sweep and `worker prompt`; `None` when
/// unset or not a finite number >= 0.
pub fn stall_sec(conn: &Connection) -> Option<f64> {
    setting(conn, "notify.stall_sec")
        .ok()
        .flatten()
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite() && *v >= 0.0)
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
