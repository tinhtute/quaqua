pub mod builtin;
pub mod gate;
pub mod legacy;
pub mod model;

pub use gate::{
    check, check_with_count, check_with_origin, resolve_target_role, shadow, validate_layout,
    SpawnOrigin,
};
pub use model::{
    Decision, Layout, LayoutRole, Member, Naming, RoleClass, TraceStep, Verb,
};

use crate::config::Config;
use crate::identity::{depth_walk, Caller, CallerDepth, IdentitySource, Role};
use std::fmt::Write as _;
use std::path::Path;

/// Resolve the effective Layout from a Config.
///
/// In v3 configs with `[layout] use = "<name>"`, resolves either from built-ins
/// or from `[layouts.<name>]`. If `[layout]` is omitted or config is v2,
/// synthesizes the legacy SLP layout.
pub fn resolve_layout(cfg: &Config) -> Result<Layout, String> {
    if cfg.schema_version >= 3 {
        // Collisions between user layout names and built-in names are a hard error
        for name in cfg.layouts.keys() {
            if builtin::get(name).is_some() {
                return Err(format!("user layout '{name}' collides with built-in layout"));
            }
        }

        if let Some(layout_ref) = &cfg.layout {
            let name = &layout_ref.use_layout;
            if let Some(mut builtin_layout) = builtin::get(name) {
                if name == "slp" {
                    // SLP topology keeps live caps and depth from config / project overrides
                    builtin_layout.depth = cfg.limits.depth.max(0) as u32;
                    if let Some(cos_role) = builtin_layout.roles.get_mut("cos") {
                        cos_role.max_children = Some(cfg.limits.max_workers_per_orchestrator);
                    }
                    if let Some(lead_role) = builtin_layout.roles.get_mut("lead") {
                        let peers = cfg
                            .roles
                            .get("lead")
                            .and_then(|r| r.max_peers)
                            .unwrap_or(crate::config::DEFAULT_LEAD_MAX_PEERS);
                        lead_role.max_children = Some(peers);
                    }
                }
                return Ok(builtin_layout);
            }
            if let Some(custom) = cfg.layouts.get(name) {
                let mut layout = custom.clone();
                if layout.name.is_empty() {
                    layout.name = name.clone();
                }
                validate_layout(&layout)?;
                return Ok(layout);
            }
            return Err(format!("unknown layout '{name}'"));
        }
    }

    Ok(Layout::from_legacy(cfg))
}

/// Resolves a Caller struct given a caller identifier/name and runs_dir.
///
/// - "cos" resolves to Role::Cos at depth 0.
/// - Generic "lead" resolves to Role::Lead at depth 1.
/// - Generic "peer" resolves to Role::Peer at depth 2.
/// - Named agents ending with "-lead" resolve to Role::Lead if runs/<name>.owner exists,
///   walking depth via depth_walk and mapping DepthError to CallerDepth.
/// - Named worker agents resolve to Role::Peer if runs/<name>.owner exists.
/// - Unknown callers (no owner file, no tokens) resolve strictly to Role::Unknown.
pub fn resolve_caller_by_name(caller_name: &str, runs_dir: &Path) -> Caller {
    let caller_trim = caller_name.trim();
    if caller_trim == "cos" {
        return Caller {
            name: "cos".to_string(),
            role: Role::Cos,
            depth: CallerDepth::Resolved(0),
            source: IdentitySource::PaneTokens,
        };
    }

    if caller_trim == "lead" {
        return Caller {
            name: "lead".to_string(),
            role: Role::Lead,
            depth: CallerDepth::Resolved(1),
            source: IdentitySource::PaneTokens,
        };
    }

    if caller_trim == "peer" {
        return Caller {
            name: "peer".to_string(),
            role: Role::Peer,
            depth: CallerDepth::Resolved(2),
            source: IdentitySource::PaneTokens,
        };
    }

    let owner_file = runs_dir.join(format!("{caller_trim}.owner"));
    if owner_file.exists() {
        if let Ok(owner) = std::fs::read_to_string(&owner_file) {
            let parent = owner.trim();
            if caller_trim.ends_with("-lead") {
                let depth = CallerDepth::from(depth_walk(runs_dir, caller_trim));
                return Caller {
                    name: caller_trim.to_string(),
                    role: Role::Lead,
                    depth,
                    source: IdentitySource::PaneTokens,
                };
            } else {
                let depth = match depth_walk(runs_dir, parent) {
                    Ok(d) => CallerDepth::Resolved(d + 1),
                    Err(e) => CallerDepth::from(Err(e)),
                };
                return Caller {
                    name: caller_trim.to_string(),
                    role: Role::Peer,
                    depth,
                    source: IdentitySource::PaneTokens,
                };
            }
        }
    }

    Caller {
        name: caller_trim.to_string(),
        role: Role::Unknown,
        depth: CallerDepth::Unknown,
        source: IdentitySource::Unknown,
    }
}

