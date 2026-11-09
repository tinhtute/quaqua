use quaqua::bus::mail::{self, SendMailOpts};
use quaqua::bus::FarmBus;
use quaqua::herdr::Herdr;
use std::path::{Path, PathBuf};

fn hermetic_temp_dir(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "quaqua-mail-test-{prefix}-{}-{nanos}",
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

impl EnvGuard {
    fn new(keys: &[&'static str]) -> Self {
        let lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let vars = keys.iter().map(|&k| (k, std::env::var(k).ok())).collect();
        Self { _lock: lock, vars }
    }
    fn set(&self, key: &'static str, val: &str) {
        std::env::set_var(key, val);
    }
    fn remove(&self, key: &'static str) {
        std::env::remove_var(key);
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

#[test]
fn test_retried_send_gives_one_row() {
    let guard = EnvGuard::new(&["QUAQUA_TEST", "QUAQUA_OWNER", "HERDR_PANE_ID"]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "bus-lead");
    let runs = hermetic_temp_dir("retry-send");
    let herdr = Herdr::new();

    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: "verdict: accepted".to_string(),
        project: Some("bus".to_string()),
        reply_to: None,
        client_id: None,
    };

    let res1 = mail::send_mail(&runs, &opts, &herdr).expect("first send");
    assert!(res1.id > 0);

    // Verify exactly 1 trigger row
    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");
    let mail_count: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM triggers WHERE source = 'mail'",
            [],
            |r| r.get(0),
        )
        .expect("count mail");
    assert_eq!(mail_count, 1);

    // Retried send with identical contents
    let res2 = mail::send_mail(&runs, &opts, &herdr).expect("retried send");
    assert_eq!(res1.id, res2.id);

    // Verify still exactly 1 trigger row
    let mail_count2: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM triggers WHERE source = 'mail'",
            [],
            |r| r.get(0),
        )
        .expect("count mail after retry");
    assert_eq!(mail_count2, 1);
}

#[test]
fn test_import_live_inbox_files_idempotent() {
    let runs = hermetic_temp_dir("import-live");
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inbox");

    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&fixtures_dir)
        .expect("read fixtures")
        .flatten()
    {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("md") {
            paths.push(p);
        }
    }
    assert!(paths.len() >= 5, "must have at least 5 fixture files");

    // First import run
    let mut total_imported = 0;
    for path in &paths {
        let res = mail::import_inbox_file(&runs, path).expect("import inbox file");
        total_imported += res.imported;
    }
    assert!(total_imported > 0, "must import at least some rows");

    // Second import run: should be completely idempotent (0 new rows)
    let mut second_imported = 0;
    for path in &paths {
        let res = mail::import_inbox_file(&runs, path).expect("re-import inbox file");
        second_imported += res.imported;
    }
    assert_eq!(
        second_imported, 0,
        "second import must add 0 rows (idempotent)"
    );
}

#[test]
fn test_corrupt_db_returns_error() {
    let guard = EnvGuard::new(&["QUAQUA_TEST", "QUAQUA_OWNER", "HERDR_PANE_ID"]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "bus-lead");
    let runs = hermetic_temp_dir("corrupt-db-fallback");
    let herdr = Herdr::new();

    // Make quaqua.db a directory so SQLite open fails
    let bad_db = runs.join("quaqua.db");
    std::fs::create_dir_all(&bad_db).expect("create dir where file expected");

    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "blocked".to_string(),
        body: "critical failure report".to_string(),
        project: Some("bus".to_string()),
        reply_to: None,
        client_id: None,
    };

    let err =
        mail::send_mail(&runs, &opts, &herdr).expect_err("send mail with corrupt db must fail");
    assert!(err.contains("cannot open database"));
}

#[test]
fn test_body_length_exceeded_error() {
    let guard = EnvGuard::new(&["QUAQUA_TEST", "QUAQUA_OWNER", "HERDR_PANE_ID"]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "bus-lead");
    let runs = hermetic_temp_dir("body-len");
    let herdr = Herdr::new();

    let long_body = "x".repeat(4097);
    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: long_body,
        project: Some("bus".to_string()),
        reply_to: None,
        client_id: None,
    };

    let err = mail::send_mail(&runs, &opts, &herdr).expect_err("body > 4096 must error");
    assert!(
        err.contains("4096"),
        "error must mention 4096 max body length: {err}"
    );
}

