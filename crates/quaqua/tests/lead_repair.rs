//! Tests for `quaqua lead repair <slug>` and token healing.

use quaqua::herdr::HerdrOps;
use quaqua::identity::heal;
use quaqua::lead::tag::{repair_all_leads, repair_lead};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

fn hermetic_temp_dir(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "quaqua-lead-repair-test-{prefix}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create hermetic temp dir");
    dir
}

static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    vars: Vec<(&'static str, Option<String>)>,
}

/// Vars that decide *caller* identity for `repair_lead`, `repair_all_leads` and `heal`:
/// `identity::resolve_caller` reads `HERDR_PANE_ID` to find the caller's pane and falls back
/// to `QUAQUA_OWNER`. A herdr session exports both, so a test that does not pin them resolves
/// the caller to the pane it happens to run in — the mock's `pane get` then hands it the
/// fixture's tokens, and `repair_lead` correctly refuses to let that Lead own itself
/// (`depth.cycle`). `EnvGuard` always pins them so no test inherits ambient identity.
const AMBIENT_IDENTITY_VARS: &[&str] = &["HERDR_PANE_ID", "QUAQUA_OWNER"];

impl EnvGuard {
    fn new(keys: &[&'static str]) -> Self {
        let lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut all: Vec<&'static str> = AMBIENT_IDENTITY_VARS.to_vec();
        for &k in keys {
            if !all.contains(&k) {
                all.push(k);
            }
        }
        let vars = all.iter().map(|&k| (k, std::env::var(k).ok())).collect();
        for k in AMBIENT_IDENTITY_VARS {
            std::env::remove_var(k);
        }
        Self { _lock: lock, vars }
    }
    fn set(&self, key: &'static str, val: &str) {
        std::env::set_var(key, val);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.vars {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

struct MockRepairHerdr {
    tokens_json: RefCell<String>,
    reported_tokens: RefCell<Vec<String>>,
    agent_list_json: RefCell<String>,
}

impl MockRepairHerdr {
    fn new(tokens_json: &str, agents_json: &str) -> Self {
        Self {
            tokens_json: RefCell::new(tokens_json.to_string()),
            reported_tokens: RefCell::new(Vec::new()),
            agent_list_json: RefCell::new(agents_json.to_string()),
        }
    }
}

impl HerdrOps for MockRepairHerdr {
    fn run(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        let stdout = if args.first() == Some(&"pane") && args.get(1) == Some(&"get") {
            format!(
                r#"{{"result":{{"pane":{{"pane_id":"wB:p1","tokens":{}}}}}}}"#,
                self.tokens_json.borrow()
            )
        } else if args.first() == Some(&"pane") && args.get(1) == Some(&"report-metadata") {
            let mut role = None;
            let mut owner = None;
            let mut farm = None;
            for arg in args {
                self.reported_tokens.borrow_mut().push(arg.to_string());
                if let Some(r) = arg.strip_prefix("grid_role=") {
                    role = Some(r);
                } else if let Some(o) = arg.strip_prefix("grid_owner=") {
                    owner = Some(o);
                } else if let Some(f) = arg.strip_prefix("grid_farm=") {
                    farm = Some(f);
                }
            }
            if let (Some(r), Some(o)) = (role, owner) {
                let farm_part = farm
                    .map(|f| format!(r#","grid_farm":"{f}""#))
                    .unwrap_or_default();
                *self.tokens_json.borrow_mut() = format!(
                    r#"{{"grid_role":"{r}","grid_owner":"{o}"{farm_part},"logo":"\ue1a3"}}"#
                );
            }
            r#"{"result":{}}"#.to_string()
        } else if args.first() == Some(&"agent") && args.get(1) == Some(&"list") {
            self.agent_list_json.borrow().clone()
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
        Ok(self.agent_list_json.borrow().clone())
    }

    fn agent_get(&self, _n: &str) -> std::io::Result<(bool, String)> {
        unimplemented!()
    }
    fn agent_read(&self, _n: &str, _s: &str, _l: u32) -> std::io::Result<String> {
        Ok(String::new())
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
    fn pane_report_metadata(&self, _p: &str, _t: &[&str]) -> std::io::Result<std::process::Output> {
        let mut args = vec!["pane", "report-metadata", _p];
        args.extend_from_slice(_t);
        self.run(&args)
    }
    fn pane_wait_output(&self, _p: &str, _m: &str, _t: u32) -> bool {
        true
    }
    fn pane_read(&self, _p: &str, _s: &str, _l: u32) -> std::io::Result<String> {
        Ok(String::new())
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

#[derive(Clone, Default)]
struct MockPaneInfo {
    tokens: HashMap<String, String>,
    agent_session: Option<String>,
    label: Option<String>,
}

struct MockMultiPaneHerdr {
    panes: RefCell<HashMap<String, MockPaneInfo>>,
    reported_tokens: RefCell<Vec<(String, Vec<String>)>>,
    renamed_panes: RefCell<Vec<(String, String)>>,
    agent_list_json: RefCell<String>,
    fail_metadata_pane: RefCell<Option<String>>,
}

impl MockMultiPaneHerdr {
    fn new(panes: HashMap<String, MockPaneInfo>, agent_list_json: &str) -> Self {
        Self {
            panes: RefCell::new(panes),
            reported_tokens: RefCell::new(Vec::new()),
            renamed_panes: RefCell::new(Vec::new()),
            agent_list_json: RefCell::new(agent_list_json.to_string()),
            fail_metadata_pane: RefCell::new(None),
        }
    }
}
impl HerdrOps for MockMultiPaneHerdr {
    fn run(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        let stdout = if args.first() == Some(&"pane") && args.get(1) == Some(&"get") {
            let pane_id = args.get(2).copied().unwrap_or("");
            let panes = self.panes.borrow();
            let info = panes.get(pane_id).cloned().unwrap_or_default();
            let tokens_val = serde_json::to_value(&info.tokens).unwrap_or(serde_json::json!({}));
            let session_val = info
                .agent_session
                .as_ref()
                .map(|s| serde_json::json!({"value": s}));
            let mut pane_obj = serde_json::json!({
                "pane_id": pane_id,
                "tokens": tokens_val,
            });
            if let Some(s) = session_val {
                pane_obj["agent_session"] = s;
            }
            if let Some(lbl) = info.label {
                pane_obj["label"] = serde_json::json!(lbl);
            }
            serde_json::to_string(&serde_json::json!({
                "result": {
                    "pane": pane_obj
                }
            }))
            .unwrap()
        } else if args.first() == Some(&"pane") && args.get(1) == Some(&"report-metadata") {
            let pane_id = args.get(2).copied().unwrap_or("").to_string();
            if self.fail_metadata_pane.borrow().as_deref() == Some(&pane_id) {
                return Ok(std::process::Output {
                    status: std::os::unix::process::ExitStatusExt::from_raw(256),
                    stdout: Vec::new(),
                    stderr: format!("simulated metadata error for {pane_id}").into_bytes(),
                });
            }
            let mut toks = Vec::new();
            let mut i = 3;
            while i < args.len() {
                if args[i] == "--token" && i + 1 < args.len() {
                    toks.push(args[i + 1].to_string());
                    if let Some((k, v)) = args[i + 1].split_once('=') {
                        self.panes
                            .borrow_mut()
                            .entry(pane_id.clone())
                            .or_default()
                            .tokens
                            .insert(k.to_string(), v.to_string());
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            self.reported_tokens.borrow_mut().push((pane_id, toks));
            r#"{"result":{}}"#.to_string()
        } else if args.first() == Some(&"pane") && args.get(1) == Some(&"rename") {
            let pane_id = args.get(2).copied().unwrap_or("").to_string();
            let label = args.get(3).copied().unwrap_or("").to_string();
            self.renamed_panes
                .borrow_mut()
                .push((pane_id.clone(), label.clone()));
            self.panes.borrow_mut().entry(pane_id).or_default().label = Some(label);
            r#"{"result":{}}"#.to_string()
        } else if args.first() == Some(&"agent") && args.get(1) == Some(&"list") {
            self.agent_list_json.borrow().clone()
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
        Ok(self.agent_list_json.borrow().clone())
    }
    fn agent_get(&self, _n: &str) -> std::io::Result<(bool, String)> {
        unimplemented!()
    }
    fn agent_read(&self, _n: &str, _s: &str, _l: u32) -> std::io::Result<String> {
        Ok(String::new())
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
    fn pane_report_metadata(&self, _p: &str, _t: &[&str]) -> std::io::Result<std::process::Output> {
        let mut args = vec!["pane", "report-metadata", _p];
        for t in _t {
            args.push("--token");
            args.push(t);
        }
        self.run(&args)
    }
    fn pane_wait_output(&self, _p: &str, _m: &str, _t: u32) -> bool {
        true
    }
    fn pane_read(&self, _p: &str, _s: &str, _l: u32) -> std::io::Result<String> {
        Ok(String::new())
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
fn test_repair_lead_success_stamps_tokens() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-lead-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole parity |\n",
    ).unwrap();

    let herdr = MockRepairHerdr::new(
        r#"{"logo":"\ue1a3"}"#,
        r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}}]}}"#,
    );

    let res = repair_lead(&herdr, "mu", None).expect("repair_lead should succeed");
    assert!(res.contains("repaired lead 'mu'"));
    assert!(res.contains("grid_role=orchestrator"));
    assert!(res.contains("grid_owner=mu-lead"));

    // Verify token was stamped
    let farm_id = quaqua::workspace::farm_id();
    assert_eq!(
        *herdr.tokens_json.borrow(),
        format!(
            r#"{{"grid_role":"orchestrator","grid_owner":"mu-lead","grid_farm":"{farm_id}","logo":"\ue1a3"}}"#
        )
    );

    // Running repair a second time reports already tagged
    let res2 = repair_lead(&herdr, "mu", None).expect("second repair should report already tagged");
    assert!(res2.contains("already tagged"));
}

/// A Lead started before `lead spawn` wrote `runs/<name>.owner` (pre-2026-10-24)
/// has no owner file: `identity::depth_walk` then fails and every spawn/mail from
/// that Lead is denied with `depth.owner_missing`. `lead repair` must restore the
/// file so the live Lead works again.
#[test]
fn test_repair_lead_restores_missing_owner_file() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-owner-missing-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole parity |\n",
    ).unwrap();

    let herdr = MockRepairHerdr::new(
        r#"{"logo":"\ue1a3"}"#,
        r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}}]}}"#,
    );

    repair_lead(&herdr, "mu", None).expect("repair_lead should succeed");

    let owner = ws.join("runs/mu-lead.owner");
    let content = std::fs::read_to_string(&owner)
        .unwrap_or_else(|e| panic!("repair must restore {}: {e}", owner.display()));
    assert_eq!(
        content.trim(),
        "cos",
        "restored owner must be the caller owner that lead spawn would record"
    );
}

/// The pane tokens may already be correct (e.g. `lead prompt` backfilled them)
/// while the owner file is still missing: repair reports "already tagged", so the
/// owner restore must happen before that early return.
#[test]
fn test_repair_lead_restores_missing_owner_when_pane_already_tagged() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-owner-tagged-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole parity |\n",
    ).unwrap();

    let herdr = MockRepairHerdr::new(
        r#"{"grid_role":"orchestrator","grid_owner":"mu-lead","logo":"\ue1a3"}"#,
        r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"grid_role":"orchestrator","grid_owner":"mu-lead"}}]}}"#,
    );

    repair_lead(&herdr, "mu", None).expect("repair_lead should succeed");

    let owner = ws.join("runs/mu-lead.owner");
    let content = std::fs::read_to_string(&owner)
        .unwrap_or_else(|e| panic!("repair must restore {}: {e}", owner.display()));
    assert_eq!(content.trim(), "cos");
}

