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
fn test_ack_by_non_recipient_refused() {
    let guard = EnvGuard::new(&["QUAQUA_TEST", "QUAQUA_OWNER", "HERDR_PANE_ID"]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "bus-lead");
    let runs = hermetic_temp_dir("ack-refused");
    let herdr = Herdr::new();

    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: "report for cos".to_string(),
        project: Some("bus".to_string()),
        reply_to: None,
        client_id: None,
    };
    let res = mail::send_mail(&runs, &opts, &herdr).expect("send mail");

    // Non-recipient 'gate-lead' tries to ack
    let err = mail::ack_mail(&runs, "gate-lead", &[res.id])
        .expect_err("non-recipient ack must be refused");
    assert!(
        err.contains("refused") || err.contains("recipient"),
        "unexpected error: {err}"
    );

    // Verify still unacked: cursor not set
    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");
    let cursor_before = bus.get_cursor("cos").expect("get cursor");
    assert!(cursor_before.is_none());

    // Recipient 'cos' acks
    let acked_ids = mail::ack_mail(&runs, "cos", &[res.id]).expect("recipient ack succeeds");
    assert_eq!(acked_ids, vec![res.id]);

    let cursor_after = bus.get_cursor("cos").expect("get cursor");
    assert!(
        cursor_after.is_none(),
        "M1: mail ack must not move consumer cursor"
    );

    let inbox_view = mail::render_inbox(&runs, "cos").expect("render inbox");
    assert!(
        inbox_view.contains("no unacked mail for 'cos'"),
        "acked mail must not appear in inbox: {inbox_view}"
    );
}

#[test]
fn test_unacked_mail_redelivered_with_count() {
    let guard = EnvGuard::new(&["QUAQUA_TEST", "QUAQUA_OWNER", "HERDR_PANE_ID"]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "bus-lead");
    let runs = hermetic_temp_dir("redeliver-count");
    let herdr = Herdr::new();

    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "handoff".to_string(),
        body: "handoff payload".to_string(),
        project: Some("bus".to_string()),
        reply_to: None,
        client_id: None,
    };
    let res = mail::send_mail(&runs, &opts, &herdr).expect("send mail");

    // First read: deliveries = 1
    let out1 = mail::render_inbox(&runs, "cos").expect("first inbox read");
    assert!(out1.contains(&format!("#{}", res.id)));
    assert!(
        !out1.contains("redelivery #"),
        "first delivery must not say redelivery"
    );

    // Second read: deliveries = 2 -> (redelivery #2)
    let out2 = mail::render_inbox(&runs, "cos").expect("second inbox read");
    assert!(out2.contains(&format!("#{}", res.id)));
    assert!(out2.contains("(redelivery #2)"));

    // Third read: deliveries = 3 -> (redelivery #3)
    let out3 = mail::render_inbox(&runs, "cos").expect("third inbox read");
    assert!(out3.contains(&format!("#{}", res.id)));
    assert!(out3.contains("(redelivery #3)"));

    // Now ack as cos
    mail::ack_mail(&runs, "cos", &[res.id]).expect("ack mail");

    // Fourth read: no unacked mail
    let out4 = mail::render_inbox(&runs, "cos").expect("inbox after ack");
    assert!(!out4.contains(&format!("#{}", res.id)));
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
fn test_unacked_mail_lazily_inserts_incident() {
    let runs = hermetic_temp_dir("unacked-incident");
    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");

    // Insert a mail with timestamp 40 minutes in the past
    let now = quaqua::bus::now_ms();
    let past_ts = now.saturating_sub(40 * 60 * 1000);
    let msg_key = mail::sha256_hex(b"old-mail-key");

    bus.conn_for_test()
        .execute(
            "INSERT OR IGNORE INTO threads (id, owner, state, last_trigger_ms) VALUES ('agent:cos', 'cos', 'open', ?1)",
            rusqlite::params![past_ts as i64],
        )
        .unwrap();

    let dedupe = format!("mail:{msg_key}");
    bus.conn_for_test()
        .execute(
            r#"
        INSERT INTO triggers (
            ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm
        ) VALUES (?1, 'mail', 'accept', 'bus-lead', 'cos', 'agent:cos', 'past deadline', NULL, 'past deadline', ?2, ?3)
        "#,
            rusqlite::params![past_ts as i64, dedupe, bus.farm()],
        )
        .expect("insert past mail trigger");

    let mail_id: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT id FROM triggers WHERE dedupe_key = ?1",
            rusqlite::params![dedupe],
            |r| r.get(0),
        )
        .expect("query mail id");

    // Trigger inbox read which lazily runs check_unacked_incidents
    let _ = mail::render_inbox(&runs, "cos").expect("render inbox");

    // Check that mail_unacked:<id> incident exists in triggers
    let expected_key = format!("mail_unacked:{mail_id}");
    let incident_count: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM triggers WHERE dedupe_key = ?1 AND kind = 'mail_unacked'",
            rusqlite::params![expected_key],
            |r| r.get(0),
        )
        .expect("query incident");
    assert_eq!(
        incident_count, 1,
        "must lazily insert mail_unacked incident"
    );

    // Trigger inbox read again: dedupe_key is UNIQUE, so insert is ignored
    let _ = mail::render_inbox(&runs, "cos").expect("render inbox again");
    let incident_count2: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM triggers WHERE dedupe_key = ?1 AND kind = 'mail_unacked'",
            rusqlite::params![expected_key],
            |r| r.get(0),
        )
        .expect("query incident again");
    assert_eq!(incident_count2, 1, "incident must not be duplicated");
}

