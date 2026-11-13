//! Positive identity and depth resolution from pane tokens and owner chains.

use crate::herdr::HerdrOps;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;

/// Positive caller roles mapped from pane tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Peer,
    Lead,
    Cos,
    Unknown,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Peer => "peer",
            Role::Lead => "lead",
            Role::Cos => "cos",
            Role::Unknown => "unknown",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Errors during depth walk over `.owner` files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepthError {
    Cycle(Vec<String>),
    NotFound(String),
}

impl DepthError {
    pub fn is_cycle(&self) -> bool {
        matches!(self, DepthError::Cycle(_))
    }
}

impl fmt::Display for DepthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DepthError::Cycle(cycle) => {
                write!(f, "cycle detected in owner chain: {}", cycle.join(" -> "))
            }
            DepthError::NotFound(name) => {
                write!(f, "owner file for '{name}' not found")
            }
        }
    }
}

impl std::error::Error for DepthError {}

/// How the caller identity was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySource {
    PaneTokens,
    EnvFallback,
    Unknown,
}

impl fmt::Display for IdentitySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentitySource::PaneTokens => write!(f, "tokens"),
            IdentitySource::EnvFallback => write!(f, "env"),
            IdentitySource::Unknown => write!(f, "unknown"),
        }
    }
}

/// Depth of a caller or an error encountered while resolving it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerDepth {
    Resolved(u32),
    Cycle(Vec<String>),
    MissingOwner(String),
    Unknown,
}

impl fmt::Display for CallerDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallerDepth::Resolved(d) => write!(f, "{d}"),
            CallerDepth::Cycle(c) => write!(f, "?(cycle {})", c.join("->")),
            CallerDepth::MissingOwner(name) => write!(f, "?(missing {name})"),
            CallerDepth::Unknown => write!(f, "?"),
        }
    }
}

impl From<Result<u32, DepthError>> for CallerDepth {
    fn from(res: Result<u32, DepthError>) -> Self {
        match res {
            Ok(d) => CallerDepth::Resolved(d),
            Err(DepthError::Cycle(c)) => CallerDepth::Cycle(c),
            Err(DepthError::NotFound(n)) => CallerDepth::MissingOwner(n),
        }
    }
}

/// Resolved identity of a caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub name: String,
    pub role: Role,
    pub depth: CallerDepth,
    pub source: IdentitySource,
}

impl fmt::Display for Caller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "name={}, role={}, depth={}, source={}",
            self.name, self.role, self.depth, self.source
        )
    }
}
/// Positive resolution of role from a map of pane tokens.
/// - `grid_role == "worker"` -> Role::Peer
/// - `grid_role == "orchestrator"` -> Role::Lead
/// - `grid_role == "cos"` -> Role::Cos
/// - absent, empty, or anything else ("none") -> Role::Unknown
///
/// NEVER defaults to Cos.
pub fn resolve(tokens: &HashMap<String, String>) -> Role {
    match tokens.get("grid_role").map(|s| s.trim()) {
        Some("worker") => Role::Peer,
        Some("orchestrator") => Role::Lead,
        Some("cos") => Role::Cos,
        _ => Role::Unknown,
    }
}

/// Resolve role from optional tokens map. Returns Role::Unknown when tokens is None.
pub fn resolve_opt(tokens: Option<&HashMap<String, String>>) -> Role {
    tokens.map(resolve).unwrap_or(Role::Unknown)
}

/// Walk .owner files starting from `start` in `runs_dir` until reaching "cos",
/// counting steps as depth.
///
/// Stops and returns `DepthError::Cycle` if a cycle is detected.
/// Returns `DepthError::NotFound` if an intermediate `.owner` file is missing.
///
/// - "cos" has depth 0.
/// - an entity whose owner is "cos" has depth 1.
/// - an entity whose owner's owner is "cos" has depth 2.
pub fn depth_walk(runs_dir: &Path, start: &str) -> Result<u32, DepthError> {
    let start = start.trim();
    if start.is_empty() {
        return Err(DepthError::NotFound(String::new()));
    }
    if start == "cos" {
        return Ok(0);
    }

    let mut current = start.to_string();
    let mut visited: Vec<String> = Vec::new();
    let mut depth: u32 = 0;

    loop {
        if current == "cos" {
            return Ok(depth);
        }
        if visited.contains(&current) {
            visited.push(current);
            return Err(DepthError::Cycle(visited));
        }
        visited.push(current.clone());

        let owner_path = runs_dir.join(format!("{current}.owner"));
        let content = match fs::read_to_string(&owner_path) {
            Ok(c) => c,
            Err(_) => return Err(DepthError::NotFound(current)),
        };

        let parent = content.trim();
        if parent.is_empty() {
            return Err(DepthError::NotFound(current));
        }

        current = parent.to_string();
        depth += 1;
    }
}

/// Query the caller pane once via herdr `pane get $HERDR_PANE_ID` and return its
/// token set.
///
/// `Some` means herdr can see the caller pane, and an empty map is a real
/// answer: the pane exists and carries no tokens. That distinction is what the
/// callers depend on. A herdr server restart restores panes from `session.json`
/// (cwd, label, agent name, agent session — never tokens) and then *omits* the
/// `tokens` field from `pane get` instead of sending an empty object, so
/// "no tokens key" must not read as "no pane": it did, and skipped both the
/// identity heal and the env guard in every caller (`HERDR_PANE_ID` stays set in
/// the shell across the restart).
///
/// `None` means there is no caller pane: `HERDR_PANE_ID` unset or empty, `pane
/// get` failed, or herdr answered with an error body (`pane get` exits 0 for a
/// dead pane, so the body decides).
pub fn caller_pane_tokens<H: HerdrOps>(herdr: &H) -> Option<HashMap<String, String>> {
    let pane_id = std::env::var("HERDR_PANE_ID").ok()?;
    if pane_id.trim().is_empty() {
        return None;
    }
    let out = herdr.run(&["pane", "get", &pane_id]).ok()?;
    if !out.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let pane = v.pointer("/result/pane")?;
    let mut tokens = HashMap::new();
    if let Some(tokens_val) = pane.get("tokens").and_then(|t| t.as_object()) {
        for (k, val) in tokens_val {
            if let Some(s) = val.as_str() {
                tokens.insert(k.clone(), s.to_string());
            }
        }
    }
    Some(tokens)
}