#[test]
fn test_mail_send_preserves_project_and_reply_to() {
    let guard = EnvGuard::new(&["QUAQUA_TEST", "QUAQUA_OWNER", "HERDR_PANE_ID"]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "alpha-lead");
    let runs = hermetic_temp_dir("mail-metadata");
    let herdr = Herdr::new();

    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "handoff".to_string(),
        body: "testing project and reply-to".to_string(),
        project: Some("explicit-custom-project".to_string()),
        reply_to: Some(99),
        client_id: None,
    };

    let res = mail::send_mail(&runs, &opts, &herdr).expect("send mail");
    assert!(res.id > 0);

    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");
    let trigs = bus.load_unread_triggers("cos", 0).expect("load triggers");
    assert_eq!(trigs.len(), 1);
    assert_eq!(trigs[0].actor, "alpha-lead");
    assert_eq!(trigs[0].to_name.as_deref(), Some("cos"));
    assert_eq!(
        trigs[0].ref_val.as_deref(),
        Some("mail:99;project:explicit-custom-project")
    );
    assert_eq!(trigs[0].payload_json, "testing project and reply-to");
}

#[test]
fn test_cli_mail_send_inbox_ls() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "bus-lead");
    let runs = hermetic_temp_dir("cli-mail");
    let herdr = Herdr::new();

    guard.set("QUAQUA_RUNS_DIR", runs.to_str().unwrap());

    // 1. CLI send (sender inferred from harness/env as bus-lead)
    let send_args = vec![
        "send".to_string(),
        "--to".to_string(),
        "cos".to_string(),
        "--kind".to_string(),
        "accept".to_string(),
        "--body".to_string(),
        "cli send verification".to_string(),
    ];
    let rc = mail::handle_mail(&send_args, &herdr).expect("cli send");
    assert_eq!(rc, 0);

    // 2. CLI ls --md
    let ls_args = vec!["ls".to_string(), "--md".to_string()];
    let rc_ls = mail::handle_mail(&ls_args, &herdr).expect("cli ls");
    assert_eq!(rc_ls, 0);

    // 3. CLI inbox --for cos
    let inbox_args = vec!["inbox".to_string(), "--for".to_string(), "cos".to_string()];
    let rc_inbox = mail::handle_mail(&inbox_args, &herdr).expect("cli inbox");
    assert_eq!(rc_inbox, 0);
}
#[test]
fn test_finding1_inbox_identity_fail_closed() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.remove("QUAQUA_TEST");
    guard.remove("QUAQUA_OWNER");
    guard.remove("HERDR_PANE_ID");

    let runs = hermetic_temp_dir("finding1-identity");
    guard.set("QUAQUA_RUNS_DIR", runs.to_str().unwrap());
    let herdr = Herdr::new();

    // 3. inbox without --for must fail closed when caller identity is unknown (no cos default)
    let res = mail::handle_mail(&["inbox".to_string()], &herdr);
    assert!(
        res.is_err(),
        "inbox without caller identity must fail closed without cos default"
    );
}

#[test]
fn test_finding2_sender_identity_fail_closed_no_from() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.remove("QUAQUA_TEST");
    guard.remove("QUAQUA_OWNER");
    guard.remove("HERDR_PANE_ID");

    let runs = hermetic_temp_dir("finding2-sender");
    guard.set("QUAQUA_RUNS_DIR", runs.to_str().unwrap());
    let herdr = Herdr::new();

    // 1. send with --from must be refused (cannot forge sender)
    let res = mail::handle_mail(
        &[
            "send".to_string(),
            "--to".to_string(),
            "cos".to_string(),
            "--kind".to_string(),
            "accept".to_string(),
            "--from".to_string(),
            "mu-lead".to_string(),
            "--body".to_string(),
            "forged accept".to_string(),
        ],
        &herdr,
    );
    assert!(res.is_err(), "send with --from override must be refused");

    // 2. send without harness identity must fail closed
    let res = mail::handle_mail(
        &[
            "send".to_string(),
            "--to".to_string(),
            "cos".to_string(),
            "--kind".to_string(),
            "accept".to_string(),
            "--body".to_string(),
            "hello".to_string(),
        ],
        &herdr,
    );
    assert!(
        res.is_err(),
        "send without harness identity must fail closed"
    );
}