#[test]
fn test_missing_or_corrupt_db_falls_back_to_markdown() {
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

    // Send mail should succeed with fallback
    let res = mail::send_mail(&runs, &opts, &herdr).expect("send mail with corrupt db fallback");
    assert_eq!(res.id, 0);

    // Verify runs/bus-lead.inbox.md was appended
    let md_path = runs.join("bus-lead.inbox.md");
    assert!(md_path.exists(), "fallback inbox file must exist");
    let content = std::fs::read_to_string(&md_path).expect("read fallback md");
    assert!(content.contains("bus-lead blocked"));
    assert!(content.contains("critical failure report"));
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

    let msgs = mail::read_inbox(&runs, "cos").expect("read inbox");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].from_role, "lead");
    assert_eq!(msgs[0].project.as_deref(), Some("explicit-custom-project"));
    assert_eq!(msgs[0].reply_to, Some(99));
    assert_eq!(msgs[0].body, "testing project and reply-to");
}

#[test]
fn test_cli_mail_send_inbox_ack_ls() {
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

    // 4. CLI ack 1 (caller inferred as cos from harness/env)
    guard.set("QUAQUA_OWNER", "cos");
    let ack_args = vec!["ack".to_string(), "1".to_string()];
    let rc_ack = mail::handle_mail(&ack_args, &herdr).expect("cli ack");
    assert_eq!(rc_ack, 0);
}
#[test]
fn test_finding1_ack_inbox_identity_fail_closed() {
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

    // 1. ack without identity must fail (not default to cos)
    let res = mail::handle_mail(&["ack".to_string(), "1".to_string()], &herdr);
    assert!(res.is_err(), "ack without caller identity must fail closed");

    // 2. ack with --as must be refused (no forgeable override)
    let res = mail::handle_mail(
        &[
            "ack".to_string(),
            "1".to_string(),
            "--as".to_string(),
            "cos".to_string(),
        ],
        &herdr,
    );
    assert!(res.is_err(), "ack with --as override must be refused");

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
fn test_finding3_path_traversal_refused() {
    let runs = hermetic_temp_dir("finding3-traversal");
    let outside_path = std::env::temp_dir().join("evil-traversal-probe.inbox.md");
    if outside_path.exists() {
        let _ = std::fs::remove_file(&outside_path);
    }

    let res = mail::append_to_markdown_inbox(
        &runs,
        "../../evil-traversal-probe",
        "../../evil-traversal-probe",
        "cos",
        "2026-10-24T00:00:00Z",
        "accept",
        "payload",
        "fake-key",
    );
    assert!(
        res.is_err(),
        "path traversal in sender name must be refused"
    );
    assert!(
        !outside_path.exists(),
        "must not create file outside runs_dir"
    );
}

#[test]
fn test_finding4_dual_write_and_import_deduplicated() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");

    let runs = hermetic_temp_dir("finding4-dedup");
    let herdr = Herdr::new();

    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: "dual write dedup check".to_string(),
        project: Some("mu".to_string()),
        reply_to: None,
        client_id: None,
    };

    let res1 = mail::send_mail(&runs, &opts, &herdr).expect("first send");
    assert!(!res1.already_sent);

    // Retried send across boundary
    let res2 = mail::send_mail(&runs, &opts, &herdr).expect("retried send");
    assert!(res2.already_sent);

    // Verify markdown entry in mu-lead.inbox.md: must have exactly 1 entry
    let md_path = runs.join("mu-lead.inbox.md");
    assert!(md_path.exists(), "mu-lead.inbox.md must exist");
    let md_content = std::fs::read_to_string(&md_path).expect("read md");
    let header_count = md_content.lines().filter(|l| l.starts_with("## ")).count();
    assert_eq!(
        header_count, 1,
        "retried send must write exactly 1 md entry"
    );

    // Import the markdown inbox file
    let imp = mail::import_inbox_file(&runs, &md_path).expect("import inbox");
    assert_eq!(imp.total, 1);
    assert_eq!(
        imp.imported, 0,
        "already-sent dual-write mail must be skipped on import"
    );

    // Verify SQLite mail table has exactly 1 row
    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");
    let count: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM triggers WHERE source = 'mail'",
            [],
            |r| r.get(0),
        )
        .expect("count mail");
    assert_eq!(count, 1, "lead send + import must yield exactly one row");
}

