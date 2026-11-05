use quaqua::config::{self, Config};
use quaqua::identity::{Caller, CallerDepth, IdentitySource, Role};
use quaqua::layout::migrate::{format_diff, migrate_team_toml, run_migrate, MigrateResult};
use quaqua::layout::{
    builtin, check, check_with_count, find_lead_in_owner_chain, layout_sha256,
    read_layout_snapshot, resolve_caller_by_name, resolve_effective_layout, resolve_layout,
    run_refreeze, run_show, run_why, why, write_layout_snapshot, Layout, LayoutOrigin, Naming,
    RoleClass, Verb,
};
use std::path::PathBuf;

#[path = "common/gitfix.rs"]
mod gitfix;

static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn hermetic_temp_dir(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "quaqua-layout-test-{prefix}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create hermetic temp dir");
    dir
}

#[test]
fn layout_evidence_1_from_legacy_edge_set_equals_slp_builtin() {
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let cfg: Config = toml::from_str(template_toml).expect("parse template team.toml");
    let legacy = Layout::from_legacy(&cfg);
    let slp = builtin::get("slp").expect("slp built-in");
    assert_eq!(legacy.edge_set(), slp.edge_set());
}

#[test]
fn layout_evidence_2_v3_use_slp_equals_legacy() {
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let v3_toml = template_toml
        .replace(
            "schema_version = 2",
            "schema_version = 3\n[layout]\nuse = \"slp\"",
        )
        .replace(
            "max_workers_per_orchestrator = 8",
            "max_workers_per_orchestrator = 3",
        )
        .replace("max_peers = 4", "max_peers = 2");
    let cfg: Config = toml::from_str(&v3_toml).expect("parse v3 config");
    let v3_layout = resolve_layout(&cfg).expect("resolve v3 layout");
    let legacy_layout = Layout::from_legacy(&cfg);

    assert_eq!(v3_layout.roles, legacy_layout.roles);
    assert_eq!(v3_layout.roles["cos"].max_children, Some(3));
    assert_eq!(v3_layout.roles["lead"].max_children, Some(2));
    assert_eq!(v3_layout.edge_set(), legacy_layout.edge_set());
    assert_eq!(v3_layout.depth, legacy_layout.depth);
}

#[test]
fn layout_evidence_3_schema_version_4_refused_with_migrate_hint() {
    let toml_v4 = r#"
schema_version = 4
"#;
    let cfg: Config = toml::from_str(toml_v4).expect("parse v4 config");
    let err = config::validate(&cfg).expect_err("schema_version 4 must be refused");
    assert!(
        err.contains("migrate"),
        "error '{err}' must contain migrate hint"
    );
}

#[test]
fn layout_evidence_4_why_rule_id_equals_check_rule_id() {
    let slp = builtin::get("slp").expect("slp built-in");
    let runs_dir = hermetic_temp_dir("ev4");
    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").expect("write owner");
    let caller = resolve_caller_by_name("mu-lead", &runs_dir);

    // Case A: allowed edge
    let dec = check(&slp, &caller, Verb::Spawn, "peer");
    let (why_rule_id, _trace) = why(&slp, &caller, Verb::Spawn, "peer");
    assert_eq!(why_rule_id, dec.rule_id);
    assert_eq!(dec.rule_id, "slp.lead.can_spawn.peer");
    assert!(dec.allow);

    // Case B: denied edge
    let dec_deny = check(&slp, &caller, Verb::Spawn, "lead");
    let (why_deny_rule_id, _trace) = why(&slp, &caller, Verb::Spawn, "lead");
    assert_eq!(why_deny_rule_id, dec_deny.rule_id);
    assert_eq!(dec_deny.rule_id, "edge.none");
    assert!(!dec_deny.allow);

    let _ = std::fs::remove_dir_all(&runs_dir);
}

#[test]
fn layout_evidence_live_team_toml_loads() {
    if let Ok(home) = std::env::var("HOME") {
        let live_path = std::path::PathBuf::from(home).join("quaqua/team.toml");
        if live_path.exists() {
            let content = std::fs::read_to_string(&live_path).expect("read live team.toml");
            let cfg: Config = toml::from_str(&content).expect("parse live team.toml");
            config::validate(&cfg).expect("validate live team.toml");
        }
    }
}

#[test]
fn layout_all_builtins_load() {
    for name in &["slp", "lead-peer", "advisor", "dual-lens", "council"] {
        let layout =
            builtin::get(name).unwrap_or_else(|| panic!("failed to load built-in '{name}'"));
        assert_eq!(&layout.name, *name);
        assert!(layout.depth > 0);
        assert!(!layout.roles.is_empty());
    }
}

#[test]
fn layout_council_members_fit_without_touching_taskforce() {
    let council = builtin::get("council").expect("council layout");
    let council_role = council.roles.get("council").expect("council role");
    assert_eq!(council_role.members.len(), 3);
    assert_eq!(council_role.members[0].purpose.as_deref(), Some("review"));
    assert_eq!(council_role.members[1].purpose.as_deref(), Some("review"));
    assert_eq!(council_role.members[2].purpose.as_deref(), Some("design"));
}

#[test]
fn layout_deny_unknown_fields_interaction_with_v3() {
    // 1. Valid v3 with [layout] use = "slp"
    let valid_v3 = r#"
schema_version = 3
[layout]
use = "slp"
"#;
    let cfg: Config = toml::from_str(valid_v3).expect("v3 [layout] use must parse");
    assert_eq!(cfg.schema_version, 3);
    assert_eq!(
        cfg.layout.as_ref().map(|l| l.use_layout.as_str()),
        Some("slp")
    );

    // 2. Valid v3 with [layouts.review-council]
    let valid_custom_v3 = r#"
schema_version = 3
[layouts.review-council]
depth = 2
[layouts.review-council.roles.cos]
class = "cos"
can_spawn = ["chair"]
can_prompt = ["chair"]
[layouts.review-council.roles.chair]
class = "lead"
can_spawn = ["council"]
reports_to = "cos"
max_children = 3
[layouts.review-council.roles.council]
class = "taskforce"
members = [{purpose="review"},{purpose="review"},{purpose="advise"}]
"#;
    let cfg2: Config = toml::from_str(valid_custom_v3).expect("v3 [layouts.*] must parse");
    assert!(cfg2.layouts.contains_key("review-council"));
    let rc = &cfg2.layouts["review-council"];
    assert_eq!(rc.depth, 2);
    assert_eq!(rc.roles["chair"].max_children, Some(3));
    assert_eq!(rc.roles["council"].members.len(), 3);

    // 3. Unknown field on Config must be rejected by deny_unknown_fields
    let bad_config = r#"
schema_version = 3
[unknown_section]
foo = "bar"
"#;
    let err = toml::from_str::<Config>(bad_config).expect_err("unknown field on Config must fail");
    assert!(err.to_string().contains("unknown field"), "err: {err}");

    // 4. Unknown field in [layout] must be rejected
    let bad_layout = r#"
schema_version = 3
[layout]
use = "slp"
bogus_key = true
"#;
    let err =
        toml::from_str::<Config>(bad_layout).expect_err("unknown field in [layout] must fail");
    assert!(err.to_string().contains("unknown field"), "err: {err}");
}

#[test]
fn layout_check_all_layers() {
    let slp = builtin::get("slp").expect("slp built-in");

    // 1. Identity layer failure
    let unknown_caller = Caller {
        name: "unknown".to_string(),
        role: Role::Unknown,
        depth: CallerDepth::Unknown,
        source: IdentitySource::Unknown,
    };
    let d1 = check(&slp, &unknown_caller, Verb::Spawn, "peer");
    assert!(!d1.allow);
    assert_eq!(d1.rule_id, "identity.unknown");

    // 2. Role layer failure: caller role defined in caller struct but absent from layout
    // (Note: we test with a caller whose role as_str is not in layout.roles)
    let unknown_role_caller = Caller {
        name: "custom-agent".to_string(),
        role: Role::Unknown,
        depth: CallerDepth::Resolved(1),
        source: IdentitySource::PaneTokens,
    };
    let d2 = check(&slp, &unknown_role_caller, Verb::Spawn, "peer");
    assert!(!d2.allow);
    assert_eq!(d2.rule_id, "identity.unknown");

    // 3. Edge layer failure: peer cannot spawn
    let peer_caller = Caller {
        name: "worker-1".to_string(),
        role: Role::Peer,
        depth: CallerDepth::Resolved(2),
        source: IdentitySource::PaneTokens,
    };
    let d3 = check(&slp, &peer_caller, Verb::Spawn, "peer");
    assert!(!d3.allow);
    assert_eq!(d3.rule_id, "edge.none");

    // 4. Depth layer failure: caller depth 2 spawning would exceed depth limit 2
    let lead_caller_depth_2 = Caller {
        name: "deep-lead".to_string(),
        role: Role::Lead,
        depth: CallerDepth::Resolved(2),
        source: IdentitySource::PaneTokens,
    };
    let d4 = check(&slp, &lead_caller_depth_2, Verb::Spawn, "peer");
    assert!(!d4.allow);
    assert_eq!(d4.rule_id, "depth.exceeded");

    // 5. Cap layer: child count at or over max_children (lead has max_children = 4)
    let lead_caller = Caller {
        name: "my-lead".to_string(),
        role: Role::Lead,
        depth: CallerDepth::Resolved(1),
        source: IdentitySource::PaneTokens,
    };
    let d5_denied = check_with_count(&slp, &lead_caller, Verb::Spawn, "peer", Some(4));
    assert!(!d5_denied.allow);
    assert_eq!(d5_denied.rule_id, "cap.exceeded");

    let d5_allowed = check_with_count(&slp, &lead_caller, Verb::Spawn, "peer", Some(3));
    assert!(d5_allowed.allow);
    assert_eq!(d5_allowed.rule_id, "slp.lead.can_spawn.peer");
}

#[test]
fn layout_check_mail_rules() {
    let slp = builtin::get("slp").expect("slp built-in");

    let lead_caller = Caller {
        name: "my-lead".to_string(),
        role: Role::Lead,
        depth: CallerDepth::Resolved(1),
        source: IdentitySource::PaneTokens,
    };
    // lead reports_to cos -> mail allowed
    let d_lead_cos = check(&slp, &lead_caller, Verb::Mail, "cos");
    assert!(d_lead_cos.allow);
    assert_eq!(d_lead_cos.rule_id, "slp.lead.reports_to.cos");

    let cos_caller = Caller {
        name: "cos".to_string(),
        role: Role::Cos,
        depth: CallerDepth::Resolved(0),
        source: IdentitySource::PaneTokens,
    };
    // cos can_prompt lead -> mail allowed
    let d_cos_lead = check(&slp, &cos_caller, Verb::Mail, "lead");
    assert!(d_cos_lead.allow);
    assert_eq!(d_cos_lead.rule_id, "slp.cos.can_mail.lead");

    let peer_caller = Caller {
        name: "worker-1".to_string(),
        role: Role::Peer,
        depth: CallerDepth::Resolved(2),
        source: IdentitySource::PaneTokens,
    };
    // peer cannot mail lead directly (reports_to = owner)
    let d_peer_lead = check(&slp, &peer_caller, Verb::Mail, "lead");
    assert!(!d_peer_lead.allow);
    assert_eq!(d_peer_lead.rule_id, "edge.none");
}