/// Explain layout decision for a caller, verb, and target.
///
/// Returns (rule_id, trace).
pub fn why(
    layout: &Layout,
    caller: &Caller,
    verb: Verb,
    target: &str,
) -> (String, Vec<TraceStep>) {
    let decision = check(layout, caller, verb, target);
    (decision.rule_id, decision.trace)
}

/// Handler for `quaqua layout show [--project <slug>] [--json]`
pub fn run_show(project: Option<&str>, json: bool) -> Result<String, String> {
    let cfg = crate::config::load_team_config()
        .map_err(|(_, msg)| msg.trim().to_string())?;
    let cfg = crate::config::resolve(cfg, project)?;
    crate::config::validate(&cfg)?;
    let layout = resolve_layout(&cfg)?;

    if json {
        return serde_json::to_string_pretty(&layout)
            .map_err(|e| format!("failed to format JSON: {e}"));
    }

    let mut out = String::new();
    let source = if cfg.schema_version >= 3 && cfg.layout.is_some() {
        "config"
    } else {
        "synthesized legacy"
    };
    let _ = writeln!(out, "Layout: {} (source: {source})", layout.name);
    let _ = writeln!(out, "Depth:  {}", layout.depth);
    let _ = writeln!(out, "\nRoles:");
    for (name, role) in &layout.roles {
        let _ = writeln!(out, "  {name} (class: {})", role.class);
        let spawns = if role.can_spawn.is_empty() {
            "(none)".to_string()
        } else {
            role.can_spawn.join(", ")
        };
        let prompts = if role.can_prompt.is_empty() {
            "(none)".to_string()
        } else {
            role.can_prompt.join(", ")
        };
        let _ = writeln!(out, "    can_spawn:    {spawns}");
        let _ = writeln!(out, "    can_prompt:   {prompts}");
        if let Some(rep) = &role.reports_to {
            let _ = writeln!(out, "    reports_to:   {rep}");
        }
        if let Some(cap) = role.max_children {
            let _ = writeln!(out, "    max_children: {cap}");
        }
        if role.class == RoleClass::Lead {
            let _ = writeln!(out, "    naming:       {}", role.naming);
        }
        if !role.members.is_empty() {
            let _ = writeln!(out, "    members:      {} configured", role.members.len());
        }
        let _ = writeln!(out);
    }

    Ok(out)
}

/// Handler for `quaqua layout why <caller> <verb> <target> [--project <slug>] [--json]`
pub fn run_why(
    caller_name: &str,
    verb_str: &str,
    target: &str,
    project: Option<&str>,
    json: bool,
) -> Result<String, String> {
    let cfg = crate::config::load_team_config()
        .map_err(|(_, msg)| msg.trim().to_string())?;
    let cfg = crate::config::resolve(cfg, project)?;
    crate::config::validate(&cfg)?;
    let layout = resolve_layout(&cfg)?;

    let verb: Verb = verb_str.parse()?;
    let runs_dir = crate::worker::cleanup::default_runs_dir();
    let caller = resolve_caller_by_name(caller_name, &runs_dir);

    let decision = check(&layout, &caller, verb, target);

    if json {
        let val = serde_json::json!({
            "code": if decision.allow { "allowed" } else { "denied" },
            "verb": verb.as_str(),
            "caller": caller.name,
            "caller_role": caller.role.as_str(),
            "target": target,
            "rule_id": decision.rule_id,
            "layout": layout.name,
            "trace": decision.trace,
        });
        return serde_json::to_string_pretty(&val)
            .map_err(|e| format!("failed to format JSON: {e}"));
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "Evaluating: caller '{}' ({}) {} '{}' against layout '{}'\n",
        caller.name, caller.role, verb, target, layout.name
    );
    for step in &decision.trace {
        let mark = if step.passed { "✓" } else { "✗" };
        let _ = writeln!(out, "[{mark}] {:8} {}", step.layer, step.detail);
    }
    let verdict = if decision.allow { "ALLOWED" } else { "DENIED" };
    let _ = writeln!(out, "\nDecision: {verdict} (rule_id: {})", decision.rule_id);

    Ok(out)
}