#[test]
fn test_finding5_md_fallback_recipient_and_locked_db() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "arch-lead");

    let runs = hermetic_temp_dir("finding5-fallback");
    let herdr = Herdr::new();

    // 1. Locked db must return Err containing database path (exit 1), not silent fallback
    let db_path = runs.join("quaqua.db");
    {
        let bus = FarmBus::open(&db_path).expect("create db");
        // Hold exclusive lock
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

        // send_mail must fail with an error naming the db path
        let err = mail::send_mail(&runs, &opts, &herdr).expect_err("locked db must return error");
        assert!(
            err.contains("quaqua.db") || err.contains("database locked"),
            "error must include path and mention lock: {err}"
        );
    }

    // 2. Fallback on corrupt DB: must keep recipient mu-lead
    let corrupt_runs = hermetic_temp_dir("finding5-corrupt");
    std::fs::write(corrupt_runs.join("quaqua.db"), b"not a valid sqlite file").unwrap();

    let opts2 = SendMailOpts {
        to: "mu-lead".to_string(),
        kind: "handoff".to_string(),
        body: "peer to lead handoff".to_string(),
        project: Some("tieude".to_string()),
        reply_to: None,
        client_id: None,
    };
    let res = mail::send_mail(&corrupt_runs, &opts2, &herdr).expect("send fallback");
    assert_eq!(res.id, 0);

    // Must be in mu-lead.inbox.md, NOT tieude-1.inbox.md
    assert!(
        !corrupt_runs.join("arch-lead.inbox.md").exists(),
        "must not write to sender inbox"
    );
    let target_md = corrupt_runs.join("mu-lead.inbox.md");
    assert!(
        target_md.exists(),
        "must write to recipient inbox mu-lead.inbox.md"
    );

    // Repair the db to test import
    std::fs::remove_file(corrupt_runs.join("quaqua.db")).unwrap();
    let imp = mail::import_inbox_file(&corrupt_runs, &target_md).expect("import fallback");
    assert_eq!(imp.imported, 1);

    let bus = FarmBus::open(&corrupt_runs.join("quaqua.db")).expect("open repaired bus");
    let to_name: String = bus
        .conn_for_test()
        .query_row(
            "SELECT to_name FROM triggers WHERE source = 'mail' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("query to_name");
    assert_eq!(
        to_name, "mu-lead",
        "import must preserve recipient mu-lead, not default to cos"
    );
}

