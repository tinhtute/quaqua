use crate::config::Config;
use crate::herdr::HerdrOps;
use crate::identity::{Caller, CallerDepth, Role};
use crate::layout::model::{Decision, Layout, RoleClass, TraceStep, Verb};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};

/// Persistent layout role marker helpers:
pub fn layout_role_path(runs_dir: &Path, worker_name: &str) -> PathBuf {
    runs_dir.join(format!("{worker_name}.layout_role"))
}

pub fn record_layout_role(
    runs_dir: &Path,
    worker_name: &str,
    role_name: &str,
) -> std::io::Result<()> {
    std::fs::write(
        layout_role_path(runs_dir, worker_name),
        format!("{role_name}\n"),
    )
}

pub fn read_layout_role(runs_dir: &Path, worker_name: &str) -> Option<String> {
    let p = layout_role_path(runs_dir, worker_name);
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
/// Origin of a worker spawn invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SpawnOrigin {
    Cli,
    Pipeline,
    Activity,
    TaskforceMember,
}

impl fmt::Display for SpawnOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpawnOrigin::Cli => write!(f, "cli"),
            SpawnOrigin::Pipeline => write!(f, "pipeline"),
            SpawnOrigin::Activity => write!(f, "activity"),
            SpawnOrigin::TaskforceMember => write!(f, "taskforce-member"),
        }
    }
}

/// Resolves the layout role name for a caller within a layout.
///
/// Resolution order:
/// 1. Recorded role in `runs/<caller.name>.layout_role` (if runs_dir is provided and file exists).
/// 2. Unique role in layout whose `class` matches `caller.role`.
/// 3. Returns Err("role.ambiguous") if >1 roles match the caller class without a recorded role.
/// 4. Returns Err("role.unknown") if 0 roles match.
pub fn resolve_caller_role<'a>(
    layout: &'a Layout,
    caller: &Caller,
    runs_dir: Option<&Path>,
) -> Result<&'a str, String> {
    if let Some(rdir) = runs_dir {
        if let Some(rec) = read_layout_role(rdir, &caller.name) {
            if layout.roles.contains_key(&rec) {
                return Ok(layout.roles.get_key_value(&rec).unwrap().0.as_str());
            }
        }
    }

    let target_class = match caller.role {
        Role::Cos => Some(RoleClass::Cos),
        Role::Lead => Some(RoleClass::Lead),
        Role::Peer => Some(RoleClass::Peer),
        Role::Unknown => None,
    };

    let Some(class) = target_class else {
        return Err("role.unknown".to_string());
    };

    let matches: Vec<&'a str> = layout
        .roles
        .iter()
        .filter_map(|(name, role)| {
            if role.class == class {
                Some(name.as_str())
            } else {
                None
            }
        })
        .collect();

    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err("role.unknown".to_string()),
        _ => Err("role.ambiguous".to_string()),
    }
}

/// Resolve the role name for a target string within a layout.
///
/// 1. Direct role name match in `layout.roles`.
/// 2. Suffix match (e.g. "foo-lead" -> "lead", "cos" -> "cos").
/// 3. Class match among caller role's `can_spawn` (if caller_role is provided and target is a class).
pub fn resolve_target_role<'a>(
    layout: &'a Layout,
    caller_role_name: Option<&str>,
    target: &'a str,
) -> Result<&'a str, String> {
    let target_trim = target.trim();
    if layout.roles.contains_key(target_trim) {
        return Ok(layout.roles.get_key_value(target_trim).unwrap().0.as_str());
    }
    if target_trim.ends_with("-lead") && layout.roles.contains_key("lead") {
        return Ok("lead");
    }
    if target_trim == "cos" {
        if layout.roles.contains_key("cos") {
            return Ok("cos");
        }
        if let Some((cos_name, _)) = layout.roles.iter().find(|(_, r)| r.class == RoleClass::Cos) {
            return Ok(cos_name.as_str());
        }
    }

    let requested_class = match target_trim {
        "cos" => Some(RoleClass::Cos),
        "lead" => Some(RoleClass::Lead),
        "peer" => Some(RoleClass::Peer),
        "taskforce" => Some(RoleClass::Taskforce),
        s if s.ends_with("-lead") => Some(RoleClass::Lead),
        _ => None,
    };

    if let Some(class) = requested_class {
        if let Some(cname) = caller_role_name {
            if let Some(crole) = layout.roles.get(cname) {
                let matches: Vec<&'a str> = crole
                    .can_spawn
                    .iter()
                    .filter_map(|tname| {
                        if let Some(trole) = layout.roles.get(tname) {
                            if trole.class == class {
                                return Some(tname.as_str());
                            }
                        }
                        None
                    })
                    .collect();

                match matches.len() {
                    1 => return Ok(matches[0]),
                    n if n > 1 => return Err("role.ambiguous".to_string()),
                    _ => {}
                }
            }
        }
    }

    Err("role.unknown".to_string())
}
/// Resolve the target role name and its Naming convention for a spawn call site.
///
/// Maps target class (e.g. "peer", "lead", "taskforce") or explicit role name
/// via `resolve_target_role`, returning the role name and its configured `Naming`.
pub fn resolve_target_role_and_naming<'a>(
    layout: &'a Layout,
    caller: &Caller,
    runs_dir: Option<&Path>,
    target_class_or_role: &'a str,
) -> Result<(&'a str, crate::layout::model::Naming), String> {
    let caller_r = resolve_caller_role(layout, caller, runs_dir).ok();
    let target_r = resolve_target_role(layout, caller_r, target_class_or_role)?;
    let naming = layout
        .roles
        .get(target_r)
        .map(|r| r.naming)
        .unwrap_or_default();
    Ok((target_r, naming))
}
fn role_class_rank(class: RoleClass) -> u8 {
    match class {
        RoleClass::Cos => 0,
        RoleClass::Lead => 1,
        RoleClass::Peer | RoleClass::Taskforce => 2,
    }
}