#[test]
fn layout_cli_show_and_why() {
    let _guard = TEST_ENV_LOCK.lock().unwrap();
    let sandbox = hermetic_temp_dir("cli");
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let cfg_path = sandbox.join("team.toml");
    std::fs::write(&cfg_path, template_toml).expect("write team.toml");

    let prev_team = std::env::var("QUAQUA_TEAM_TOML").ok();
    let prev_runs = std::env::var("QUAQUA_RUNS_DIR").ok();
    std::env::set_var("QUAQUA_TEAM_TOML", &cfg_path);
    std::env::set_var("QUAQUA_RUNS_DIR", &sandbox);

    std::fs::write(sandbox.join("mu-lead.owner"), "cos\n").expect("write mu-lead owner");

    // Show text format
    let show_text = run_show(None, false).expect("run_show text");
    assert!(show_text.contains("Layout: slp"));
    assert!(show_text.contains("Depth:  2"));
    assert!(show_text.contains("cos (class: cos)"));

    // Show JSON format
    let show_json = run_show(None, true).expect("run_show json");
    let json_val: serde_json::Value = serde_json::from_str(&show_json).expect("parse json");
    assert_eq!(json_val["name"], "slp");
    assert_eq!(json_val["depth"], 2);

    // Why text allowed
    let why_allow = run_why("mu-lead", "spawn", "peer", None, false).expect("run_why allow");
    assert!(why_allow.contains("[✓] identity"));
    assert!(why_allow.contains("[✓] role"));
    assert!(why_allow.contains("[✓] edge"));
    assert!(why_allow.contains("Decision: ALLOWED (rule_id: slp.lead.can_spawn.peer)"));

    // Why text denied
    let why_deny = run_why("mu-lead", "spawn", "lead", None, false).expect("run_why deny");
    assert!(why_deny.contains("[✗] edge"));
    assert!(why_deny.contains("Decision: DENIED (rule_id: edge.none)"));

    // Why JSON
    let why_json = run_why("mu-lead", "spawn", "peer", None, true).expect("run_why json");
    let json_why: serde_json::Value = serde_json::from_str(&why_json).expect("parse why json");
    assert_eq!(json_why["code"], "allowed");
    assert_eq!(json_why["rule_id"], "slp.lead.can_spawn.peer");
    assert_eq!(json_why["caller"], "mu-lead");
    assert_eq!(json_why["target"], "peer");

    match prev_team {
        Some(t) => std::env::set_var("QUAQUA_TEAM_TOML", t),
        None => std::env::remove_var("QUAQUA_TEAM_TOML"),
    }
    match prev_runs {
        Some(r) => std::env::set_var("QUAQUA_RUNS_DIR", r),
        None => std::env::remove_var("QUAQUA_RUNS_DIR"),
    }
    let _ = std::fs::remove_dir_all(&sandbox);
}

// ---------------------------------------------------------------------------
// Regression tests for review findings F1 - F7
// ---------------------------------------------------------------------------

#[test]
fn layout_repro_f1_unknown_caller_resolves_to_identity_unknown() {
    let sandbox = hermetic_temp_dir("f1");
    let caller = resolve_caller_by_name("nobody-xyz", &sandbox);
    assert_eq!(caller.role, Role::Unknown);
    let slp = builtin::get("slp").expect("slp built-in");
    let dec = check(&slp, &caller, Verb::Spawn, "peer");
    assert!(!dec.allow);
    assert_eq!(dec.rule_id, "identity.unknown");
    let _ = std::fs::remove_dir_all(&sandbox);
}

#[test]
fn layout_repro_f2_depth_cycle_error_reaches_gate() {
    let sandbox = hermetic_temp_dir("f2");
    std::fs::write(sandbox.join("cyc-lead.owner"), "cyc-lead\n").expect("write cycle owner");
    let caller = resolve_caller_by_name("cyc-lead", &sandbox);
    assert!(matches!(caller.depth, CallerDepth::Cycle(_)));
    let slp = builtin::get("slp").expect("slp built-in");
    let dec = check(&slp, &caller, Verb::Spawn, "peer");
    assert!(!dec.allow);
    assert_eq!(dec.rule_id, "depth.cycle");
    let _ = std::fs::remove_dir_all(&sandbox);
}

#[test]
fn layout_repro_f3_v3_use_slp_retains_live_caps_and_depth() {
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let v3_toml = template_toml
        .replace(
            "schema_version = 2",
            "schema_version = 3\n[layout]\nuse = \"slp\"",
        )
        .replace(
            "max_workers_per_orchestrator = 8",
            "max_workers_per_orchestrator = 3",
        )
        .replace("max_peers = 4", "max_peers = 2");
    let cfg: Config = toml::from_str(&v3_toml).expect("parse v3 config");
    let resolved = resolve_layout(&cfg).expect("resolve v3 slp");
    let legacy = Layout::from_legacy(&cfg);
    assert_eq!(resolved.roles["cos"].max_children, Some(3));
    assert_eq!(resolved.roles["lead"].max_children, Some(2));
    assert_eq!(resolved.roles, legacy.roles);
    assert_eq!(resolved.edge_set(), legacy.edge_set());
    assert_eq!(resolved.depth, legacy.depth);
}

#[test]
fn layout_repro_f4_v2_with_layout_refused() {
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let v2_with_layout = format!("{template_toml}\n[layout]\nuse = \"nope\"\n");
    let cfg: Config = toml::from_str(&v2_with_layout).expect("parse v2 with layout");
    let err = config::validate(&cfg).expect_err("v2 with [layout] must be refused");
    assert!(err.contains("schema_version") || err.contains("[layout]"));
}

#[test]
fn layout_repro_f5_unknown_target_role_denied() {
    let slp = builtin::get("slp").expect("slp built-in");
    let cos = Caller {
        name: "cos".to_string(),
        role: Role::Cos,
        depth: CallerDepth::Resolved(0),
        source: IdentitySource::PaneTokens,
    };
    let dec = check(&slp, &cos, Verb::Spawn, "bogus");
    assert!(!dec.allow);
    assert_ne!(dec.rule_id, "slp.cos.can_spawn.peer");
}

#[test]
fn layout_repro_f6_user_layout_colliding_with_builtin_refused() {
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let v3_collision = template_toml
        .replace("schema_version = 2", "schema_version = 3\n[layout]\nuse = \"slp\"\n[layouts.slp]\ndepth = 1\n[layouts.slp.roles.cos]\nclass = \"cos\"");
    let cfg: Config = toml::from_str(&v3_collision).expect("parse collision config");
    let err = config::validate(&cfg).expect_err("collision with built-in must be refused");
    assert!(err.contains("collides"));
}

#[test]
fn layout_repro_f7_v3_layout_invalid_role_refs_refused() {
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let v3_bad_role = template_toml
        .replace("schema_version = 2", "schema_version = 3\n[layout]\nuse = \"x\"\n[layouts.x]\ndepth = 2\n[layouts.x.roles.cos]\nclass = \"cos\"\ncan_spawn = [\"chiar\"]");
    let cfg: Config = toml::from_str(&v3_bad_role).expect("parse invalid role config");
    let err = config::validate(&cfg).expect_err("invalid role ref must be refused");
    assert!(err.contains("unknown role") || err.contains("chiar"));
}

// ---------------------------------------------------------------------------
// P7 Proving Tests: Snapshots, Refreeze, Drift Warning, and Migration
// ---------------------------------------------------------------------------

#[test]
fn layout_evidence_a_team_toml_edit_does_not_change_gate_decisions() {
    let dir = hermetic_temp_dir("p7-a");
    let runs_dir = dir.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();

    // Binding rule: real lead name '<slug>-lead'
    std::fs::write(runs_dir.join("frz-lead.owner"), "cos\n").unwrap();
    std::fs::write(runs_dir.join("frz-peer.owner"), "frz-lead\n").unwrap();

    // Base template config where peer cannot prompt peer
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let cfg: Config = toml::from_str(template_toml).expect("parse template team.toml");

    // Write frozen layout snapshot for project "frz"
    let snap_path = write_layout_snapshot(&runs_dir, "frz", &cfg).unwrap();
    assert!(snap_path.is_file());

    // Initial gate check: frz-peer cannot prompt peer
    let caller = resolve_caller_by_name("frz-peer", &runs_dir);
    let (layout_before, origin_before) =
        resolve_effective_layout("frz-peer", &runs_dir, &cfg).unwrap();
    assert!(origin_before.is_snapshot());
    let dec_before = check(&layout_before, &caller, Verb::Prompt, "peer");
    assert!(!dec_before.allow);
    assert_eq!(dec_before.rule_id, "edge.none");

    // Edit team.toml config to allow peer prompt
    let mut edited_cfg = cfg.clone();
    edited_cfg.roles.get_mut("peer").unwrap().can_prompt = vec!["peer".to_string()];

    // Gate decision for frz-peer does NOT change because it resolves from frozen snapshot
    let (layout_after, origin_after) =
        resolve_effective_layout("frz-peer", &runs_dir, &edited_cfg).unwrap();
    assert!(origin_after.is_snapshot());
    let dec_after = check(&layout_after, &caller, Verb::Prompt, "peer");
    assert!(!dec_after.allow);
    assert_eq!(dec_after.rule_id, "edge.none");
    assert_eq!(layout_after.roles["peer"].can_prompt, Vec::<String>::new());

    // Verify CoS-owned work uses live file, not snapshot
    std::fs::write(runs_dir.join("cos-worker.owner"), "cos\n").unwrap();
    let cos_caller = resolve_caller_by_name("cos-worker", &runs_dir);
    let (cos_layout, cos_origin) =
        resolve_effective_layout("cos-worker", &runs_dir, &edited_cfg).unwrap();
    assert!(!cos_origin.is_snapshot());
    let cos_dec = check(&cos_layout, &cos_caller, Verb::Prompt, "peer");
    assert!(
        cos_dec.allow,
        "CoS-owned work must use live config immediately"
    );
}

#[test]
fn layout_evidence_b_refreeze_applies_team_toml_edit() {
    let dir = hermetic_temp_dir("p7-b");
    let runs_dir = dir.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();

    // Real lead name 'frz-lead'
    std::fs::write(runs_dir.join("frz-lead.owner"), "cos\n").unwrap();
    std::fs::write(runs_dir.join("frz-peer.owner"), "frz-lead\n").unwrap();

    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let cfg: Config = toml::from_str(template_toml).expect("parse template team.toml");
    write_layout_snapshot(&runs_dir, "frz", &cfg).unwrap();

    // Edit live config
    let mut edited_cfg = cfg.clone();
    edited_cfg.roles.get_mut("peer").unwrap().can_prompt = vec!["peer".to_string()];

    // Refreeze snapshot with edited config
    let _guard = TEST_ENV_LOCK.lock().unwrap();
    let edited_toml = template_toml.replace("can_prompt = []", "can_prompt = [\"peer\"]");
    let team_path = dir.join("team.toml");
    std::fs::write(&team_path, &edited_toml).unwrap();
    let prev_runs = std::env::var("QUAQUA_RUNS_DIR").ok();
    let prev_team = std::env::var("QUAQUA_TEAM_TOML").ok();
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEAM_TOML", &team_path);
    let refreeze_msg = run_refreeze("frz").expect("run_refreeze should succeed");
    assert!(refreeze_msg.contains("refroze layout for 'frz'"));
    match prev_runs {
        Some(v) => std::env::set_var("QUAQUA_RUNS_DIR", v),
        None => std::env::remove_var("QUAQUA_RUNS_DIR"),
    }
    match prev_team {
        Some(v) => std::env::set_var("QUAQUA_TEAM_TOML", v),
        None => std::env::remove_var("QUAQUA_TEAM_TOML"),
    }

    let snap = read_layout_snapshot(&runs_dir, "frz")
        .unwrap()
        .expect("snapshot exists");
    assert_eq!(
        snap.layout.roles["peer"].can_prompt,
        vec!["peer".to_string()]
    );

    // Gate decision for frz-peer now ALLOWS prompt because snapshot was refrozen
    let caller = resolve_caller_by_name("frz-peer", &runs_dir);
    let (layout_refrozen, origin_refrozen): (Layout, LayoutOrigin) =
        resolve_effective_layout("frz-peer", &runs_dir, &edited_cfg).unwrap();
    assert!(origin_refrozen.is_snapshot());
    assert_eq!(origin_refrozen.source_desc(), "snapshot frz.layout.toml");
    let sha = layout_sha256(&layout_refrozen).unwrap();
    assert_eq!(sha, snap.sha256);
    let dec_refrozen = check(&layout_refrozen, &caller, Verb::Prompt, "peer");
    assert!(dec_refrozen.allow);
    assert_eq!(
        layout_refrozen.roles["peer"].can_prompt,
        vec!["peer".to_string()]
    );
}