/// An existing owner file is authoritative: repair never rewrites it.
#[test]
fn test_repair_lead_keeps_existing_owner_file() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-owner-kept-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole parity |\n",
    ).unwrap();
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::write(runs_dir.join("mu-lead.owner"), "arch-lead\n").unwrap();

    let herdr = MockRepairHerdr::new(
        r#"{"logo":"\ue1a3"}"#,
        r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}}]}}"#,
    );

    repair_lead(&herdr, "mu", None).expect("repair_lead should succeed");

    assert_eq!(
        std::fs::read_to_string(runs_dir.join("mu-lead.owner"))
            .unwrap()
            .trim(),
        "arch-lead",
        "repair must not overwrite an existing owner file"
    );
}

/// A Lead must never become its own owner: `depth_walk` would report a cycle, so
/// the restore refuses instead of writing `mu-lead -> mu-lead`.
#[test]
fn test_repair_lead_refuses_to_own_itself() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-owner-self-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    std::env::remove_var("HERDR_PANE_ID");
    guard.set("QUAQUA_OWNER", "mu-lead");

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole parity |\n",
    ).unwrap();

    let herdr = MockRepairHerdr::new(
        r#"{"logo":"\ue1a3"}"#,
        r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}}]}}"#,
    );

    let err = repair_lead(&herdr, "mu", None)
        .expect_err("repair must refuse to make a Lead its own owner");
    assert!(err.contains("own itself"), "got: {err}");
    assert!(
        !ws.join("runs/mu-lead.owner").exists(),
        "refused restore must not write an owner file"
    );
}

#[test]
fn test_repair_lead_rejects_unregistered_slug() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-unreg-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole parity |\n",
    ).unwrap();

    let herdr = MockRepairHerdr::new("{}", r#"{"result":{"agents":[]}}"#);
    let err = repair_lead(&herdr, "unknown-proj", None).expect_err("unknown slug must fail");
    assert!(
        err.contains("unknown project slug 'unknown-proj'"),
        "got: {err}"
    );
}