fn is_valid_role_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 24 {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return false;
    }
    for c in chars {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return false;
        }
    }
    true
}

fn check_no_cycles(layout: &Layout) -> Result<(), String> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unvisited,
        Visiting,
        Visited,
    }

    let mut state: BTreeMap<&str, State> = layout
        .roles
        .keys()
        .map(|k| (k.as_str(), State::Unvisited))
        .collect();

    fn dfs<'a>(
        u: &'a str,
        layout: &'a Layout,
        state: &mut BTreeMap<&'a str, State>,
        path: &mut Vec<&'a str>,
    ) -> Result<(), String> {
        state.insert(u, State::Visiting);
        path.push(u);

        if let Some(role) = layout.roles.get(u) {
            for v in &role.can_spawn {
                let v_str = v.as_str();
                match state.get(v_str) {
                    Some(State::Visiting) => {
                        let cycle_start = path.iter().position(|&x| x == v_str).unwrap_or(0);
                        let cycle = path[cycle_start..].join(" -> ") + " -> " + v_str;
                        return Err(format!(
                            "layout.cycle: cycle detected in can_spawn: {cycle}"
                        ));
                    }
                    Some(State::Unvisited) => {
                        dfs(v_str, layout, state, path)?;
                    }
                    _ => {}
                }
            }
        }

        path.pop();
        state.insert(u, State::Visited);
        Ok(())
    }

    let mut path = Vec::new();
    for role_name in layout.roles.keys() {
        if state.get(role_name.as_str()) == Some(&State::Unvisited) {
            dfs(role_name.as_str(), layout, &mut state, &mut path)?;
        }
    }

    Ok(())
}

fn check_reachability(layout: &Layout, cos_role_name: &str) -> Result<(), String> {
    let mut min_depth: BTreeMap<&str, u32> = BTreeMap::new();
    let mut queue: VecDeque<(&str, u32)> = VecDeque::new();

    min_depth.insert(cos_role_name, 0);
    queue.push_back((cos_role_name, 0));

    while let Some((u, d)) = queue.pop_front() {
        if let Some(role) = layout.roles.get(u) {
            for v in &role.can_spawn {
                let v_str = v.as_str();
                let next_d = d + 1;
                if !min_depth.contains_key(v_str) || next_d < min_depth[v_str] {
                    min_depth.insert(v_str, next_d);
                    queue.push_back((v_str, next_d));
                }
            }
        }
    }

    for role_name in layout.roles.keys() {
        let r_str = role_name.as_str();
        match min_depth.get(r_str) {
            Some(&d) => {
                if d > layout.depth {
                    return Err(format!(
                        "layout '{}' role '{r_str}' reachable only at depth {d} which exceeds layout depth {}",
                        layout.name, layout.depth
                    ));
                }
            }
            None => {
                return Err(format!(
                    "layout '{}' role '{r_str}' is unreachable from root '{cos_role_name}'",
                    layout.name
                ));
            }
        }
    }

    Ok(())
}

