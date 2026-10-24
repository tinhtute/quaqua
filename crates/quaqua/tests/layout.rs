use quaqua::config::{self, Config};
use quaqua::identity::{Caller, CallerDepth, IdentitySource, Role};
use quaqua::layout::{
    builtin, check, check_with_count, resolve_caller_by_name, resolve_layout, run_show,
    run_why, why, Layout, Verb,
};
use std::path::PathBuf;

fn hermetic_temp_dir(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("quaqua-layout-test-{prefix}-{}-{nanos}", std::process::id()));
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
        .replace("schema_version = 2", "schema_version = 3\n[layout]\nuse = \"slp\"")
        .replace("max_workers_per_orchestrator = 8", "max_workers_per_orchestrator = 3")
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
        let layout = builtin::get(name).unwrap_or_else(|| panic!("failed to load built-in '{name}'"));
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
    assert_eq!(council_role.members[2].purpose.as_deref(), Some("advise"));
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
    assert_eq!(cfg.layout.as_ref().map(|l| l.use_layout.as_str()), Some("slp"));

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
    let err = toml::from_str::<Config>(bad_layout).expect_err("unknown field in [layout] must fail");
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
        .replace("schema_version = 2", "schema_version = 3\n[layout]\nuse = \"slp\"")
        .replace("max_workers_per_orchestrator = 8", "max_workers_per_orchestrator = 3")
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
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
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
    fn run_in(&self, _cwd: &std::path::Path, args: &[&str]) -> std::io::Result<std::process::Output> {
        let stdout = if args.first() == Some(&"rev-parse") {
            "391d347df43909dec802c632156fb6ff23c98815\n".as_bytes().to_vec()
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
    fn worktree_remove(&self, _repo: &std::path::Path, wt: &std::path::Path) -> std::io::Result<std::process::Output> {
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
    fn enable_worktree_config(&self, _repo: &std::path::Path) -> std::io::Result<std::process::Output> {
        self.run_in(_repo, &[])
    }
    fn set_worktree_pushurl_blocked(&self, _wt: &std::path::Path, _remote: &str) -> std::io::Result<()> {
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
    tokens_json: String,
    started: std::cell::Cell<bool>,
}

impl MockHerdrTokens {
    fn new(tokens_json: impl Into<String>) -> Self {
        Self {
            tokens_json: tokens_json.into(),
            started: std::cell::Cell::new(false),
        }
    }
}

impl quaqua::herdr::HerdrOps for MockHerdrTokens {
    fn run(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        let stdout = if args.first() == Some(&"pane") && args.get(1) == Some(&"get") {
            format!(r#"{{"result":{{"pane":{{"pane_id":"w1:p1","tokens":{}}}}}}}"#, self.tokens_json)
        } else if args.first() == Some(&"workspace") && args.get(1) == Some(&"create") {
            r#"{"result":{"workspace":{"workspace_id":"w1"},"root_pane":{"pane_id":"w1:p1"}}}"#.to_string()
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
    fn agent_list(&self) -> std::io::Result<String> { Ok(r#"{"result":{"agents":[]}}"#.to_string()) }
    fn agent_get(&self, _name: &str) -> std::io::Result<(bool, String)> {
        if self.started.get() {
            Ok((true, r#"{"result":{"agent":{"agent_status":"working","pane_id":"w1:p1"}}}"#.to_string()))
        } else {
            Ok((false, r#"{"error":{"code":"agent_not_found"}}"#.to_string()))
        }
    }
    fn agent_read(&self, _n: &str, _s: &str, _l: u32) -> std::io::Result<String> { Ok(String::new()) }
    fn agent_start(&self, _n: &str, _k: &str, _p: &str, _t: u32, _a: &[&str]) -> std::io::Result<std::process::Output> {
        self.started.set(true);
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: r#"{"result":{}}"#.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }
    fn pane_split(&self, _c: &str, _o: &str, _e: &[(&str, &str)]) -> std::io::Result<std::process::Output> {
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
    fn pane_wait_output(&self, _p: &str, _m: &str, _t: u32) -> bool { true }
    fn pane_read(&self, _p: &str, _s: &str, _l: u32) -> std::io::Result<String> { Ok(String::new()) }
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
    fn agent_prompt_wait_background(&self, _n: &str, _p: &str, _t: u32, _f: std::fs::File) -> std::io::Result<std::process::Child> {
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
    ]);

    let cfg = test_slp_cfg();
    let git = DummyGit;
    let herdr = MockHerdrTokens::new(r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#);

    let opts = quaqua::worker::spawn::parse_spawn_args(&[
        "sude-worker1".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ]).unwrap();

    let res = quaqua::worker::spawn::spawn_worker(opts, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);
    assert!(res.is_ok(), "spawn_worker must succeed (proceed past shadow gate): {:?}", res.err());

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let would_denies: Vec<_> = events.iter().filter(|e| e.event_type == "would_deny").collect();
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
    ]);

    let git = DummyGit;
    let herdr = MockHerdrTokens::new(r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#);

    let res = quaqua::lead::spawn::spawn_lead(&["foo".to_string()], &git, &herdr);
    assert!(res.is_ok(), "spawn_lead must succeed (proceed past shadow gate): {:?}", res.err());

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let would_denies: Vec<_> = events.iter().filter(|e| e.event_type == "would_deny").collect();
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
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").unwrap();
    let _ = std::process::Command::new("git").args(["init", repo_dir.to_str().unwrap()]).output();
    let _ = std::process::Command::new("git").args(["-C", repo_dir.to_str().unwrap(), "config", "user.name", "Test"]).output();
    let _ = std::process::Command::new("git").args(["-C", repo_dir.to_str().unwrap(), "config", "user.email", "test@test.com"]).output();
    let _ = std::process::Command::new("git").args(["-C", repo_dir.to_str().unwrap(), "commit", "--allow-empty", "-m", "init"]).output();
    let brief_file = temp.join("brief.md");
    std::fs::write(&brief_file, "Brief content").unwrap();

    let stub = stub_herdr_script(&temp, r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#);

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("HERDR_BIN_PATH", stub.to_str().unwrap().to_string()),
    ]);

    let git = quaqua::git::Git::new();
    let herdr = quaqua::herdr::Herdr::new();

    let res = quaqua::taskforce::handle_taskforce(&[
        "advisor".to_string(),
        "mytest".to_string(),
        repo_dir.to_str().unwrap().to_string(),
        "--brief".to_string(),
        brief_file.to_str().unwrap().to_string(),
    ], &git, &herdr);
    assert!(res.is_ok(), "handle_taskforce must succeed (proceed past shadow gate): {:?}", res.err());

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
    std::fs::write(runs_dir.join("sude-target.md"), "Outcome: COMPLETE\n\n## Blockers\nnone\n\n## Lessons\nnone\n").unwrap();

    let stub = stub_herdr_script(&temp, r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#);

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("HERDR_BIN_PATH", stub.to_str().unwrap().to_string()),
        ("PROMPT_WORKER_POLL_SEC", "1".to_string()),
        ("PROMPT_WORKER_IDLE_SEC", "1".to_string()),
        ("PROMPT_WORKER_STALL_SEC", "1".to_string()),
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
    assert!(res.is_ok(), "prompt_worker must succeed (proceed past shadow gate): {:?}", res.err());

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let would_denies: Vec<_> = events.iter().filter(|e| e.event_type == "would_deny").collect();
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
    ]);

    let herdr = MockHerdrTokens::new(r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#);
    herdr.started.set(true);

    let res = quaqua::lead::prompt::prompt_lead(&["baro".to_string(), "hello".to_string()], &herdr);
    assert!(res.is_ok(), "prompt_lead must succeed (proceed past shadow gate): {:?}", res.err());

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events = bus.load_events(0).expect("load events");
    let would_denies: Vec<_> = events.iter().filter(|e| e.event_type == "would_deny").collect();
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
    let would_denies: Vec<_> = events.iter().filter(|e| e.event_type == "would_deny").collect();
    assert_eq!(would_denies.len(), 0, "CoS lead spawn must write 0 would_deny events");
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
    ]);

    let cfg = test_slp_cfg();
    let git = DummyGit;
    // Caller is a peer directly owned by cos (depth 1, target depth 2 <= layout limit 2).
    // In SLP, peer has no can_spawn edge to peer.
    let herdr = MockHerdrTokens::new(r#"{"grid_role":"worker","grid_owner":"cos","grid_name":"tieude-peer"}"#);

    // 1) With Origin::Cli: peer has no spawn edge, so shadow writes 1 would_deny row for edge.none.
    let opts_cli = quaqua::worker::spawn::parse_spawn_args(&[
        "sude-cliworker".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ]).unwrap();
    let res_cli = quaqua::worker::spawn::spawn_worker(opts_cli, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::Cli);
    assert!(res_cli.is_ok(), "spawn_worker with Cli must succeed (shadow never blocks): {:?}", res_cli.err());

    let db_path = runs_dir.join("quaqua.db");
    let bus = quaqua::bus::FarmBus::open(&db_path).expect("open bus");
    let events_after_cli = bus.load_events(0).expect("load events");
    let cli_would_denies: Vec<_> = events_after_cli.iter().filter(|e| e.event_type == "would_deny").collect();
    assert_eq!(cli_would_denies.len(), 1, "Cli origin must record 1 would_deny event for peer caller");
    assert_eq!(cli_would_denies[0].actor.as_deref(), Some("tieude-peer"));
    assert_eq!(cli_would_denies[0].subject.as_deref(), Some("sude-cliworker"));
    assert!(cli_would_denies[0].payload_json.contains("edge.none"));

    // 2) With Origin::TaskforceMember: edge check is skipped, depth 2 <= 2 passes, so NO new would_deny event is written.
    let opts_tf = quaqua::worker::spawn::parse_spawn_args(&[
        "sude-tfworker".to_string(),
        "sude".to_string(),
        repo_dir.to_str().unwrap().to_string(),
    ]).unwrap();
    let res_tf = quaqua::worker::spawn::spawn_worker(opts_tf, &cfg, &git, &herdr, quaqua::layout::SpawnOrigin::TaskforceMember);
    assert!(res_tf.is_ok(), "spawn_worker with TaskforceMember must succeed: {:?}", res_tf.err());

    let events_after_tf = bus.load_events(0).expect("load events");
    let tf_would_denies: Vec<_> = events_after_tf.iter().filter(|e| e.event_type == "would_deny").collect();
    assert_eq!(tf_would_denies.len(), 1, "TaskforceMember skips edge, so count of would_deny events stays 1 (no edge.none row added)");
}

#[test]
fn layout_shadow_evidence_worker_prompt_resume_dedupes_would_deny() {
    let _lock = env_lock();
    let temp = hermetic_temp_dir("shadow-resume");
    let runs_dir = temp.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::write(runs_dir.join("sude-resumetgt.owner"), "other-lead\n").unwrap();
    std::fs::write(runs_dir.join("sude-resumetgt.md"), "Outcome: COMPLETE\n\n## Blockers\nnone\n\n## Lessons\nnone\n").unwrap();

    let stub = stub_herdr_script(&temp, r#"{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"tieude-peer"}"#);

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap().to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("HERDR_PANE_ID", "w1:p1".to_string()),
        ("HERDR_BIN_PATH", stub.to_str().unwrap().to_string()),
        ("PROMPT_WORKER_POLL_SEC", "1".to_string()),
        ("PROMPT_WORKER_IDLE_SEC", "1".to_string()),
        ("PROMPT_WORKER_STALL_SEC", "1".to_string()),
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
    assert!(res1.is_ok(), "initial prompt must succeed: {:?}", res1.err());

    let bus = quaqua::bus::FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let events1 = bus.load_events(0).expect("load events");
    let count1 = events1.iter().filter(|e| e.event_type == "would_deny").count();
    assert_eq!(count1, 1, "initial prompt writes 1 would_deny");

    // Resume prompt (resume=true) dedupes and does not write another would_deny
    let res2 = quaqua::worker::prompt::prompt_worker(
        "sude-resumetgt",
        None,
        true,
        None,
        &git,
        &herdr,
    );
    assert!(res2.is_ok(), "resume prompt must succeed: {:?}", res2.err());

    let events2 = bus.load_events(0).expect("load events");
    let count2 = events2.iter().filter(|e| e.event_type == "would_deny").count();
    assert_eq!(count2, 1, "resume prompt must dedupe and not write another would_deny");
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
    assert_eq!(dec_missing.rule_id, "depth.owner_missing", "missing owner must deny depth.owner_missing, not depth.exceeded");

    let lead_cycle = Caller {
        name: "cycle-lead".to_string(),
        role: Role::Lead,
        depth: CallerDepth::Cycle(vec!["cycle-lead".to_string()]),
        source: IdentitySource::PaneTokens,
    };
    let dec_cycle = check(&slp, &lead_cycle, Verb::Spawn, "peer");
    assert!(!dec_cycle.allow);
    assert_eq!(dec_cycle.rule_id, "depth.cycle", "cycle in owner chain must deny depth.cycle, not depth.exceeded");
}