#[test]
fn test_repair_lead_rejects_cos_owned_project() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-cos-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| arch | ~/Lab/quaqua | cos | active | arch goal |\n",
    ).unwrap();

    let herdr = MockRepairHerdr::new("{}", r#"{"result":{"agents":[]}}"#);
    let err = repair_lead(&herdr, "arch", None).expect_err("cos owned project must fail");
    assert!(
        err.contains("owner in projects/INDEX.md is 'cos', not 'arch-lead'"),
        "got: {err}"
    );
}

#[test]
fn test_repair_lead_fails_when_agent_not_running() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-noagent-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole parity |\n",
    ).unwrap();

    let herdr = MockRepairHerdr::new("{}", r#"{"result":{"agents":[]}}"#);
    let err = repair_lead(&herdr, "mu", None).expect_err("agent not running must fail");
    assert!(
        err.contains("agent 'mu-lead' is not registered or running in herdr"),
        "got: {err}"
    );
}

#[test]
fn test_repair_all_leads_repairs_multiple() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-all-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole |\n| llmhub | ~/Lab/llmhub | llmhub-lead | active | CLI |\n",
    ).unwrap();

    let herdr = MockRepairHerdr::new(
        r#"{"logo":"\ue1a3"}"#,
        r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}},{"name":"llmhub-lead","pane_id":"wA:p1","tokens":{"logo":"\ue1a3"}}]}}"#,
    );

    let res = repair_all_leads(&herdr).expect("repair_all_leads should succeed");
    assert_eq!(res.len(), 2);
    assert!(res[0].contains("repaired lead 'mu'"));
    assert!(res[1].contains("repaired lead 'llmhub'"));
}

#[test]
fn test_repair_lead_fails_closed_when_workspace_root_unresolvable() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_HOME", "HOME"]);
    let dir = hermetic_temp_dir("no-index-ws");
    guard.set("QUAQUA_ROOT", dir.to_str().unwrap());

    let herdr = MockRepairHerdr::new("{}", r#"{"result":{"agents":[]}}"#);
    let err = repair_lead(&herdr, "totally-made-up", Some("wC:p7"))
        .expect_err("repair_lead must fail closed when workspace INDEX is unresolvable");
    assert!(
        err.contains("projects/INDEX.md is missing")
            || err.contains("no quaqua workspace")
            || err.contains("cannot resolve workspace root"),
        "got: {err}"
    );
}

#[test]
fn test_repair_lead_rejects_pane_mismatch() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-mismatch-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole parity |\n",
    ).unwrap();

    let herdr = MockRepairHerdr::new(
        r#"{"logo":"\ue1a3"}"#,
        r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}}]}}"#,
    );

    let err = repair_lead(&herdr, "mu", Some("wC:p7"))
        .expect_err("repair_lead must refuse when explicit pane does not match agent pane");
    assert!(err.contains("wC:p7") && err.contains("wB:p1"), "got: {err}");
}

#[test]
fn test_repair_all_leads_fails_if_any_lead_fails() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-all-fail-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole |\n| llmhub | ~/Lab/llmhub | llmhub-lead | active | CLI |\n",
    ).unwrap();

    // Only mu-lead is running; llmhub-lead is offline
    let herdr = MockRepairHerdr::new(
        r#"{"logo":"\ue1a3"}"#,
        r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}}]}}"#,
    );

    let err = repair_all_leads(&herdr)
        .expect_err("repair_all_leads must return error when any lead fails to repair");
    assert!(err.contains("llmhub"), "got: {err}");
}

#[test]
fn test_repair_all_leads_ignores_non_matching_lead_owner() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT"]);
    let ws = hermetic_temp_dir("repair-all-filter-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | other-lead | active | Mole |\n| llmhub | ~/Lab/llmhub | llmhub-lead | active | CLI |\n",
    ).unwrap();

    // Only llmhub-lead is running
    let herdr = MockRepairHerdr::new(
        r#"{"logo":"\ue1a3"}"#,
        r#"{"result":{"agents":[{"name":"llmhub-lead","pane_id":"wA:p1","tokens":{"logo":"\ue1a3"}}]}}"#,
    );

    let res = repair_all_leads(&herdr)
        .expect("repair_all_leads should ignore row where owner is not <slug>-lead and succeed for matching rows");
    assert_eq!(res.len(), 1);
    assert!(res[0].contains("repaired lead 'llmhub'"));
}

#[test]
fn test_identity_heal_lead_with_owner_file_restores_tokens() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir("heal-lead-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    // mu-lead has runs/mu-lead.owner
    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").unwrap();

    let mut panes = HashMap::new();
    let mut mu_tokens = HashMap::new();
    mu_tokens.insert("logo".to_string(), "\u{e1a3}".to_string());
    panes.insert(
        "wB:p1".to_string(),
        MockPaneInfo {
            tokens: mu_tokens,
            agent_session: None,
            label: Some("mu-lead".to_string()),
        },
    );

    let agents_json = r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}}]}}"#;
    let herdr = MockMultiPaneHerdr::new(panes, agents_json);

    let actions = heal(&herdr).expect("heal should succeed");
    assert_eq!(actions.len(), 1);
    assert!(actions[0].contains("healed lead 'mu'"));

    // Verify report-metadata was emitted with grid_role=orchestrator and grid_owner=mu-lead
    let rep = herdr.reported_tokens.borrow();
    assert_eq!(rep.len(), 1);
    assert_eq!(rep[0].0, "wB:p1");
    assert!(rep[0].1.iter().any(|t| t == "grid_role=orchestrator"));
    assert!(rep[0].1.iter().any(|t| t == "grid_owner=mu-lead"));

    // Verify tokens were updated on the mock pane
    let p = herdr.panes.borrow();
    assert_eq!(
        p["wB:p1"].tokens.get("grid_role").map(String::as_str),
        Some("orchestrator")
    );
    assert_eq!(
        p["wB:p1"].tokens.get("grid_owner").map(String::as_str),
        Some("mu-lead")
    );
    assert_eq!(
        p["wB:p1"].tokens.get("logo").map(String::as_str),
        Some("\u{e1a3}")
    );
}