/// Pure structural layout validation (spec §3.2–3.4).
pub fn validate_layout(layout: &Layout) -> Result<(), String> {
    if layout.depth < 1 {
        return Err(format!("layout '{}' depth must be at least 1", layout.name));
    }
    if layout.roles.is_empty() {
        return Err(format!(
            "layout '{}' must define at least one role",
            layout.name
        ));
    }

    // Role naming and reserved keywords
    for role_name in layout.roles.keys() {
        if role_name == "owner" {
            return Err(format!(
                "layout '{}' uses reserved role name 'owner'",
                layout.name
            ));
        }
        if !is_valid_role_name(role_name) {
            return Err(format!(
                "layout '{}' role '{role_name}' name is invalid (must match ^[a-z][a-z0-9-]{{0,23}}$)",
                layout.name
            ));
        }
    }

    // Exactly one cos root
    let cos_roles: Vec<&String> = layout
        .roles
        .iter()
        .filter_map(|(name, role)| {
            if role.class == RoleClass::Cos {
                Some(name)
            } else {
                None
            }
        })
        .collect();

    if cos_roles.is_empty() {
        return Err(format!(
            "layout '{}' must define exactly one cos role",
            layout.name
        ));
    }
    if cos_roles.len() > 1 {
        return Err(format!(
            "layout '{}' defines multiple cos roles: {:?}",
            layout.name, cos_roles
        ));
    }
    let cos_role_name = cos_roles[0];
    let cos_role = &layout.roles[cos_role_name];
    if cos_role.reports_to.is_some() {
        return Err(format!(
            "layout '{}' cos role '{cos_role_name}' must not have reports_to",
            layout.name
        ));
    }

    // Referential integrity and role-level properties
    for (role_name, role) in &layout.roles {
        for target in &role.can_spawn {
            if !layout.roles.contains_key(target) {
                return Err(format!(
                    "layout '{}' role '{role_name}' can_spawn references unknown role '{target}'",
                    layout.name
                ));
            }
        }
        for target in &role.can_prompt {
            if !layout.roles.contains_key(target) {
                return Err(format!(
                    "layout '{}' role '{role_name}' can_prompt references unknown role '{target}'",
                    layout.name
                ));
            }
        }
        if let Some(rep) = &role.reports_to {
            if rep != "owner" && !layout.roles.contains_key(rep) {
                return Err(format!(
                    "layout '{}' role '{role_name}' reports_to references unknown role '{rep}'",
                    layout.name
                ));
            }
        }
        if let Some(cap) = role.max_children {
            if cap < 1 || cap > crate::config::CAP_CEILING {
                return Err(format!(
                    "layout '{}' role '{role_name}' max_children {cap} must be in 1..={}",
                    layout.name,
                    crate::config::CAP_CEILING
                ));
            }
        }
        if role.naming == crate::layout::model::Naming::SlugLead && role.class != RoleClass::Lead {
            return Err(format!(
                "layout '{}' role '{role_name}' naming 'slug-lead' only valid on lead class",
                layout.name
            ));
        }

        // Upward roles check: target class cannot outrank source class (rank cos=0 > lead=1 > peer/taskforce=2)
        for target_name in &role.can_spawn {
            if let Some(target_role) = layout.roles.get(target_name) {
                if role_class_rank(target_role.class) < role_class_rank(role.class) {
                    return Err(format!(
                        "layout.upward: layout '{}' role '{role_name}' ({}) cannot spawn higher-ranking role '{target_name}' ({})",
                        layout.name, role.class, target_role.class
                    ));
                }
            }
        }
        if let Some(rep) = &role.reports_to {
            if rep != "owner" {
                if let Some(parent_role) = layout.roles.get(rep) {
                    if role_class_rank(parent_role.class) > role_class_rank(role.class) {
                        return Err(format!(
                            "layout.upward: layout '{}' role '{role_name}' ({}) cannot report to lower-ranking role '{rep}' ({})",
                            layout.name, role.class, parent_role.class
                        ));
                    }
                }
            }
        }

        // Taskforce members
        if !role.members.is_empty() {
            if role.class != RoleClass::Taskforce {
                return Err(format!(
                    "layout '{}' role '{role_name}': members allowed only on taskforce class",
                    layout.name
                ));
            }
            if role.members.len() > 5 {
                return Err(format!(
                    "layout '{}' role '{role_name}': member count {} must be in 1..=5",
                    layout.name,
                    role.members.len()
                ));
            }
            for (idx, member) in role.members.iter().enumerate() {
                match (&member.purpose, &member.kind) {
                    (Some(_), None) | (None, Some(_)) => {}
                    (Some(_), Some(_)) => {
                        return Err(format!(
                            "layout '{}' role '{role_name}' member {}: purpose and kind cannot both be set",
                            layout.name,
                            idx + 1
                        ));
                    }
                    (None, None) => {
                        return Err(format!(
                            "layout '{}' role '{role_name}' member {}: must define purpose or kind",
                            layout.name,
                            idx + 1
                        ));
                    }
                }
            }
        }

        // Quorum
        if let Some(q) = role.quorum {
            if role.members.is_empty() {
                return Err(format!(
                    "layout '{}' role '{role_name}': quorum set but no members defined",
                    layout.name
                ));
            }
            if q < 1 || q > role.members.len() {
                return Err(format!(
                    "layout '{}' role '{role_name}': quorum {q} must be in 1..={}",
                    layout.name,
                    role.members.len()
                ));
            }
        }
    }

    // Shadowing built-in check for user layouts
    if crate::layout::builtin::is_builtin(&layout.name) {
        if let Some(builtin_l) = crate::layout::builtin::get(&layout.name) {
            if builtin_l != *layout {
                return Err(format!(
                    "layout.shadows_builtin: user layout '{}' shadows built-in layout",
                    layout.name
                ));
            }
        }
    }

    // Cycle detection on can_spawn graph
    check_no_cycles(layout)?;

    // Reachability and past-depth rejection from cos root
    check_reachability(layout, cos_role_name)?;

    Ok(())
}

