//! `team.toml` typed model, project-override resolution, validation and key lookup.
//!
//! The shape mirrors docs/research/herdr-grid-rust-migration-plan.md § `team.toml`.
//! Resolution order is project override -> defaults, then validate. A validation
//! failure is rejected with one clear message (first failure wins).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 2;
/// AGENTS.md `## Rules`: the ceiling on `[limits] max_workers_per_orchestrator`.
pub const CAP_CEILING: i64 = 8;
/// AGENTS.md `## Roles (SLP)`: a project Lead spawns at most 4 Peers.
/// `[roles.lead] max_peers` overrides it; the CoS keeps `[limits]`.
pub const DEFAULT_LEAD_MAX_PEERS: i64 = 4;
/// `can_spawn` / `can_prompt` vocabulary (role classes).
pub const ROLE_CLASSES: [&str; 4] = ["cos", "lead", "peer", "taskforce"];
/// `[roles.<r>]` table keys.
pub const ROLE_NAMES: [&str; 3] = ["cos", "lead", "peer"];
/// `[kinds.<k>].spawn` values: free spawn vs explicit-Principal approval.
pub const SPAWN_AUTO: &str = "auto";
pub const SPAWN_APPROVE: &str = "approve";
pub const SPAWN_POLICIES: [&str; 2] = [SPAWN_AUTO, SPAWN_APPROVE];

fn default_spawn() -> String {
    SPAWN_AUTO.to_string()
}
/// `[kinds.<k>].on_cost_cap` values: what the watchdog does at 100% of the
/// cost budget (issue #48). `warn` only logs; `kill` interrupts the worker
/// once and stops waiting, keeping pane, worktree and journal.
pub const COST_CAP_WARN: &str = "warn";
pub const COST_CAP_KILL: &str = "kill";
pub const COST_CAP_ACTIONS: [&str; 2] = [COST_CAP_WARN, COST_CAP_KILL];

fn default_on_cost_cap() -> String {
    COST_CAP_WARN.to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LayoutRefConfig {
    #[serde(rename = "use")]
    pub use_layout: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub timers: Timers,
    #[serde(default)]
    pub roles: BTreeMap<String, Role>,
    #[serde(default)]
    pub strategy: Strategy,
    #[serde(default)]
    pub kinds: BTreeMap<String, Kind>,
    #[serde(default)]
    pub purposes: BTreeMap<String, String>,
    #[serde(
        default = "default_opencode_models",
        deserialize_with = "deserialize_opencode_models"
    )]
    pub opencode_models: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub taskforce: BTreeMap<String, Taskforce>,
    #[serde(default)]
    pub merge: Merge,
    #[serde(default)]
    pub grid: Grid,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectOverride>,
    #[serde(default)]
    pub layout: Option<LayoutRefConfig>,
    #[serde(default)]
    pub layouts: BTreeMap<String, crate::layout::Layout>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            schema_version: SCHEMA_VERSION,
            limits: Limits::default(),
            timers: Timers::default(),
            roles: BTreeMap::new(),
            strategy: Strategy::default(),
            kinds: BTreeMap::new(),
            purposes: BTreeMap::new(),
            opencode_models: default_opencode_models(),
            taskforce: BTreeMap::new(),
            merge: Merge::default(),
            grid: Grid::default(),
            projects: BTreeMap::new(),
            layout: None,
            layouts: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_depth")]
    pub depth: i64,
    pub max_workers_per_orchestrator: i64,
    #[serde(default)]
    pub weights: BTreeMap<String, i64>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            depth: default_depth(),
            max_workers_per_orchestrator: CAP_CEILING,
            weights: BTreeMap::new(),
        }
    }
}

fn default_depth() -> i64 {
    2
}