#[test]
fn test_identity_heal_cos_matching_session_stamps_cos_token() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir("heal-cos-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    // CoS identity recorded in runs/cos.identity.json
    std::fs::write(
        runs_dir.join("cos.identity.json"),
        r#"{"agent_session": "ses-cos-target-42", "pane_id": "w1:p1"}"#,
    )
    .unwrap();

    let mut panes = HashMap::new();
    let mut cos_tokens = HashMap::new();
    cos_tokens.insert("logo".to_string(), "\u{e1a0}".to_string());
    panes.insert(
        "w1:p1".to_string(),
        MockPaneInfo {
            tokens: cos_tokens,
            agent_session: Some("ses-cos-target-42".to_string()),
            label: None,
        },
    );

    // Pane has NO herdr agent name, and tokens has only logo (no grid_role)
    let agents_json = r#"{"result":{"agents":[{"pane_id":"w1:p1","agent":"claude","tokens":{"logo":"\ue1a0"},"agent_session":{"value":"ses-cos-target-42"}}]}}"#;
    let herdr = MockMultiPaneHerdr::new(panes, agents_json);

    let actions = heal(&herdr).expect("heal should succeed");
    assert_eq!(actions.len(), 1);
    assert!(actions[0].contains("healed CoS"));

    // Verify report-metadata was emitted with grid_role=cos
    let rep = herdr.reported_tokens.borrow();
    assert!(rep
        .iter()
        .any(|(p, toks)| p == "w1:p1" && toks.iter().any(|t| t == "grid_role=cos")));

    // Verify tokens were updated on the pane
    let p = herdr.panes.borrow();
    assert_eq!(
        p["w1:p1"].tokens.get("grid_role").map(String::as_str),
        Some("cos")
    );
    assert_eq!(
        p["w1:p1"].tokens.get("grid_owner").map(String::as_str),
        Some("cos")
    );
    assert_eq!(
        p["w1:p1"].tokens.get("logo").map(String::as_str),
        Some("\u{e1a0}")
    );

    // Verify rename was emitted
    let renames = herdr.renamed_panes.borrow();
    assert!(renames
        .iter()
        .any(|(p, title)| p == "w1:p1" && title == "🍑 CoS"));
}

#[test]
fn test_identity_heal_cos_fails_closed_on_named_pane_or_session_mismatch() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir("heal-cos-fail-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    // CoS identity expects ses-cos-target-42
    std::fs::write(
        runs_dir.join("cos.identity.json"),
        r#"{"agent_session": "ses-cos-target-42", "pane_id": "w1:p1"}"#,
    )
    .unwrap();

    let mut panes = HashMap::new();

    // Pane 1: Session mismatch (ses-different)
    panes.insert(
        "w1:p1".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: Some("ses-different".to_string()),
            label: None,
        },
    );

    // Pane 2: Matching session, BUT has an agent name ("worker-alice")
    panes.insert(
        "w2:p1".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: Some("ses-cos-target-42".to_string()),
            label: Some("worker-alice".to_string()),
        },
    );

    // Pane 3: Matching session, no name, BUT already carries grid_role=worker
    let mut p3_tokens = HashMap::new();
    p3_tokens.insert("grid_role".to_string(), "worker".to_string());
    panes.insert(
        "w3:p1".to_string(),
        MockPaneInfo {
            tokens: p3_tokens,
            agent_session: Some("ses-cos-target-42".to_string()),
            label: None,
        },
    );

    let agents_json = r#"{"result":{"agents":[
        {"pane_id":"w1:p1","agent":"claude","agent_session":{"value":"ses-different"}},
        {"pane_id":"w2:p1","name":"worker-alice","agent":"claude","agent_session":{"value":"ses-cos-target-42"}},
        {"pane_id":"w3:p1","agent":"claude","tokens":{"grid_role":"worker"},"agent_session":{"value":"ses-cos-target-42"}}
    ]}}"#;

    let herdr = MockMultiPaneHerdr::new(panes, agents_json);

    let actions = heal(&herdr).expect("heal should succeed without errors");
    // None of the panes should have been tagged cos
    assert!(actions.is_empty(), "expected 0 actions, got: {:?}", actions);
    assert!(herdr.reported_tokens.borrow().is_empty());
    assert!(herdr.renamed_panes.borrow().is_empty());
}

#[test]
fn test_identity_heal_worker_restores_tokens() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir("heal-worker-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    // Worker metadata files
    std::fs::write(runs_dir.join("sude-task.owner"), "mu-lead\n").unwrap();
    std::fs::write(runs_dir.join("sude-task.grid"), "mu-workers\n").unwrap();
    std::fs::write(runs_dir.join("sude-task.repo"), "~/Lab/mu\n").unwrap();

    let mut panes = HashMap::new();
    let mut worker_tokens = HashMap::new();
    worker_tokens.insert("logo".to_string(), "\u{e1a3}".to_string());
    panes.insert(
        "wC:p2".to_string(),
        MockPaneInfo {
            tokens: worker_tokens,
            agent_session: None,
            label: Some("sude-task".to_string()),
        },
    );

    let agents_json = r#"{"result":{"agents":[{"name":"sude-task","pane_id":"wC:p2","tokens":{"logo":"\ue1a3"}}]}}"#;
    let herdr = MockMultiPaneHerdr::new(panes, agents_json);

    let actions = heal(&herdr).expect("heal should succeed");
    assert_eq!(actions.len(), 1);
    assert!(actions[0].contains("healed worker 'sude-task'"));

    let rep = herdr.reported_tokens.borrow();
    assert_eq!(rep.len(), 1);
    assert_eq!(rep[0].0, "wC:p2");
    assert_eq!(rep[0].1.len(), 5);
    assert!(rep[0].1.iter().any(|t| t == "grid_role=worker"));
    assert!(rep[0].1.iter().any(|t| t == "grid_group=mu"));
    assert!(rep[0].1.iter().any(|t| t == "grid_owner=mu-lead"));
    assert!(rep[0].1.iter().any(|t| t == "grid_tab=mu-workers"));
    assert!(rep[0].1.iter().any(|t| t.starts_with("grid_farm=")));
    assert!(rep[0].1.iter().all(|t| !t.starts_with("grid_name=")));
    let p = herdr.panes.borrow();
    assert_eq!(
        p["wC:p2"].tokens.get("grid_role").map(String::as_str),
        Some("worker")
    );
    assert_eq!(
        p["wC:p2"].tokens.get("grid_group").map(String::as_str),
        Some("mu")
    );
    assert_eq!(
        p["wC:p2"].tokens.get("grid_owner").map(String::as_str),
        Some("mu-lead")
    );
    assert_eq!(
        p["wC:p2"].tokens.get("grid_tab").map(String::as_str),
        Some("mu-workers")
    );
    assert_eq!(p["wC:p2"].tokens.get("grid_name"), None);
    assert_eq!(
        p["wC:p2"].tokens.get("grid_group").map(String::as_str),
        Some("mu")
    );
    assert_eq!(
        p["wC:p2"].tokens.get("logo").map(String::as_str),
        Some("\u{e1a3}")
    );
}