#[test]
fn test_locked_db_returns_error() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "arch-lead");

    let runs = hermetic_temp_dir("finding5-locked");
    let herdr = Herdr::new();

    let db_path = runs.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("create db");
    let conn = bus.conn_for_test();
    conn.execute_batch("BEGIN EXCLUSIVE;")
        .expect("begin exclusive lock");

    let opts = SendMailOpts {
        to: "mu-lead".to_string(),
        kind: "accept".to_string(),
        body: "locked db test".to_string(),
        project: Some("tieude".to_string()),
        reply_to: None,
        client_id: None,
    };

    let err = mail::send_mail(&runs, &opts, &herdr).expect_err("locked db must return error");
    assert!(
        err.contains("quaqua.db") || err.contains("database locked"),
        "error must include path and mention lock: {err}"
    );
}

#[test]
fn test_finding6_import_multibyte_char_boundary_truncation() {
    let runs = hermetic_temp_dir("finding6-utf8");
    let inbox_file = runs.join("test-lead-utf8.md");

    // 4095 ASCII bytes + 2-byte Vietnamese character 'ế' (starts at 4095, ends at 4097)
    let mut body = "a".repeat(4095);
    body.push_str("ế is multibyte UTF-8");

    let content = format!("## 2026-10-24T00:00:00Z test-lead handoff\n{body}\n");
    std::fs::write(&inbox_file, content).expect("write inbox file");

    // Must not panic on char boundary
    let res = mail::import_inbox_file(&runs, &inbox_file).expect("import must not panic");
    assert_eq!(res.imported, 1);

    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");
    let imported_body: String = bus
        .conn_for_test()
        .query_row(
            "SELECT payload_json FROM triggers WHERE source = 'mail' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("query body");
    assert!(imported_body.chars().count() <= 4096);
}

#[test]
fn test_finding7_checked_in_fixtures_idempotent() {
    let runs = hermetic_temp_dir("finding7-fixtures");
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inbox");
    assert!(fixtures_dir.exists(), "fixtures dir must exist");

    let entries: Vec<PathBuf> = std::fs::read_dir(&fixtures_dir)
        .expect("read fixtures dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    assert!(
        entries.len() >= 5,
        "must have at least 5 checked-in fixture files, got {}",
        entries.len()
    );

    // First import run
    let mut total_imported = 0;
    for path in &entries {
        let res = mail::import_inbox_file(&runs, path).expect("import fixture");
        total_imported += res.imported;
    }
    assert!(total_imported >= 5, "must import all fixture rows");

    // Verify beta-lead fixture with +07:00 offset and multibyte body was imported
    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");
    let beta_lead_body: String = bus
        .conn_for_test()
        .query_row(
            "SELECT payload_json FROM triggers WHERE actor = 'beta-lead' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("query beta-lead body");
    assert!(beta_lead_body.contains("Đã xác nhận hoàn thành giai đoạn beta"));

    // Verify delta-lead fixture with multibyte body > 4096 bytes was truncated on char boundary
    let delta_lead_body: String = bus
        .conn_for_test()
        .query_row(
            "SELECT payload_json FROM triggers WHERE actor = 'delta-lead' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("query delta-lead body");
    assert!(delta_lead_body.chars().count() <= 4096);
    // Second import run: strictly idempotent (0 imported)
    let mut second_imported = 0;
    for path in &entries {
        let res = mail::import_inbox_file(&runs, path).expect("re-import fixture");
        second_imported += res.imported;
    }
    assert_eq!(
        second_imported, 0,
        "second import must be idempotent (0 new rows)"
    );
}

#[test]
fn test_manual_cat_append_live_format_roundtrip() {
    let runs = hermetic_temp_dir("manual-cat-append");
    let inbox_file = runs.join("freeze-lead.md");

    // Live format from runs/freeze-lead.md (synthetic content)
    let content = "\
## 2026-10-24T21:15:00+07:00 freeze-lead handoff
P7 done: merged into quaqua/freeze-integ at 35aeb26, review clean after 3 rounds.
Verify: layout 28/28 ok; workspace suite 0 failed.
Scope: layout/{snapshot,migrate}.rs, hash.rs sha256.
Cost: builder $4.58/$6; reviewers unpriced.
Ask: CoS review + push/PR decision.

## 2026-10-24T22:00:00+07:00 freeze-lead accept
Review completed with clean verdict.
All criteria satisfied.
";
    std::fs::write(&inbox_file, content).expect("write inbox file");

    let imp = mail::import_inbox_file(&runs, &inbox_file).expect("import manual entries");
    assert_eq!(imp.total, 2);
    assert_eq!(imp.imported, 2);

    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");
    let rows: Vec<(String, String, String, String)> = {
        let conn = bus.conn_for_test();
        let mut stmt = conn
            .prepare("SELECT actor, to_name, kind, payload_json FROM triggers WHERE source = 'mail' ORDER BY id ASC")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get(2)?,
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "freeze-lead");
    assert_eq!(rows[0].1, "cos");
    assert_eq!(rows[0].2, "handoff");
    assert!(rows[0]
        .3
        .contains("P7 done: merged into quaqua/freeze-integ"));
    assert!(rows[0].3.contains("Ask: CoS review + push/PR decision."));

    assert_eq!(rows[1].0, "freeze-lead");
    assert_eq!(rows[1].1, "cos");
    assert_eq!(rows[1].2, "accept");
    assert!(rows[1].3.contains("Review completed with clean verdict."));

    // Idempotent re-import
    let imp2 = mail::import_inbox_file(&runs, &inbox_file).expect("re-import");
    assert_eq!(imp2.imported, 0);
}

struct MockUntaggedLeadHerdr {
    tokens_json: std::cell::RefCell<String>,
    cwd: std::cell::RefCell<String>,
    /// Whole `pane get` stdout override — models a dead pane or a broken daemon answer.
    pane_raw: std::cell::RefCell<Option<String>>,
    calls: std::cell::RefCell<Vec<String>>,
}

impl MockUntaggedLeadHerdr {
    fn new() -> Self {
        Self {
            tokens_json: std::cell::RefCell::new(r#"{"logo":"\ue1a3"}"#.to_string()),
            cwd: std::cell::RefCell::new(String::new()),
            pane_raw: std::cell::RefCell::new(None),
            calls: std::cell::RefCell::new(Vec::new()),
        }
    }
    /// herdr always reports the pane's cwd; the caller-farm gate needs it.
    fn set_cwd(&self, cwd: &Path) {
        *self.cwd.borrow_mut() = cwd.to_string_lossy().to_string();
    }
    fn set_tokens(&self, tokens_json: &str) {
        *self.tokens_json.borrow_mut() = tokens_json.to_string();
    }
    fn set_pane_raw(&self, raw: &str) {
        *self.pane_raw.borrow_mut() = Some(raw.to_string());
    }
}

impl quaqua::herdr::HerdrOps for MockUntaggedLeadHerdr {
    fn run(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        self.calls.borrow_mut().push(args.join(" "));
        let stdout = if args.first() == Some(&"pane") && args.get(1) == Some(&"get") {
            match self.pane_raw.borrow().as_ref() {
                Some(raw) => raw.clone(),
                None => format!(
                    r#"{{"result":{{"pane":{{"pane_id":"wB:p1","cwd":"{}","tokens":{}}}}}}}"#,
                    self.cwd.borrow(),
                    self.tokens_json.borrow()
                ),
            }
        } else if args.first() == Some(&"pane") && args.get(1) == Some(&"report-metadata") {
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
            }
            r#"{"result":{}}"#.to_string()
        } else if args.first() == Some(&"agent") && args.get(1) == Some(&"list") {
            r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}}]}}"#
                .to_string()
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
        Ok(r#"{"result":{"agents":[{"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}}]}}"#.to_string())
    }
    fn agent_get(&self, _name: &str) -> std::io::Result<(bool, String)> {
        Ok((
            true,
            r#"{"result":{"agent":{"name":"mu-lead","pane_id":"wB:p1"}}}"#.to_string(),
        ))
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
        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
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
#[test]
fn test_mail_send_from_untagged_lead_pane_self_heals() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_ROOT",
        "HOME",
    ]);
    guard.remove("QUAQUA_OWNER");
    guard.remove("QUAQUA_TEST");
    guard.set("HERDR_PANE_ID", "wB:p1");

    let ws = hermetic_temp_dir("untagged-lead-ws");
    let runs = ws.join("runs");
    std::fs::create_dir_all(&runs).expect("create runs");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    // Create projects/INDEX.md registering mu-lead
    let projects_dir = ws.join("projects");
    std::fs::create_dir_all(&projects_dir).expect("create projects");
    std::fs::write(
        projects_dir.join("INDEX.md"),
        "| slug | repo | owner | status | goal |\n|------|------|-------|--------|------|\n| mu | ~/Lab/mu | mu-lead | active | Mole parity |\n",
    ).expect("write INDEX.md");

    let herdr = MockUntaggedLeadHerdr::new();
    herdr.set_cwd(&ws);
    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: "verdict: parity verified".to_string(),
        project: Some("mu".to_string()),
        reply_to: None,
        client_id: None,
    };

    let res =
        mail::send_mail(&runs, &opts, &herdr).expect("send_mail must self-heal untagged lead pane");
    assert!(res.id > 0);

    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");
    let sender: String = bus
        .conn_for_test()
        .query_row("SELECT actor FROM triggers WHERE id = ?", [res.id], |r| {
            r.get(0)
        })
        .expect("get sender");
    assert_eq!(sender, "mu-lead");
}