fn default_budget() -> String {
    "45m".to_string()
}
fn default_hard_cap() -> String {
    "120m".to_string()
}
fn default_stall() -> String {
    "10m".to_string()
}
fn default_idle() -> String {
    "2m".to_string()
}
fn default_nudge_at() -> f64 {
    0.8
}
fn default_cost_buffer() -> f64 {
    0.2
}
fn default_grace() -> f64 {
    0.25
}
fn default_pilot_budget() -> String {
    "30m".to_string()
}
fn default_mail_ack() -> String {
    "30m".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timers {
    #[serde(default = "default_budget", rename = "budget_default")]
    pub default_budget_duration: String,
    #[serde(default = "default_hard_cap")]
    pub hard_cap: String,
    #[serde(default = "default_stall")]
    pub stall: String,
    #[serde(default = "default_idle")]
    pub idle: String,
    #[serde(default = "default_nudge_at")]
    pub nudge_at: f64,
    #[serde(default = "default_grace", rename = "grace")]
    pub overrun_ratio: f64,
    #[serde(default = "default_pilot_budget")]
    pub pilot_budget: String,
    /// Overspend allowed past 100% of the cost budget before the watchdog
    /// escalates: 0.2 → ESCALATE at 120%.
    #[serde(default = "default_cost_buffer")]
    pub cost_buffer: f64,
    #[serde(default = "default_mail_ack")]
    pub mail_ack: String,
}

impl Default for Timers {
    fn default() -> Self {
        Timers {
            default_budget_duration: default_budget(),
            hard_cap: default_hard_cap(),
            stall: default_stall(),
            idle: default_idle(),
            nudge_at: default_nudge_at(),
            overrun_ratio: default_grace(),
            pilot_budget: default_pilot_budget(),
            cost_buffer: default_cost_buffer(),
            mail_ack: default_mail_ack(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Role {
    #[serde(default)]
    pub can_spawn: Vec<String>,
    #[serde(default)]
    pub can_prompt: Vec<String>,
    #[serde(default)]
    pub reports_to: Option<String>,
    /// `[roles.lead]` only: the most live worker slots (weights included) the
    /// owner `<slug>-lead` may hold. Absent → `DEFAULT_LEAD_MAX_PEERS`.
    #[serde(default)]
    pub max_peers: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Strategy {
    pub default_kind: String,
    #[serde(default)]
    pub default_reviewer: Vec<String>,
    #[serde(default = "default_differ")]
    pub builder_reviewer_differ: String,
    #[serde(default)]
    pub lead_kind: Option<String>,
    /// Kinds whose live task workers hold a build slot in the Lead gate
    /// (`worker/leadgate.rs`). Absent: every kind in `[kinds]` counts.
    #[serde(default)]
    pub build_kinds: Option<Vec<String>>,
}

impl Default for Strategy {
    fn default() -> Self {
        Strategy {
            default_kind: String::new(),
            default_reviewer: Vec::new(),
            builder_reviewer_differ: default_differ(),
            lead_kind: None,
            build_kinds: None,
        }
    }
}

fn default_differ() -> String {
    "harness".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Kind {
    pub harness: String,
    pub model: String,
    #[serde(default)]
    pub effort: Option<String>,
    /// Optional one-hop fallback kind, taken only when the agent fails to start.
    #[serde(default)]
    pub fallback: Option<String>,
    #[serde(default)]
    pub can_review: bool,
    /// "auto" (spawn freely) or "approve" (require --approve-kind <kind>).
    #[serde(default = "default_spawn")]
    pub spawn: String,
    /// "warn" (log only) or "kill" (interrupt at 100% of the cost budget).
    #[serde(default = "default_on_cost_cap")]
    pub on_cost_cap: String,
}

/// The effort values a harness accepts (`crate::harness::Harness::effort_allowed`);
/// false for an unknown harness.
pub fn effort_allowed(harness: &str, effort: &str) -> bool {
    crate::harness::Harness::parse(harness).is_some_and(|h| h.effort_allowed(effort))
}

fn default_opencode_models() -> BTreeMap<String, Vec<String>> {
    let mut m = BTreeMap::new();
    m.insert(
        "deepseek-v4.1-flash".to_string(),
        vec!["low".to_string(), "high".to_string(), "max".to_string()],
    );
    m
}

fn deserialize_opencode_models<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let user_models = BTreeMap::<String, Vec<String>>::deserialize(deserializer)?;
    let mut models = default_opencode_models();
    models.extend(user_models);
    Ok(models)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelVariants(pub Vec<String>);

impl ModelVariants {
    pub fn contains(&self, effort: &str) -> bool {
        self.0.iter().any(|s| s == effort)
    }

    pub fn join(&self, sep: &str) -> String {
        self.0.join(sep)
    }
}

impl std::ops::Deref for ModelVariants {
    type Target = [String];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Config {
    /// The kinds whose live task workers hold a build slot in the Lead gate:
    /// `strategy.build_kinds`, or — when the key is absent, as in a `team.toml`
    /// written before it existed — every configured kind.
    pub fn build_kinds(&self) -> Vec<&str> {
        match &self.strategy.build_kinds {
            Some(kinds) => kinds.iter().map(String::as_str).collect(),
            None => self.kinds.keys().map(String::as_str).collect(),
        }
    }

    pub fn opencode_model_variants(&self, model: &str) -> Option<ModelVariants> {
        let model_id = model.rsplit('/').next().unwrap_or(model);
        self.opencode_models
            .get(model)
            .or_else(|| self.opencode_models.get(model_id))
            .map(|v| ModelVariants(v.clone()))
    }
}

/// Resolve a kind from a purpose name in config.
pub fn resolve_purpose<'a>(cfg: &'a Config, purpose: &str) -> Result<&'a str, String> {
    let kind = cfg.purposes.get(purpose).ok_or_else(|| {
        format!("unknown purpose '{purpose}'")
    })?;
    if !cfg.kinds.contains_key(kind) {
        return Err(format!("purpose '{purpose}': unknown kind '{kind}'"));
    }
    Ok(kind.as_str())
}
/// Resolve the kind of a worker from its name by finding the longest key K in
/// `cfg.kinds` where `name` starts with `format!("{K}-")`.
/// Returns `None` if `name` ends with "-lead" or matches no configured kind prefix.
pub fn kind_of_worker<'a>(cfg: &'a Config, name: &str) -> Option<&'a str> {
    if name.ends_with("-lead") {
        return None;
    }
    let mut best: Option<&str> = None;
    for k in cfg.kinds.keys() {
        let prefix = format!("{k}-");
        if name.starts_with(&prefix) {
            match best {
                Some(prev) if k.len() <= prev.len() => {}
                _ => best = Some(k.as_str()),
            }
        }
    }
    best
}


/// The variant vocabulary allowed for known opencode models.
/// Key is the last path segment of the model ID (e.g. "deepseek-v4.1-flash").
pub fn opencode_model_variants(model: &str) -> Option<ModelVariants> {
    let cfg = load_team_config().ok()?;
    cfg.opencode_model_variants(model)
}

/// Check if an effort/variant value is allowed for a specific harness and model.
/// opencode models can restrict variant vocabulary (e.g. deepseek-v4.1-flash takes low, high, max).
pub fn model_effort_allowed(harness: &str, model: &str, effort: &str) -> bool {
    if crate::harness::Harness::parse(harness) != Some(crate::harness::Harness::Opencode) {
        return true;
    }
    match opencode_model_variants(model) {
        Some(allowed) => allowed.contains(effort),
        None => false,
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Taskforce {
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Merge {
    pub integ_branch: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Grid {
    pub workers_tab: String,
    #[serde(default)]
    pub deferred_statuses: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ProjectOverride {
    pub max_workers_per_orchestrator: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
}

/// Parse a duration string like "45m", "120s", "2h" into seconds.
pub fn parse_duration(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration string".to_string());
    }
    let split_idx = s.find(|c: char| !c.is_ascii_digit());
    let (num_str, unit_str) = match split_idx {
        Some(idx) => (&s[..idx], &s[idx..]),
        None => return Err(format!("missing unit in duration '{s}' (e.g. 45m, 120s)")),
    };
    if num_str.is_empty() {
        return Err(format!("invalid duration '{s}': missing number"));
    }
    let n: u64 = num_str
        .parse()
        .map_err(|_| format!("invalid duration number in '{s}'"))?;
    let multiplier: u64 = match unit_str {
        "s" | "sec" | "secs" => 1,
        "m" | "min" | "mins" => 60,
        "h" | "hr" | "hrs" => 3600,
        _ => return Err(format!("unknown duration unit '{unit_str}' in '{s}'")),
    };
    let total = n
        .checked_mul(multiplier)
        .ok_or_else(|| format!("duration '{s}' overflowed"))?;
    if total == 0 {
        return Err(format!("duration '{s}' must be greater than zero"));
    }
    Ok(total)
}

/// Read and parse a team.toml. Parse errors name the file.
pub fn load(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}
/// The team.toml every reader resolves, in the one order: `$QUAQUA_TEAM_TOML`,
/// else `./team.toml`, else `<workspace_root>/team.toml`. `None` means the
/// embedded default. A `$QUAQUA_TEAM_TOML` path is returned even when it does
/// not exist, so an explicit override fails loudly instead of silently falling
/// back to another file.
pub fn team_config_path() -> Option<PathBuf> {
    if let Ok(path_str) = std::env::var("QUAQUA_TEAM_TOML") {
        return Some(PathBuf::from(path_str));
    }
    if Path::new("team.toml").is_file() {
        return Some(PathBuf::from("team.toml"));
    }
    let home_team = crate::workspace::workspace_root().join("team.toml");
    if home_team.is_file() {
        return Some(home_team);
    }
    None
}

/// The `team.toml` baked into the binary: the config used when no file is found.
///
/// Callers that must not read process-global env (tests) pass this explicitly.
pub fn embedded_team_config() -> Result<Config, (i32, String)> {
    const DEFAULT_TOML: &str = include_str!("../../../templates/workspace/team.toml");
    toml::from_str(DEFAULT_TOML).map_err(|e| {
        (
            1,
            format!("error: failed to parse default team.toml: {e}\n"),
        )
    })
}

/// Load team config from environment, relative path, workspace root, or embedded fallback.
pub fn load_team_config() -> Result<Config, (i32, String)> {
    match team_config_path() {
        Some(path) => load(&path).map_err(|e| (1, format!("error: {e}\n"))),
        None => embedded_team_config(),
    }
}

/// Resolves the effective layout for a project (or global):
/// Resolution order is project override -> global [layout] -> synthesized legacy layout.
///
/// - If `project` is `Some(slug)` and `cfg.projects` has a `[projects.<slug>]` entry with `layout = Some(name)`,
///   that layout name takes precedence.
/// - Otherwise, falls back to global `cfg.layout` (`[layout] use = "<name>"`).
/// - If `cfg.layout` is None (or schema_version < 3), synthesizes the legacy SLP layout.
pub fn resolve_project_layout(
    cfg: &Config,
    project: Option<&str>,
) -> Result<crate::layout::Layout, String> {
    let mut effective_cfg = cfg.clone();
    if let Some(slug) = project {
        if let Some(ov) = cfg.projects.get(slug) {
            if let Some(cap) = ov.max_workers_per_orchestrator {
                effective_cfg.limits.max_workers_per_orchestrator = cap;
            }
            if cfg.schema_version >= 3 {
                if let Some(layout_name) = &ov.layout {
                    effective_cfg.layout = Some(LayoutRefConfig {
                        use_layout: layout_name.clone(),
                    });
                }
            }
        }
    }
    crate::layout::resolve_layout(&effective_cfg)
}

/// Apply a project override over the defaults. Unknown project is a hard error.
/// Updates `cfg.limits.max_workers_per_orchestrator`, and if the project explicitly
/// configures `layout = Some(...)` under `schema_version >= 3`, updates `cfg.layout`.
/// Never invents `cfg.layout` when the project did not specify one.
pub fn resolve(mut cfg: Config, project: Option<&str>) -> Result<Config, String> {
    if let Some(slug) = project {
        let ov = cfg
            .projects
            .get(slug)
            .ok_or_else(|| format!("unknown project '{slug}'"))?
            .clone();
        if let Some(cap) = ov.max_workers_per_orchestrator {
            cfg.limits.max_workers_per_orchestrator = cap;
        }
        if cfg.schema_version >= 3 {
            if let Some(layout_key) = ov.layout {
                let _ = resolve_project_layout(&cfg, Some(slug))?;
                cfg.layout = Some(LayoutRefConfig {
                    use_layout: layout_key,
                });
            }
        }
    }
    Ok(cfg)
}

/// Derives the static taskforce weight for a role kind from config weights, layout role members,
/// or legacy defaults:
/// 1. `[limits.weights.<role>]` in config if explicitly configured
/// 2. Layout role `members` count from the layout
/// 3. Per-kind legacy default (dual-lens: 2, council: 3, else 1)
pub fn taskforce_weight_from_layout(
    cfg: &Config,
    kind: &str,
    layout: &crate::layout::Layout,
) -> i64 {
    if let Some(&w) = cfg.limits.weights.get(kind) {
        return w;
    }
    if let Some(role) = layout.roles.get(kind) {
        if !role.members.is_empty() {
            return role.members.len() as i64;
        }
    }
    match kind {
        "dual-lens" => 2,
        "council" => 3,
        _ => 1,
    }
}

/// Statically validates that for every role with members in the layout,
/// the effective weight (derived via `taskforce_weight_from_layout`) is less than or equal
/// to the effective cap of every spawner role that can spawn it.
pub fn validate_layout_caps(layout: &crate::layout::Layout, cfg: &Config) -> Result<(), String> {
    for (role_name, role) in &layout.roles {
        if role.members.is_empty() {
            continue;
        }
        let effective_weight = taskforce_weight_from_layout(cfg, role_name, layout);
        for (spawner_name, spawner_role) in &layout.roles {
            let can_spawn = spawner_role.can_spawn.iter().any(|target| {
                target == role_name
                    || (role.class == crate::layout::RoleClass::Taskforce && target == "taskforce")
            });
            if !can_spawn {
                continue;
            }
            let (spawner_cap, knob) = match spawner_role.class {
                crate::layout::RoleClass::Cos => (
                    cfg.limits.max_workers_per_orchestrator,
                    "[limits] max_workers_per_orchestrator",
                ),
                crate::layout::RoleClass::Lead => {
                    let lead_max = cfg
                        .roles
                        .get("lead")
                        .and_then(|r| r.max_peers)
                        .unwrap_or(DEFAULT_LEAD_MAX_PEERS);
                    (
                        lead_max.min(cfg.limits.max_workers_per_orchestrator),
                        "[roles.lead] max_peers",
                    )
                }
                crate::layout::RoleClass::Peer | crate::layout::RoleClass::Taskforce => {
                    let cap = spawner_role.max_children.unwrap_or(i64::MAX);
                    (
                        cap,
                        "layout role max_children (no config knob: peer spawns are gated by the layout only)",
                    )
                }
            };
            if effective_weight > spawner_cap {
                return Err(format!(
                    "layout '{}' role '{role_name}' member count N={effective_weight} exceeds spawner '{spawner_name}' effective cap {spawner_cap} (knob to raise: {knob})",
                    layout.name
                ));
            }
        }
    }
    Ok(())
}

/// Validate the resolved config. team.toml is the only source of
/// harness/model/effort, so every kind is checked here; models.md is prose.
pub fn validate(cfg: &Config) -> Result<(), String> {
    if cfg.schema_version > 3 {
        return Err(format!(
            "unsupported schema_version {} (max 3; run `quaqua config migrate` or upgrade quaqua)",
            cfg.schema_version
        ));
    }
    if cfg.schema_version < 2 {
        return Err(format!(
            "unsupported schema_version {} (want 2 or 3)",
            cfg.schema_version
        ));
    }

    if cfg.schema_version < 3 {
        if cfg.layout.is_some() || !cfg.layouts.is_empty() {
            return Err(format!(
                "schema_version {} does not support [layout] (requires schema_version = 3)",
                cfg.schema_version
            ));
        }
        for (slug, ov) in &cfg.projects {
            if ov.layout.is_some() {
                return Err(format!(
                    "project '{slug}': project layout override requires schema_version = 3"
                ));
            }
        }
    }

    for (slug, ov) in &cfg.projects {
        if let Some(layout_name) = &ov.layout {
            if crate::layout::builtin::get(layout_name).is_none()
                && !cfg.layouts.contains_key(layout_name)
            {
                return Err(format!(
                    "project '{slug}' references unknown layout '{layout_name}'"
                ));
            }
        }
    }

    if cfg.schema_version == 3 {
        for name in cfg.layouts.keys() {
            if crate::layout::builtin::get(name).is_some() {
                return Err(format!("user layout '{name}' collides with built-in layout"));
            }
        }
        if let Some(layout_ref) = &cfg.layout {
            let name = &layout_ref.use_layout;
            if crate::layout::builtin::get(name).is_none() && !cfg.layouts.contains_key(name) {
                return Err(format!("unknown layout '{name}'"));
            }
            if let Some(bl) = crate::layout::builtin::get(name) {
                crate::layout::validate_layout_with_config(&bl, cfg)?;
                validate_layout_caps(&bl, cfg)?;
            }
        }
        for (name, layout) in &cfg.layouts {
            let mut l = layout.clone();
            if l.name.is_empty() {
                l.name = name.clone();
            }
            crate::layout::validate_user_layout_with_config(&l, cfg)?;
            validate_layout_caps(&l, cfg)?;
        }

        for (slug, ov) in &cfg.projects {
            let proj_cfg = resolve(cfg.clone(), Some(slug))?;
            if let Some(layout_name) = &ov.layout {
                if let Some(bl) = crate::layout::builtin::get(layout_name) {
                    crate::layout::validate_layout_with_config(&bl, &proj_cfg)?;
                    validate_layout_caps(&bl, &proj_cfg)?;
                } else if let Some(custom) = cfg.layouts.get(layout_name) {
                    let mut l = custom.clone();
                    if l.name.is_empty() {
                        l.name = layout_name.clone();
                    }
                    crate::layout::validate_user_layout_with_config(&l, &proj_cfg)?;
                    validate_layout_caps(&l, &proj_cfg)?;
                }
            } else if let Some(layout_ref) = &cfg.layout {
                let name = &layout_ref.use_layout;
                if let Some(bl) = crate::layout::builtin::get(name) {
                    validate_layout_caps(&bl, &proj_cfg)?;
                } else if let Some(custom) = cfg.layouts.get(name) {
                    let mut l = custom.clone();
                    if l.name.is_empty() {
                        l.name = name.clone();
                    }
                    validate_layout_caps(&l, &proj_cfg)?;
                }
            }
        }
    }

    let cap = cfg.limits.max_workers_per_orchestrator;
    if cap < 1 || cap > CAP_CEILING {
        return Err(format!("workers over the cap: {cap} (max {CAP_CEILING})"));
    }
    for (name, w) in &cfg.limits.weights {
        if *w < 1 || *w > cap {
            return Err(format!("weight {name}={w} exceeds cap {cap}"));
        }
    }
    for (slug, ov) in &cfg.projects {
        if let Some(p) = ov.max_workers_per_orchestrator {
            if p < 1 {
                return Err(format!("project {slug} cap {p} is below 1"));
            }
            if p > cap {
                return Err(format!("project {slug} cap {p} exceeds global cap {cap}"));
            }
        }
    }
    parse_duration(&cfg.timers.default_budget_duration)
        .map_err(|e| format!("timers.{}: {e}", "budget_default"))?;
    parse_duration(&cfg.timers.hard_cap).map_err(|e| format!("timers.hard_cap: {e}"))?;
    parse_duration(&cfg.timers.stall).map_err(|e| format!("timers.stall: {e}"))?;
    parse_duration(&cfg.timers.idle).map_err(|e| format!("timers.idle: {e}"))?;
    parse_duration(&cfg.timers.pilot_budget).map_err(|e| format!("timers.pilot_budget: {e}"))?;
    parse_duration(&cfg.timers.mail_ack).map_err(|e| format!("timers.mail_ack: {e}"))?;
    if cfg.timers.nudge_at <= 0.0 || cfg.timers.nudge_at >= 1.0 || cfg.timers.nudge_at.is_nan() {
        return Err(format!(
            "timers.nudge_at {} must be in (0, 1)",
            cfg.timers.nudge_at
        ));
    }
    if cfg.timers.cost_buffer <= 0.0 || cfg.timers.cost_buffer > 1.0 || cfg.timers.cost_buffer.is_nan() {
        return Err(format!(
            "timers.cost_buffer {} must be in (0, 1]",
            cfg.timers.cost_buffer
        ));
    }
    if cfg.timers.overrun_ratio <= 0.0
        || cfg.timers.overrun_ratio >= 1.0
        || cfg.timers.overrun_ratio.is_nan()
    {
        return Err(format!(
            "timers.{} {} must be in (0, 1)",
            "grace", cfg.timers.overrun_ratio
        ));
    }

    for (name, role) in &cfg.roles {
        if !ROLE_NAMES.contains(&name.as_str()) {
            return Err(format!("unknown role '{name}'"));
        }
        for cls in role.can_spawn.iter().chain(role.can_prompt.iter()) {
            if !ROLE_CLASSES.contains(&cls.as_str()) {
                return Err(format!("unknown role-class '{cls}'"));
            }
        }
        if let Some(to) = &role.reports_to {
            let known = ROLE_NAMES.contains(&to.as_str()) || to == "owner";
            if !known {
                return Err(format!("unknown role in reports_to '{to}'"));
            }
        }
        if let Some(peers) = role.max_peers {
            if name != "lead" {
                return Err(format!(
                    "role '{name}' takes no max_peers (only roles.lead caps its Peers)"
                ));
            }
            if peers < 1 || peers > cap {
                return Err(format!("roles.lead.max_peers {peers} must be in 1..={cap}"));
            }
        }
    }

    if !["none", "harness", "kind"].contains(&cfg.strategy.builder_reviewer_differ.as_str()) {
        return Err(format!(
            "unknown differ policy '{}'",
            cfg.strategy.builder_reviewer_differ
        ));
    }

    if !cfg.kinds.contains_key(&cfg.strategy.default_kind) {
        return Err(format!("unknown kind '{}'", cfg.strategy.default_kind));
    }
    if cfg.strategy.default_reviewer.is_empty() {
        return Err("strategy.default_reviewer is empty: no reviewer to fall back to".to_string());
    }
    for r in &cfg.strategy.default_reviewer {
        if !cfg.kinds.contains_key(r) {
            return Err(format!("unknown kind '{r}'"));
        }
    }
    for (name, tf) in &cfg.taskforce {
        for m in &tf.members {
            if !cfg.kinds.contains_key(m) {
                return Err(format!("taskforce {name}: unknown kind '{m}'"));
            }
        }
    }

    // builder/reviewer harness rule (strategy.builder_reviewer_differ = "harness").
    // The pair checked is the one a default `quaqua pipeline` runs: the builder
    // resolves from `purposes.build`, else `default_kind` (pipeline/mod.rs), and
    // the reviewer from `default_reviewer` — so a Lead that overrode the builder
    // purpose stays SLP-compliant without making `default_kind` the builder.
    if cfg.strategy.builder_reviewer_differ == "harness" {
        let builder = cfg
            .purposes
            .get("build")
            .map(String::as_str)
            .unwrap_or(cfg.strategy.default_kind.as_str());
        if let Some(b) = cfg.kinds.get(builder) {
            for r in &cfg.strategy.default_reviewer {
                let rk = &cfg.kinds[r];
                if rk.harness == b.harness {
                    return Err(format!(
                        "builder '{builder}' and reviewer '{r}' share harness '{}'",
                        b.harness
                    ));
                }
            }
        }
    }

    for r in &cfg.strategy.default_reviewer {
        if !cfg.kinds[r].can_review {
            return Err(format!("reviewer '{r}' has can_review = false"));
        }
    }

    if !cfg.merge.integ_branch.contains("{slug}") {
        return Err(format!(
            "integ_branch '{}' needs a {{slug}} placeholder",
            cfg.merge.integ_branch
        ));
    }
    for (purpose, kind) in &cfg.purposes {
        if !cfg.kinds.contains_key(kind) {
            return Err(format!("purpose '{purpose}': unknown kind '{kind}'"));
        }
    }
    for kind in cfg.strategy.build_kinds.iter().flatten() {
        if !cfg.kinds.contains_key(kind) {
            return Err(format!("strategy.build_kinds: unknown kind '{kind}'"));
        }
    }


    // team.toml is the only source of harness/model/effort: fail closed on a kind
    // that cannot launch, or on a fallback that names a missing kind or itself.
    for (name, kind) in &cfg.kinds {
        if !SPAWN_POLICIES.contains(&kind.spawn.as_str()) {
            return Err(format!(
                "kind '{name}': unknown spawn policy '{}' (want auto or approve)",
                kind.spawn
            ));
        }
        if !COST_CAP_ACTIONS.contains(&kind.on_cost_cap.as_str()) {
            return Err(format!(
                "kind '{name}': unknown on_cost_cap '{}' (want warn or kill)",
                kind.on_cost_cap
            ));
        }
        if crate::harness::Harness::parse(&kind.harness).is_none() {
            return Err(format!(
                "kind '{name}': unsupported harness '{}'",
                kind.harness
            ));
        }
        if kind.model.trim().is_empty() {
            return Err(format!("kind '{name}': model is empty"));
        }
        let effort = match kind.effort.as_deref() {
            Some(e) if !e.trim().is_empty() => e,
            _ => return Err(format!("kind '{name}': effort is missing")),
        };
        if !effort_allowed(&kind.harness, effort) {
            return Err(format!(
                "kind '{name}': harness '{}' does not take effort '{effort}'",
                kind.harness
            ));
        }
        if crate::harness::Harness::parse(&kind.harness)
            == Some(crate::harness::Harness::Opencode)
        {
            match cfg.opencode_model_variants(&kind.model) {
                Some(allowed) => {
                    if !allowed.contains(effort) {
                        return Err(format!(
                            "kind '{name}': harness 'opencode' model '{}' does not take effort '{effort}' (allowed: {})",
                            kind.model,
                            allowed.join(", ")
                        ));
                    }
                }
                None => {
                    return Err(format!(
                        "kind '{name}': opencode model '{}' is not in allowlist (fix: add model and variants to team.toml [opencode_models])",
                        kind.model
                    ));
                }
            }
        }
        if let Some(fb) = kind.fallback.as_deref() {
            if fb.trim().is_empty() {
                return Err(format!("kind '{name}': fallback is empty"));
            }
            if fb == name {
                return Err(format!("kind '{name}': fallback names itself"));
            }
            let fb_kind = match cfg.kinds.get(fb) {
                Some(k) => k,
                None => return Err(format!("kind '{name}': fallback '{fb}' is not a kind")),
            };
            if kind.can_review && !fb_kind.can_review {
                return Err(format!(
                    "kind '{name}': can_review kind has fallback '{fb}' which cannot review"
                ));
            }
        }
    }
    Ok(())
}

/// The one spawn-policy gate every spawn path calls (worker, pipeline,
/// taskforce, lead). A kind with `spawn = "approve"` refuses unless the caller
/// passed `--approve-kind <kind>` for it.
pub fn check_spawn_policy(
    kind_name: &str,
    kind: &Kind,
    approved_kinds: &[String],
) -> Result<(), (i32, String)> {
    if kind.spawn != SPAWN_APPROVE || approved_kinds.iter().any(|k| k == kind_name) {
        return Ok(());
    }
    Err((
        1,
        format!(
            "error: kind '{kind_name}' has spawn = \"approve\" in team.toml: pass --approve-kind {kind_name} to approve this spawn (or set kinds.{kind_name}.spawn = \"auto\")\n"
        ),
    ))
}

/// Resolve one dotted key to its printable value. Arrays print one item per line.
pub fn get(raw: &Config, key: &str, project: Option<&str>) -> Result<String, String> {
    let cfg = resolve(raw.clone(), project)?;
    let unknown = || format!("unknown key '{key}'");
    if let Some(m) = key.strip_prefix("opencode_models.") {
        let model_id = m.rsplit('/').next().unwrap_or(m);
        return cfg
            .opencode_models
            .get(m)
            .or_else(|| cfg.opencode_models.get(model_id))
            .map(|v| v.join("\n"))
            .ok_or_else(unknown);
    }
    let parts: Vec<&str> = key.split('.').collect();
    let out = match parts.as_slice() {
        ["schema_version"] => cfg.schema_version.to_string(),
        ["layout", "use"] => cfg
            .layout
            .as_ref()
            .map(|l| l.use_layout.clone())
            .ok_or_else(unknown)?,
        ["limits", "depth"] => cfg.limits.depth.to_string(),
        ["limits", "max_workers_per_orchestrator"] => {
            cfg.limits.max_workers_per_orchestrator.to_string()
        }
        ["limits", "weights", name] => cfg
            .limits
            .weights
            .get(*name)
            .map(i64::to_string)
            .ok_or_else(unknown)?,
        ["timers", "budget_default"] => cfg.timers.default_budget_duration.clone(),
        ["timers", "hard_cap"] => cfg.timers.hard_cap.clone(),
        ["timers", "stall"] => cfg.timers.stall.clone(),
        ["timers", "idle"] => cfg.timers.idle.clone(),
        ["timers", "nudge_at"] => cfg.timers.nudge_at.to_string(),
        ["timers", "grace"] => cfg.timers.overrun_ratio.to_string(),
        ["timers", "pilot_budget"] => cfg.timers.pilot_budget.clone(),
        ["timers", "cost_buffer"] => cfg.timers.cost_buffer.to_string(),
        ["timers", "mail_ack"] => cfg.timers.mail_ack.clone(),
        ["strategy", "default_kind"] => cfg.strategy.default_kind.clone(),
        ["strategy", "lead_kind"] => cfg
            .strategy
            .lead_kind
            .clone()
            .unwrap_or_else(|| cfg.strategy.default_kind.clone()),
        ["purposes", p] => cfg.purposes.get(*p).cloned().ok_or_else(unknown)?,
        ["opencode_models", m] => cfg
            .opencode_models
            .get(*m)
            .map(|v| v.join("\n"))
            .ok_or_else(unknown)?,
        ["strategy", "default_reviewer"] => cfg.strategy.default_reviewer.join("\n"),
        ["strategy", "build_kinds"] => cfg.build_kinds().join("\n"),
        ["strategy", "builder_reviewer_differ"] => cfg.strategy.builder_reviewer_differ.clone(),
        ["kinds", k, field] => {
            let kind = cfg.kinds.get(*k).ok_or_else(unknown)?;
            match *field {
                "harness" => kind.harness.clone(),
                "model" => kind.model.clone(),
                "effort" => kind
                    .effort
                    .clone()
                    .ok_or_else(|| format!("key '{key}' is unset"))?,
                "fallback" => kind
                    .fallback
                    .clone()
                    .ok_or_else(|| format!("key '{key}' is unset"))?,
                "can_review" => kind.can_review.to_string(),
                "spawn" => kind.spawn.clone(),
                _ => return Err(unknown()),
            }
        }
        ["roles", "lead", "max_peers"] => cfg
            .roles
            .get("lead")
            .and_then(|role| role.max_peers)
            .unwrap_or(DEFAULT_LEAD_MAX_PEERS)
            .to_string(),
        ["roles", r, field] => {
            let role = cfg.roles.get(*r).ok_or_else(unknown)?;
            match *field {
                "can_spawn" => role.can_spawn.join("\n"),
                "can_prompt" => role.can_prompt.join("\n"),
                "reports_to" => role.reports_to.clone().unwrap_or_default(),
                "max_peers" => role
                    .max_peers
                    .map(|n| n.to_string())
                    .ok_or_else(|| format!("key '{key}' is unset"))?,
                _ => return Err(unknown()),
            }
        }
        ["taskforce", t, field] => {
            let tf = cfg.taskforce.get(*t).ok_or_else(unknown)?;
            match *field {
                "members" => tf.members.join("\n"),
                "read_only" => tf.read_only.to_string(),
                _ => return Err(unknown()),
            }
        }
        ["merge", "integ_branch"] => cfg.merge.integ_branch.clone(),
        ["grid", "workers_tab"] => cfg.grid.workers_tab.clone(),
        ["grid", "deferred_statuses"] => cfg.grid.deferred_statuses.join("\n"),
        ["projects", p, "max_workers_per_orchestrator"] => cfg
            .projects
            .get(*p)
            .and_then(|o| o.max_workers_per_orchestrator)
            .map(|v| v.to_string())
            .ok_or_else(unknown)?,
        _ => return Err(unknown()),
    };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        toml::from_str(
            r#"
schema_version = 2
[limits]
max_workers_per_orchestrator = 4
weights = { default = 1, pipeline = 2 }
[timers]
budget_default = "45m"
hard_cap = "120m"
stall = "10m"
idle = "2m"
nudge_at = 0.8
grace = 0.25
pilot_budget = "30m"
[roles.cos]
can_spawn = ["lead"]
can_prompt = ["lead"]
[strategy]
default_kind = "tieude"
default_reviewer = ["suhuynh"]
builder_reviewer_differ = "harness"
[kinds.tieude]
harness = "opencode"
model = "opencode-go/deepseek-v4.1-flash"
effort = "max"
fallback = "suhuynh"
can_review = false
[kinds.suhuynh]
harness = "claude"
model = "m"
effort = "high"
can_review = true
[merge]
integ_branch = "quaqua/{slug}-integ"
[grid]
workers_tab = "workers"
"#,
        )
        .unwrap()
    }

    #[test]
    fn valid_base_passes() {
        assert!(validate(&base()).is_ok());
    }

    #[test]
    fn build_kinds_default_to_every_configured_kind() {
        // R4: a team.toml written before the key existed still parses and
        // counts every kind as a build kind.
        assert_eq!(base().build_kinds(), vec!["suhuynh", "tieude"]);
    }

    #[test]
    fn build_kinds_from_team_toml_win_and_must_name_kinds() {
        let mut c = base();
        c.strategy.build_kinds = Some(vec!["tieude".to_string()]);
        assert!(validate(&c).is_ok());
        assert_eq!(c.build_kinds(), vec!["tieude"]);

        c.strategy
            .build_kinds
            .as_mut()
            .unwrap()
            .push("ghost-kind".to_string());
        let err = validate(&c).unwrap_err();
        assert!(
            err.contains("strategy.build_kinds: unknown kind 'ghost-kind'"),
            "{err}"
        );
    }

    #[test]
    fn template_counts_the_builder_roster() {
        // The Lead gate used to hardcode these three; the template now says so.
        let cfg = embedded_team_config().expect("embedded team.toml");
        assert_eq!(validate(&cfg), Ok(()), "template must validate");
        assert_eq!(cfg.build_kinds(), vec!["tieude", "sude", "ni"]);
    }

    #[test]
    fn pi_kind_validates_by_pi_thinking_set() {
        let mut c = base();
        let k = c.kinds.get_mut("tieude").unwrap();
        k.harness = "pi".to_string();
        assert!(validate(&c).is_ok());
        c.kinds.get_mut("tieude").unwrap().effort = Some("auto".to_string());
        let err = validate(&c).unwrap_err();
        assert!(err.contains("harness 'pi' does not take effort 'auto'"), "{err}");
    }

    #[test]
    fn agy_kind_validates_by_agy_effort_set() {
        let mut c = base();
        let k = c.kinds.get_mut("tieude").unwrap();
        k.harness = "agy".to_string();
        k.model = "gemini-3.8-flash".to_string();
        k.effort = Some("high".to_string());
        assert!(validate(&c).is_ok());
        c.kinds.get_mut("tieude").unwrap().effort = Some("off".to_string());
        let err = validate(&c).unwrap_err();
        assert!(err.contains("harness 'agy' does not take effort 'off'"), "{err}");
    }

    #[test]
    fn cap_over_is_rejected() {
        let mut c = base();
        c.limits.max_workers_per_orchestrator = 9;
        assert!(validate(&c).unwrap_err().contains("over the cap"));
    }

    #[test]
    fn lead_max_peers_must_fit_the_global_cap() {
        let mut c = base();
        c.roles.insert(
            "lead".to_string(),
            Role {
                max_peers: Some(5),
                ..Default::default()
            },
        );
        let err = validate(&c).unwrap_err();
        assert!(err.contains("roles.lead.max_peers 5 must be in 1..=4"), "{err}");

        c.roles.get_mut("lead").unwrap().max_peers = Some(0);
        let err = validate(&c).unwrap_err();
        assert!(err.contains("roles.lead.max_peers 0 must be in 1..=4"), "{err}");

        c.roles.get_mut("lead").unwrap().max_peers = Some(4);
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn max_peers_belongs_to_the_lead_role_only() {
        let mut c = base();
        c.roles.insert(
            "cos".to_string(),
            Role {
                can_spawn: vec!["lead".to_string()],
                max_peers: Some(2),
                ..Default::default()
            },
        );
        let err = validate(&c).unwrap_err();
        assert!(err.contains("role 'cos' takes no max_peers"), "{err}");
    }

    #[test]
    fn lead_max_peers_gets_the_default_without_a_roles_lead_table() {
        // An older team.toml has no [roles.lead] at all: the cap still resolves.
        let c = base();
        assert!(!c.roles.contains_key("lead"));
        assert_eq!(get(&c, "roles.lead.max_peers", None).unwrap(), "4");
        assert!(get(&c, "roles.cos.max_peers", None)
            .unwrap_err()
            .contains("is unset"));
    }

    #[test]
    fn harness_differ_checks_the_pipeline_default_builder() {
        // `quaqua pipeline` builds with purposes.build when set, so a config whose
        // default worker kind shares the reviewer's harness is still SLP-compliant.
        let mut c = base();
        let tieude = c.kinds.get_mut("tieude").unwrap();
        tieude.harness = "claude".to_string();
        tieude.model = "m".to_string();
        tieude.effort = Some("high".to_string());
        tieude.fallback = None;

        // no build purpose: the default builder IS default_kind, so it is rejected
        let err = validate(&c).unwrap_err();
        assert!(
            err.contains("builder 'tieude' and reviewer 'suhuynh' share harness 'claude'"),
            "{err}"
        );

        // purposes.build names the builder a default pipeline runs; it differs
        c.purposes.insert("build".to_string(), "sude".to_string());
        c.kinds.insert(
            "sude".to_string(),
            Kind {
                harness: "omp".to_string(),
                model: "google-antigravity/gemini-3.8-flash".to_string(),
                effort: Some("high".to_string()),
                fallback: None,
                can_review: false,
                spawn: SPAWN_AUTO.to_string(),
                on_cost_cap: COST_CAP_WARN.to_string(),
            },
        );
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn get_array_prints_lines() {
        assert_eq!(get(&base(), "kinds.suhuynh.effort", None).unwrap(), "high");
        assert_eq!(
            get(&base(), "kinds.tieude.fallback", None).unwrap(),
            "suhuynh"
        );
        assert!(get(&base(), "strategy.default_reviewer", None)
            .unwrap()
            .contains("suhuynh"));
        assert!(get(&base(), "nope.nope", None).is_err());
    }

    #[test]
    fn spawn_policy_defaults_to_auto() {
        assert_eq!(base().kinds["tieude"].spawn, SPAWN_AUTO);
        assert_eq!(get(&base(), "kinds.tieude.spawn", None).unwrap(), SPAWN_AUTO);
    }

    #[test]
    fn on_cost_cap_defaults_to_warn_and_rejects_unknown() {
        assert_eq!(base().kinds["tieude"].on_cost_cap, COST_CAP_WARN);
        let mut c = base();
        c.kinds.get_mut("tieude").unwrap().on_cost_cap = COST_CAP_KILL.to_string();
        validate(&c).unwrap();
        c.kinds.get_mut("tieude").unwrap().on_cost_cap = "pause".to_string();
        let err = validate(&c).unwrap_err();
        assert!(err.contains("unknown on_cost_cap 'pause'"), "{err}");
    }

    #[test]
    fn cost_buffer_defaults_to_20pct_and_is_bounded() {
        assert_eq!(base().timers.cost_buffer, 0.2);
        assert_eq!(get(&base(), "timers.cost_buffer", None).unwrap(), "0.2");
        let mut c = base();
        c.timers.cost_buffer = 0.5;
        validate(&c).unwrap();
        for bad in [0.0, -0.1, 1.5, f64::NAN] {
            c.timers.cost_buffer = bad;
            let err = validate(&c).unwrap_err();
            assert!(err.contains("timers.cost_buffer"), "{err}");
        }
    }

    #[test]
    fn mail_ack_defaults_to_30m_and_validates_duration() {
        assert_eq!(base().timers.mail_ack, "30m");
        assert_eq!(get(&base(), "timers.mail_ack", None).unwrap(), "30m");
        let mut c = base();
        c.timers.mail_ack = "1h".to_string();
        validate(&c).unwrap();
        c.timers.mail_ack = "invalid".to_string();
        let err = validate(&c).unwrap_err();
        assert!(err.contains("timers.mail_ack"), "{err}");
    }

    #[test]
    fn unknown_spawn_policy_is_rejected() {
        let mut c = base();
        c.kinds.get_mut("tieude").unwrap().spawn = "sometimes".to_string();
        let err = validate(&c).unwrap_err();
        assert!(err.contains("unknown spawn policy 'sometimes'"), "{err}");
    }

    #[test]
    fn approve_policy_needs_the_matching_flag() {
        let mut c = base();
        c.kinds.get_mut("suhuynh").unwrap().spawn = SPAWN_APPROVE.to_string();

        let err = check_spawn_policy("suhuynh", &c.kinds["suhuynh"], &[]).unwrap_err();
        assert!(err.1.contains("--approve-kind suhuynh"), "{}", err.1);

        // approving another kind does not approve this one
        let other = vec!["tieude".to_string()];
        assert!(check_spawn_policy("suhuynh", &c.kinds["suhuynh"], &other).is_err());

        let approved = vec!["suhuynh".to_string()];
        assert!(check_spawn_policy("suhuynh", &c.kinds["suhuynh"], &approved).is_ok());
        // auto kinds never need the flag
        assert!(check_spawn_policy("tieude", &c.kinds["tieude"], &[]).is_ok());
    }

    #[test]
    fn missing_effort_is_rejected() {
        let mut c = base();
        c.kinds.get_mut("tieude").unwrap().effort = None;
        let err = validate(&c).unwrap_err();
        assert!(err.contains("effort is missing"), "{err}");
    }

    #[test]
    fn empty_model_is_rejected() {
        let mut c = base();
        c.kinds.get_mut("tieude").unwrap().model = "  ".to_string();
        let err = validate(&c).unwrap_err();
        assert!(err.contains("model is empty"), "{err}");
    }

    #[test]
    fn unsupported_effort_is_rejected() {
        let mut c = base();
        c.kinds.get_mut("tieude").unwrap().effort = Some("banana".to_string());
        let err = validate(&c).unwrap_err();
        assert!(err.contains("does not take effort 'banana'"), "{err}");
    }

    #[test]
    fn claude_effort_vocabulary_is_enforced() {
        // opencode's "none" is not a claude effort value.
        let mut c = base();
        c.kinds.get_mut("suhuynh").unwrap().effort = Some("none".to_string());
        assert!(validate(&c).is_err());
    }

    #[test]
    fn fallback_to_missing_kind_is_rejected() {
        let mut c = base();
        c.kinds.get_mut("tieude").unwrap().fallback = Some("ghost".to_string());
        let err = validate(&c).unwrap_err();
        assert!(err.contains("fallback 'ghost' is not a kind"), "{err}");
    }

    #[test]
    fn fallback_to_self_is_rejected() {
        let mut c = base();
        c.kinds.get_mut("tieude").unwrap().fallback = Some("tieude".to_string());
        let err = validate(&c).unwrap_err();
        assert!(err.contains("fallback names itself"), "{err}");
    }

    #[test]
    fn fallback_from_can_review_to_non_review_is_rejected() {
        let mut c = base();
        c.kinds.get_mut("suhuynh").unwrap().fallback = Some("tieude".to_string());
        let err = validate(&c).unwrap_err();
        assert!(err.contains("cannot review"), "{err}");
    }

    #[test]
    fn opencode_model_variant_is_enforced() {
        let mut c = base();
        c.kinds.get_mut("tieude").unwrap().model = "opencode-go/deepseek-v4.1-flash".to_string();
        // medium is a valid opencode variant in general, but deepseek-v4.1-flash only takes low, high, max
        c.kinds.get_mut("tieude").unwrap().effort = Some("medium".to_string());
        let err = validate(&c).unwrap_err();
        assert!(err.contains("does not take effort 'medium'"), "{err}");

        // openrouter path also matches last segment and rejects medium
        c.kinds.get_mut("tieude").unwrap().model =
            "openrouter/deepseek/deepseek-v4.1-flash".to_string();
        c.kinds.get_mut("tieude").unwrap().effort = Some("medium".to_string());
        let err = validate(&c).unwrap_err();
        assert!(err.contains("does not take effort 'medium'"), "{err}");

        // unknown opencode model rejected with fix message
        c.kinds.get_mut("tieude").unwrap().model = "opencode-go/unknown-model".to_string();
        c.kinds.get_mut("tieude").unwrap().effort = Some("max".to_string());
        let err = validate(&c).unwrap_err();
        assert!(
            err.contains("is not in allowlist (fix: add model and variants to team.toml [opencode_models])"),
            "{err}"
        );

        // low and max work for known models
        c.kinds.get_mut("tieude").unwrap().model = "opencode-go/deepseek-v4.1-flash".to_string();
        c.kinds.get_mut("tieude").unwrap().effort = Some("low".to_string());
        assert!(validate(&c).is_ok());
        c.kinds.get_mut("tieude").unwrap().effort = Some("max".to_string());
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn test_opencode_model_from_team_toml_fixture() {
        let mut c = base();
        // Add a new model via opencode_models table in config
        c.opencode_models.insert(
            "my-custom-model".to_string(),
            vec!["low".to_string(), "high".to_string()],
        );
        c.kinds.get_mut("tieude").unwrap().model = "my-custom-model".to_string();
        c.kinds.get_mut("tieude").unwrap().effort = Some("high".to_string());
        assert!(validate(&c).is_ok());

        // Removing it fails with the fix hint naming team.toml
        c.opencode_models.remove("my-custom-model");
        let err = validate(&c).unwrap_err();
        assert!(
            err.contains("is not in allowlist (fix: add model and variants to team.toml [opencode_models])"),
            "{err}"
        );
    }

    #[test]
    fn test_purposes_resolution_and_validation() {
        let mut c = base();
        c.purposes.insert("review".to_string(), "suhuynh".to_string());
        c.purposes.insert("build".to_string(), "tieude".to_string());

        // Valid purpose resolves to kind
        assert_eq!(resolve_purpose(&c, "review").unwrap(), "suhuynh");
        assert_eq!(resolve_purpose(&c, "build").unwrap(), "tieude");

        // Unknown purpose fails with offending key
        let err = resolve_purpose(&c, "research").unwrap_err();
        assert!(err.contains("unknown purpose 'research'"), "{err}");

        // Dangling kind fails config validation with offending key
        c.purposes.insert("design".to_string(), "ghost-kind".to_string());
        let err = validate(&c).unwrap_err();
        assert!(
            err.contains("purpose 'design': unknown kind 'ghost-kind'"),
            "{err}"
        );
    }

    #[test]
    fn test_opencode_models_deserialization_merges_defaults() {
        let toml_str = r#"
schema_version = 2
[opencode_models]
"custom-model" = ["low", "high"]
"#;
        let cfg: Config = toml::from_str(toml_str).expect("parse config with custom opencode_models");
        assert_eq!(
            cfg.opencode_models.get("deepseek-v4.1-flash"),
            Some(&vec!["low".to_string(), "high".to_string(), "max".to_string()])
        );
        assert_eq!(
            cfg.opencode_models.get("custom-model"),
            Some(&vec!["low".to_string(), "high".to_string()])
        );

        let override_str = r#"
schema_version = 2
[opencode_models]
"deepseek-v4.1-flash" = ["max"]
"#;
        let cfg2: Config = toml::from_str(override_str).expect("parse config overriding default model");
        assert_eq!(
            cfg2.opencode_models.get("deepseek-v4.1-flash"),
            Some(&vec!["max".to_string()])
        );
    }
}