#[test]
fn test_cos_tag_records_cos_identity_json() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir("cos-tag-record-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    let mut panes = HashMap::new();
    panes.insert(
        "w1:p1".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: Some("ses-claude-999".to_string()),
            label: None,
        },
    );

    let agents_json = r#"{"result":{"agents":[{"pane_id":"w1:p1","agent":"claude","tokens":{},"agent_session":{"value":"ses-claude-999"}}]}}"#;
    let herdr = MockMultiPaneHerdr::new(panes, agents_json);

    let res = quaqua::cos::tag_cos_pane_in(&herdr, "w1:p1", &runs_dir)
        .expect("tag_cos_pane_in should succeed");
    assert!(res.contains("Tagged pane w1:p1 as grid_role=cos"));

    // Verify runs/cos.identity.json was written with agent_session and pane_id
    let ident_path = runs_dir.join("cos.identity.json");
    assert!(ident_path.is_file(), "cos.identity.json should be created");
    let content = std::fs::read_to_string(ident_path).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(val["agent_session"], "ses-claude-999");
    assert_eq!(val["pane_id"], "w1:p1");

    // Simulate herdr restart: tokens are wiped back to only logo
    herdr
        .panes
        .borrow_mut()
        .get_mut("w1:p1")
        .unwrap()
        .tokens
        .clear();
    herdr
        .panes
        .borrow_mut()
        .get_mut("w1:p1")
        .unwrap()
        .tokens
        .insert("logo".to_string(), "\u{e1a0}".to_string());
    let post_restart_agents = r#"{"result":{"agents":[{"pane_id":"w1:p1","agent":"claude","tokens":{"logo":"\ue1a0"},"agent_session":{"value":"ses-claude-999"}}]}}"#;
    *herdr.agent_list_json.borrow_mut() = post_restart_agents.to_string();

    // Heal restores the CoS pane
    let actions = heal(&herdr).expect("heal should restore CoS");
    assert_eq!(actions.len(), 1);
    assert!(actions[0].contains("healed CoS"));
    assert_eq!(
        herdr.panes.borrow()["w1:p1"]
            .tokens
            .get("grid_role")
            .map(String::as_str),
        Some("cos")
    );
}

#[test]
fn test_identity_heal_cos_two_panes_one_session_crowns_only_matching_pane_id() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir("heal-cos-tiebreak-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    // cos.identity.json records pane_id = "w1:p1"
    std::fs::write(
        runs_dir.join("cos.identity.json"),
        r#"{"agent_session": "SES-COS-1", "pane_id": "w1:p1"}"#,
    )
    .unwrap();

    let mut panes = HashMap::new();
    panes.insert(
        "w1:p1".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: Some("SES-COS-1".to_string()),
            label: None,
        },
    );
    panes.insert(
        "w1:p9".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: Some("SES-COS-1".to_string()),
            label: None,
        },
    );

    let agents_json = r#"{"result":{"agents":[
        {"pane_id":"w1:p1","agent":"claude","tokens":{},"agent_session":{"value":"SES-COS-1"}},
        {"pane_id":"w1:p9","agent":"claude","tokens":{},"agent_session":{"value":"SES-COS-1"}}
    ]}}"#;
    let herdr = MockMultiPaneHerdr::new(panes, agents_json);

    let report = heal(&herdr).expect("heal should succeed");
    assert_eq!(
        report.actions.len(),
        1,
        "exactly one CoS pane must be crowned, got {:?}",
        report.actions
    );
    assert!(report.actions[0].contains("pane w1:p1"));
    assert!(report.errors.is_empty());

    let p = herdr.panes.borrow();
    // w1:p1 must be crowned
    assert_eq!(
        p["w1:p1"].tokens.get("grid_role").map(String::as_str),
        Some("cos")
    );
    // w1:p9 must NOT be crowned
    assert_eq!(p["w1:p9"].tokens.get("grid_role"), None);
}

#[test]
fn test_identity_heal_cos_two_panes_one_session_neither_matching_pane_id_crowns_none() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir("heal-cos-ambig-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    // Recorded pane is w1:pOriginal, but current candidate panes are w1:p2 and w1:p9
    std::fs::write(
        runs_dir.join("cos.identity.json"),
        r#"{"agent_session": "SES-COS-1", "pane_id": "w1:pOriginal"}"#,
    )
    .unwrap();

    let mut panes = HashMap::new();
    panes.insert(
        "w1:p2".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: Some("SES-COS-1".to_string()),
            label: None,
        },
    );
    panes.insert(
        "w1:p9".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: Some("SES-COS-1".to_string()),
            label: None,
        },
    );

    let agents_json = r#"{"result":{"agents":[
        {"pane_id":"w1:p2","agent":"claude","tokens":{},"agent_session":{"value":"SES-COS-1"}},
        {"pane_id":"w1:p9","agent":"claude","tokens":{},"agent_session":{"value":"SES-COS-1"}}
    ]}}"#;
    let herdr = MockMultiPaneHerdr::new(panes, agents_json);

    let report = heal(&herdr).expect("heal should return report");
    assert!(
        report.actions.is_empty(),
        "expected 0 crowned panes, got: {:?}",
        report.actions
    );
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("ambiguous CoS identity"));
    assert!(report.errors[0].contains("run `quaqua cos tag` in the intended CoS pane"));

    let p = herdr.panes.borrow();
    assert_eq!(p["w1:p2"].tokens.get("grid_role"), None);
    assert_eq!(p["w1:p9"].tokens.get("grid_role"), None);
}

#[test]
fn test_identity_heal_failing_pane_does_not_abort_loop() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir("heal-best-effort-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    // Set up: broken worker wC:p1, Lead mu-lead wB:p1, CoS w1:p1
    std::fs::write(runs_dir.join("broken-worker.owner"), "mu-lead\n").unwrap();
    std::fs::write(runs_dir.join("broken-worker.grid"), "mu-workers\n").unwrap();
    std::fs::write(runs_dir.join("broken-worker.repo"), "~/Lab/mu\n").unwrap();
    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").unwrap();
    std::fs::write(
        runs_dir.join("cos.identity.json"),
        r#"{"agent_session": "SES-COS-1", "pane_id": "w1:p1"}"#,
    )
    .unwrap();

    let mut panes = HashMap::new();
    panes.insert(
        "wC:p1".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: None,
            label: Some("broken-worker".to_string()),
        },
    );
    panes.insert(
        "wB:p1".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: None,
            label: Some("mu-lead".to_string()),
        },
    );
    panes.insert(
        "w1:p1".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: Some("SES-COS-1".to_string()),
            label: None,
        },
    );

    let agents_json = r#"{"result":{"agents":[
        {"name":"broken-worker","pane_id":"wC:p1","tokens":{}},
        {"name":"mu-lead","pane_id":"wB:p1","tokens":{}},
        {"pane_id":"w1:p1","agent":"claude","tokens":{},"agent_session":{"value":"SES-COS-1"}}
    ]}}"#;
    let herdr = MockMultiPaneHerdr::new(panes, agents_json);
    *herdr.fail_metadata_pane.borrow_mut() = Some("wC:p1".to_string());

    let report = heal(&herdr).expect("heal should return report");
    // Both Lead and CoS must have been healed despite worker failure!
    assert_eq!(
        report.actions.len(),
        2,
        "expected 2 healed panes, got: {:?}",
        report.actions
    );
    assert!(report
        .actions
        .iter()
        .any(|a| a.contains("healed lead 'mu'")));
    assert!(report.actions.iter().any(|a| a.contains("healed CoS")));
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("simulated metadata error for wC:p1"));

    let p = herdr.panes.borrow();
    assert_eq!(
        p["wB:p1"].tokens.get("grid_role").map(String::as_str),
        Some("orchestrator")
    );
    assert_eq!(
        p["w1:p1"].tokens.get("grid_role").map(String::as_str),
        Some("cos")
    );
}