#[test]
fn test_mail_send_unresolvable_error_names_pane_and_repair_command() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_ROOT",
    ]);
    guard.remove("QUAQUA_OWNER");
    guard.remove("QUAQUA_TEST");
    guard.set("HERDR_PANE_ID", "wX:p99");

    let ws = hermetic_temp_dir("unresolvable-ws");
    let runs = ws.join("runs");
    std::fs::create_dir_all(&runs).expect("create runs");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let herdr = MockUntaggedLeadHerdr::new();
    herdr.set_cwd(&ws);
    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: "verdict: test".to_string(),
        project: None,
        reply_to: None,
        client_id: None,
    };

    let err = mail::send_mail(&runs, &opts, &herdr).expect_err("unresolvable pane must fail");
    assert!(
        err.contains("wX:p99"),
        "error must name pane wX:p99, got: {err}"
    );
    assert!(
        err.contains("quaqua lead repair"),
        "error must name 'quaqua lead repair', got: {err}"
    );
}
fn farm_gate_opts() -> SendMailOpts {
    SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: "verdict: farm gate".to_string(),
        project: None,
        reply_to: None,
        client_id: None,
    }
}

#[test]
fn test_mail_send_refuses_pane_from_another_workspace() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_HOME",
        "QUAQUA_ROOT",
    ]);
    guard.remove("QUAQUA_TEST");
    guard.remove("QUAQUA_OWNER");
    guard.remove("QUAQUA_HOME");
    guard.set("HERDR_PANE_ID", "w7:p2");

    let ws = hermetic_temp_dir("foreign-farm-ws");
    let runs = ws.join("runs");
    std::fs::create_dir_all(&runs).expect("create runs");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    // A pane from another workspace: no grid_farm token, cwd outside this workspace.
    let foreign_ws = hermetic_temp_dir("foreign-farm-other-ws");
    let herdr = MockUntaggedLeadHerdr::new();
    herdr.set_cwd(&foreign_ws);

    let err = mail::send_mail(&runs, &farm_gate_opts(), &herdr)
        .expect_err("a caller pane from another farm must be refused");
    assert!(
        err.contains("w7:p2"),
        "error must name the caller pane, got: {err}"
    );
    assert!(
        err.contains("QUAQUA_HOME"),
        "error must name QUAQUA_HOME, got: {err}"
    );
    assert!(
        !runs.join("quaqua.db").exists(),
        "refusal must not create the mailbox database"
    );
}