#[test]
fn layout_evidence_c_migrate_templates_workspace_team_toml_byte_identical_and_valid() {
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let res = migrate_team_toml(template_toml).expect("migrate templates/workspace/team.toml");
    let migrated = match res {
        MigrateResult::Migrated(s) => s,
        MigrateResult::AlreadyV3 => panic!("expected Migrated, got AlreadyV3"),
    };

    // Byte-identical except version line and [layout]
    let expected = template_toml.replace(
        "schema_version = 2",
        "schema_version = 3\n\n[layout]\nuse = \"slp\"",
    );
    assert_eq!(
        migrated, expected,
        "migrated template must be byte-identical except version line and [layout]"
    );

    // Result must pass validate()
    let parsed: Config = toml::from_str(&migrated).expect("parse migrated TOML as Config");
    config::validate(&parsed).expect("validate migrated Config");
}

#[test]
fn layout_evidence_d_v4_refused_with_hint() {
    let v4_toml = "# Comments\nschema_version = 4\n[limits]\ndepth = 2\n";
    let err = migrate_team_toml(v4_toml).unwrap_err();
    assert!(err.contains("schema_version 4"), "error was: {err}");
    assert!(err.contains("hint: upgrade quaqua"), "error was: {err}");
}

#[test]
fn layout_evidence_e_drift_warning_shown_on_mismatch() {
    let _guard = TEST_ENV_LOCK.lock().unwrap();
    let dir = hermetic_temp_dir("p7-e");
    let runs_dir = dir.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();

    std::fs::write(runs_dir.join("frzdrift-lead.owner"), "cos\n").unwrap();

    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let cfg: Config = toml::from_str(template_toml).expect("parse template team.toml");
    let snap_path = write_layout_snapshot(&runs_dir, "frzdrift", &cfg).unwrap();
    assert!(snap_path.is_file());

    // Mutate live team.toml role topology
    let mutated_toml = template_toml.replace("\"lead\", \"peer\", \"taskforce\"", "\"lead\"");
    assert_ne!(
        mutated_toml, template_toml,
        "drift fixture must actually change the template's CoS role"
    );
    let team_path = dir.join("team.toml");
    std::fs::write(&team_path, &mutated_toml).unwrap();

    let prev_runs = std::env::var("QUAQUA_RUNS_DIR").ok();
    let prev_team = std::env::var("QUAQUA_TEAM_TOML").ok();
    std::env::set_var("QUAQUA_RUNS_DIR", &runs_dir);
    std::env::set_var("QUAQUA_TEAM_TOML", &team_path);

    let res = run_show(Some("frzdrift"), false);

    match prev_runs {
        Some(v) => std::env::set_var("QUAQUA_RUNS_DIR", v),
        None => std::env::remove_var("QUAQUA_RUNS_DIR"),
    }
    match prev_team {
        Some(v) => std::env::set_var("QUAQUA_TEAM_TOML", v),
        None => std::env::remove_var("QUAQUA_TEAM_TOML"),
    }

    let out = res.expect("run_show should succeed");
    assert!(
        out.contains("warning: frozen sha ≠ live sha"),
        "expected drift warning in output: {out}"
    );
    assert!(
        out.contains("Layout: slp (source: snapshot frzdrift.layout.toml)"),
        "expected snapshot source in output: {out}"
    );
}

#[test]
fn layout_p7_migrate_already_v3_is_noop_exit_0() {
    let v3_toml = "schema_version = 3\n\n[layout]\nuse = \"slp\"\n\n[limits]\ndepth = 2\nmax_workers_per_orchestrator = 4\n";
    let res = migrate_team_toml(v3_toml).expect("migrate already-v3 succeeds");
    assert_eq!(res, MigrateResult::AlreadyV3);
}

#[test]
fn layout_p7_migrate_custom_roles_moves_to_layouts_custom() {
    let v2_custom = r#"# custom v2
schema_version = 2

[limits]
depth = 2
max_workers_per_orchestrator = 4

[merge]
integ_branch = "quaqua/{slug}-integ"
[roles.cos]
can_spawn = ["lead", "taskforce"]
can_prompt = ["lead"]

[roles.lead]
can_spawn = ["peer", "taskforce"]
can_prompt = ["peer"]
reports_to = "cos"
max_peers = 3

[roles.peer]
can_spawn = []
can_prompt = []
reports_to = "owner"

[strategy]
default_kind = "sude"
default_reviewer = ["suhuynh"]
builder_reviewer_differ = "none"

[kinds.sude]
harness = "omp"
model = "google-antigravity/gemini-3.8-flash"
effort = "high"
can_review = true

[kinds.suhuynh]
harness = "claude"
model = "opus-5.5"
effort = "medium"
can_review = true
"#;
    let res = migrate_team_toml(v2_custom).expect("migrate custom roles");
    let migrated = match res {
        MigrateResult::Migrated(s) => s,
        _ => panic!("expected Migrated"),
    };
    assert!(migrated.contains("[layout]\nuse = \"custom\""));
    assert!(migrated.contains("[layouts.custom]"));
    assert!(migrated.contains("depth = 2"));
    assert!(migrated.contains("[layouts.custom.roles.cos]"));
    assert!(migrated.contains("[layouts.custom.roles.lead]"));
    assert!(migrated.contains("[layouts.custom.roles.peer]"));
    assert!(migrated.contains("[layouts.custom.roles.taskforce]"));
    assert!(migrated.contains("max_children = 3"));
    assert!(migrated.contains("[roles.lead]\nmax_peers = 3"));

    let v2_cfg: Config = toml::from_str(v2_custom).expect("parse v2 custom TOML");
    let v2_legacy = Layout::from_legacy(&v2_cfg);

    let migrated_cfg: Config = toml::from_str(&migrated).expect("parse migrated custom TOML");
    config::validate(&migrated_cfg).expect("validate migrated custom Config");

    let custom_layout = resolve_layout(&migrated_cfg).expect("resolve migrated layout");
    assert_eq!(
        custom_layout.edge_set(),
        v2_legacy.edge_set(),
        "migrated custom layout must preserve exact edge_set from legacy synthesis"
    );
    assert_eq!(
        custom_layout.roles["lead"].max_children,
        v2_legacy.roles["lead"].max_children
    );
    assert_eq!(
        custom_layout.roles["cos"].max_children,
        v2_legacy.roles["cos"].max_children
    );
    assert_eq!(custom_layout.roles["lead"].naming, Naming::SlugLead);
    assert_eq!(custom_layout.roles["taskforce"].class, RoleClass::Taskforce);
    assert_eq!(
        quaqua::cap::owner_cap(&migrated_cfg, "x-lead"),
        quaqua::cap::owner_cap(&v2_cfg, "x-lead"),
        "migrated config must preserve live Lead Peer cap"
    );
}

#[test]
fn layout_p7_migrate_dry_run_unified_diff() {
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let res = migrate_team_toml(template_toml).expect("migrate succeeds");
    let migrated = match res {
        MigrateResult::Migrated(s) => s,
        _ => panic!("expected Migrated"),
    };

    let diff = format_diff(template_toml, &migrated, "team.toml");
    assert!(
        diff.starts_with("--- team.toml\n+++ team.toml\n@@ -"),
        "diff must be a unified diff over team.toml: {diff}"
    );
    assert!(diff.contains("-schema_version = 2\n"));
    assert!(diff.contains("+schema_version = 3\n"));
    assert!(diff.contains("+[layout]\n+use = \"slp\"\n"));
    // Crucial check from Finding 1: verify no false churn after the insertion
    assert!(!diff.contains("-[limits]"));
    assert!(!diff.contains("+[limits]"));
    assert!(!diff.contains("-[timers]"));
    assert!(!diff.contains("+[timers]"));
}

#[test]
fn layout_p7_run_migrate_write_and_bak() {
    let dir = hermetic_temp_dir("p7-bak");
    let team_path = dir.join("team.toml");
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    std::fs::write(&team_path, template_toml).unwrap();

    // Dry run first
    let dry_out = run_migrate(Some(&team_path), false).expect("dry run succeeds");
    assert!(dry_out.contains("-schema_version = 2\n"));
    assert!(dry_out.contains("+schema_version = 3\n"));
    assert!(!dir.join("team.toml.bak").exists());

    // Write run
    let write_out = run_migrate(Some(&team_path), true).expect("write run succeeds");
    assert!(write_out.contains("backup at"));

    // Verify backup created with original content
    let bak_path = dir.join("team.toml.bak");
    assert!(bak_path.is_file(), "backup file must exist");
    let bak_content = std::fs::read_to_string(&bak_path).unwrap();
    assert_eq!(bak_content, template_toml);

    // Verify team.toml updated
    let new_content = std::fs::read_to_string(&team_path).unwrap();
    assert!(new_content.contains("schema_version = 3"));
    assert!(new_content.contains("[layout]\nuse = \"slp\""));

    // Subsequent run on already-v3 file is a no-op
    let noop_out = run_migrate(Some(&team_path), false).expect("noop succeeds");
    assert_eq!(noop_out, "already at schema_version 3\n");
}