#[test]
fn test_identity_heal_idempotency_second_run_is_noop() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir("heal-idempotency-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    std::fs::write(runs_dir.join("mu-lead.owner"), "cos\n").unwrap();
    std::fs::write(runs_dir.join("worker-1.owner"), "mu-lead\n").unwrap();
    std::fs::write(runs_dir.join("worker-1.grid"), "mu-workers\n").unwrap();
    std::fs::write(runs_dir.join("worker-1.repo"), "~/Lab/mu\n").unwrap();
    std::fs::write(
        runs_dir.join("cos.identity.json"),
        r#"{"agent_session": "SES-COS-1", "pane_id": "w1:p1"}"#,
    )
    .unwrap();

    let mut panes = HashMap::new();
    panes.insert(
        "wB:p1".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: None,
            label: Some("mu-lead".to_string()),
        },
    );
    panes.insert(
        "wC:p2".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: None,
            label: Some("worker-1".to_string()),
        },
    );
    panes.insert(
        "w1:p1".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: Some("SES-COS-1".to_string()),
            label: None,
        },
    );

    let agents_json = r#"{"result":{"agents":[
        {"name":"mu-lead","pane_id":"wB:p1","tokens":{}},
        {"name":"worker-1","pane_id":"wC:p2","tokens":{}},
        {"pane_id":"w1:p1","agent":"claude","tokens":{},"agent_session":{"value":"SES-COS-1"}}
    ]}}"#;
    let herdr = MockMultiPaneHerdr::new(panes, agents_json);

    // First run: all 3 healed
    let report1 = heal(&herdr).expect("first heal should succeed");
    assert_eq!(report1.actions.len(), 3);
    assert!(report1.errors.is_empty());
    let first_call_count = herdr.reported_tokens.borrow().len();
    assert_eq!(first_call_count, 3);

    // Second run: 0 actions, 0 errors, 0 new calls to report-metadata
    let report2 = heal(&herdr).expect("second heal should succeed");
    assert_eq!(
        report2.actions.len(),
        0,
        "second run must be no-op, got: {:?}",
        report2.actions
    );
    assert!(report2.errors.is_empty());
    let second_call_count = herdr.reported_tokens.borrow().len();
    assert_eq!(
        first_call_count, second_call_count,
        "second run must make no new report-metadata calls"
    );
}

#[test]
fn test_cos_tag_without_session_on_unrelated_pane_keeps_existing_record() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir("cos-tag-narrow-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    // Pre-existing valid record for w1:p1
    let ident_path = runs_dir.join("cos.identity.json");
    std::fs::write(
        &ident_path,
        r#"{"agent_session": "SES-REAL", "pane_id": "w1:p1"}"#,
    )
    .unwrap();

    let mut panes = HashMap::new();
    // w1:p7 has no agent_session
    panes.insert(
        "w1:p7".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: None,
            label: None,
        },
    );
    // w1:p1 also has no agent_session in this mock
    panes.insert(
        "w1:p1".to_string(),
        MockPaneInfo {
            tokens: HashMap::new(),
            agent_session: None,
            label: None,
        },
    );

    let herdr = MockMultiPaneHerdr::new(panes, r#"{"result":{"agents":[]}}"#);

    // Tagging unrelated pane w1:p7 without session must NOT delete the record for w1:p1
    let res = quaqua::cos::tag_cos_pane_in(&herdr, "w1:p7", &runs_dir)
        .expect("tag_cos_pane_in should succeed");
    assert!(res.contains("grid_role=cos"));
    assert!(
        ident_path.is_file(),
        "record for w1:p1 must be kept when tagging unrelated pane"
    );
    let content = std::fs::read_to_string(&ident_path).unwrap();
    assert!(content.contains("SES-REAL"));
    assert!(content.contains("w1:p1"));

    // Tagging w1:p1 itself without session DOES remove its own stale record
    let res2 = quaqua::cos::tag_cos_pane_in(&herdr, "w1:p1", &runs_dir)
        .expect("tag_cos_pane_in should succeed");
    assert!(res2.contains("grid_role=cos"));
    assert!(
        !ident_path.exists(),
        "record for w1:p1 must be removed when tagging w1:p1 itself without session"
    );
}