#[test]
fn test_mail_send_refuses_pane_carrying_another_farm_token() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_HOME",
        "QUAQUA_ROOT",
    ]);
    guard.remove("QUAQUA_TEST");
    guard.remove("QUAQUA_OWNER");
    guard.remove("QUAQUA_HOME");
    guard.set("HERDR_PANE_ID", "wA:p9");

    let ws = hermetic_temp_dir("farm-token-ws");
    let runs = ws.join("runs");
    std::fs::create_dir_all(&runs).expect("create runs");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    // cwd sits inside this workspace, but the pane is stamped as another farm's.
    let herdr = MockUntaggedLeadHerdr::new();
    herdr.set_cwd(&ws);
    herdr.set_tokens(
        r#"{"grid_role":"orchestrator","grid_owner":"mu-lead","grid_farm":"otherfarm"}"#,
    );

    let err = mail::send_mail(&runs, &farm_gate_opts(), &herdr)
        .expect_err("a pane stamped with another farm must be refused");
    assert!(
        err.contains("QUAQUA_HOME"),
        "error must name QUAQUA_HOME, got: {err}"
    );
    assert!(
        !runs.join("quaqua.db").exists(),
        "refusal must not create the mailbox database"
    );
}

#[test]
fn test_mail_send_refuses_dead_caller_pane() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_HOME",
        "QUAQUA_ROOT",
    ]);
    guard.remove("QUAQUA_TEST");
    guard.remove("QUAQUA_OWNER");
    guard.remove("QUAQUA_HOME");
    guard.set("HERDR_PANE_ID", "w8:p1");

    let ws = hermetic_temp_dir("dead-pane-ws");
    let runs = ws.join("runs");
    std::fs::create_dir_all(&runs).expect("create runs");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    let herdr = MockUntaggedLeadHerdr::new();
    herdr.set_pane_raw(r#"{"error":{"code":"pane_not_found","message":"no such pane"}}"#);

    let err = mail::send_mail(&runs, &farm_gate_opts(), &herdr)
        .expect_err("a caller pane herdr reports as gone must be refused");
    assert!(
        err.contains("w8:p1"),
        "error must name pane w8:p1, got: {err}"
    );
    assert!(
        err.contains("QUAQUA_HOME"),
        "error must name QUAQUA_HOME, got: {err}"
    );
}