#[test]
fn layout_p7_find_lead_in_owner_chain_cycle_safe() {
    let dir = hermetic_temp_dir("cycle");
    let runs_dir = dir.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();

    // Cycle: worker-a -> worker-b -> worker-a
    std::fs::write(runs_dir.join("worker-a.owner"), "worker-b\n").unwrap();
    std::fs::write(runs_dir.join("worker-b.owner"), "worker-a\n").unwrap();

    let found = find_lead_in_owner_chain(&runs_dir, "worker-a");
    assert_eq!(found, None, "cycles must terminate cleanly with None");
}
struct EnvGuard(Vec<(&'static str, Option<String>)>);

impl EnvGuard {
    fn set(pairs: &[(&'static str, String)]) -> Self {
        let saved = pairs
            .iter()
            .map(|(k, v)| {
                let prev = std::env::var(k).ok();
                std::env::set_var(k, v);
                (*k, prev)
            })
            .collect();
        EnvGuard(saved)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, prev) in &self.0 {
            match prev {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn test_slp_cfg() -> Config {
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    toml::from_str(template_toml).expect("parse template team.toml")
}

fn test_slp_team_toml() -> &'static str {
    include_str!("../../../templates/workspace/team.toml")
}

fn stub_herdr_script(dir: &std::path::Path, tokens_json: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("herdr-stub");
    let script = format!(
        r#"#!/bin/sh
case "$1 $2" in
  "pane get")
    printf '{{"result":{{"pane":{{"pane_id":"w1:p1","tokens":{tokens_json}}}}}}}\n'
    exit 0 ;;
  "pane split")
    printf '{{"result":{{"pane":{{"pane_id":"w1:p1"}}}}}}\n'
    exit 0 ;;
  "agent get")
    printf '{{"result":{{"agent":{{"agent_status":"working","pane_id":"w1:p1"}}}}}}\n'
    exit 0 ;;
  "agent wait")
    printf '{{"result":{{"agent":{{"agent_status":"working"}}}}}}\n'
    exit 0 ;;
  "agent prompt")
    printf 'Outcome: COMPLETE\n\n## Playbook\n1: done\n\nVerdict: clean\n\n## Blockers\nnone\n\n## Lessons\nnone\n' > "$QUAQUA_RUNS_DIR/$3.md"
    printf '{{"result":{{"agent":{{"agent_status":"working"}}}}}}\n'
    exit 0 ;;
esac
printf '{{"result":{{}}}}\n'
exit 0
"#
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct DummyGit;
impl quaqua::git::GitOps for DummyGit {
    fn run_in(
        &self,
        _cwd: &std::path::Path,
        args: &[&str],
    ) -> std::io::Result<std::process::Output> {
        let stdout = if args.first() == Some(&"rev-parse") {
            "391d347df43909dec802c632156fb6ff23c98815\n"
                .as_bytes()
                .to_vec()
        } else {
            Vec::new()
        };
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout,
            stderr: Vec::new(),
        })
    }
    fn run(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        self.run_in(std::path::Path::new("."), args)
    }
    fn rev_parse_commit(&self, _repo: &std::path::Path, _base: &str) -> bool {
        true
    }
    fn worktree_list_porcelain(&self, _repo: &std::path::Path) -> std::io::Result<Vec<String>> {
        Ok(vec![])
    }
    fn worktree_add(
        &self,
        _repo: &std::path::Path,
        _branch: &str,
        wt: &std::path::Path,
        _base: Option<&str>,
    ) -> std::io::Result<std::process::Output> {
        let _ = std::fs::create_dir_all(wt);
        self.run_in(_repo, &[])
    }
    fn branch_exists(&self, _repo: &std::path::Path, _branch: &str) -> bool {
        false
    }
    fn worktree_add_existing(
        &self,
        _repo: &std::path::Path,
        _branch: &str,
        _wt: &std::path::Path,
    ) -> std::io::Result<std::process::Output> {
        self.run_in(_repo, &[])
    }
    fn worktree_remove(
        &self,
        _repo: &std::path::Path,
        wt: &std::path::Path,
    ) -> std::io::Result<std::process::Output> {
        let _ = std::fs::remove_dir_all(wt);
        self.run_in(_repo, &[])
    }
    fn remote_list(&self, _repo: &std::path::Path) -> std::io::Result<Vec<String>> {
        Ok(vec![])
    }
    fn is_core_bare(&self, _repo: &std::path::Path) -> bool {
        false
    }
    fn has_core_worktree(&self, _repo: &std::path::Path) -> bool {
        false
    }
    fn enable_worktree_config(
        &self,
        _repo: &std::path::Path,
    ) -> std::io::Result<std::process::Output> {
        self.run_in(_repo, &[])
    }
    fn set_worktree_pushurl_blocked(
        &self,
        _wt: &std::path::Path,
        _remote: &str,
    ) -> std::io::Result<()> {
        Ok(())
    }
    fn is_git_dir(&self, _cwd: &std::path::Path) -> bool {
        true
    }
    fn diff_and_status(&self, _cwd: &std::path::Path) -> (String, String) {
        ("dh".to_string(), "clean".to_string())
    }
}

struct MockHerdrTokens {
    tokens_json: std::cell::RefCell<String>,
    started: std::cell::Cell<bool>,
    calls: std::cell::RefCell<Vec<String>>,
    started_agents: std::cell::RefCell<Vec<String>>,
    agent_list_json: std::cell::RefCell<Option<String>>,
    agent_session: std::cell::RefCell<Option<String>>,
    /// Reproduce herdr after a server restart: the pane is restored from
    /// `session.json` (cwd, label, agent_session — never tokens), so `pane get`
    /// omits the `tokens` field instead of sending an empty object
    /// (`PaneInfo.tokens` is optional in herdr's schema). Cleared again when a
    /// `pane report-metadata` stamps tokens back onto the pane.
    tokens_absent: std::cell::Cell<bool>,
    /// Pane a `pane report-metadata` stamped tokens on. Only that pane answers
    /// `pane get` with tokens while `tokens_absent` is set.
    tagged_pane: std::cell::RefCell<Option<String>>,
}

impl MockHerdrTokens {
    fn new(tokens_json: impl Into<String>) -> Self {
        Self {
            tokens_json: std::cell::RefCell::new(tokens_json.into()),
            started: std::cell::Cell::new(false),
            calls: std::cell::RefCell::new(Vec::new()),
            started_agents: std::cell::RefCell::new(Vec::new()),
            agent_list_json: std::cell::RefCell::new(None),
            agent_session: std::cell::RefCell::new(None),
            tokens_absent: std::cell::Cell::new(false),
            tagged_pane: std::cell::RefCell::new(None),
        }
    }

    fn with_session(
        tokens_json: impl Into<String>,
        session: impl Into<String>,
        agent_list: impl Into<String>,
    ) -> Self {
        Self {
            tokens_json: std::cell::RefCell::new(tokens_json.into()),
            started: std::cell::Cell::new(false),
            calls: std::cell::RefCell::new(Vec::new()),
            started_agents: std::cell::RefCell::new(Vec::new()),
            agent_list_json: std::cell::RefCell::new(Some(agent_list.into())),
            agent_session: std::cell::RefCell::new(Some(session.into())),
            tokens_absent: std::cell::Cell::new(false),
            tagged_pane: std::cell::RefCell::new(None),
        }
    }

    /// A pane herdr restored without any token: `pane get` succeeds but carries
    /// no `tokens` key at all.
    fn tokenless(session: impl Into<String>, agent_list: impl Into<String>) -> Self {
        let mock = Self::with_session("", session, agent_list);
        mock.tokens_absent.set(true);
        mock
    }
}

impl quaqua::herdr::HerdrOps for MockHerdrTokens {
    fn run(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        self.calls.borrow_mut().push(args.join(" "));
        let stdout = if args.first() == Some(&"pane") && args.get(1) == Some(&"get") {
            let pane_id = args.get(2).copied().unwrap_or("w1:p1");
            let mut pane = serde_json::json!({ "pane_id": pane_id });
            if let Some(sess) = self.agent_session.borrow().as_deref() {
                pane["agent_session"] = serde_json::json!({ "value": sess });
            }
            let tokens = if self.tokens_absent.get() {
                let tagged = self.tagged_pane.borrow();
                match tagged.as_deref() {
                    Some(tagged) if tagged == pane_id => Some(self.tokens_json.borrow().clone()),
                    _ => None,
                }
            } else {
                Some(self.tokens_json.borrow().clone())
            };
            if let Some(tokens) = tokens {
                pane["tokens"] = serde_json::from_str(&tokens).unwrap_or_default();
            }
            serde_json::json!({ "result": { "pane": pane } }).to_string()
        } else if args.first() == Some(&"pane") && args.get(1) == Some(&"report-metadata") {
            *self.tagged_pane.borrow_mut() = args.get(2).map(|p| p.to_string());
            let mut role = None;
            let mut owner = None;
            for arg in args {
                if let Some(r) = arg.strip_prefix("grid_role=") {
                    role = Some(r);
                } else if let Some(o) = arg.strip_prefix("grid_owner=") {
                    owner = Some(o);
                }
            }
            if let (Some(r), Some(o)) = (role, owner) {
                *self.tokens_json.borrow_mut() =
                    format!(r#"{{"grid_role":"{r}","grid_owner":"{o}","logo":"\ue1a3"}}"#);
            } else if let Some(r) = role {
                *self.tokens_json.borrow_mut() =
                    format!(r#"{{"grid_role":"{r}","logo":"\ue1a3"}}"#);
            }
            r#"{"result":{}}"#.to_string()
        } else if args.first() == Some(&"pane") && args.get(1) == Some(&"rename") {
            r#"{"result":{}}"#.to_string()
        } else if args.first() == Some(&"agent") && args.get(1) == Some(&"list") {
            if let Some(custom) = self.agent_list_json.borrow().as_deref() {
                custom.to_string()
            } else {
                self.agent_list()?
            }
        } else if args.first() == Some(&"workspace") && args.get(1) == Some(&"create") {
            r#"{"result":{"workspace":{"workspace_id":"w1"},"root_pane":{"pane_id":"w1:p1"}}}"#
                .to_string()
        } else if args.first() == Some(&"agent") && args.get(1) == Some(&"start") {
            self.started.set(true);
            r#"{"result":{}}"#.to_string()
        } else if args.first() == Some(&"agent") && args.get(1) == Some(&"get") {
            r#"{"result":{"agent":{"agent_status":"working","pane_id":"w1:p1"}}}"#.to_string()
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
        if let Some(custom) = self.agent_list_json.borrow().as_deref() {
            return Ok(custom.to_string());
        }
        let agents: Vec<serde_json::Value> = self
            .started_agents
            .borrow()
            .iter()
            .map(|n| {
                serde_json::json!({
                    "name": n,
                    "agent_status": "working",
                })
            })
            .collect();
        Ok(serde_json::json!({
            "result": {
                "agents": agents
            }
        })
        .to_string())
    }
    fn agent_get(&self, _name: &str) -> std::io::Result<(bool, String)> {
        if self.started.get() {
            Ok((
                true,
                r#"{"result":{"agent":{"agent_status":"working","pane_id":"w1:p1"}}}"#.to_string(),
            ))
        } else {
            Ok((false, r#"{"error":{"code":"agent_not_found"}}"#.to_string()))
        }
    }
    fn agent_read(&self, _n: &str, _s: &str, _l: u32) -> std::io::Result<String> {
        Ok(String::new())
    }
    fn agent_start(
        &self,
        n: &str,
        _k: &str,
        _p: &str,
        _t: u32,
        _a: &[&str],
    ) -> std::io::Result<std::process::Output> {
        self.started.set(true);
        self.started_agents.borrow_mut().push(n.to_string());
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: r#"{"result":{}}"#.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }
    fn pane_split(
        &self,
        _c: &str,
        _o: &str,
        _e: &[(&str, &str)],
    ) -> std::io::Result<std::process::Output> {
        self.calls.borrow_mut().push("pane_split".to_string());
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: r#"{"result":{"pane":{"pane_id":"w1:p1"}}}"#.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }
    fn pane_close(&self, _p: &str) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
    fn pane_report_metadata(&self, _p: &str, _t: &[&str]) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
    fn pane_wait_output(&self, _p: &str, _m: &str, _t: u32) -> bool {
        true
    }
    fn pane_read(&self, _p: &str, _s: &str, _l: u32) -> std::io::Result<String> {
        Ok(String::new())
    }
    fn pane_send_keys(&self, _p: &str, _k: &str) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
    fn agent_send_keys(&self, _n: &str, _key: &str) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
    fn agent_wait_idle(&self, _n: &str, _t: u32) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
    fn agent_prompt_nowait(&self, _n: &str, _p: &str) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
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
fn layout_shadow_evidence_cp1_spawn_worker_peer_denied_but_proceeds() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("shadow-cp1");
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").unwrap();

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("QUAQUA_LAYOUT_SHADOW", "1".to_string()),
    ]);

    let cfg = test_slp_cfg();
    let git = DummyGit;
    let herdr = MockHerdrTokens::new(
        r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#,
    );

    let opts = quaqua::worker::spawn::parse_spawn_args(&[
        "sude-worker1".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    let res = quaqua::worker::spawn::spawn_worker(
        opts,
        &cfg,
        &git,
        &herdr,
        quaqua::layout::SpawnOrigin::Cli,
    );
    assert!(
        res.is_ok(),
        "spawn_worker must succeed (proceed past shadow gate): {:?}",
        res.err()
    );

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let would_denies: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .collect();
    assert_eq!(would_denies.len(), 1, "expected 1 would_deny event");
    assert_eq!(would_denies[0].actor.as_deref(), Some("tieude-peer"));
    assert_eq!(would_denies[0].subject.as_deref(), Some("sude-worker1"));
    assert!(would_denies[0].payload_json.contains("edge.none"));
    assert!(would_denies[0].payload_json.contains("edge"));
    assert!(would_denies[0].payload_json.contains("\"detail\":"));
}
#[test]
fn layout_shadow_evidence_cp2_lead_spawn_peer_denied_but_proceeds() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("shadow-cp2");
    let root = temp.join("root");
    let runs_dir = root.join("runs");
    let repo_dir = root.join("projects/foo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::create_dir_all(root.join("rules/_all")).unwrap();
    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").unwrap();
    std::fs::write(root.join("team.toml"), test_slp_team_toml()).unwrap();
    let index_content = format!(
        "| slug | repo | owner | status | one-line goal |\n|------|------|-------|--------|---------------|\n| foo | {} | cos | active | goal |\n",
        repo_dir.display()
    );
    std::fs::write(root.join("projects/INDEX.md"), index_content).unwrap();
    std::fs::write(root.join("projects/foo.md"), "# foo card\n").unwrap();
    let _env = EnvGuard::set(&[
        ("QUAQUA_HOME", root.to_str().unwrap().to_string()),
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("QUAQUA_LAYOUT_SHADOW", "1".to_string()),
    ]);

    let git = DummyGit;
    let herdr = MockHerdrTokens::new(
        r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#,
    );

    let res = quaqua::lead::spawn::spawn_lead(&["foo".to_string()], &git, &herdr);
    assert!(
        res.is_ok(),
        "spawn_lead must succeed (proceed past shadow gate): {:?}",
        res.err()
    );

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let would_denies: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .collect();
    assert_eq!(would_denies.len(), 1, "expected 1 would_deny event");
    assert_eq!(would_denies[0].actor.as_deref(), Some("tieude-peer"));
    assert_eq!(would_denies[0].subject.as_deref(), Some("foo-lead"));
    assert!(would_denies[0].payload_json.contains("edge.none"));
}

#[test]
fn layout_shadow_evidence_cp3_taskforce_peer_denied_but_proceeds() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("shadow-cp3");
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").unwrap();
    gitfix::init_repo(&repo_dir);
    let brief_file = temp.join("brief.md");
    std::fs::write(&brief_file, "Brief content").unwrap();

    let stub = stub_herdr_script(
        &temp,
        r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#,
    );

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("HERDR_BIN_PATH", stub.to_str().unwrap().to_string()),
        ("QUAQUA_LAYOUT_SHADOW", "1".to_string()),
    ]);

    let git = quaqua::git::Git::new();
    let herdr = quaqua::herdr::Herdr::new();

    let res = quaqua::taskforce::handle_taskforce(
        &[
            "advisor".to_string(),
            "mytest".to_string(),
            repo_dir.to_str().unwrap().to_string(),
            "--brief".to_string(),
            brief_file.to_str().unwrap().to_string(),
        ],
        &git,
        &herdr,
    );
    assert!(
        res.is_ok(),
        "handle_taskforce must succeed (proceed past shadow gate): {:?}",
        res.err()
    );

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let tf_deny = events
        .iter()
        .find(|e| e.event_type == "would_deny" && e.subject.as_deref() == Some("mytest"))
        .expect("expected taskforce would_deny event");
    assert_eq!(tf_deny.actor.as_deref(), Some("tieude-peer"));
    assert!(tf_deny.payload_json.contains("edge.none"));
}

#[test]
fn layout_shadow_evidence_cp4_worker_prompt_peer_denied_but_proceeds() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("shadow-cp4");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::write(runs_dir.join("sude-target.owner"), "other-lead\n").unwrap();
    std::fs::write(
        runs_dir.join("sude-target.md"),
        "Outcome: COMPLETE\n\n## Blockers\nnone\n\n## Lessons\nnone\n",
    )
    .unwrap();

    let stub = stub_herdr_script(
        &temp,
        r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#,
    );

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("HERDR_BIN_PATH", stub.to_str().unwrap().to_string()),
        ("PROMPT_WORKER_POLL_SEC", "1".to_string()),
        ("PROMPT_WORKER_IDLE_SEC", "1".to_string()),
        ("PROMPT_WORKER_STALL_SEC", "1".to_string()),
        ("QUAQUA_LAYOUT_SHADOW", "1".to_string()),
    ]);

    let git = quaqua::git::Git::new();
    let herdr = quaqua::herdr::Herdr::new();

    let res = quaqua::worker::prompt::prompt_worker(
        "sude-target",
        Some("Goal: test\n"),
        false,
        None,
        &git,
        &herdr,
    );
    assert!(
        res.is_ok(),
        "prompt_worker must succeed (proceed past shadow gate): {:?}",
        res.err()
    );

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let would_denies: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .collect();
    assert_eq!(would_denies.len(), 1, "expected 1 would_deny event");
    assert_eq!(would_denies[0].actor.as_deref(), Some("tieude-peer"));
    assert_eq!(would_denies[0].subject.as_deref(), Some("sude-target"));
    assert!(would_denies[0].payload_json.contains("edge.none"));
}

#[test]
fn layout_shadow_evidence_cp5_lead_prompt_peer_denied_but_proceeds() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("shadow-cp5");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("QUAQUA_LAYOUT_SHADOW", "1".to_string()),
    ]);

    let herdr = MockHerdrTokens::new(
        r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#,
    );
    herdr.started.set(true);

    let res = quaqua::lead::prompt::prompt_lead(&["baro".to_string(), "hello".to_string()], &herdr);
    assert!(
        res.is_ok(),
        "prompt_lead must succeed (proceed past shadow gate): {:?}",
        res.err()
    );

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let would_denies: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .collect();
    assert_eq!(would_denies.len(), 1, "expected 1 would_deny event");
    assert_eq!(would_denies[0].actor.as_deref(), Some("tieude-peer"));
    assert_eq!(would_denies[0].subject.as_deref(), Some("baro-lead"));
    assert!(would_denies[0].payload_json.contains("edge.none"));
}