#[test]
fn test_identity_heal_recovers_lead_and_cos_from_live_agent_list_fixture() {
    let guard = EnvGuard::new(&[
        "QUAQUA_ROOT",
        "QUAQUA_RUNS_DIR",
        "HERDR_PANE_ID",
        "QUAQUA_TEST",
    ]);
    let ws = hermetic_temp_dir("live-shape-repro-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    // Registered leads in projects/INDEX.md
    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole |\n| llmhub | ~/Lab/llmhub | llmhub-lead | active | CLI |\n| quaqua-rs | ~/Lab/quaqua | quaqua-rs-lead | active | Engine |\n",
    ).unwrap();

    // Neither mu-lead.owner, llmhub-lead.owner, quaqua-rs-lead.owner NOR cos.identity.json exists in runs_dir!
    let mut panes = HashMap::new();
    let mut cos_tokens = HashMap::new();
    cos_tokens.insert("grid_owner".to_string(), "cos".to_string());
    cos_tokens.insert("grid_role".to_string(), "cos".to_string());
    cos_tokens.insert("logo".to_string(), "\u{e1a0}".to_string());
    panes.insert(
        "w1:p1X".to_string(),
        MockPaneInfo {
            tokens: cos_tokens,
            agent_session: Some("ses-cos-target-42".to_string()),
            label: Some("🍑 CoS".to_string()),
        },
    );

    let mut mu_tokens = HashMap::new();
    mu_tokens.insert("grid_owner".to_string(), "mu-lead".to_string());
    mu_tokens.insert("grid_role".to_string(), "orchestrator".to_string());
    mu_tokens.insert("logo".to_string(), "\u{e1a3}".to_string());
    panes.insert(
        "wB:p1".to_string(),
        MockPaneInfo {
            tokens: mu_tokens,
            agent_session: None,
            label: Some("mu-lead".to_string()),
        },
    );

    let mut llmhub_tokens = HashMap::new();
    llmhub_tokens.insert("grid_owner".to_string(), "llmhub-lead".to_string());
    llmhub_tokens.insert("grid_role".to_string(), "orchestrator".to_string());
    panes.insert(
        "wA:p1".to_string(),
        MockPaneInfo {
            tokens: llmhub_tokens,
            agent_session: None,
            label: Some("llmhub-lead".to_string()),
        },
    );

    let mut quaqua_tokens = HashMap::new();
    quaqua_tokens.insert("grid_owner".to_string(), "quaqua-rs-lead".to_string());
    quaqua_tokens.insert("grid_role".to_string(), "orchestrator".to_string());
    panes.insert(
        "wE:p1".to_string(),
        MockPaneInfo {
            tokens: quaqua_tokens,
            agent_session: None,
            label: Some("quaqua-rs-lead".to_string()),
        },
    );

    let agents_json = r#"{"result":{"agents":[
        {"pane_id":"w1:p1X","agent":"claude","tokens":{"grid_owner":"cos","grid_role":"cos","logo":"\ue1a0"},"agent_session":{"value":"ses-cos-target-42"}},
        {"name":"mu-lead","pane_id":"wB:p1","agent":"omp","tokens":{"logo":"\ue1a3"}},
        {"name":"llmhub-lead","pane_id":"wA:p1","agent":"omp","tokens":{"grid_owner":"llmhub-lead","grid_role":"orchestrator"}},
        {"name":"quaqua-rs-lead","pane_id":"wE:p1","agent":"omp","tokens":{"grid_owner":"quaqua-rs-lead","grid_role":"orchestrator"}}
    ]}}"#;

    let herdr = MockMultiPaneHerdr::new(panes, agents_json);

    // Simulate token-less Lead pane wB:p1 (lost tokens after herdr restart)
    herdr
        .panes
        .borrow_mut()
        .get_mut("wB:p1")
        .unwrap()
        .tokens
        .clear();
    herdr
        .panes
        .borrow_mut()
        .get_mut("wB:p1")
        .unwrap()
        .tokens
        .insert("logo".to_string(), "\u{e1a3}".to_string());

    // Before heal: the pane carries no grid_role token, so resolution can only come
    // from tokens the heal itself restores (`resolve_caller` already attempts that
    // repair on its own, so the token-less state is asserted directly).
    guard.set("HERDR_PANE_ID", "wB:p1");
    let caller_before = quaqua::identity::resolve_caller_from_tokens(
        quaqua::identity::caller_pane_tokens(&herdr).as_ref(),
        &runs_dir,
    );
    assert_eq!(caller_before.role, quaqua::identity::Role::Unknown);

    // Run identity heal
    let report = heal(&herdr).expect("heal should succeed");
    assert!(
        !report.actions.is_empty(),
        "heal must perform recovery actions when tokens/ledger are missing, got 0 actions"
    );

    // After heal: wB:p1 must be tagged grid_role=orchestrator, grid_owner=mu-lead
    let p = herdr.panes.borrow();
    assert_eq!(
        p["wB:p1"].tokens.get("grid_role").map(String::as_str),
        Some("orchestrator")
    );
    assert_eq!(
        p["wB:p1"].tokens.get("grid_owner").map(String::as_str),
        Some("mu-lead")
    );

    // And runs/mu-lead.owner must be restored
    assert!(
        runs_dir.join("mu-lead.owner").is_file(),
        "runs/mu-lead.owner must be restored"
    );

    // And caller resolution from wB:p1 now succeeds
    let caller_after = quaqua::identity::resolve_caller(&herdr, &runs_dir);
    assert_eq!(caller_after.role, quaqua::identity::Role::Lead);
    assert_eq!(caller_after.name, "mu-lead");
    assert_eq!(
        caller_after.depth,
        quaqua::identity::CallerDepth::Resolved(1)
    );

    // And mail send resolves sender successfully
    let sender = quaqua::bus::mail::resolve_sender(&herdr, &runs_dir)
        .expect("sender should resolve after heal");
    assert_eq!(sender, ("mu-lead".to_string(), "lead".to_string()));
}

#[test]
fn test_identity_heal_dry_run_previews_actions_without_mutation() {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir("dry-run-ws");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());

    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole |\n",
    ).unwrap();

    let mut panes = HashMap::new();
    let mut mu_tokens = HashMap::new();
    mu_tokens.insert("logo".to_string(), "\u{e1a3}".to_string());
    panes.insert(
        "wB:p1".to_string(),
        MockPaneInfo {
            tokens: mu_tokens,
            agent_session: None,
            label: Some("mu-lead".to_string()),
        },
    );

    let agents_json = r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}}]}}"#;
    let herdr = MockMultiPaneHerdr::new(panes, agents_json);

    let report = quaqua::identity::heal_dry_run(&herdr).expect("heal_dry_run should succeed");
    assert_eq!(report.actions.len(), 1);
    assert!(
        report.actions[0].starts_with("[dry-run] healed lead 'mu'"),
        "got: {}",
        report.actions[0]
    );

    // Ensure nothing was mutated on the pane or filesystem
    assert!(
        herdr.reported_tokens.borrow().is_empty(),
        "dry-run must make no report-metadata calls"
    );
    let p = herdr.panes.borrow();
    assert_eq!(
        p["wB:p1"].tokens.get("grid_role"),
        None,
        "pane must not be modified in dry-run"
    );
    assert!(
        !runs_dir.join("mu-lead.owner").exists(),
        "owner file must not be created in dry-run"
    );
}

type FarmPane = (HashMap<String, String>, String);

/// Pane state for the CoS farm-heal tests: like `MockMultiPaneHerdr`, but each
/// pane also reports a `cwd`, which `heal` reads to prove a pane is local
/// before stamping `grid_farm`.
struct MockFarmHerdr {
    panes: RefCell<HashMap<String, FarmPane>>,
    reported_tokens: RefCell<Vec<(String, Vec<String>)>>,
    agent_list_json: String,
}

impl MockFarmHerdr {
    fn new(
        panes: HashMap<String, (HashMap<String, String>, String)>,
        agent_list_json: &str,
    ) -> Self {
        Self {
            panes: RefCell::new(panes),
            reported_tokens: RefCell::new(Vec::new()),
            agent_list_json: agent_list_json.to_string(),
        }
    }

    fn tokens(&self, pane: &str) -> HashMap<String, String> {
        self.panes
            .borrow()
            .get(pane)
            .cloned()
            .map(|(tokens, _)| tokens)
            .unwrap_or_default()
    }
}