/// Validates layout internal consistency and checks member purposes/kinds against config.
pub fn validate_layout_with_config(layout: &Layout, cfg: &Config) -> Result<(), String> {
    validate_layout(layout)?;
    for (role_name, role) in &layout.roles {
        for (idx, member) in role.members.iter().enumerate() {
            if let Some(p) = &member.purpose {
                if !cfg.purposes.contains_key(p) {
                    return Err(format!(
                        "layout '{}' role '{role_name}' member {} purpose '{p}' not found in config purposes",
                        layout.name,
                        idx + 1
                    ));
                }
            }
            if let Some(k) = &member.kind {
                if !cfg.kinds.contains_key(k) {
                    return Err(format!(
                        "layout '{}' role '{role_name}' member {} kind '{k}' not found in config kinds",
                        layout.name,
                        idx + 1
                    ));
                }
            }
        }
    }
    Ok(())
}

pub fn validate_user_layout(layout: &Layout) -> Result<(), String> {
    if crate::layout::builtin::is_builtin(&layout.name) {
        return Err(format!(
            "layout.shadows_builtin: user layout '{}' shadows built-in layout",
            layout.name
        ));
    }
    validate_layout(layout)
}

pub fn validate_user_layout_with_config(layout: &Layout, cfg: &Config) -> Result<(), String> {
    if crate::layout::builtin::is_builtin(&layout.name) {
        return Err(format!(
            "layout.shadows_builtin: user layout '{}' shadows built-in layout",
            layout.name
        ));
    }
    validate_layout_with_config(layout, cfg)
}

/// Pure layout gate check: layers identity -> role -> edge -> depth -> cap.
pub fn check(layout: &Layout, caller: &Caller, verb: Verb, target: &str) -> Decision {
    check_with_count(layout, caller, verb, target, None)
}

/// Pure layout gate check with optional current child count passed in.
pub fn check_with_count(
    layout: &Layout,
    caller: &Caller,
    verb: Verb,
    target: &str,
    current_children: Option<i64>,
) -> Decision {
    check_with_origin(layout, caller, verb, target, current_children, false)
}

/// Pure layout gate check with optional child count and edge-skip flag.
pub fn check_with_origin(
    layout: &Layout,
    caller: &Caller,
    verb: Verb,
    target: &str,
    current_children: Option<i64>,
    skip_edge: bool,
) -> Decision {
    check_with_runs_dir(
        layout,
        caller,
        verb,
        target,
        current_children,
        skip_edge,
        None,
    )
}