#[test]
fn layout_shadow_evidence_cp6_cos_lead_spawn_allowed_no_would_deny() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("shadow-cp6");
    let root = temp.join("root");
    let runs_dir = root.join("runs");
    let repo_dir = root.join("projects/baro");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::create_dir_all(root.join("rules/_all")).unwrap();
    std::fs::write(root.join("team.toml"), test_slp_team_toml()).unwrap();
    let index_content = format!(
        "| slug | repo | owner | status | one-line goal |\n|------|------|-------|--------|---------------|\n| baro | {} | cos | active | goal |\n",
        repo_dir.display()
    );
    std::fs::write(root.join("projects/INDEX.md"), index_content).unwrap();
    std::fs::write(root.join("projects/baro.md"), "# baro card\n").unwrap();

    let _env = EnvGuard::set(&[
        ("QUAQUA_HOME", root.to_str().unwrap().to_string()),
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
    ]);

    let git = DummyGit;
    let herdr = MockHerdrTokens::new(r#"{"grid_role":"cos"}"#);

    let res = quaqua::lead::spawn::spawn_lead(&["baro".to_string()], &git, &herdr);
    assert!(res.is_ok(), "cos spawn_lead must succeed: {:?}", res.err());

    let db_path = runs_dir.join("quaqua.db");
    let bus = quaqua::bus::FarmBus::open(&db_path).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let would_denies: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .collect();
    assert_eq!(
        would_denies.len(),
        0,
        "CoS lead spawn must write 0 would_deny events"
    );
}
#[test]
fn layout_shadow_evidence_taskforce_member_skips_edge() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("shadow-tf-member");
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("QUAQUA_LAYOUT_SHADOW", "1".to_string()),
    ]);

    let cfg = test_slp_cfg();
    let git = DummyGit;
    // Caller is a peer directly owned by cos (depth 1, target depth 2 <= layout limit 2).
    // In SLP, peer has no can_spawn edge to peer.
    let herdr = MockHerdrTokens::new(
        r#"{"grid_role":"worker","grid_owner":"cos","grid_name":"tieude-peer"}"#,
    );

    // 1) With Origin::Cli: peer has no spawn edge, so shadow writes 1 would_deny row for edge.none.
    let opts_cli = quaqua::worker::spawn::parse_spawn_args(&[
        "sude-cliworker".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();
    let res_cli = quaqua::worker::spawn::spawn_worker(
        opts_cli,
        &cfg,
        &git,
        &herdr,
        quaqua::layout::SpawnOrigin::Cli,
    );
    assert!(
        res_cli.is_ok(),
        "spawn_worker with Cli must succeed (shadow never blocks): {:?}",
        res_cli.err()
    );

    let db_path = runs_dir.join("quaqua.db");
    let bus = quaqua::bus::FarmBus::open(&db_path).expect("open bus");
    let events_after_cli = bus.load_events(0).expect("load events");
    let cli_would_denies: Vec<_> = events_after_cli
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .collect();
    assert_eq!(
        cli_would_denies.len(),
        1,
        "Cli origin must record 1 would_deny event for peer caller"
    );
    assert_eq!(cli_would_denies[0].actor.as_deref(), Some("tieude-peer"));
    assert_eq!(
        cli_would_denies[0].subject.as_deref(),
        Some("sude-cliworker")
    );
    assert!(cli_would_denies[0].payload_json.contains("edge.none"));

    // 2) With Origin::TaskforceMember: edge check is skipped, depth 2 <= 2 passes, so NO new would_deny event is written.
    let opts_tf = quaqua::worker::spawn::parse_spawn_args(&[
        "sude-tfworker".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();
    let res_tf = quaqua::worker::spawn::spawn_worker(
        opts_tf,
        &cfg,
        &git,
        &herdr,
        quaqua::layout::SpawnOrigin::TaskforceMember,
    );
    assert!(
        res_tf.is_ok(),
        "spawn_worker with TaskforceMember must succeed: {:?}",
        res_tf.err()
    );

    let events_after_tf = bus.load_events(0).expect("load events");
    let tf_would_denies: Vec<_> = events_after_tf
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .collect();
    assert_eq!(tf_would_denies.len(), 1, "TaskforceMember skips edge, so count of would_deny events stays 1 (no edge.none row added)");
}

#[test]
fn layout_shadow_evidence_worker_prompt_resume_dedupes_would_deny() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("shadow-resume");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::write(runs_dir.join("sude-resumetgt.owner"), "other-lead\n").unwrap();
    std::fs::write(
        runs_dir.join("sude-resumetgt.md"),
        "Outcome: COMPLETE\n\n## Blockers\nnone\n\n## Lessons\nnone\n",
    )
    .unwrap();

    let stub = stub_herdr_script(
        &temp,
        r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#,
    );

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("HERDR_BIN_PATH", stub.to_str().unwrap().to_string()),
        ("PROMPT_WORKER_POLL_SEC", "1".to_string()),
        ("PROMPT_WORKER_IDLE_SEC", "1".to_string()),
        ("PROMPT_WORKER_STALL_SEC", "1".to_string()),
        ("QUAQUA_LAYOUT_SHADOW", "1".to_string()),
    ]);

    let git = quaqua::git::Git::new();
    let herdr = quaqua::herdr::Herdr::new();

    // First prompt (resume=false) records 1 would_deny
    let res1 = quaqua::worker::prompt::prompt_worker(
        "sude-resumetgt",
        Some("Goal: test\n"),
        false,
        None,
        &git,
        &herdr,
    );
    assert!(
        res1.is_ok(),
        "initial prompt must succeed: {:?}",
        res1.err()
    );

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events1 = bus.load_events(0).expect("load events");
    let count1 = events1
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .count();
    assert_eq!(count1, 1, "initial prompt writes 1 would_deny");

    // Resume prompt (resume=true) dedupes and does not write another would_deny
    let res2 =
        quaqua::worker::prompt::prompt_worker("sude-resumetgt", None, true, None, &git, &herdr);
    assert!(res2.is_ok(), "resume prompt must succeed: {:?}", res2.err());

    let events2 = bus.load_events(0).expect("load events");
    let count2 = events2
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .count();
    assert_eq!(
        count2, 1,
        "resume prompt must dedupe and not write another would_deny"
    );
}