/// Resolves caller identity from an already-fetched tokens map (or None).
pub fn resolve_caller_from_tokens(
    tokens: Option<&HashMap<String, String>>,
    runs_dir: &Path,
) -> Caller {
    if let Some(tokens) = tokens {
        let role = resolve(tokens);
        let name = match role {
            Role::Cos => "cos".to_string(),
            Role::Lead => tokens
                .get("grid_owner")
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            Role::Peer => tokens
                .get("grid_name")
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            Role::Unknown => tokens
                .get("grid_owner")
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
        };
        let depth = match role {
            Role::Cos => CallerDepth::Resolved(0),
            Role::Lead => CallerDepth::from(depth_walk(runs_dir, &name)),
            Role::Peer => {
                if let Some(owner) = tokens.get("grid_owner") {
                    match depth_walk(runs_dir, owner) {
                        Ok(d) => CallerDepth::Resolved(d + 1),
                        Err(e) => CallerDepth::from(Err(e)),
                    }
                } else {
                    CallerDepth::Unknown
                }
            }
            Role::Unknown => CallerDepth::Unknown,
        };
        return Caller {
            name,
            role,
            depth,
            source: IdentitySource::PaneTokens,
        };
    }

    // Fallback: QUAQUA_OWNER environment variable is ONLY allowed under QUAQUA_TEST=1
    // (design.md:151; fail-closed in production, "never ambient env").
    let is_test = std::env::var("QUAQUA_TEST").as_deref() == Ok("1");
    if is_test {
        let env_owner = std::env::var("QUAQUA_OWNER")
            .ok()
            .filter(|s| !s.trim().is_empty());
        if let Some(owner) = env_owner {
            let role = if owner == "cos" {
                Role::Cos
            } else if crate::cap::is_lead_owner(&owner) {
                Role::Lead
            } else {
                Role::Unknown
            };
            let depth = match role {
                Role::Cos => CallerDepth::Resolved(0),
                Role::Lead => match depth_walk(runs_dir, &owner) {
                    Ok(d) => CallerDepth::Resolved(d),
                    Err(DepthError::Cycle(cycle)) => CallerDepth::Cycle(cycle),
                    Err(DepthError::NotFound(_)) => {
                        if is_test {
                            CallerDepth::Resolved(1)
                        } else {
                            CallerDepth::MissingOwner(owner.clone())
                        }
                    }
                },
                Role::Unknown | Role::Peer => CallerDepth::Unknown,
            };
            return Caller {
                name: owner,
                role,
                depth,
                source: IdentitySource::EnvFallback,
            };
        }
    }

    Caller {
        name: "unknown".to_string(),
        role: Role::Unknown,
        depth: CallerDepth::Unknown,
        source: IdentitySource::Unknown,
    }
}

/// Resolves the caller identity from herdr pane tokens, environment fallback, and owner chains.
/// If caller role is unknown but herdr can see the caller pane, attempts
/// self-healing via `heal_with_runs_dir` to recover pane tokens from persisted
/// identity. The pane must be the trigger, not its tokens object: after a
/// restart herdr omits `tokens` entirely (see `caller_pane_tokens`).
pub fn resolve_caller<H: HerdrOps>(herdr: &H, runs_dir: &Path) -> Caller {
    let tokens = caller_pane_tokens(herdr);
    let caller = resolve_caller_from_tokens(tokens.as_ref(), runs_dir);
    if caller.role != Role::Unknown {
        return caller;
    }

    // Only attempt healing when a caller pane actually exists in herdr
    // and carries no grid_role token (tokens lost after a herdr restart).
    let has_grid_role = tokens
        .as_ref()
        .and_then(|t| t.get("grid_role"))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    if tokens.is_some() && !has_grid_role {
        let _ = heal_with_runs_dir(herdr, runs_dir, None, false);
        let healed_tokens = caller_pane_tokens(herdr);
        let healed_caller = resolve_caller_from_tokens(healed_tokens.as_ref(), runs_dir);
        if healed_caller.role != Role::Unknown {
            return healed_caller;
        }
    }

    caller
}

#[derive(Debug, Clone, Default)]
pub struct HealReport {
    pub actions: Vec<String>,
    pub errors: Vec<String>,
}

impl std::ops::Deref for HealReport {
    type Target = [String];
    fn deref(&self) -> &Self::Target {
        &self.actions
    }
}