pub fn check_with_runs_dir(
    layout: &Layout,
    caller: &Caller,
    verb: Verb,
    target: &str,
    current_children: Option<i64>,
    skip_edge: bool,
    runs_dir: Option<&Path>,
) -> Decision {
    let mut trace = Vec::new();

    // 1. Identity layer
    if caller.role == Role::Unknown {
        trace.push(TraceStep {
            layer: "identity".to_string(),
            passed: false,
            detail: format!("caller '{}' identity is unknown", caller.name),
        });
        return Decision::deny("identity.unknown", trace);
    }
    trace.push(TraceStep {
        layer: "identity".to_string(),
        passed: true,
        detail: format!(
            "caller '{}' identified as {} (depth {})",
            caller.name, caller.role, caller.depth
        ),
    });

    // 2. Role layer (caller role and target role must exist in layout)
    let caller_role_name = match resolve_caller_role(layout, caller, runs_dir) {
        Ok(r) => {
            trace.push(TraceStep {
                layer: "role".to_string(),
                passed: true,
                detail: format!("role '{}' is defined in layout '{}'", r, layout.name),
            });
            r
        }
        Err(err_id) => {
            trace.push(TraceStep {
                layer: "role".to_string(),
                passed: false,
                detail: format!(
                    "caller '{}' role cannot be resolved in layout '{}': {err_id}",
                    caller.name, layout.name
                ),
            });
            return Decision::deny(err_id.as_str(), trace);
        }
    };
    let role = &layout.roles[caller_role_name];

    let target_role = match resolve_target_role(layout, Some(caller_role_name), target) {
        Ok(r) => r,
        Err(err_id) => {
            trace.push(TraceStep {
                layer: "role".to_string(),
                passed: false,
                detail: format!(
                    "target role '{target}' cannot be resolved in layout '{}': {err_id}",
                    layout.name
                ),
            });
            return Decision::deny(err_id.as_str(), trace);
        }
    };

    // 3. Edge layer
    let mut reports_to_edge = false;
    let edge_allowed = if skip_edge {
        trace.push(TraceStep {
            layer: "edge".to_string(),
            passed: true,
            detail: "edge check skipped".to_string(),
        });
        true
    } else {
        match verb {
            Verb::Spawn => role.can_spawn.iter().any(|r| r == target_role),
            Verb::Prompt => role.can_prompt.iter().any(|r| r == target_role),
            Verb::Mail => {
                let is_cos_target = target_role == "cos"
                    || layout
                        .roles
                        .get(target_role)
                        .map_or(false, |tr| tr.class == RoleClass::Cos);
                if role.can_prompt.iter().any(|r| r == target_role) {
                    true
                } else if role.reports_to.as_deref() == Some(target_role) {
                    reports_to_edge = true;
                    true
                } else if role.reports_to.as_deref() == Some("owner") && is_cos_target {
                    reports_to_edge = true;
                    true
                } else {
                    false
                }
            }
        }
    };

    if !edge_allowed {
        let detail = match verb {
            Verb::Spawn => format!(
                "role '{}' cannot spawn role '{}' (can_spawn = {:?})",
                caller_role_name, target_role, role.can_spawn
            ),
            Verb::Prompt => format!(
                "role '{}' cannot prompt role '{}' (can_prompt = {:?})",
                caller_role_name, target_role, role.can_prompt
            ),
            Verb::Mail => format!(
                "role '{}' cannot send mail to role '{}'",
                caller_role_name, target_role
            ),
        };
        trace.push(TraceStep {
            layer: "edge".to_string(),
            passed: false,
            detail,
        });
        return Decision::deny("edge.none", trace);
    }
    if !skip_edge {
        trace.push(TraceStep {
            layer: "edge".to_string(),
            passed: true,
            detail: format!(
                "{} edge permits '{}' -> '{}'",
                verb, caller_role_name, target_role
            ),
        });
    }

    // 4. Depth layer
    if verb == Verb::Spawn {
        match &caller.depth {
            CallerDepth::Resolved(d) => {
                let target_depth = d + 1;
                if target_depth > layout.depth {
                    trace.push(TraceStep {
                        layer: "depth".to_string(),
                        passed: false,
                        detail: format!(
                            "spawn target depth {} would exceed layout limit {}",
                            target_depth, layout.depth
                        ),
                    });
                    return Decision::deny("depth.exceeded", trace);
                }
                trace.push(TraceStep {
                    layer: "depth".to_string(),
                    passed: true,
                    detail: format!(
                        "target depth {} <= layout limit {}",
                        target_depth, layout.depth
                    ),
                });
            }
            CallerDepth::Cycle(cycle) => {
                trace.push(TraceStep {
                    layer: "depth".to_string(),
                    passed: false,
                    detail: format!("cycle in owner chain: {:?}", cycle),
                });
                return Decision::deny("depth.cycle", trace);
            }
            CallerDepth::MissingOwner(owner) => {
                trace.push(TraceStep {
                    layer: "depth".to_string(),
                    passed: false,
                    detail: format!("missing owner in chain: {}", owner),
                });
                return Decision::deny("depth.owner_missing", trace);
            }
            CallerDepth::Unknown => {
                let test_mode = std::env::var("QUAQUA_TEST").as_deref() == Ok("1");
                if is_shadow_mode() || test_mode {
                    trace.push(TraceStep {
                        layer: "depth".to_string(),
                        passed: true,
                        detail: "caller depth unknown (allowed in shadow/test)".to_string(),
                    });
                } else {
                    trace.push(TraceStep {
                        layer: "depth".to_string(),
                        passed: false,
                        detail: "caller depth unknown (denied in enforce mode)".to_string(),
                    });
                    return Decision::deny("depth.unknown", trace);
                }
            }
        }
    } else {
        trace.push(TraceStep {
            layer: "depth".to_string(),
            passed: true,
            detail: format!("verb '{}' does not create child roles", verb),
        });
    }

    // 5. Cap layer
    if let Some(count) = current_children {
        if verb == Verb::Spawn {
            if let Some(max) = role.max_children {
                if count >= max {
                    let source = if caller_role_name == "lead"
                        || role.class == RoleClass::Lead
                        || crate::cap::is_lead_owner(&caller.name)
                    {
                        "roles.lead.max_peers"
                    } else {
                        "limits.max_workers_per_orchestrator"
                    };
                    trace.push(TraceStep {
                        layer: "cap".to_string(),
                        passed: false,
                        detail: format!(
                            "worker cap exceeded ({count} live worker slots for owner '{}'; this spawn needs 1, {source} is {max})",
                            caller.name
                        ),
                    });
                    return Decision::deny("cap.exceeded", trace);
                }
                trace.push(TraceStep {
                    layer: "cap".to_string(),
                    passed: true,
                    detail: format!("role '{}' count {} < cap {}", caller_role_name, count, max),
                });
            } else {
                trace.push(TraceStep {
                    layer: "cap".to_string(),
                    passed: true,
                    detail: format!("role '{}' has no child cap", caller_role_name),
                });
            }
        } else {
            trace.push(TraceStep {
                layer: "cap".to_string(),
                passed: true,
                detail: format!("verb '{}' does not consume child slots", verb),
            });
        }
    } else {
        trace.push(TraceStep {
            layer: "cap".to_string(),
            passed: true,
            detail: "child count not passed (skipped)".to_string(),
        });
    }

    let rule_id = if reports_to_edge {
        format!(
            "{}.{}.reports_to.{}",
            layout.name, caller_role_name, target_role
        )
    } else {
        format!(
            "{}.{}.can_{}.{}",
            layout.name,
            caller_role_name,
            verb.as_str(),
            target_role
        )
    };

    Decision::allow(rule_id, trace)
}