#[test]
fn layout_repro_finding1_missing_owner_and_cycle_have_distinct_rule_ids() {
    let slp = builtin::get("slp").expect("slp built-in");
    let lead_missing = Caller {
        name: "gate-lead".to_string(),
        role: Role::Lead,
        depth: CallerDepth::MissingOwner("gate-lead".to_string()),
        source: IdentitySource::PaneTokens,
    };
    let dec_missing = check(&slp, &lead_missing, Verb::Spawn, "peer");
    assert!(!dec_missing.allow);
    assert_eq!(
        dec_missing.rule_id, "depth.owner_missing",
        "missing owner must deny depth.owner_missing, not depth.exceeded"
    );

    let lead_cycle = Caller {
        name: "cycle-lead".to_string(),
        role: Role::Lead,
        depth: CallerDepth::Cycle(vec!["cycle-lead".to_string()]),
        source: IdentitySource::PaneTokens,
    };
    let dec_cycle = check(&slp, &lead_cycle, Verb::Spawn, "peer");
    assert!(!dec_cycle.allow);
    assert_eq!(
        dec_cycle.rule_id, "depth.cycle",
        "cycle in owner chain must deny depth.cycle, not depth.exceeded"
    );
}

#[test]
fn layout_shadow_evidence_lead_owned_peer_uses_frozen_snapshot_not_live_config() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("shadow-snapshot");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();

    // Lead-owned peer: frz-peer -> frz-lead -> cos
    std::fs::write(runs_dir.join("frz-lead.owner"), "cos\n").unwrap();
    std::fs::write(runs_dir.join("frz-peer.owner"), "frz-lead\n").unwrap();
    std::fs::write(runs_dir.join("other-peer.owner"), "other-lead\n").unwrap();

    // Base template config where peer cannot prompt peer
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let base_cfg: Config = toml::from_str(template_toml).expect("parse template team.toml");

    // Freeze snapshot from base_cfg (peer cannot prompt peer)
    let snap_path = write_layout_snapshot(&runs_dir, "frz", &base_cfg).unwrap();
    assert!(snap_path.is_file());

    // Edit live config to allow peer to prompt peer
    let mut live_cfg = base_cfg.clone();
    live_cfg.roles.get_mut("peer").unwrap().can_prompt = vec!["peer".to_string()];

    // Caller is frz-peer
    let caller = resolve_caller_by_name("frz-peer", &runs_dir);

    // Run shadow with the edited live config
    let decision = quaqua::layout::shadow::<quaqua::herdr::Herdr>(
        Some(&caller),
        None,
        Verb::Prompt,
        "peer",
        Some("other-peer"),
        quaqua::layout::SpawnOrigin::Cli,
        None,
        None,
        &runs_dir,
        Some(&live_cfg),
    );

    // Decision must come from the frozen snapshot (which denies peer -> peer prompt),
    // NOT from live_cfg (which allows it).
    assert!(
        !decision.allow,
        "Lead-owned peer must resolve layout from frozen snapshot, denying prompt"
    );
    assert!(
        decision.rule_id == "slp.peer.prompt.peer" || decision.rule_id == "edge.none",
        "rule_id was {}",
        decision.rule_id
    );

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let would_denies: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .collect();
    assert_eq!(
        would_denies.len(),
        1,
        "expected 1 would_deny event from frozen snapshot"
    );
    assert_eq!(would_denies[0].actor.as_deref(), Some("frz-peer"));
    assert_eq!(would_denies[0].subject.as_deref(), Some("other-peer"));
    assert!(would_denies[0].payload_json.contains(&decision.rule_id));

    // Contrast with a CoS-owned caller with the same live_cfg:
    // CoS-owned work uses live config immediately, so peer prompt is allowed and no would_deny is emitted.
    std::fs::write(runs_dir.join("cos-peer.owner"), "cos\n").unwrap();
    let cos_caller = resolve_caller_by_name("cos-peer", &runs_dir);
    let cos_decision = quaqua::layout::shadow::<quaqua::herdr::Herdr>(
        Some(&cos_caller),
        None,
        Verb::Prompt,
        "peer",
        Some("other-peer"),
        quaqua::layout::SpawnOrigin::Cli,
        None,
        None,
        &runs_dir,
        Some(&live_cfg),
    );
    assert!(
        cos_decision.allow,
        "CoS-owned caller must use live config immediately, allowing prompt"
    );

    let events_after = bus.load_events(0).expect("load events");
    let would_denies_after: Vec<_> = events_after
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .collect();
    assert_eq!(
        would_denies_after.len(),
        1,
        "no new would_deny event for CoS-owned caller"
    );
}

#[test]
fn layout_shadow_evidence_corrupt_snapshot_falls_back_to_live_layout() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("shadow-corrupt-snap");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();

    // Lead-owned peer: frz-peer -> frz-lead -> cos
    std::fs::write(runs_dir.join("frz-lead.owner"), "cos\n").unwrap();
    std::fs::write(runs_dir.join("frz-peer.owner"), "frz-lead\n").unwrap();

    // Write a corrupt snapshot file for project "frz"
    std::fs::write(
        runs_dir.join("frz.layout.toml"),
        "invalid [toml content :::\n",
    )
    .unwrap();

    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let base_cfg: Config = toml::from_str(template_toml).expect("parse template team.toml");

    let caller = resolve_caller_by_name("frz-peer", &runs_dir);

    // shadow must not panic, and must fall back to live config
    let decision = quaqua::layout::shadow::<quaqua::herdr::Herdr>(
        Some(&caller),
        None,
        Verb::Prompt,
        "peer",
        Some("other-peer"),
        quaqua::layout::SpawnOrigin::Cli,
        None,
        None,
        &runs_dir,
        Some(&base_cfg),
    );

    // In base_cfg, peer cannot prompt peer, so it gets denied by the live fallback layout
    assert!(!decision.allow);
    assert_eq!(decision.rule_id, "edge.none");
}
#[test]
fn test_layout_enforce_peer_spawn_denied_exit_1_no_worktree_or_pane() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("enforce-peer-spawn");
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").unwrap();

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
    ]);

    let cfg = test_slp_cfg();
    let git = DummyGit;
    let herdr = MockHerdrTokens::new(
        r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#,
    );

    let opts = quaqua::worker::spawn::parse_spawn_args(&[
        "sude-worker1".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    let res = quaqua::worker::spawn::spawn_worker(
        opts,
        &cfg,
        &git,
        &herdr,
        quaqua::layout::SpawnOrigin::Cli,
    );
    assert!(res.is_err(), "peer spawn must be denied under enforce mode");
    let (code, msg) = res.unwrap_err();
    assert_eq!(code, 1, "deny must exit 1");
    assert!(
        msg.contains("edge.none"),
        "expected rule_id edge.none in msg: {msg}"
    );
    // The recovery hint is scoped to identity.unknown: any other deny must not
    // tell the operator to re-tag a pane that was read fine.
    assert!(
        !msg.contains("hint:"),
        "edge.none deny must carry no identity hint: {msg}"
    );
    assert!(
        !temp.join(".wt/sude-worker1").exists(),
        "no worktree must be created on deny"
    );
    assert_eq!(
        herdr
            .calls
            .borrow()
            .iter()
            .filter(|c| c.contains("pane_split"))
            .count(),
        0,
        "no pane_split on deny"
    );

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let denies: Vec<_> = events.iter().filter(|e| e.event_type == "denied").collect();
    assert_eq!(denies.len(), 1, "expected 1 denied event in triggers");
    assert!(denies[0].payload_json.contains("edge.none"));
}

#[test]
fn test_layout_enforce_cos_lead_spawn_allowed() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("enforce-cos-lead");
    let root = temp.join("root");
    let runs_dir = root.join("runs");
    let repo_dir = root.join("projects/foo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::create_dir_all(root.join("rules/_all")).unwrap();
    std::fs::write(root.join("team.toml"), test_slp_team_toml()).unwrap();
    let index_content = format!(
        "| slug | repo | owner | status | one-line goal |\n|------|------|-------|--------|---------------|\n| foo | {} | cos | active | goal |\n",
        repo_dir.display()
    );
    std::fs::write(root.join("projects/INDEX.md"), index_content).unwrap();
    std::fs::write(root.join("projects/foo.md"), "# foo card\n").unwrap();
    let _env = EnvGuard::set(&[
        ("QUAQUA_HOME", root.to_str().unwrap().to_string()),
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
    ]);

    let git = DummyGit;
    let herdr = MockHerdrTokens::new(r#"{"grid_role":"cos","grid_owner":"cos"}"#);

    let res = quaqua::lead::spawn::spawn_lead(&["foo".to_string()], &git, &herdr);
    assert!(
        res.is_ok(),
        "CoS lead spawn must be allowed: {:?}",
        res.err()
    );
}

#[test]
fn test_layout_enforce_shadow_mode_escape_via_env() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("enforce-shadow-escape");
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").unwrap();

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("QUAQUA_LAYOUT_SHADOW", "1".to_string()),
    ]);

    let cfg = test_slp_cfg();
    let git = DummyGit;
    let herdr = MockHerdrTokens::new(
        r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#,
    );

    let opts = quaqua::worker::spawn::parse_spawn_args(&[
        "sude-worker-esc".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    let res = quaqua::worker::spawn::spawn_worker(
        opts,
        &cfg,
        &git,
        &herdr,
        quaqua::layout::SpawnOrigin::Cli,
    );
    assert!(
        res.is_ok(),
        "QUAQUA_LAYOUT_SHADOW=1 must restore shadow mode and proceed: {:?}",
        res.err()
    );

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let would_denies: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "would_deny")
        .collect();
    assert_eq!(
        would_denies.len(),
        1,
        "expected 1 would_deny event in shadow mode"
    );
}