#[test]
fn test_finding6_import_multibyte_char_boundary_truncation() {
    let runs = hermetic_temp_dir("finding6-utf8");
    let inbox_file = runs.join("test-lead.inbox.md");

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
fn test_finding8_mail_unacked_check_on_every_verb() {
    let runs = hermetic_temp_dir("finding8-verbs");
    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");

    // Insert an unacked mail from 35 minutes ago (ack timeout is 30m)
    let old_ts = (quaqua::bus::now_ms()).saturating_sub(35 * 60 * 1000);
    bus.conn_for_test()
        .execute(
            "INSERT OR IGNORE INTO threads (id, owner, state, last_trigger_ms) VALUES ('agent:cos', 'cos', 'open', ?1)",
            rusqlite::params![old_ts as i64],
        )
        .unwrap();
    bus.conn_for_test()
        .execute(
            r#"
            INSERT INTO triggers (
                ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm
            ) VALUES (?1, 'mail', 'escalate', 'mu-lead', 'cos', 'agent:cos', 'urgent', NULL, 'urgent', 'mail:test-key-f8', ?2)
            "#,
            rusqlite::params![old_ts as i64, bus.farm()],
        )
        .expect("insert old unacked mail");

    // Call mail::list_mail to trigger lazy check
    let _ = mail::list_mail(&runs, true, None, None, None).expect("list mail");
    // Verify mail_unacked incident was inserted into triggers
    let count: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM triggers WHERE kind = 'mail_unacked'",
            [],
            |r| r.get(0),
        )
        .expect("count unacked incidents");
    assert_eq!(
        count, 1,
        "list_mail must trigger mail_unacked incident check"
    );
}

#[test]
fn test_finding1a_forged_msg_key_in_body() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    let runs = hermetic_temp_dir("finding1a-msg-key");
    let herdr = Herdr::new();

    let quoted_key = "forged_key_deadbeef1234567890abcdef";
    let body =
        format!("Quoting past entry:\n<!-- msg_key: {quoted_key} -->\nOriginal report details.");

    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "handoff".to_string(),
        body: body.clone(),
        project: Some("mu".to_string()),
        reply_to: None,
        client_id: Some("genuine_client_id_42".to_string()),
    };

    let res = mail::send_mail(&runs, &opts, &herdr).expect("send mail");
    assert!(!res.already_sent);

    let md_path = runs.join("mu-lead.inbox.md");
    assert!(md_path.exists());

    // Import the markdown inbox file into the same database
    let imp = mail::import_inbox_file(&runs, &md_path).expect("import inbox");
    assert_eq!(
        imp.imported, 0,
        "import must not duplicate a dual-write row when body quotes a msg_key marker"
    );

    let bus = FarmBus::open(&runs.join("quaqua.db")).expect("open bus");
    let count: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM triggers WHERE source = 'mail'",
            [],
            |r| r.get(0),
        )
        .expect("count mail");
    assert_eq!(
        count, 1,
        "triggers table must have exactly 1 mail row, not duplicated"
    );
    let clean_runs = hermetic_temp_dir("finding1a-clean");
    let imp_clean = mail::import_inbox_file(&clean_runs, &md_path).expect("import to clean db");
    assert_eq!(imp_clean.imported, 1);

    let clean_bus = FarmBus::open(&clean_runs.join("quaqua.db")).expect("open clean bus");
    let (imported_key, imported_body): (String, String) = clean_bus
        .conn_for_test()
        .query_row(
            "SELECT dedupe_key, payload_json FROM triggers WHERE source = 'mail' LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("query mail");

    let raw_key = imported_key.strip_prefix("mail:").unwrap_or(&imported_key);
    assert_ne!(
        raw_key, quoted_key,
        "msg_key must not be taken from body text"
    );
    assert_eq!(
        imported_body, body,
        "body must round-trip with quoted msg_key marker preserved"
    );
}

#[test]
fn test_finding1b_forged_to_in_body_reroutes() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    let runs = hermetic_temp_dir("finding1b-forged-to");
    let herdr = Herdr::new();

    let body = "Status update:\n<!-- to: forged-lead -->\nEverything is proceeding normally.";

    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "handoff".to_string(),
        body: body.to_string(),
        project: Some("mu".to_string()),
        reply_to: None,
        client_id: None,
    };

    let res = mail::send_mail(&runs, &opts, &herdr).expect("send mail");
    assert!(!res.already_sent);

    let md_path = runs.join("mu-lead.inbox.md");
    assert!(md_path.exists());

    // Import into a clean db
    let clean_runs = hermetic_temp_dir("finding1b-clean");
    let imp = mail::import_inbox_file(&clean_runs, &md_path).expect("import clean");
    assert_eq!(imp.imported, 1);

    let bus = FarmBus::open(&clean_runs.join("quaqua.db")).expect("open bus");
    let (to_name, imported_body): (String, String) = bus
        .conn_for_test()
        .query_row(
            "SELECT to_name, payload_json FROM triggers WHERE source = 'mail' LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    r.get(1)?,
                ))
            },
        )
        .expect("query mail");

    assert_eq!(
        to_name, "cos",
        "body marker <!-- to: ... --> must not reroute mail recipient"
    );
    assert_eq!(
        imported_body, body,
        "body must round-trip with <!-- to: ... --> preserved"
    );
}