/// One-line recovery hint appended to an `identity.unknown` deny.
///
/// `Role::Unknown` covers three states: no `grid_role`, an empty one, or a value
/// that is not `cos`/`orchestrator`/`worker` (the gate's documented `none`). The
/// hint therefore names the state it can actually read and keeps the herdr
/// restart as the usual cause, not as a fact. `$HERDR_PANE_ID` is read here
/// because the pure check (`check_with_runs_dir`) has no pane handle and its
/// signature is not widened for wording.
fn identity_unknown_hint() -> String {
    let pane = std::env::var("HERDR_PANE_ID")
        .ok()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    let subject = match pane {
        Some(p) => format!("pane '{p}' has {UNUSABLE_ROLE}"),
        None => format!("$HERDR_PANE_ID is unset and the pane has {UNUSABLE_ROLE}"),
    };
    format!(
        "hint: {subject} — usually herdr dropped the pane tokens on a server restart; \
         CoS: run `quaqua cos tag`; Lead: run `quaqua lead repair <slug>` (escape hatch: QUAQUA_LAYOUT_SHADOW=1)\n"
    )
}

const UNUSABLE_ROLE: &str = "no usable grid_role (missing, empty, or not cos|orchestrator|worker)";

/// Returns true if layout gate is running in shadow mode.
///
/// In P4, the default is enforce mode (false). Setting `QUAQUA_LAYOUT_SHADOW=1`
/// (or `"true"`) provides an escape hatch to restore shadow mode without blocking.
pub fn is_shadow_mode() -> bool {
    std::env::var("QUAQUA_LAYOUT_SHADOW")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Choke-point evaluation of layout rules in enforce mode (default) or shadow mode.
///
/// Under enforce mode: returns `Ok(decision)` if allowed; if denied, records a `denied`
/// FarmEvent in `runs_dir` and returns `Err((1, format!("error: [{}] {}\n", decision.rule_id, detail)))`.
/// Under shadow mode (`QUAQUA_LAYOUT_SHADOW=1`): prints `would_deny` to stderr and records
/// a `would_deny` FarmEvent, but returns `Ok(decision)` without blocking.
pub fn gate<H: HerdrOps>(
    caller: Option<&Caller>,
    herdr: Option<&H>,
    verb: Verb,
    target: &str,
    target_name: Option<&str>,
    origin: SpawnOrigin,
    owner: Option<&str>,
    project: Option<&str>,
    runs_dir: &Path,
    cfg: Option<&Config>,
) -> Result<Decision, (i32, String)> {
    gate_inner(
        caller,
        herdr,
        verb,
        target,
        target_name,
        origin,
        owner,
        project,
        runs_dir,
        cfg,
        false,
    )
}

/// Shadow mode evaluation of layout rules at a choke point.
///
/// Evaluates the decision, prints one `would_deny` stderr line and writes a
/// `farm_events` row if denied. Never blocks, never changes exit codes.
pub fn shadow<H: HerdrOps>(
    caller: Option<&Caller>,
    herdr: Option<&H>,
    verb: Verb,
    target: &str,
    target_name: Option<&str>,
    origin: SpawnOrigin,
    owner: Option<&str>,
    project: Option<&str>,
    runs_dir: &Path,
    cfg: Option<&Config>,
) -> Decision {
    gate_inner(
        caller,
        herdr,
        verb,
        target,
        target_name,
        origin,
        owner,
        project,
        runs_dir,
        cfg,
        true,
    )
    .unwrap_or_else(|_| Decision::deny("shadow.skipped.error", Vec::new()))
}

fn gate_inner<H: HerdrOps>(
    caller: Option<&Caller>,
    herdr: Option<&H>,
    verb: Verb,
    target: &str,
    target_name: Option<&str>,
    origin: SpawnOrigin,
    owner: Option<&str>,
    project: Option<&str>,
    runs_dir: &Path,
    cfg: Option<&Config>,
    force_shadow: bool,
) -> Result<Decision, (i32, String)> {
    let caller_resolved;
    let caller_healed;
    let mut effective_caller = match caller {
        Some(c) => c,
        None => {
            caller_resolved = match herdr {
                Some(h) => {
                    let mut c = crate::identity::resolve_caller(h, runs_dir);
                    if c.role == crate::identity::Role::Unknown
                        && (std::env::var("QUAQUA_TEST").as_deref() == Ok("1") || cfg!(test))
                        && crate::identity::caller_pane_tokens(h).is_none()
                        && std::env::var("QUAQUA_OWNER")
                            .map(|o| !o.trim().is_empty())
                            .unwrap_or(false)
                    {
                        c = crate::identity::Caller {
                            name: owner.unwrap_or("cos").to_string(),
                            role: if owner.map(|o| crate::cap::is_lead_owner(o)).unwrap_or(false) {
                                crate::identity::Role::Lead
                            } else {
                                crate::identity::Role::Cos
                            },
                            depth: crate::identity::CallerDepth::Resolved(
                                if owner.map(|o| crate::cap::is_lead_owner(o)).unwrap_or(false) {
                                    1
                                } else {
                                    0
                                },
                            ),
                            source: crate::identity::IdentitySource::EnvFallback,
                        };
                    }
                    c
                }
                None => crate::identity::Caller {
                    name: "unknown".to_string(),
                    role: crate::identity::Role::Unknown,
                    depth: crate::identity::CallerDepth::Unknown,
                    source: crate::identity::IdentitySource::Unknown,
                },
            };
            &caller_resolved
        }
    };

    if effective_caller.role == crate::identity::Role::Unknown {
        if let Some(h) = herdr {
            // `Some` here means herdr can see the caller pane, not that the pane
            // carries tokens: a restart restores panes without them (herdr omits
            // the `tokens` field). Keying the heal on the tokens object skipped
            // it in exactly the case it exists for.
            if crate::identity::caller_pane_tokens(h).is_some() {
                let _ = crate::identity::heal_with_runs_dir(h, runs_dir, None, false);
                caller_healed = crate::identity::resolve_caller(h, runs_dir);
                if caller_healed.role != crate::identity::Role::Unknown {
                    effective_caller = &caller_healed;
                }
            }
        }
    }

    let loaded_cfg;
    let effective_cfg = match cfg {
        Some(c) => c,
        None => {
            match crate::config::load_team_config() {
                Ok(c) => {
                    loaded_cfg = c;
                    &loaded_cfg
                }
                Err((_, e)) => {
                    eprintln!("warning: [layout gate] cannot resolve layout: failed to load team config: {e}");
                    return Ok(Decision::allow("gate.skipped.layout_err", Vec::new()));
                }
            }
        }
    };

    let lead_snapshot_exists = |name: &str| -> bool {
        if let Some(slug) = crate::layout::snapshot::find_lead_in_owner_chain(runs_dir, name) {
            crate::state::WorkerMetadata::layout_path(runs_dir, &slug).is_file()
        } else {
            false
        }
    };

    let layout_lookup_name = if verb == Verb::Prompt {
        if let Some(tname) = target_name.filter(|t| lead_snapshot_exists(t)) {
            tname
        } else if let Some(ow) = owner.filter(|o| lead_snapshot_exists(o)) {
            ow
        } else {
            effective_caller.name.as_str()
        }
    } else {
        effective_caller.name.as_str()
    };

    let layout = match crate::layout::resolve_effective_layout(
        layout_lookup_name,
        runs_dir,
        effective_cfg,
    ) {
        Ok((l, _)) => l,
        Err(e) => {
            eprintln!("warning: [layout gate] effective layout failed, falling back to live: {e}");
            match crate::layout::resolve_layout(effective_cfg) {
                Ok(l) => l,
                Err(live_err) => {
                    eprintln!("warning: [layout gate] cannot resolve layout: {live_err}");
                    return Ok(Decision::allow("gate.skipped.layout_err", Vec::new()));
                }
            }
        }
    };

    let mut skip_edge = origin == SpawnOrigin::TaskforceMember;
    if !skip_edge && verb == Verb::Prompt {
        if let Some(tname) = target_name {
            let owner_file = runs_dir.join(format!("{tname}.owner"));
            if let Ok(target_owner) = std::fs::read_to_string(&owner_file) {
                if target_owner.trim() == effective_caller.name {
                    skip_edge = true;
                }
            }
        }
    }
    let current_children = if verb == Verb::Spawn
        && origin != SpawnOrigin::Pipeline
        && std::env::var("PIPELINE_TASK").is_err()
    {
        if let Some(h) = herdr {
            let owner_to_count = owner.unwrap_or(effective_caller.name.as_str());
            Some(crate::cap::count_owner_slots(
                runs_dir,
                owner_to_count,
                target_name,
                h,
                effective_cfg,
            ))
        } else {
            None
        }
    } else {
        None
    };

    let decision = check_with_runs_dir(
        &layout,
        effective_caller,
        verb,
        target,
        current_children,
        skip_edge,
        Some(runs_dir),
    );

    if !decision.allow {
        let failing_step = decision.trace.iter().find(|t| !t.passed);
        let layer = failing_step.map(|t| t.layer.as_str()).unwrap_or("unknown");
        let detail = failing_step.map(|t| t.detail.as_str()).unwrap_or("");

        let subj = target_name
            .map(String::from)
            .or_else(|| Some(target.to_string()));
        let mut payload = serde_json::json!({
            "rule_id": decision.rule_id,
            "layer": layer,
            "detail": detail,
            "layout": layout.name,
            "verb": verb.as_str(),
            "caller_role": effective_caller.role.as_str(),
            "target": target,
            "origin": origin.to_string(),
        });
        let caller_r = resolve_caller_role(&layout, effective_caller, Some(runs_dir)).ok();
        if let Ok(target_role) = resolve_target_role(&layout, caller_r, target) {
            payload["target_role"] = serde_json::Value::String(target_role.to_string());
        }

        let is_shadow = force_shadow || is_shadow_mode();
        if is_shadow {
            eprintln!(
                "would_deny: rule_id={} layer={} layout={}",
                decision.rule_id, layer, layout.name
            );

            let event = crate::bus::FarmEvent::new(
                "would_deny",
                Some(effective_caller.name.clone()),
                subj,
                owner.map(String::from),
                project.map(String::from),
            )
            .with_payload_json(payload.to_string());

            let _ = std::fs::create_dir_all(runs_dir);
            crate::bus::record_event_safely_in_runs(runs_dir, &event);
            return Ok(decision);
        }

        // Enforce mode
        let event = crate::bus::FarmEvent::new(
            "denied",
            Some(effective_caller.name.clone()),
            subj,
            owner.map(String::from),
            project.map(String::from),
        )
        .with_payload_json(payload.to_string());

        let _ = std::fs::create_dir_all(runs_dir);
        crate::bus::record_event_safely_in_runs(runs_dir, &event);

        let mut err_msg = if let Some(tname) = target_name {
            if verb == Verb::Spawn {
                format!(
                    "error: [{}] {} — cannot spawn {}\n",
                    decision.rule_id, detail, tname
                )
            } else {
                format!("error: [{}] {}\n", decision.rule_id, detail)
            }
        } else {
            format!("error: [{}] {}\n", decision.rule_id, detail)
        };
        if decision.rule_id == "identity.unknown" {
            err_msg.push_str(&identity_unknown_hint());
        }

        return Err((1, err_msg));
    }

    if decision.allow && verb == Verb::Spawn {
        if let Some(tname) = target_name {
            let caller_r = resolve_caller_role(&layout, effective_caller, Some(runs_dir)).ok();
            if let Ok(target_role) = resolve_target_role(&layout, caller_r, target) {
                if let Err(e) = record_layout_role(runs_dir, tname, target_role) {
                    let is_shadow = force_shadow || is_shadow_mode();
                    if is_shadow {
                        eprintln!(
                            "warning: [layout shadow] failed to record layout role for '{tname}': {e}"
                        );
                    } else {
                        eprintln!(
                            "warning: [layout gate] failed to record layout role for '{tname}': {e}"
                        );
                        return Err((
                            1,
                            format!(
                                "error: [gate.layout_role_err] failed to record layout role for '{tname}': {e}\n"
                            ),
                        ));
                    }
                }
            }
        }
    }
    Ok(decision)
}