#[test]
fn test_layout_enforce_cap_message_source_preserved() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("enforce-cap-source");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();

    let _cfg = test_slp_cfg();
    let lead_caller = Caller {
        name: "baro-lead".to_string(),
        role: Role::Lead,
        depth: CallerDepth::Resolved(1),
        source: IdentitySource::PaneTokens,
    };

    let slp = builtin::get("slp").expect("slp");
    // baro-lead with current_children = 4 (cap is 4)
    let dec = check_with_count(&slp, &lead_caller, Verb::Spawn, "peer", Some(4));
    assert!(!dec.allow);
    assert_eq!(dec.rule_id, "cap.exceeded");
    let trace_cap = dec.trace.iter().find(|t| !t.passed).expect("failing step");
    assert!(
        trace_cap.detail.contains("roles.lead.max_peers is 4"),
        "must preserve lead cap source: {}",
        trace_cap.detail
    );
    assert!(
        trace_cap.detail.contains("worker cap exceeded"),
        "must preserve cap error prefix: {}",
        trace_cap.detail
    );

    let cos_caller = Caller {
        name: "cos".to_string(),
        role: Role::Cos,
        depth: CallerDepth::Resolved(0),
        source: IdentitySource::PaneTokens,
    };
    // cos with current_children = 8 (cap is 8)
    let dec_cos = check_with_count(&slp, &cos_caller, Verb::Spawn, "peer", Some(8));
    assert!(!dec_cos.allow);
    assert_eq!(dec_cos.rule_id, "cap.exceeded");
    let trace_cos = dec_cos
        .trace
        .iter()
        .find(|t| !t.passed)
        .expect("failing step");
    assert!(
        trace_cos
            .detail
            .contains("limits.max_workers_per_orchestrator is 8"),
        "must preserve cos cap source: {}",
        trace_cos.detail
    );
    assert!(
        trace_cos.detail.contains("worker cap exceeded"),
        "must preserve cap error prefix: {}",
        trace_cos.detail
    );
}