impl HerdrOps for MockFarmHerdr {
    fn run(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        let stdout = if args.first() == Some(&"pane") && args.get(1) == Some(&"get") {
            let pane_id = args.get(2).copied().unwrap_or("");
            let panes = self.panes.borrow();
            let (tokens, cwd) = panes.get(pane_id).cloned().unwrap_or_default();
            serde_json::json!({
                "result": {"pane": {"pane_id": pane_id, "cwd": cwd, "tokens": tokens}}
            })
            .to_string()
        } else if args.first() == Some(&"pane") && args.get(1) == Some(&"report-metadata") {
            let pane_id = args.get(2).copied().unwrap_or("").to_string();
            let mut toks = Vec::new();
            let mut i = 3;
            while i < args.len() {
                if args[i] == "--token" && i + 1 < args.len() {
                    toks.push(args[i + 1].to_string());
                    if let Some((k, v)) = args[i + 1].split_once('=') {
                        self.panes
                            .borrow_mut()
                            .entry(pane_id.clone())
                            .or_default()
                            .0
                            .insert(k.to_string(), v.to_string());
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            self.reported_tokens.borrow_mut().push((pane_id, toks));
            r#"{"result":{}}"#.to_string()
        } else if args.first() == Some(&"agent") && args.get(1) == Some(&"list") {
            self.agent_list_json.clone()
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
        Ok(self.agent_list_json.clone())
    }
    fn agent_get(&self, _n: &str) -> std::io::Result<(bool, String)> {
        unimplemented!()
    }
    fn agent_read(&self, _n: &str, _s: &str, _l: u32) -> std::io::Result<String> {
        Ok(String::new())
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
    fn pane_report_metadata(&self, _p: &str, _t: &[&str]) -> std::io::Result<std::process::Output> {
        let mut args = vec!["pane", "report-metadata", _p];
        for t in _t {
            args.push("--token");
            args.push(t);
        }
        self.run(&args)
    }
    fn pane_wait_output(&self, _p: &str, _m: &str, _t: u32) -> bool {
        true
    }
    fn pane_read(&self, _p: &str, _s: &str, _l: u32) -> std::io::Result<String> {
        Ok(String::new())
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

/// Hermetic workspace root + pinned runs dir, so `heal` resolves the same farm
/// the fixture writes into.
fn cos_farm_workspace(prefix: &str) -> (EnvGuard, PathBuf) {
    let guard = EnvGuard::new(&["QUAQUA_ROOT", "QUAQUA_RUNS_DIR"]);
    let ws = hermetic_temp_dir(prefix);
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let runs_dir = ws.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    (guard, ws)
}

#[test]
fn test_identity_heal_cos_tagged_without_farm_stamps_local_farm() {
    let (_guard, ws) = cos_farm_workspace("heal-cos-farm-ws");
    let runs_dir = ws.join("runs");
    std::fs::write(
        runs_dir.join("cos.identity.json"),
        r#"{"agent_session":"ses-cos","pane_id":"w1:p1"}"#,
    )
    .unwrap();

    // A CoS pane tagged before farm stamping existed: role and owner, no farm.
    let mut tokens = HashMap::new();
    tokens.insert("grid_role".to_string(), "cos".to_string());
    tokens.insert("grid_owner".to_string(), "cos".to_string());
    let mut panes = HashMap::new();
    panes.insert(
        "w1:p1".to_string(),
        (tokens, ws.to_string_lossy().to_string()),
    );

    let agents_json = r#"{"result":{"agents":[{"pane_id":"w1:p1","agent":"claude","tokens":{"grid_role":"cos","grid_owner":"cos"},"agent_session":{"value":"ses-cos"}}]}}"#;
    let herdr = MockFarmHerdr::new(panes, agents_json);

    let farm = quaqua::workspace::farm_of_runs_dir(&runs_dir);
    let actions = heal(&herdr).expect("heal should succeed");
    assert_eq!(
        actions.actions,
        vec![format!("healed CoS (pane w1:p1): tagged grid_farm={farm}")]
    );
    assert_eq!(
        herdr.tokens("w1:p1").get("grid_farm").map(String::as_str),
        Some(farm.as_str()),
        "CoS pane must carry the local farm token"
    );
    assert_eq!(
        herdr.tokens("w1:p1").get("grid_role").map(String::as_str),
        Some("cos")
    );

    // Idempotent: the token already names this farm, so a second heal does nothing.
    let again = heal(&herdr).expect("second heal should succeed");
    assert!(
        again.is_empty(),
        "second heal must be a no-op, got: {again:?}"
    );
    assert_eq!(herdr.reported_tokens.borrow().len(), 1);
}

#[test]
fn test_identity_heal_does_not_stamp_foreign_farm_cos_pane() {
    let (_guard, ws) = cos_farm_workspace("heal-cos-foreign-ws");

    // Another farm's CoS pane: explicit foreign farm wins over its local-looking cwd.
    let mut tokens = HashMap::new();
    tokens.insert("grid_role".to_string(), "cos".to_string());
    tokens.insert("grid_owner".to_string(), "cos".to_string());
    tokens.insert("grid_farm".to_string(), "other-farm".to_string());
    let mut panes = HashMap::new();
    panes.insert(
        "w1:p1".to_string(),
        (tokens, ws.to_string_lossy().to_string()),
    );

    let agents_json = r#"{"result":{"agents":[{"pane_id":"w1:p1","agent":"claude","tokens":{"grid_role":"cos","grid_owner":"cos","grid_farm":"other-farm"}}]}}"#;
    let herdr = MockFarmHerdr::new(panes, agents_json);

    let actions = heal(&herdr).expect("heal should succeed");
    assert!(
        actions.is_empty(),
        "foreign pane must not be healed: {actions:?}"
    );
    assert!(
        herdr.reported_tokens.borrow().is_empty(),
        "foreign pane must receive no metadata call"
    );
    assert_eq!(
        herdr.tokens("w1:p1").get("grid_farm").map(String::as_str),
        Some("other-farm")
    );
}

#[test]
fn test_identity_heal_does_not_stamp_cos_pane_outside_local_farm() {
    let (_guard, ws) = cos_farm_workspace("heal-cos-outside-ws");
    let elsewhere = hermetic_temp_dir("heal-cos-elsewhere");

    // A CoS-tagged pane whose cwd is not inside this farm: no farm token to read,
    // and no local proof either, so heal must leave it alone.
    let mut tokens = HashMap::new();
    tokens.insert("grid_role".to_string(), "cos".to_string());
    tokens.insert("grid_owner".to_string(), "cos".to_string());
    let mut panes = HashMap::new();
    panes.insert(
        "w1:p1".to_string(),
        (tokens, elsewhere.to_string_lossy().to_string()),
    );

    let agents_json = r#"{"result":{"agents":[{"pane_id":"w1:p1","agent":"claude","tokens":{"grid_role":"cos","grid_owner":"cos"}}]}}"#;
    let herdr = MockFarmHerdr::new(panes, agents_json);

    let actions = heal(&herdr).expect("heal should succeed");
    assert!(
        actions.is_empty(),
        "pane outside the local farm must not be healed: {actions:?}"
    );
    assert!(herdr.reported_tokens.borrow().is_empty());
    assert_eq!(herdr.tokens("w1:p1").get("grid_farm"), None);
    let _ = ws;
}