fn find_index_file(runs_dir: &Path) -> Option<std::path::PathBuf> {
    if let Some(parent) = runs_dir.parent() {
        let candidate = parent.join("projects/INDEX.md");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Ok(root) = crate::workspace::workspace_root_checked() {
        let candidate = root.join("projects/INDEX.md");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let default_candidate = crate::workspace::workspace_root().join("projects/INDEX.md");
    if default_candidate.is_file() {
        return Some(default_candidate);
    }
    None
}

fn is_registered_lead(runs_dir: &Path, slug: &str, lead_name: &str) -> bool {
    if let Some(index_file) = find_index_file(runs_dir) {
        if let Ok(content) = std::fs::read_to_string(&index_file) {
            let rows = crate::worker::leadgate::index_rows(&content);
            return rows.iter().any(|r| r.slug == slug && r.owner == lead_name);
        }
    }
    false
}

/// Heal lost pane identity tokens after a herdr server restart.
///
/// Order of repair:
/// (a) Lead: a herdr agent named `<slug>-lead` (or carrying grid_owner=<slug>-lead) registered in `projects/INDEX.md` ->
///     restores `runs/<slug>-lead.owner` and tags `grid_role=orchestrator grid_owner=<slug>-lead`
/// (b) Worker: a herdr-named agent with `runs/<name>.owner`, `runs/<name>.grid`, and `runs/<name>.repo` ->
///     re-report its `grid_*` tokens (`grid_role=worker`, `grid_group`, `grid_owner`, `grid_tab`)
/// (c) CoS: tag `cos` ONLY when the pane's herdr-persisted `agent_session.value` matches
///     `runs/cos.identity.json`, the pane has no herdr agent name, and it carries no `grid_role`.
///     At most one CoS pane is ever crowned; multiple matches resolve via recorded pane ID or fail closed.
///     A pane already carrying `grid_role=cos` but no `grid_farm` also gets `grid_farm=<local farm>`,
///     but only when its cwd places it inside this farm (a foreign CoS pane is never stamped).
/// Every other case: no-op (fail closed).
pub fn heal<H: HerdrOps>(herdr: &H) -> Result<HealReport, String> {
    heal_with_runs_dir(
        herdr,
        &crate::worker::cleanup::default_runs_dir(),
        None,
        false,
    )
}

pub fn heal_dry_run<H: HerdrOps>(herdr: &H) -> Result<HealReport, String> {
    heal_with_runs_dir(
        herdr,
        &crate::worker::cleanup::default_runs_dir(),
        None,
        true,
    )
}

pub fn heal_with_runs_dir<H: HerdrOps>(
    herdr: &H,
    runs_dir: &Path,
    filter_pane: Option<&str>,
    dry_run: bool,
) -> Result<HealReport, String> {
    let agents_val = match crate::lead::tag::get_agent_list_json(herdr) {
        Some(val) => val,
        None => return Ok(HealReport::default()),
    };
    let agents = match agents_val
        .pointer("/result/agents")
        .and_then(|a| a.as_array())
    {
        Some(a) => a,
        None => return Ok(HealReport::default()),
    };

    // Farm of the runs_dir this heal was given: a heal never stamps another workspace's id
    // onto a pane, and a pane is only "already tagged" when its token names this farm.
    let local_farm = crate::workspace::farm_of_runs_dir(runs_dir);

    let mut cos_ident = crate::cos::load_cos_identity(runs_dir);

    let mut actions = Vec::new();
    let mut errors = Vec::new();
    let mut cos_candidates = Vec::new();
    for agent in agents {
        let pane_id = match agent.get("pane_id").and_then(|p| p.as_str()) {
            Some(p) if !p.is_empty() => p,
            _ => continue,
        };

        if let Some(fp) = filter_pane {
            if pane_id != fp {
                continue;
            }
        }

        let mut agent_name = agent
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        if agent_name.is_empty() {
            if let Some(tokens) = agent.get("tokens").and_then(|t| t.as_object()) {
                if tokens.get("grid_role").and_then(|v| v.as_str()) == Some("orchestrator") {
                    if let Some(owner) = tokens.get("grid_owner").and_then(|v| v.as_str()) {
                        if owner.ends_with("-lead") {
                            agent_name = owner.to_string();
                        }
                    }
                }
            }
        }

        // (a) Lead — an agent named `<slug>-lead` (or carrying grid_owner=<slug>-lead)
        if agent_name.ends_with("-lead") && agent_name.len() > "-lead".len() {
            let slug = &agent_name[..agent_name.len() - "-lead".len()];
            let owner_file = runs_dir.join(format!("{agent_name}.owner"));
            let is_registered = is_registered_lead(runs_dir, slug, &agent_name);
            if is_registered || owner_file.is_file() {
                let mut already_tagged = false;
                if let Ok(out) = herdr.run(&["pane", "get", pane_id]) {
                    if out.status.success() {
                        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                            if let Some(tokens) = val.pointer("/result/pane/tokens") {
                                let has_role = tokens.get("grid_role").and_then(|v| v.as_str())
                                    == Some("orchestrator");
                                let has_owner = tokens.get("grid_owner").and_then(|v| v.as_str())
                                    == Some(&agent_name);
                                let has_farm = tokens.get("grid_farm").and_then(|v| v.as_str())
                                    == Some(local_farm.as_str());
                                if has_role && has_owner && has_farm {
                                    already_tagged = true;
                                }
                            }
                        }
                    }
                }

                let owner_missing = !owner_file.is_file();
                if owner_missing && !dry_run {
                    if let Err(e) = std::fs::write(&owner_file, "cos\n") {
                        errors.push(format!("failed to restore {}: {e}", owner_file.display()));
                    }
                }

                if !already_tagged {
                    if dry_run {
                        actions.push(format!(
                            "[dry-run] healed lead '{slug}' (pane {pane_id}): tagged grid_role=orchestrator grid_owner={agent_name} grid_farm={local_farm}"
                        ));
                    } else {
                        match crate::lead::tag::tag_lead_pane(herdr, slug, pane_id, &local_farm) {
                            Ok(_) => {
                                actions.push(format!(
                                    "healed lead '{slug}' (pane {pane_id}): tagged grid_role=orchestrator grid_owner={agent_name} grid_farm={local_farm}"
                                ));
                            }
                            Err(e) => {
                                errors.push(format!(
                                    "failed to heal lead '{slug}' (pane {pane_id}): {e}"
                                ));
                            }
                        }
                    }
                } else if owner_missing {
                    let prefix = if dry_run { "[dry-run] " } else { "" };
                    actions.push(format!(
                        "{prefix}healed lead '{slug}' (pane {pane_id}): restored {agent_name}.owner=cos"
                    ));
                }
                continue;
            }
        }

        // (b) Worker — a herdr-named agent with `runs/<name>.owner`, `runs/<name>.grid`, and `runs/<name>.repo`
        if !agent_name.is_empty() && !agent_name.ends_with("-lead") {
            let owner_file = runs_dir.join(format!("{agent_name}.owner"));
            let grid_file = runs_dir.join(format!("{agent_name}.grid"));
            let repo_file = runs_dir.join(format!("{agent_name}.repo"));
            if owner_file.is_file() && grid_file.is_file() && repo_file.is_file() {
                let owner = std::fs::read_to_string(&owner_file)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let grid_tab = std::fs::read_to_string(&grid_file)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let repo_content = std::fs::read_to_string(&repo_file)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let group = Path::new(&repo_content)
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();

                if !owner.is_empty() && !grid_tab.is_empty() && !group.is_empty() {
                    let farm_id = local_farm.as_str();
                    let mut already_tagged = false;
                    if let Ok(out) = herdr.run(&["pane", "get", pane_id]) {
                        if out.status.success() {
                            if let Ok(val) =
                                serde_json::from_slice::<serde_json::Value>(&out.stdout)
                            {
                                if let Some(tokens) = val.pointer("/result/pane/tokens") {
                                    let has_role = tokens.get("grid_role").and_then(|v| v.as_str())
                                        == Some("worker");
                                    let has_owner =
                                        tokens.get("grid_owner").and_then(|v| v.as_str())
                                            == Some(&owner);
                                    let has_tab = tokens.get("grid_tab").and_then(|v| v.as_str())
                                        == Some(&grid_tab);
                                    let has_group =
                                        tokens.get("grid_group").and_then(|v| v.as_str())
                                            == Some(&group);
                                    let has_farm = tokens.get("grid_farm").and_then(|v| v.as_str())
                                        == Some(farm_id);
                                    if has_role && has_owner && has_tab && has_group && has_farm {
                                        already_tagged = true;
                                    }
                                }
                            }
                        }
                    }
                    if !already_tagged {
                        let role_tok = "grid_role=worker";
                        let owner_tok = format!("grid_owner={owner}");
                        let tab_tok = format!("grid_tab={grid_tab}");
                        let group_tok = format!("grid_group={group}");
                        let farm_tok = format!("grid_farm={farm_id}");

                        let tags = [
                            "pane",
                            "report-metadata",
                            pane_id,
                            "--source",
                            "herdr-quaqua",
                            "--token",
                            role_tok,
                            "--token",
                            &group_tok,
                            "--token",
                            &owner_tok,
                            "--token",
                            &tab_tok,
                            "--token",
                            &farm_tok,
                        ];
                        if dry_run {
                            actions.push(format!(
                                "[dry-run] healed worker '{agent_name}' (pane {pane_id}): tagged grid_role=worker grid_owner={owner} grid_farm={farm_id}"
                            ));
                        } else {
                            match herdr.run(&tags) {
                                Ok(out) if out.status.success() => {
                                    actions.push(format!(
                                        "healed worker '{agent_name}' (pane {pane_id}): tagged grid_role=worker grid_owner={owner} grid_farm={farm_id}"
                                    ));
                                }
                                Ok(out) => {
                                    let stderr = String::from_utf8_lossy(&out.stderr);
                                    errors.push(format!(
                                        "failed to tag worker pane {pane_id}: {stderr}"
                                    ));
                                }
                                Err(e) => {
                                    errors
                                        .push(format!("failed to tag worker pane {pane_id}: {e}"));
                                }
                            }
                        }
                    }
                    continue;
                }
            }
        }

        // (c) CoS candidate collection — unnamed pane
        if agent_name.is_empty() {
            let mut carries_grid_role = false;
            let mut is_cos_tagged = false;
            let mut cos_role_without_farm = false;
            if let Some(tokens) = agent.get("tokens").and_then(|t| t.as_object()) {
                if tokens.get("grid_role").and_then(|v| v.as_str()) == Some("cos") {
                    is_cos_tagged = true;
                }
                if tokens.contains_key("grid_role") {
                    carries_grid_role = true;
                }
            }
            let mut pane_session = agent
                .get("agent_session")
                .and_then(|s| {
                    s.get("value")
                        .and_then(|v| v.as_str())
                        .or_else(|| s.as_str())
                })
                .map(String::from);

            if let Ok(out) = herdr.run(&["pane", "get", pane_id]) {
                if out.status.success() {
                    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                        if let Some(tokens) = val
                            .pointer("/result/pane/tokens")
                            .and_then(|t| t.as_object())
                        {
                            if tokens.get("grid_role").and_then(|v| v.as_str()) == Some("cos") {
                                is_cos_tagged = true;
                                cos_role_without_farm =
                                    tokens.get("grid_farm").and_then(|v| v.as_str()).is_none();
                            }
                            if tokens.contains_key("grid_role") {
                                carries_grid_role = true;
                            }
                        }
                        if pane_session.is_none() {
                            pane_session = val
                                .pointer("/result/pane/agent_session/value")
                                .and_then(|v| v.as_str())
                                .or_else(|| {
                                    val.pointer("/result/pane/agent_session")
                                        .and_then(|v| v.as_str())
                                })
                                .map(String::from);
                        }

                        // A CoS pane tagged before farm stamping (or after herdr
                        // dropped the token) resolves as cos but carries no farm, so
                        // attribution falls back to its cwd. Add the farm only when
                        // the pane is provably local: a pane carrying another farm's
                        // token, or one whose cwd sits outside this farm, is not ours
                        // to stamp.
                        if cos_role_without_farm
                            && matches!(
                                crate::bus::events::attribute_pane(&val, &local_farm, runs_dir),
                                crate::bus::events::PaneAttribution::Local { .. }
                            )
                        {
                            if dry_run {
                                actions.push(format!(
                                    "[dry-run] healed CoS (pane {pane_id}): tagged grid_farm={local_farm}"
                                ));
                            } else {
                                match crate::cos::stamp_cos_farm(herdr, pane_id, &local_farm) {
                                    Ok(_) => actions.push(format!(
                                        "healed CoS (pane {pane_id}): tagged grid_farm={local_farm}"
                                    )),
                                    Err(e) => errors
                                        .push(format!("failed to tag CoS pane {pane_id}: {e}")),
                                }
                            }
                        }
                    }
                }
            }

            // If runs/cos.identity.json is missing, but this pane carries grid_role=cos and agent_session, restore it
            if cos_ident.is_none() && is_cos_tagged {
                if let Some(sess) = &pane_session {
                    let prefix = if dry_run { "[dry-run] " } else { "" };
                    if !dry_run {
                        let ident_json = serde_json::json!({
                            "agent_session": sess,
                            "pane_id": pane_id,
                        });
                        let ident_path = runs_dir.join(crate::cos::COS_IDENTITY_FILE);
                        if let Ok(content) = serde_json::to_string_pretty(&ident_json) {
                            let _ = std::fs::write(&ident_path, content);
                        }
                    }
                    actions.push(format!(
                        "{prefix}healed CoS (pane {pane_id}): restored cos.identity.json"
                    ));
                    cos_ident = Some((sess.clone(), pane_id.to_string()));
                }
            }

            if let Some((expected_session, _)) = &cos_ident {
                if !carries_grid_role {
                    if let Some(sess) = &pane_session {
                        if sess == expected_session {
                            cos_candidates.push(pane_id.to_string());
                        }
                    }
                }
            }
        }
    }

    // Resolve CoS: at most one pane crowned, ever
    if let Some((expected_session, recorded_pane)) = &cos_ident {
        if cos_candidates.len() == 1 {
            let target = &cos_candidates[0];
            if dry_run {
                actions.push(format!(
                    "[dry-run] healed CoS (pane {target}): tagged grid_role=cos"
                ));
            } else {
                match crate::cos::tag_cos_pane_in(herdr, target, runs_dir) {
                    Ok(_) => {
                        actions.push(format!("healed CoS (pane {target}): tagged grid_role=cos"));
                    }
                    Err(e) => {
                        errors.push(format!("failed to tag CoS pane {target}: {e}"));
                    }
                }
            }
        } else if cos_candidates.len() > 1 {
            let recorded = recorded_pane.trim();
            if !recorded.is_empty() && cos_candidates.iter().any(|p| p == recorded) {
                if dry_run {
                    actions.push(format!(
                        "[dry-run] healed CoS (pane {recorded}): tagged grid_role=cos"
                    ));
                } else {
                    match crate::cos::tag_cos_pane_in(herdr, recorded, runs_dir) {
                        Ok(_) => {
                            actions.push(format!(
                                "healed CoS (pane {recorded}): tagged grid_role=cos"
                            ));
                        }
                        Err(e) => {
                            errors.push(format!("failed to tag CoS pane {recorded}: {e}"));
                        }
                    }
                }
            } else {
                let cand_list = cos_candidates.join(", ");
                errors.push(format!(
                    "ambiguous CoS identity: multiple panes ({cand_list}) match session '{expected_session}' but none match recorded pane '{recorded}'; run `quaqua cos tag` in the intended CoS pane"
                ));
            }
        }
    }

    Ok(HealReport { actions, errors })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TestDir(std::path::PathBuf);
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_resolve_token_cases() {
        // 1. worker -> Peer
        let mut worker_tokens = HashMap::new();
        worker_tokens.insert("grid_role".to_string(), "worker".to_string());
        assert_eq!(resolve(&worker_tokens), Role::Peer);

        // 2. orchestrator -> Lead
        let mut orch_tokens = HashMap::new();
        orch_tokens.insert("grid_role".to_string(), "orchestrator".to_string());
        assert_eq!(resolve(&orch_tokens), Role::Lead);

        // 3. cos -> Cos
        let mut cos_tokens = HashMap::new();
        cos_tokens.insert("grid_role".to_string(), "cos".to_string());
        assert_eq!(resolve(&cos_tokens), Role::Cos);

        // 4. none / missing grid_role -> Unknown (NEVER default to Cos!)
        let empty_tokens = HashMap::new();
        assert_eq!(resolve(&empty_tokens), Role::Unknown);

        // 4b. explicit "none" -> Unknown
        let mut none_tokens = HashMap::new();
        none_tokens.insert("grid_role".to_string(), "none".to_string());
        assert_eq!(resolve(&none_tokens), Role::Unknown);

        // 4c. unrecognized role -> Unknown
        let mut other_tokens = HashMap::new();
        other_tokens.insert("grid_role".to_string(), "random".to_string());
        assert_eq!(resolve(&other_tokens), Role::Unknown);

        // 4d. resolve_opt(None) -> Unknown
        assert_eq!(resolve_opt(None), Role::Unknown);
    }

    #[test]
    fn test_depth_walk_cycle_detection() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let runs_dir = std::env::temp_dir().join(format!(
            "quaqua-identity-test-{}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&runs_dir);
        fs::create_dir_all(&runs_dir).unwrap();
        let _guard = TestDir(runs_dir.clone());

        // Chain: worker -> lead -> cos
        fs::write(runs_dir.join("worker.owner"), "lead\n").unwrap();
        fs::write(runs_dir.join("lead.owner"), "cos\n").unwrap();

        assert_eq!(depth_walk(&runs_dir, "cos"), Ok(0));
        assert_eq!(depth_walk(&runs_dir, "lead"), Ok(1));
        assert_eq!(depth_walk(&runs_dir, "worker"), Ok(2));

        // Self cycle: a -> a
        fs::write(runs_dir.join("self.owner"), "self\n").unwrap();
        let res = depth_walk(&runs_dir, "self");
        assert!(res.is_err(), "self cycle should return Err");
        let err = res.unwrap_err();
        assert!(err.is_cycle(), "expected cycle error, got: {:?}", err);

        // 2-node cycle: a -> b -> a
        fs::write(runs_dir.join("cyc_a.owner"), "cyc_b\n").unwrap();
        fs::write(runs_dir.join("cyc_b.owner"), "cyc_a\n").unwrap();
        let res = depth_walk(&runs_dir, "cyc_a");
        assert!(res.is_err(), "2-node cycle should return Err");
        assert!(res.unwrap_err().is_cycle());

        // 3-node cycle: c1 -> c2 -> c3 -> c1
        fs::write(runs_dir.join("c1.owner"), "c2\n").unwrap();
        fs::write(runs_dir.join("c2.owner"), "c3\n").unwrap();
        fs::write(runs_dir.join("c3.owner"), "c1\n").unwrap();
        let res = depth_walk(&runs_dir, "c1");
        assert!(res.is_err(), "3-node cycle should return Err");
        assert!(res.unwrap_err().is_cycle());

        // Missing owner file
        let res = depth_walk(&runs_dir, "missing_entity");
        assert_eq!(res, Err(DepthError::NotFound("missing_entity".to_string())));
    }
    struct MockHerdrWithTokens {
        tokens_json: String,
    }
    impl HerdrOps for MockHerdrWithTokens {
        fn run(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
            let stdout = if args.first() == Some(&"pane") && args.get(1) == Some(&"get") {
                format!(
                    r#"{{"result":{{"pane":{{"pane_id":"w1:p1","tokens":{}}}}}}}"#,
                    self.tokens_json
                )
            } else {
                r#"{"result":{}}"#.to_string()
            };
            Ok(std::process::Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: stdout.into_bytes(),
                stderr: Vec::new(),
            })
        }
        fn agent_list(&self) -> std::io::Result<String> {
            Ok(r#"{"result":{"agents":[]}}"#.to_string())
        }
        fn agent_get(&self, _name: &str) -> std::io::Result<(bool, String)> {
            unimplemented!()
        }
        fn agent_read(&self, _n: &str, _s: &str, _l: u32) -> std::io::Result<String> {
            unimplemented!()
        }
        fn agent_start(
            &self,
            _n: &str,
            _k: &str,
            _p: &str,
            _t: u32,
            _a: &[&str],
        ) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn pane_split(
            &self,
            _c: &str,
            _o: &str,
            _e: &[(&str, &str)],
        ) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn pane_close(&self, _p: &str) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn pane_report_metadata(
            &self,
            _p: &str,
            _t: &[&str],
        ) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn pane_wait_output(&self, _p: &str, _m: &str, _t: u32) -> bool {
            unimplemented!()
        }
        fn pane_read(&self, _p: &str, _s: &str, _l: u32) -> std::io::Result<String> {
            unimplemented!()
        }
        fn pane_send_keys(&self, _p: &str, _k: &str) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn agent_send_keys(&self, _n: &str, _key: &str) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn agent_wait_idle(&self, _n: &str, _t: u32) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn agent_prompt_nowait(&self, _n: &str, _p: &str) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn agent_prompt_wait_background(
            &self,
            _n: &str,
            _p: &str,
            _t: u32,
            _f: std::fs::File,
        ) -> std::io::Result<std::process::Child> {
            unimplemented!()
        }
    }

    #[test]
    fn test_resolve_caller_with_pane_tokens() {
        let _lock = crate::test_env::lock_env();
        let _pane_guard = crate::test_env::TestEnvVarGuard::set("HERDR_PANE_ID", "w1:p1");
        // The FailingHerdr case below must reach "no identity"; an inherited
        // QUAQUA_OWNER (set in a CoS pane) would resolve through the env fallback.
        let _owner_guard = crate::test_env::TestEnvVarGuard::remove("QUAQUA_OWNER");

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let runs_dir = std::env::temp_dir().join(format!(
            "quaqua-identity-pane-{}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&runs_dir);
        fs::create_dir_all(&runs_dir).unwrap();
        let _guard = TestDir(runs_dir.clone());

        fs::write(runs_dir.join("my-lead.owner"), "cos\n").unwrap();

        // CoS pane
        let cos_herdr = MockHerdrWithTokens {
            tokens_json: r#"{"grid_role":"cos"}"#.to_string(),
        };
        let caller = resolve_caller(&cos_herdr, &runs_dir);
        assert_eq!(caller.role, Role::Cos);
        assert_eq!(caller.name, "cos");
        assert_eq!(caller.depth, CallerDepth::Resolved(0));
        assert_eq!(caller.source, IdentitySource::PaneTokens);
        assert_eq!(
            format!("{caller}"),
            "name=cos, role=cos, depth=0, source=tokens"
        );

        // Lead pane
        let lead_herdr = MockHerdrWithTokens {
            tokens_json: r#"{"grid_role":"orchestrator","grid_owner":"my-lead"}"#.to_string(),
        };
        let caller = resolve_caller(&lead_herdr, &runs_dir);
        assert_eq!(caller.role, Role::Lead);
        assert_eq!(caller.name, "my-lead");
        assert_eq!(caller.depth, CallerDepth::Resolved(1));
        assert_eq!(caller.source, IdentitySource::PaneTokens);
        assert_eq!(
            format!("{caller}"),
            "name=my-lead, role=lead, depth=1, source=tokens"
        );

        // Peer pane
        let peer_herdr = MockHerdrWithTokens {
            tokens_json: r#"{"grid_role":"worker","grid_owner":"my-lead","grid_name":"sude-task"}"#
                .to_string(),
        };
        let caller = resolve_caller(&peer_herdr, &runs_dir);
        assert_eq!(caller.role, Role::Peer);
        assert_eq!(caller.name, "sude-task");
        assert_eq!(caller.depth, CallerDepth::Resolved(2));
        assert_eq!(caller.source, IdentitySource::PaneTokens);
        assert_eq!(
            format!("{caller}"),
            "name=sude-task, role=peer, depth=2, source=tokens"
        );

        // R4: Orchestrator pane without grid_owner -> name is "unknown", never "cos"
        let no_owner_lead = MockHerdrWithTokens {
            tokens_json: r#"{"grid_role":"orchestrator"}"#.to_string(),
        };
        let caller = resolve_caller(&no_owner_lead, &runs_dir);
        assert_eq!(caller.role, Role::Lead);
        assert_eq!(caller.name, "unknown");

        // R3: Cycle in depth walk logged as ?(cycle ...)
        fs::write(runs_dir.join("cyc-lead.owner"), "cyc-lead\n").unwrap();
        let cyc_herdr = MockHerdrWithTokens {
            tokens_json: r#"{"grid_role":"orchestrator","grid_owner":"cyc-lead"}"#.to_string(),
        };
        let caller = resolve_caller(&cyc_herdr, &runs_dir);
        assert_eq!(
            caller.depth,
            CallerDepth::Cycle(vec!["cyc-lead".into(), "cyc-lead".into()])
        );
        assert_eq!(
            format!("{caller}"),
            "name=cyc-lead, role=lead, depth=?(cycle cyc-lead->cyc-lead), source=tokens"
        );

        // R3: Missing owner file logged as ?(missing ...)
        let missing_herdr = MockHerdrWithTokens {
            tokens_json: r#"{"grid_role":"orchestrator","grid_owner":"ghost-lead"}"#.to_string(),
        };
        let caller = resolve_caller(&missing_herdr, &runs_dir);
        assert_eq!(
            caller.depth,
            CallerDepth::MissingOwner("ghost-lead".to_string())
        );
        assert_eq!(
            format!("{caller}"),
            "name=ghost-lead, role=lead, depth=?(missing ghost-lead), source=tokens"
        );
        // Role::Unknown with grid_owner -> keeps grid_owner as name, depth=?
        let unknown_with_owner = MockHerdrWithTokens {
            tokens_json: r#"{"grid_role":"custom","grid_owner":"some-owner"}"#.to_string(),
        };
        let caller = resolve_caller(&unknown_with_owner, &runs_dir);
        assert_eq!(caller.role, Role::Unknown);
        assert_eq!(caller.name, "some-owner");
        assert_eq!(caller.depth, CallerDepth::Unknown);
        assert_eq!(caller.source, IdentitySource::PaneTokens);

        // Failing herdr pane get -> returns fallback Unknown
        struct FailingHerdr;
        impl HerdrOps for FailingHerdr {
            fn run(&self, _args: &[&str]) -> std::io::Result<std::process::Output> {
                Ok(std::process::Output {
                    status: std::os::unix::process::ExitStatusExt::from_raw(1),
                    stdout: Vec::new(),
                    stderr: b"pane not found".to_vec(),
                })
            }
            fn agent_list(&self) -> std::io::Result<String> {
                unimplemented!()
            }
            fn agent_get(&self, _name: &str) -> std::io::Result<(bool, String)> {
                unimplemented!()
            }
            fn agent_read(&self, _n: &str, _s: &str, _l: u32) -> std::io::Result<String> {
                unimplemented!()
            }
            fn agent_start(
                &self,
                _n: &str,
                _k: &str,
                _p: &str,
                _t: u32,
                _a: &[&str],
            ) -> std::io::Result<std::process::Output> {
                unimplemented!()
            }
            fn pane_split(
                &self,
                _c: &str,
                _o: &str,
                _e: &[(&str, &str)],
            ) -> std::io::Result<std::process::Output> {
                unimplemented!()
            }
            fn pane_close(&self, _p: &str) -> std::io::Result<std::process::Output> {
                unimplemented!()
            }
            fn pane_report_metadata(
                &self,
                _p: &str,
                _t: &[&str],
            ) -> std::io::Result<std::process::Output> {
                unimplemented!()
            }
            fn pane_wait_output(&self, _p: &str, _m: &str, _t: u32) -> bool {
                unimplemented!()
            }
            fn pane_read(&self, _p: &str, _s: &str, _l: u32) -> std::io::Result<String> {
                unimplemented!()
            }
            fn pane_send_keys(&self, _p: &str, _k: &str) -> std::io::Result<std::process::Output> {
                unimplemented!()
            }
            fn agent_send_keys(
                &self,
                _n: &str,
                _key: &str,
            ) -> std::io::Result<std::process::Output> {
                unimplemented!()
            }
            fn agent_wait_idle(&self, _n: &str, _t: u32) -> std::io::Result<std::process::Output> {
                unimplemented!()
            }
            fn agent_prompt_nowait(
                &self,
                _n: &str,
                _p: &str,
            ) -> std::io::Result<std::process::Output> {
                unimplemented!()
            }
            fn agent_prompt_wait_background(
                &self,
                _n: &str,
                _p: &str,
                _t: u32,
                _f: std::fs::File,
            ) -> std::io::Result<std::process::Child> {
                unimplemented!()
            }
        }
        let caller = resolve_caller(&FailingHerdr, &runs_dir);
        assert_eq!(caller.role, Role::Unknown);
        assert_eq!(caller.name, "unknown");
        assert_eq!(caller.depth, CallerDepth::Unknown);
        assert_eq!(caller.source, IdentitySource::Unknown);
    }

    #[test]
    fn test_resolve_caller_env_fallback() {
        let _lock = crate::test_env::lock_env();
        let _pane_guard = crate::test_env::TestEnvVarGuard::set("HERDR_PANE_ID", "");

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let runs_dir = std::env::temp_dir().join(format!(
            "quaqua-identity-env-{}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&runs_dir);
        fs::create_dir_all(&runs_dir).unwrap();
        let _guard = TestDir(runs_dir.clone());
        fs::write(runs_dir.join("env-lead.owner"), "cos\n").unwrap();

        let dummy_herdr = MockHerdrWithTokens {
            tokens_json: "{}".to_string(),
        };

        // Without QUAQUA_TEST=1, env fallback is forbidden (R2 fail-closed)
        {
            let _test_guard = crate::test_env::TestEnvVarGuard::remove("QUAQUA_TEST");
            let _owner_guard = crate::test_env::TestEnvVarGuard::set("QUAQUA_OWNER", "cos");
            let caller = resolve_caller(&dummy_herdr, &runs_dir);
            assert_eq!(caller.role, Role::Unknown);
            assert_eq!(caller.name, "unknown");
            assert_eq!(caller.depth, CallerDepth::Unknown);
            assert_eq!(caller.source, IdentitySource::Unknown);
            assert_eq!(
                format!("{caller}"),
                "name=unknown, role=unknown, depth=?, source=unknown"
            );
        }

        // Under QUAQUA_TEST=1, env fallback is active
        let _test_guard = crate::test_env::TestEnvVarGuard::set("QUAQUA_TEST", "1");

        // QUAQUA_OWNER = "cos"
        {
            let _owner_guard = crate::test_env::TestEnvVarGuard::set("QUAQUA_OWNER", "cos");
            let caller = resolve_caller(&dummy_herdr, &runs_dir);
            assert_eq!(caller.role, Role::Cos);
            assert_eq!(caller.name, "cos");
            assert_eq!(caller.depth, CallerDepth::Resolved(0));
            assert_eq!(caller.source, IdentitySource::EnvFallback);
            assert_eq!(
                format!("{caller}"),
                "name=cos, role=cos, depth=0, source=env"
            );
        }

        // QUAQUA_OWNER = "env-lead"
        {
            let _owner_guard = crate::test_env::TestEnvVarGuard::set("QUAQUA_OWNER", "env-lead");
            let caller = resolve_caller(&dummy_herdr, &runs_dir);
            assert_eq!(caller.role, Role::Lead);
            assert_eq!(caller.name, "env-lead");
            assert_eq!(caller.depth, CallerDepth::Resolved(1));
            assert_eq!(caller.source, IdentitySource::EnvFallback);
            assert_eq!(
                format!("{caller}"),
                "name=env-lead, role=lead, depth=1, source=env"
            );
        }

        // QUAQUA_OWNER unset
        {
            let _owner_guard = crate::test_env::TestEnvVarGuard::set("QUAQUA_OWNER", "");
            let caller = resolve_caller(&dummy_herdr, &runs_dir);
            assert_eq!(caller.role, Role::Unknown);
            assert_eq!(caller.name, "unknown");
            assert_eq!(caller.depth, CallerDepth::Unknown);
            assert_eq!(caller.source, IdentitySource::Unknown);
            assert_eq!(
                format!("{caller}"),
                "name=unknown, role=unknown, depth=?, source=unknown"
            );
        }
    }

    #[test]
    fn test_depth_walk_whitespace_and_longer_cycle() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let runs_dir = std::env::temp_dir().join(format!(
            "quaqua-identity-edge-{}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&runs_dir);
        fs::create_dir_all(&runs_dir).unwrap();
        let _guard = TestDir(runs_dir.clone());

        // Trailing whitespace and newline in owner files
        fs::write(runs_dir.join("w1.owner"), "  l1  \n\n").unwrap();
        fs::write(runs_dir.join("l1.owner"), "\ncos\r\n").unwrap();
        assert_eq!(depth_walk(&runs_dir, "  w1  "), Ok(2));
        assert_eq!(depth_walk(&runs_dir, "l1"), Ok(1));

        // Cycle with lead-in: x -> y -> a -> b -> a
        fs::write(runs_dir.join("x.owner"), "y\n").unwrap();
        fs::write(runs_dir.join("y.owner"), "a\n").unwrap();
        fs::write(runs_dir.join("a.owner"), "b\n").unwrap();
        fs::write(runs_dir.join("b.owner"), "a\n").unwrap();
        let res = depth_walk(&runs_dir, "x");
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert!(err.is_cycle());
        assert_eq!(
            err,
            DepthError::Cycle(vec![
                "x".into(),
                "y".into(),
                "a".into(),
                "b".into(),
                "a".into()
            ])
        );
    }
}