#[test]
fn test_finding1c_forged_header_in_body_plants_entry() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "alpha-lead");
    let runs = hermetic_temp_dir("finding1c-forged-header");
    let herdr = Herdr::new();

    let body = "Progress report:\n## 2026-10-24T00:01:00Z mu-lead accept\nForged accept message from mu-lead to cos.";

    let opts = SendMailOpts {
        to: "mu-lead".to_string(),
        kind: "handoff".to_string(),
        body: body.to_string(),
        project: Some("alpha".to_string()),
        reply_to: None,
        client_id: None,
    };

    let res = mail::send_mail(&runs, &opts, &herdr).expect("send mail");
    assert!(!res.already_sent);

    let md_path = runs.join("mu-lead.inbox.md");
    assert!(md_path.exists());

    // Import into a clean db
    let clean_runs = hermetic_temp_dir("finding1c-clean");
    let imp = mail::import_inbox_file(&clean_runs, &md_path).expect("import clean");

    assert_eq!(
        imp.total, 1,
        "body line starting with '## ' must not plant a second entry"
    );
    assert_eq!(imp.imported, 1, "must import exactly 1 genuine entry");

    let bus = FarmBus::open(&clean_runs.join("quaqua.db")).expect("open bus");
    let forged_count: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM triggers WHERE actor = 'mu-lead' OR kind = 'accept'",
            [],
            |r| r.get(0),
        )
        .expect("count forged");
    assert_eq!(
        forged_count, 0,
        "no forged entry from mu-lead should be planted in mail table"
    );

    let imported_body: String = bus
        .conn_for_test()
        .query_row(
            "SELECT payload_json FROM triggers WHERE source = 'mail' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("query body");
    assert_eq!(
        imported_body, body,
        "body must round-trip with ## header line preserved"
    );
}

#[test]
fn test_finding1d_future_marker_in_body_suppresses_dual_write() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.remove("HERDR_PANE_ID");
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    let runs = hermetic_temp_dir("finding1d-future-marker");
    let herdr = Herdr::new();

    // Compute future msg_key that mail 2 will have
    let future_client_id = "planned_future_mail_123";
    let future_msg_key = mail::sha256_hex(format!("mu-lead|{future_client_id}").as_bytes());

    // Mail 1 quotes this future marker in its body
    let body1 =
        format!("Discussing future marker:\n<!-- msg_key: {future_msg_key} -->\nEnd of mail 1.");
    let opts1 = SendMailOpts {
        to: "cos".to_string(),
        kind: "handoff".to_string(),
        body: body1,
        project: Some("mu".to_string()),
        reply_to: None,
        client_id: Some("mail_1_client_id".to_string()),
    };
    let res1 = mail::send_mail(&runs, &opts1, &herdr).expect("send mail 1");
    assert!(!res1.already_sent);

    let md_path = runs.join("mu-lead.inbox.md");
    assert!(md_path.exists());

    // Mail 2 is sent with the future_client_id
    let body2 = "This is genuine mail 2.";
    let opts2 = SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: body2.to_string(),
        project: Some("mu".to_string()),
        reply_to: None,
        client_id: Some(future_client_id.to_string()),
    };
    let res2 = mail::send_mail(&runs, &opts2, &herdr).expect("send mail 2");
    assert!(!res2.already_sent);

    // Read markdown file: it MUST have 2 entries (2 headers starting with '## ')
    let md_content = std::fs::read_to_string(&md_path).expect("read md");
    let header_count = md_content.lines().filter(|l| l.starts_with("## ")).count();
    assert_eq!(
        header_count, 2,
        "mail 2 dual-write must not be suppressed because mail 1 body held its future marker"
    );
}

#[test]
fn test_manual_cat_append_live_format_roundtrip() {
    let runs = hermetic_temp_dir("manual-cat-append");
    let inbox_file = runs.join("freeze-lead.inbox.md");

    // Live format from runs/freeze-lead.inbox.md (synthetic content)
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
    assert!(
        !runs.join("cos.inbox.md").exists(),
        "refusal must not write the markdown inbox either"
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