#[test]
fn test_layout_pre_p3_untagged_lead_pane_reproduction_and_healing() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("pre-p3-lead-healing");
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").unwrap();

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "wB:p1".to_string()),
    ]);

    let cfg = test_slp_cfg();
    let git = DummyGit;
    // Step 1: Pre-P3 untagged Lead pane (only logo, no grid_role/grid_owner)
    let herdr = MockHerdrTokens::new(r#"{"logo":"\ue1a3"}"#);

    let opts1 = quaqua::worker::spawn::parse_spawn_args(&[
        "tieude-mulows1".to_string(),
        "tieude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Spawning from untagged pane reproduces FALSE DENY (denied with identity.unknown)
    let res1 = quaqua::worker::spawn::spawn_worker(
        opts1,
        &cfg,
        &git,
        &herdr,
        quaqua::layout::SpawnOrigin::Cli,
    );
    assert!(
        res1.is_err(),
        "untagged pre-P3 lead pane must fail under enforce mode"
    );
    let (code1, msg1) = res1.unwrap_err();
    assert_eq!(code1, 1);
    assert!(
        msg1.contains("identity.unknown"),
        "expected identity.unknown: {msg1}"
    );
    // An untagged pane gets the recovery hint (pane id + repair command), so
    // the operator does not have to read the gate source to recover.
    assert!(
        msg1.contains("hint:"),
        "identity.unknown deny must carry the recovery hint: {msg1}"
    );

    // Step 2: Heal the Lead pane via tag_lead_pane (setting grid_role=orchestrator, grid_owner=mu-lead)
    let tag_res =
        quaqua::lead::tag::tag_lead_pane(&herdr, "mu", "wB:p1", &quaqua::workspace::farm_id());
    assert!(tag_res.is_ok(), "tagging lead pane must succeed");
    // Update herdr tokens to reflect the tag

    let opts2 = quaqua::worker::spawn::parse_spawn_args(&[
        "tieude-mulows2".to_string(),
        "tieude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    // Spawning from healed Lead pane now succeeds with ALLOW!
    let res2 = quaqua::worker::spawn::spawn_worker(
        opts2,
        &cfg,
        &git,
        &herdr,
        quaqua::layout::SpawnOrigin::Cli,
    );
    assert!(
        res2.is_ok(),
        "spawn from healed Lead pane must succeed: {:?}",
        res2.err()
    );
}

#[test]
fn test_lead_tagged_caller_records_owner_slug_lead_and_accounts_to_lead_cap() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("lead-tagged-owner-cap");
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").unwrap();

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "wB:p1".to_string()),
        ("QUAQUA_OWNER", "cos".to_string()), // Stale shell env saying cos!
    ]);

    let cfg = test_slp_cfg();
    let git = DummyGit;
    // Lead pane tagged with grid_role=orchestrator, grid_owner=mu-lead
    let herdr = MockHerdrTokens::new(
        r#"{"grid_role":"orchestrator","grid_owner":"mu-lead","logo":"\ue1a3"}"#,
    );

    let opts = quaqua::worker::spawn::parse_spawn_args(&[
        "tieude-mulows-owned".to_string(),
        "tieude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    let res = quaqua::worker::spawn::spawn_worker(
        opts,
        &cfg,
        &git,
        &herdr,
        quaqua::layout::SpawnOrigin::Cli,
    );
    assert!(res.is_ok(), "spawn must succeed: {:?}", res.err());

    // Invariant 1: runs/<name>.owner must be mu-lead (not stale shell env cos)
    let owner_file = runs_dir.join("tieude-mulows-owned.owner");
    let owner = std::fs::read_to_string(&owner_file).expect("read owner file");
    assert_eq!(owner.trim(), "mu-lead", "owner file must record mu-lead");

    // Invariant 2: runs/<name>.grid must be mu-workers
    let grid_file = runs_dir.join("tieude-mulows-owned.grid");
    let grid = std::fs::read_to_string(&grid_file).expect("read grid file");
    assert_eq!(
        grid.trim(),
        "mu-workers",
        "grid tab must route to mu-workers"
    );

    // Invariant 3: slots must count against mu-lead, NOT cos!
    let mu_slots = quaqua::cap::count_owner_slots(&runs_dir, "mu-lead", None, &herdr, &cfg);
    assert_eq!(mu_slots, 1, "mu-lead slots must be 1");
    let cos_slots = quaqua::cap::count_owner_slots(&runs_dir, "cos", None, &herdr, &cfg);
    assert_eq!(
        cos_slots, 0,
        "cos slots must be 0 (not polluted by lead peer)"
    );
}

#[test]
fn test_layout_slp_full_matrix_60_cases() {
    let fixture_str = include_str!("fixtures/layout/slp_replay_matrix.json");
    let fixture: serde_json::Value = serde_json::from_str(fixture_str).expect("parse fixture");
    let cases = fixture["cases"].as_array().expect("cases array");
    assert_eq!(cases.len(), 60, "fixture must contain all 60 cases");

    let slp = builtin::get("slp").expect("slp layout");
    let runs_dir = hermetic_temp_dir("matrix-check");
    std::fs::write(runs_dir.join("my-lead.owner"), "cos\n").unwrap();
    std::fs::write(runs_dir.join("my-peer.owner"), "my-lead\n").unwrap();

    for case in cases {
        let caller_role_str = case["caller_role"].as_str().unwrap();
        let verb_str = case["verb"].as_str().unwrap();
        let target_role_str = case["target_role"].as_str().unwrap();
        let want_allowed = case["allowed"].as_bool().unwrap();
        let want_rule_id = case["rule_id"].as_str().unwrap();

        let (caller_role, caller_name, depth) = match caller_role_str {
            "cos" => (Role::Cos, "cos".to_string(), CallerDepth::Resolved(0)),
            "lead" => (Role::Lead, "my-lead".to_string(), CallerDepth::Resolved(1)),
            "peer" => (Role::Peer, "my-peer".to_string(), CallerDepth::Resolved(2)),
            "taskforce" => (Role::Peer, "my-tf".to_string(), CallerDepth::Resolved(2)),
            _ => (Role::Unknown, "unknown".to_string(), CallerDepth::Unknown),
        };

        let caller = Caller {
            name: caller_name,
            role: caller_role,
            depth,
            source: IdentitySource::PaneTokens,
        };

        let verb = match verb_str {
            "spawn" => Verb::Spawn,
            "prompt" => Verb::Prompt,
            "mail" => Verb::Mail,
            other => panic!("unknown verb: {other}"),
        };

        let target = match target_role_str {
            "cos" => "cos",
            "lead" => "target-lead",
            "peer" => "peer",
            "taskforce" => "taskforce",
            other => other,
        };

        let decision = check(&slp, &caller, verb, target);
        assert_eq!(
            decision.allow, want_allowed,
            "case {caller_role_str} -> {verb_str} -> {target_role_str}: want allow={}, got allow={} (rule_id={})",
            want_allowed, decision.allow, decision.rule_id
        );
        assert_eq!(
            decision.rule_id, want_rule_id,
            "case {caller_role_str} -> {verb_str} -> {target_role_str}: want rule_id={}, got rule_id={}",
            want_rule_id, decision.rule_id
        );
    }
}
#[test]
fn test_layout_gate_cos_token_loss_after_herdr_restart_self_heals() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("cos-restart-self-heals");
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();

    let root = temp.join("workspace");
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::create_dir_all(root.join("rules/_all")).unwrap();
    std::fs::write(root.join("team.toml"), test_slp_team_toml()).unwrap();

    // Record initial CoS identity in runs/cos.identity.json
    let cos_session_id = "ses-cos-restart-test-99";
    std::fs::write(
        runs_dir.join("cos.identity.json"),
        format!(r#"{{"agent_session": "{cos_session_id}", "pane_id": "w1:p1"}}"#),
    )
    .unwrap();

    let _env = EnvGuard::set(&[
        ("QUAQUA_HOME", root.to_str().unwrap().to_string()),
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
    ]);
    // Ensure QUAQUA_OWNER and QUAQUA_LAYOUT_SHADOW are not set, testing true enforcement without env bypass
    std::env::remove_var("QUAQUA_OWNER");
    std::env::remove_var("QUAQUA_LAYOUT_SHADOW");

    let cfg = test_slp_cfg();

    // After a herdr server restart:
    // 1. Pane tokens are lost (no grid_role or grid_owner; only logo remains)
    // 2. Herdr still persists agent_session on the pane
    // 3. herdr agent list reports the pane with matching agent_session and no agent name
    let agents_json = format!(
        r#"{{"result":{{"agents":[{{"pane_id":"w1:p1","agent":"claude","tokens":{{"logo":"\ue1a0"}},"agent_session":{{"value":"{cos_session_id}"}}}}]}}}}"#
    );
    let herdr = MockHerdrTokens::with_session(r#"{"logo":"\ue1a0"}"#, cos_session_id, agents_json);

    // Call layout gate for CoS spawning a peer (e.g. `quaqua worker spawn`)
    let res = quaqua::layout::gate(
        None,
        Some(&herdr),
        quaqua::layout::Verb::Spawn,
        "peer",
        Some("sude-worker-repro"),
        quaqua::layout::SpawnOrigin::Cli,
        Some("cos"),
        Some("repo"),
        &runs_dir,
        Some(&cfg),
    );

    // Goal: After a herdr restart, CoS call through layout gate must be allowed without manual `quaqua cos tag`
    assert!(
        res.is_ok(),
        "CoS call through layout gate after herdr restart must succeed via self-healing, but got: {:?}",
        res.err()
    );

    // And verify that pane tokens were healed to grid_role=cos
    assert!(
        herdr.tokens_json.borrow().contains(r#""grid_role":"cos""#),
        "pane tokens must be healed with grid_role=cos: {}",
        herdr.tokens_json.borrow()
    );

    // Also verify spawn_worker directly succeeds using the self-healed identity
    let git = DummyGit;
    let opts = quaqua::worker::spawn::parse_spawn_args(&[
        "sude-worker-repro2".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    let spawn_res = quaqua::worker::spawn::spawn_worker(
        opts,
        &cfg,
        &git,
        &herdr,
        quaqua::layout::SpawnOrigin::Cli,
    );
    assert!(
        spawn_res.is_ok(),
        "spawn_worker from self-healed CoS pane must succeed: {:?}",
        spawn_res.err()
    );
}

#[test]
fn test_layout_gate_unknown_pane_mismatched_session_fails_closed() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("cos-mismatch-fails-closed");
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();

    let root = temp.join("workspace");
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::create_dir_all(root.join("rules/_all")).unwrap();
    std::fs::write(root.join("team.toml"), test_slp_team_toml()).unwrap();

    // Record legitimate CoS identity
    std::fs::write(
        runs_dir.join("cos.identity.json"),
        r#"{"agent_session": "ses-real-cos", "pane_id": "w1:p1"}"#,
    )
    .unwrap();

    let _env = EnvGuard::set(&[
        ("QUAQUA_HOME", root.to_str().unwrap().to_string()),
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
    ]);
    std::env::remove_var("QUAQUA_OWNER");
    std::env::remove_var("QUAQUA_LAYOUT_SHADOW");

    let cfg = test_slp_cfg();

    // The pane has a DIFFERENT session ("ses-impostor")
    let agents_json = r#"{"result":{"agents":[{"pane_id":"w1:p1","agent":"claude","tokens":{"logo":"\ue1a0"},"agent_session":{"value":"ses-impostor"}}]}}"#;
    let herdr = MockHerdrTokens::with_session(r#"{"logo":"\ue1a0"}"#, "ses-impostor", agents_json);

    let res = quaqua::layout::gate(
        None,
        Some(&herdr),
        quaqua::layout::Verb::Spawn,
        "peer",
        Some("sude-worker-fail"),
        quaqua::layout::SpawnOrigin::Cli,
        Some("cos"),
        Some("repo"),
        &runs_dir,
        Some(&cfg),
    );

    // Mismatched session must FAIL closed: denied with identity.unknown
    assert!(
        res.is_err(),
        "unmatched pane session must fail closed with identity denial"
    );
    let (code, msg) = res.unwrap_err();
    assert_eq!(code, 1);
    assert!(
        msg.contains("identity.unknown"),
        "must deny with identity.unknown: {msg}"
    );
    assert!(
        msg.contains("escape hatch: QUAQUA_LAYOUT_SHADOW=1"),
        "hint must carry QUAQUA_LAYOUT_SHADOW escape hatch: {msg}"
    );

    // Tokens must NOT have been modified to cos
    assert!(
        !herdr.tokens_json.borrow().contains(r#""grid_role":"cos""#),
        "unmatched pane must not receive cos token: {}",
        herdr.tokens_json.borrow()
    );
}

#[test]
fn test_layout_gate_cos_restart_without_tokens_key_self_heals() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("cos-restart-no-tokens-key");
    let repo_dir = temp.join("repo");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();

    let root = temp.join("workspace");
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::create_dir_all(root.join("rules/_all")).unwrap();
    std::fs::write(root.join("team.toml"), test_slp_team_toml()).unwrap();

    let cos_session_id = "ses-cos-no-tokens-41";
    std::fs::write(
        runs_dir.join("cos.identity.json"),
        format!(r#"{{"agent_session":"{cos_session_id}","pane_id":"w1:p1"}}"#),
    )
    .unwrap();

    // QUAQUA_OWNER empty: this case is about the pane, not about the env seam.
    let _env = EnvGuard::set(&[
        ("QUAQUA_HOME", root.to_str().unwrap().to_string()),
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("QUAQUA_OWNER", String::new()),
        ("QUAQUA_LAYOUT_SHADOW", String::new()),
    ]);

    let cfg = test_slp_cfg();

    // herdr restored this pane from session.json — cwd, label and agent_session
    // survive the restart, the tokens do not, and `pane get` then omits the
    // `tokens` field instead of sending an empty object.
    let agents_json = format!(
        r#"{{"result":{{"agents":[{{"pane_id":"w1:p1","agent":"claude","agent_session":{{"value":"{cos_session_id}"}}}}]}}}}"#
    );
    let herdr = MockHerdrTokens::tokenless(cos_session_id, agents_json);
    assert!(
        herdr.tokens_absent.get(),
        "fixture must omit the tokens key"
    );

    let res = quaqua::layout::gate(
        None,
        Some(&herdr),
        quaqua::layout::Verb::Spawn,
        "peer",
        Some("sude-worker-repro"),
        quaqua::layout::SpawnOrigin::Cli,
        Some("cos"),
        Some("repo"),
        &runs_dir,
        Some(&cfg),
    );

    assert!(
        res.is_ok(),
        "CoS call after a restart whose pane carries no tokens key must self-heal, got: {:?}",
        res.err()
    );
    assert!(
        herdr.tokens_json.borrow().contains(r#""grid_role":"cos""#),
        "healed pane must carry grid_role=cos: {}",
        herdr.tokens_json.borrow()
    );
}

#[test]
fn test_worker_spawn_from_tokenless_worker_pane_with_env_cos_is_denied() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("env-cos-spoof");
    let root = temp.join("root");
    let runs_dir = root.join("runs");
    let repo_dir = root.join("projects/peers");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::create_dir_all(root.join("rules/_all")).unwrap();
    std::fs::write(root.join("team.toml"), test_slp_team_toml()).unwrap();

    let cos_session_id = "ses-cos-live-7";
    std::fs::write(
        runs_dir.join("cos.identity.json"),
        format!(r#"{{"agent_session":"{cos_session_id}","pane_id":"w1:p1X"}}"#),
    )
    .unwrap();

    // Shape of a real worker pane after a herdr restart: herdr exports
    // HERDR_PANE_ID and HERDR_BIN_PATH, the CoS export QUAQUA_OWNER=cos into the
    // shell, the restart dropped the pane's tokens, and nothing sets QUAQUA_TEST.
    let _env = EnvGuard::set(&[
        ("QUAQUA_HOME", root.to_str().unwrap().to_string()),
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", String::new()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:pBJ".to_string()),
        ("HERDR_BIN_PATH", "/usr/local/bin/herdr".to_string()),
        ("QUAQUA_OWNER", "cos".to_string()),
        ("QUAQUA_LAYOUT_SHADOW", String::new()),
    ]);

    let cfg = test_slp_cfg();
    let agents_json = format!(
        r#"{{"result":{{"agents":[
            {{"pane_id":"w1:p1X","agent":"claude","agent_session":{{"value":"{cos_session_id}"}}}},
            {{"pane_id":"w1:pBJ","name":"suhuynh-163","agent":"pi"}}
        ]}}}}"#
    );
    let herdr = MockHerdrTokens::tokenless(cos_session_id, agents_json);

    let opts = quaqua::worker::spawn::parse_spawn_args(&[
        "sude-spoof".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ])
    .unwrap();

    let res = quaqua::worker::spawn::spawn_worker(
        opts,
        &cfg,
        &DummyGit,
        &herdr,
        quaqua::layout::SpawnOrigin::Cli,
    );

    let (code, msg) = res.expect_err(
        "QUAQUA_OWNER=cos from a worker pane must never make the caller the CoS, even \
         after the restart dropped the pane's tokens",
    );
    assert_eq!(code, 1, "identity deny must exit 1: {msg}");
    assert!(
        msg.contains("identity.unknown"),
        "expected an identity.unknown deny, got: {msg}"
    );
    assert!(
        !root.join("projects/.wt/sude-spoof").exists(),
        "no worktree must be created for a denied spawn"
    );
    assert_eq!(
        herdr
            .calls
            .borrow()
            .iter()
            .filter(|c| c.contains("pane_split"))
            .count(),
        0,
        "no pane_split on deny"
    );
}

#[test]
fn test_lead_to_lead_prompt_allowed_and_peer_denied() {
    let lead_caller = Caller {
        name: "alpha-lead".to_string(),
        role: Role::Lead,
        depth: CallerDepth::Resolved(1),
        source: IdentitySource::PaneTokens,
    };
    let peer_caller = Caller {
        name: "worker-peer".to_string(),
        role: Role::Peer,
        depth: CallerDepth::Resolved(2),
        source: IdentitySource::PaneTokens,
    };

    // 1. Built-in SLP layout
    let slp = builtin::get("slp").expect("slp layout");
    let dec_lead_lead = check(&slp, &lead_caller, Verb::Prompt, "lead");
    assert!(
        dec_lead_lead.allow,
        "lead must be allowed to prompt lead under slp layout"
    );
    assert_eq!(dec_lead_lead.rule_id, "slp.lead.can_prompt.lead");

    let dec_lead_peer = check(&slp, &lead_caller, Verb::Prompt, "peer");
    assert!(
        dec_lead_peer.allow,
        "lead must be allowed to prompt peer under slp layout"
    );
    assert_eq!(dec_lead_peer.rule_id, "slp.lead.can_prompt.peer");

    let dec_peer_lead = check(&slp, &peer_caller, Verb::Prompt, "lead");
    assert!(
        !dec_peer_lead.allow,
        "peer must not be allowed to prompt lead under slp layout"
    );
    assert_eq!(dec_peer_lead.rule_id, "edge.none");

    // 2. Built-in lead-peer layout
    let lp = builtin::get("lead-peer").expect("lead-peer layout");
    let dec_lp_lead_lead = check(&lp, &lead_caller, Verb::Prompt, "lead");
    assert!(
        dec_lp_lead_lead.allow,
        "lead must be allowed to prompt lead under lead-peer layout"
    );
    assert_eq!(dec_lp_lead_lead.rule_id, "lead-peer.lead.can_prompt.lead");

    let dec_lp_peer_lead = check(&lp, &peer_caller, Verb::Prompt, "lead");
    assert!(
        !dec_lp_peer_lead.allow,
        "peer must not be allowed to prompt lead under lead-peer layout"
    );
    assert_eq!(dec_lp_peer_lead.rule_id, "edge.none");

    // 3. Workspace template team.toml
    let template_toml = include_str!("../../../templates/workspace/team.toml");
    let cfg: Config = toml::from_str(template_toml).expect("parse template team.toml");
    let legacy = Layout::from_legacy(&cfg);
    let dec_tmpl_lead_lead = check(&legacy, &lead_caller, Verb::Prompt, "lead");
    assert!(
        dec_tmpl_lead_lead.allow,
        "lead must be allowed to prompt lead under workspace template"
    );
    assert_eq!(dec_tmpl_lead_lead.rule_id, "slp.lead.can_prompt.lead");

    let dec_tmpl_peer_lead = check(&legacy, &peer_caller, Verb::Prompt, "lead");
    assert!(
        !dec_tmpl_peer_lead.allow,
        "peer must not be allowed to prompt lead under workspace template"
    );
    assert_eq!(dec_tmpl_peer_lead.rule_id, "edge.none");
}