#[test]
fn test_mail_send_same_farm_pane_stamps_local_farm() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_HOME",
        "QUAQUA_ROOT",
    ]);
    guard.remove("QUAQUA_TEST");
    guard.remove("QUAQUA_OWNER");
    guard.remove("QUAQUA_HOME");
    guard.set("HERDR_PANE_ID", "wA:p1");

    let ws = hermetic_temp_dir("same-farm-ws");
    let runs = ws.join("runs");
    std::fs::create_dir_all(&runs).expect("create runs");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());
    let local_farm = quaqua::workspace::farm_of_runs_dir(&runs);

    let herdr = MockUntaggedLeadHerdr::new();
    herdr.set_cwd(&ws);
    herdr.set_tokens(&format!(
        r#"{{"grid_role":"orchestrator","grid_owner":"mu-lead","grid_group":"mu","grid_farm":"{local_farm}"}}"#
    ));

    let res = mail::send_mail(&runs, &farm_gate_opts(), &herdr).expect("same-farm send must work");
    assert!(res.id > 0);

    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");
    let (sender, row_farm): (String, String) = bus
        .conn_for_test()
        .query_row(
            "SELECT actor, farm FROM triggers WHERE id = ?",
            [res.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("get mail row");
    assert_eq!(sender, "mu-lead");
    assert_eq!(row_farm, local_farm, "mail row must carry the local farm");
}

#[test]
fn test_mail_send_fails_open_when_pane_get_is_unreadable() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_HOME",
        "QUAQUA_ROOT",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    guard.remove("QUAQUA_HOME");
    guard.set("HERDR_PANE_ID", "wB:p1");

    let ws = hermetic_temp_dir("fail-open-ws");
    let runs = ws.join("runs");
    std::fs::create_dir_all(&runs).expect("create runs");
    guard.set("QUAQUA_ROOT", ws.to_str().unwrap());

    // herdr answering garbage must not block mail: the gate fails open.
    let herdr = MockUntaggedLeadHerdr::new();
    herdr.set_pane_raw("herdr: socket closed");

    let res = mail::send_mail(&runs, &farm_gate_opts(), &herdr)
        .expect("an unreadable pane answer must not block mail");
    assert!(res.id > 0);
}
