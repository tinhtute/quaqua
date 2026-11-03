//! Tests for `quaqua inbox` consumer digest and per-reader cursor idempotence.

use quaqua::bus::mail::{self, SendMailOpts};
use quaqua::bus::FarmBus;
use quaqua::herdr::Herdr;
use std::path::PathBuf;

fn hermetic_temp_dir(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "quaqua-inbox-test-{prefix}-{}-{nanos}",
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
fn test_inbox_cursor_read_idempotent() {
    let runs_dir = hermetic_temp_dir("cursor-idempotent");
    let db_path = runs_dir.join("quaqua.db");

    // Initialize FarmBus with schema
    let bus = FarmBus::open(&db_path).expect("open bus");

    // Record an incident event
    let ev1 = quaqua::bus::FarmEvent::new(
        "blocked",
        Some("herdr"),
        Some("worker-1"),
        Some("cos"),
        Some("myproj"),
    )
    .with_incident_key("blocked:p1:1000");

    let inserted = bus.record(&ev1).expect("record event");
    assert!(inserted, "ev1 must be newly inserted");

    // First inbox read for 'cos'
    let digest1 = bus.inbox_digest(&runs_dir, "cos").expect("first digest");
    assert_eq!(
        digest1.incidents.len(),
        1,
        "first digest must see 1 incident"
    );
    assert_eq!(
        digest1.incidents[0].incident_key.as_deref(),
        Some("blocked:p1:1000")
    );
    let cursor_after_first = bus
        .get_cursor("cos")
        .expect("get cursor")
        .expect("cursor set");
    assert_eq!(cursor_after_first, digest1.incidents[0].seq);

    // Repeated inbox read for 'cos' with no new events
    let digest2 = bus.inbox_digest(&runs_dir, "cos").expect("second digest");
    assert_eq!(
        digest2.incidents.len(),
        0,
        "repeated read with no new events must yield 0 new incidents"
    );
    let cursor_after_second = bus
        .get_cursor("cos")
        .expect("get cursor")
        .expect("cursor set");
    assert_eq!(
        cursor_after_second, cursor_after_first,
        "cursor must remain at same sequence across repeated reads"
    );
    // Record an incident owned by mu-lead
    let ev2 = quaqua::bus::FarmEvent::new(
        "blocked",
        Some("herdr"),
        Some("worker-2"),
        Some("mu-lead"),
        Some("myproj"),
    )
    .with_incident_key("blocked:p2:2000");
    bus.record(&ev2).expect("record ev2");

    // Second consumer 'mu-lead' has isolated cursor
    let digest_lead = bus.inbox_digest(&runs_dir, "mu-lead").expect("lead digest");
    assert_eq!(
        digest_lead.incidents.len(),
        1,
        "lead must see its 1 incident"
    );
    let lead_cursor = bus.get_cursor("mu-lead").expect("get lead cursor");
    assert_eq!(lead_cursor, Some(ev2_seq(&bus)));
}

fn ev2_seq(bus: &FarmBus) -> u64 {
    bus.load_events(0)
        .unwrap()
        .iter()
        .find(|e| e.incident_key.as_deref() == Some("blocked:p2:2000"))
        .unwrap()
        .seq
}

#[test]
fn test_inbox_digest_with_mail_and_incidents() {
    let runs_dir = hermetic_temp_dir("mail-and-incidents");
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");
    let herdr = Herdr::new();

    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    guard.remove("HERDR_PANE_ID");
    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: "verification passed on sha 12345".to_string(),
        project: Some("mu".to_string()),
        reply_to: None,
        client_id: None,
    };

    let send_res = mail::send_mail(&runs_dir, &opts, &herdr).expect("send mail");
    assert!(send_res.id > 0);

    // Record an incident
    let ev = quaqua::bus::FarmEvent::new(
        "stalled",
        Some("watchdog"),
        Some("sude-worker"),
        Some("cos"),
        Some("mu"),
    )
    .with_incident_key("stall:sude-worker:5000");
    bus.record(&ev).expect("record incident");

    // Read inbox digest
    let digest = bus.inbox_digest(&runs_dir, "cos").expect("digest");
    assert_eq!(digest.unacked_mail.len(), 1, "must have 1 unacked mail");
    assert_eq!(digest.unacked_mail[0].id, send_res.id);
    assert_eq!(digest.incidents.len(), 1, "must have 1 incident");

    let rendered = digest.render("cos");
    assert!(
        rendered.contains(&format!("#{} mu-lead accept", send_res.id)),
        "rendered mail header"
    );
    assert!(
        rendered.contains("stall:sude-worker:5000"),
        "rendered incident"
    );
}

#[test]
fn test_finding_3_inbox_follow_no_duplicate_deliveries_and_only_new_items() {
    let runs_dir = hermetic_temp_dir("finding-3-follow");
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");
    let herdr = Herdr::new();

    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    guard.remove("HERDR_PANE_ID");
    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: "ready for review".to_string(),
        project: Some("mu".to_string()),
        reply_to: None,
        client_id: None,
    };
    let send_res = mail::send_mail(&runs_dir, &opts, &herdr).expect("send mail");
    assert!(send_res.id > 0);

    // Run `quaqua inbox --for cos --follow` as a child process for 2.5 seconds (spans tick 0 and tick 1)
    let quaqua_bin = env!("CARGO_BIN_EXE_quaqua");
    let mut child = std::process::Command::new(quaqua_bin)
        .args(["inbox", "--for", "cos", "--follow"])
        .env("QUAQUA_RUNS_DIR", &runs_dir)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn quaqua inbox --follow");

    std::thread::sleep(std::time::Duration::from_millis(2500));
    let _ = child.kill();
    let output = child.wait_with_output().expect("wait on child");
    let stdout = String::from_utf8_lossy(&output.stdout);

    // In 2.5 seconds, the follow loop executed tick 0 and tick 1.
    // Finding 3: `--follow` must print only new items and NOT re-print the existing mail,
    // nor bump `deliveries` repeatedly on each tick.
    let occurrences = stdout
        .matches(&format!("#{} mu-lead accept", send_res.id))
        .count();
    assert_eq!(
        occurrences, 1,
        "mail should be printed exactly once across ticks, but was printed {occurrences} times: {stdout}"
    );

    let key = format!("mail_delivery:{}", send_res.id);
    let deliveries: u64 = bus
        .conn_for_test()
        .query_row(
            "SELECT value FROM source_state WHERE source = ?1",
            rusqlite::params![&key],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    assert_eq!(
        deliveries, 1,
        "follow loop must not bump deliveries on each tick; deliveries was {deliveries}"
    );
}

#[test]
fn test_inbox_noise_filter_drops_consumer_own_pane_blocked_incidents() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("HERDR_PANE_ID", "w1:p1X");

    let runs_dir = hermetic_temp_dir("noise-filter");
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");

    // Incident 1: consumer's own pane (w1:p1X) blocked on a question -> NOISE
    let ev_noise1 = quaqua::bus::FarmEvent::new(
        "blocked",
        Some("herdr"),
        Some("w1:p1X"),
        Some("cos"),
        None::<String>,
    )
    .with_incident_key("blocked:w1:p1X:1792984227318");
    bus.record(&ev_noise1).expect("record noise1");

    // Incident 2: another worker/lead pane (w7:p2) blocked -> REAL INCIDENT (keep)
    let ev_real = quaqua::bus::FarmEvent::new(
        "blocked",
        Some("herdr"),
        Some("w7:p2"),
        Some("cos"),
        Some("projA"),
    )
    .with_incident_key("blocked:w7:p2:1792985802177");
    bus.record(&ev_real).expect("record real");

    // Incident 3: consumer's own pane blocked again with JSON payload -> NOISE
    let ev_noise2 = quaqua::bus::FarmEvent::new(
        "blocked",
        Some("herdr"),
        Some("w1:p1X"),
        Some("cos"),
        None::<String>,
    )
    .with_incident_key("blocked:w1:p1X:1792986671622")
    .with_payload_json(r#"{"event":"pane_agent_status_changed","data":{"pane_id":"w1:p1X","agent_status":"blocked"}}"#);
    bus.record(&ev_noise2).expect("record noise2");

    // Read inbox digest for cos:
    // Must drop ev_noise1 and ev_noise2, but keep ev_real!
    let digest = bus.inbox_digest(&runs_dir, "cos").expect("digest");
    assert_eq!(
        digest.incidents.len(),
        1,
        "must only include 1 real incident, dropping 2 noise incidents"
    );
    assert_eq!(digest.incidents[0].subject.as_deref(), Some("w7:p2"));
    assert_eq!(
        digest.incidents[0].incident_key.as_deref(),
        Some("blocked:w7:p2:1792985802177")
    );

    let rendered = digest.render("cos");
    assert!(
        rendered.contains("blocked:w7:p2:1792985802177"),
        "rendered must contain real incident"
    );
    assert!(
        !rendered.contains("w1:p1X"),
        "rendered must not contain any w1:p1X noise incidents"
    );

    // Cursor must have advanced to the highest event sequence evaluated
    let cursor = bus
        .get_cursor("cos")
        .expect("get cursor")
        .expect("cursor set");
    assert!(cursor >= digest.incidents[0].seq);

    // Repeated call: 0 incidents (idempotent)
    let digest2 = bus.inbox_digest(&runs_dir, "cos").expect("digest2");
    assert_eq!(
        digest2.incidents.len(),
        0,
        "repeated digest must yield 0 incidents"
    );
}

#[test]
fn test_inbox_high_water_mark_mail_cursor_advances_and_deduplicates() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    guard.remove("HERDR_PANE_ID");

    let runs_dir = hermetic_temp_dir("hwm-mail");
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");
    let herdr = Herdr::new();

    // Initially, mail cursor is 0 or unset
    assert_eq!(bus.get_mail_cursor("cos").expect("get mail cursor"), None);

    // Send Mail 1
    let opts1 = SendMailOpts {
        to: "cos".to_string(),
        kind: "handoff".to_string(),
        body: "task 1 complete".to_string(),
        project: Some("mu".to_string()),
        reply_to: None,
        client_id: None,
    };
    let res1 = mail::send_mail(&runs_dir, &opts1, &herdr).expect("send mail 1");
    assert!(res1.id > 0);

    // First inbox read: sees Mail 1
    let digest1 = bus.inbox_digest(&runs_dir, "cos").expect("digest 1");
    assert_eq!(
        digest1.unacked_mail.len(),
        1,
        "first digest must see mail 1"
    );
    assert_eq!(digest1.unacked_mail[0].id, res1.id);
    assert_eq!(
        bus.get_mail_cursor("cos").expect("get cursor"),
        Some(res1.id)
    );

    // Second inbox read: returns 0 mail (high-water mark prevents re-delivering)
    let digest2 = bus.inbox_digest(&runs_dir, "cos").expect("digest 2");
    assert_eq!(
        digest2.unacked_mail.len(),
        0,
        "repeated digest must not re-yield already seen mail"
    );
    assert_eq!(
        bus.get_mail_cursor("cos").expect("get cursor"),
        Some(res1.id)
    );

    // Send Mail 2
    let opts2 = SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: "review approved".to_string(),
        project: Some("mu".to_string()),
        reply_to: None,
        client_id: None,
    };
    let res2 = mail::send_mail(&runs_dir, &opts2, &herdr).expect("send mail 2");
    assert!(res2.id > res1.id);

    // Third inbox read: sees ONLY Mail 2
    let digest3 = bus.inbox_digest(&runs_dir, "cos").expect("digest 3");
    assert_eq!(
        digest3.unacked_mail.len(),
        1,
        "third digest must see only new mail"
    );
    assert_eq!(digest3.unacked_mail[0].id, res2.id);
    assert_eq!(
        bus.get_mail_cursor("cos").expect("get cursor"),
        Some(res2.id)
    );
}

#[test]
fn test_e2e_smoke_mail_send_dual_write_hook_digest_idempotent_resend_ack() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
        "QUAQUA_BIN",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    guard.remove("HERDR_PANE_ID");

    let runs_dir = hermetic_temp_dir("e2e-smoke");
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");
    let herdr = Herdr::new();

    let quaqua_bin = env!("CARGO_BIN_EXE_quaqua");
    guard.set("QUAQUA_BIN", quaqua_bin);

    // Step 1: mail send
    let body_text = "verification passed on sha feedbeef123";
    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "handoff".to_string(),
        body: body_text.to_string(),
        project: Some("mu".to_string()),
        reply_to: None,
        client_id: None,
    };
    let res1 = mail::send_mail(&runs_dir, &opts, &herdr).expect("send mail");
    assert!(res1.id > 0);
    assert!(!res1.already_sent);

    // Step 2: DB row + md dual-write
    let row_count: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT count(*) FROM triggers WHERE id = ?1",
            rusqlite::params![res1.id as i64],
            |r| r.get(0),
        )
        .expect("query trigger row");
    assert_eq!(row_count, 1, "DB row must exist");

    let inbox_md = runs_dir.join("mu-lead.inbox.md");
    assert!(
        inbox_md.is_file(),
        "markdown dual-write file mu-lead.inbox.md must exist"
    );
    let md_content = std::fs::read_to_string(&inbox_md).expect("read inbox md");
    assert!(md_content.contains("## "), "markdown must contain header");
    assert!(
        md_content.contains("mu-lead handoff"),
        "markdown must contain sender and kind"
    );
    assert!(md_content.contains(body_text), "markdown must contain body");

    // Step 3: hook digest shows it
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let hook_script = manifest_dir.join("../../templates/workspace/scripts/hooks/cos-inbox.sh");
    assert!(
        hook_script.is_file(),
        "hook script must exist at {}",
        hook_script.display()
    );

    let out_hook = std::process::Command::new("sh")
        .arg(&hook_script)
        .arg("--user-prompt-submit")
        .env("QUAQUA_BIN", quaqua_bin)
        .env("QUAQUA_RUNS_DIR", &runs_dir)
        .output()
        .expect("execute cos-inbox.sh");

    assert_eq!(out_hook.status.code(), Some(0), "hook must exit 0");
    let hook_stdout = String::from_utf8_lossy(&out_hook.stdout);
    assert!(
        hook_stdout.contains("UserPromptSubmit"),
        "hook JSON must specify UserPromptSubmit"
    );
    assert!(
        hook_stdout.contains("additionalContext"),
        "hook JSON must have additionalContext"
    );
    assert!(
        hook_stdout.contains(&format!("#{} mu-lead handoff", res1.id)),
        "hook digest must show the new mail"
    );
    assert!(
        hook_stdout.contains(body_text),
        "hook digest must include the mail body"
    );

    // Verify digest length is <= 10 lines
    let val: serde_json::Value = serde_json::from_str(&hook_stdout).expect("parse hook json");
    let ctx = val
        .pointer("/hookSpecificOutput/additionalContext")
        .and_then(|v| v.as_str())
        .unwrap();
    let line_count = ctx.lines().count();
    assert!(
        line_count <= 10,
        "digest must be <= 10 lines, but was {line_count}"
    );

    // Also test Stop hook format:
    let out_stop = std::process::Command::new("sh")
        .arg(&hook_script)
        .arg("--stop")
        .env("QUAQUA_BIN", quaqua_bin)
        .env("QUAQUA_RUNS_DIR", &runs_dir)
        .output()
        .expect("execute cos-inbox.sh --stop");
    assert_eq!(out_stop.status.code(), Some(0));
    // Note: high-water mark already advanced by the first hook run, so output should be silent (nothing new)!
    assert!(
        out_stop.stdout.is_empty(),
        "stop hook must be silent when nothing new"
    );

    // Step 4: idempotent resend
    let res2 = mail::send_mail(&runs_dir, &opts, &herdr).expect("resend mail");
    assert_eq!(res2.id, res1.id, "resend must return same mail id");
    assert!(res2.already_sent, "resend must report already_sent = true");

    let row_count_after: i64 = bus
        .conn_for_test()
        .query_row(
            "SELECT count(*) FROM triggers WHERE to_name = 'cos'",
            [],
            |r| r.get(0),
        )
        .expect("count triggers rows");
    assert_eq!(
        row_count_after, 1,
        "DB triggers table must still have exactly 1 row"
    );

    let md_content2 = std::fs::read_to_string(&inbox_md).expect("read inbox md again");
    let header_count = md_content2.matches("mu-lead handoff").count();
    assert_eq!(
        header_count, 1,
        "markdown inbox dual-write must not duplicate entries"
    );

    // Hook run after idempotent resend: must be SILENT (0 new mail since high-water mark advanced)
    let out_hook2 = std::process::Command::new("sh")
        .arg(&hook_script)
        .arg("--user-prompt-submit")
        .env("QUAQUA_BIN", quaqua_bin)
        .env("QUAQUA_RUNS_DIR", &runs_dir)
        .output()
        .expect("execute cos-inbox.sh again");

    assert_eq!(out_hook2.status.code(), Some(0));
    assert!(
        out_hook2.stdout.is_empty(),
        "hook must be silent on idempotent resend (nothing new)"
    );

    // Step 5: ack hides it
    let acked = mail::ack_mail(&runs_dir, "cos", &[res1.id]).expect("ack mail");
    assert_eq!(acked, vec![res1.id]);

    let cursor: Option<u64> = bus.get_cursor("cos").expect("query cursor");
    assert!(cursor.is_some(), "cursor must be set in DB");

    // Render inbox (which shows all unacked mail) must now hide it
    let inbox_view = mail::render_inbox(&runs_dir, "cos").expect("render inbox");
    assert!(
        inbox_view.contains("no unacked mail for 'cos'"),
        "ack hides it from inbox: {inbox_view}"
    );
    assert!(!inbox_view.contains(&format!("#{}", res1.id)));

    // Inbox digest also hides it
    let digest_after_ack = bus
        .inbox_digest(&runs_dir, "cos")
        .expect("digest after ack");
    assert_eq!(digest_after_ack.unacked_mail.len(), 0);
}

#[test]
fn test_cos_inbox_hook_stop_blocks_once_and_skips_when_stop_hook_active() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
        "QUAQUA_BIN",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "arch-lead");
    guard.remove("HERDR_PANE_ID");

    let runs_dir = hermetic_temp_dir("hook-stop");
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    let _bus = FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");
    let herdr = Herdr::new();

    let quaqua_bin = env!("CARGO_BIN_EXE_quaqua");
    guard.set("QUAQUA_BIN", quaqua_bin);

    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "escalate".to_string(),
        body: "cost cap exceeded".to_string(),
        project: Some("arch".to_string()),
        reply_to: None,
        client_id: None,
    };
    mail::send_mail(&runs_dir, &opts, &herdr).expect("send mail");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let hook_script = manifest_dir.join("../../templates/workspace/scripts/hooks/cos-inbox.sh");

    // 1. Stop hook with stop_hook_active: false -> must BLOCK with decision: block and reason
    let stdin_inactive = r#"{"hook_event_name":"Stop","stop_hook_active":false}"#;
    let mut child = std::process::Command::new("sh")
        .arg(&hook_script)
        .arg("--stop")
        .env("QUAQUA_BIN", quaqua_bin)
        .env("QUAQUA_RUNS_DIR", &runs_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn hook");

    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_inactive.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait on hook");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(r#""decision": "block""#) || stdout.contains(r#""decision":"block""#),
        "must block on stop: {stdout}"
    );
    assert!(
        stdout.contains("arch-lead escalate"),
        "must mention new mail: {stdout}"
    );

    // Send another mail to test stop_hook_active: true
    let opts2 = SendMailOpts {
        to: "cos".to_string(),
        kind: "blocked".to_string(),
        body: "waiting on decision".to_string(),
        project: Some("arch".to_string()),
        reply_to: None,
        client_id: None,
    };
    mail::send_mail(&runs_dir, &opts2, &herdr).expect("send mail 2");

    // 2. Stop hook with stop_hook_active: true -> must NOT block (silent exit 0)
    let stdin_active = r#"{"hook_event_name":"Stop","stop_hook_active":true}"#;
    let mut child2 = std::process::Command::new("sh")
        .arg(&hook_script)
        .arg("--stop")
        .env("QUAQUA_BIN", quaqua_bin)
        .env("QUAQUA_RUNS_DIR", &runs_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn hook 2");

    child2
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_active.as_bytes())
        .unwrap();
    let out2 = child2.wait_with_output().expect("wait on hook 2");
    assert_eq!(out2.status.code(), Some(0));
    assert!(
        out2.stdout.is_empty(),
        "stop hook must not block when stop_hook_active is true"
    );
}

#[test]
fn test_inbox_drops_foreign_blocked_pane_incidents_and_keeps_real_incidents() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("HERDR_PANE_ID", "w1:p1X");

    let runs_dir = hermetic_temp_dir("foreign-pane");
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");

    // Incident 1: real worker pane belonging to CoS (w1:p2) blocked -> KEEP
    let ev_real = quaqua::bus::FarmEvent::new(
        "blocked",
        Some("herdr"),
        Some("w1:p2"),
        Some("cos"),
        Some("quaqua"),
    )
    .with_incident_key("blocked:w1:p2:1792985802000");
    bus.record(&ev_real).expect("record real");

    // Incident 2: foreign pane (w7:p2) from another workspace/project (tokens only logo, no grid_owner/grid_role)
    // went blocked on 2026-10-29 -> MUST DROP (never surface to CoS). This row is stamped with the
    // local farm (the hook wrote it before attribution existed), so only the consumer-group filter
    // can reject it; the farm predicate alone is not enough.
    let ev_foreign = quaqua::bus::FarmEvent::new(
        "blocked",
        Some("herdr"),
        Some("w7:p2"),
        None::<String>,
        None::<String>,
    )
    .with_incident_key("blocked:w7:p2:1792985802999")
    .with_payload_json(r#"{"event":"pane.agent_status_changed","data":{"pane_id":"w7:p2","agent":"claude","agent_status":"blocked","workspace_id":"w7","tokens":{"logo":"\ue1a3"}}}"#);
    bus.record(&ev_foreign).expect("record foreign");

    // Read inbox digest for cos:
    let digest = bus.inbox_digest(&runs_dir, "cos").expect("digest");

    assert_eq!(
        digest.incidents.len(),
        1,
        "must only surface 1 real incident, dropping foreign pane incident"
    );
    assert_eq!(digest.incidents[0].subject.as_deref(), Some("w1:p2"));
    assert_eq!(
        digest.incidents[0].incident_key.as_deref(),
        Some("blocked:w1:p2:1792985802000")
    );

    let rendered = digest.render("cos");
    assert!(
        rendered.contains("w1:p2"),
        "rendered digest must contain real incident for w1:p2"
    );
    assert!(
        !rendered.contains("w7:p2"),
        "rendered digest must never surface foreign pane w7:p2"
    );
}

#[test]
fn test_inbox_foreign_farm_rows_never_reach_cos_or_lead() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.remove("HERDR_PANE_ID");

    let runs_dir = hermetic_temp_dir("foreign-farm");
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");

    let now = quaqua::bus::now_ms();
    // Foreign row 1: farm='TTDVKH', owner='cos'
    bus.conn_for_test().execute(
        "INSERT OR IGNORE INTO threads (id, owner, state, last_trigger_ms) VALUES ('agent:w7:p2', 'cos', 'open', ?1)",
        rusqlite::params![now as i64],
    ).unwrap();
    bus.conn_for_test().execute(
        r#"
        INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm)
        VALUES (?1, 'herdr', 'blocked', 'herdr', NULL, 'agent:w7:p2', 'blocked w7:p2', NULL, '{"owner":"cos"}', 'blocked:w7:p2:foreign_cos', 'TTDVKH')
        "#,
        rusqlite::params![now as i64],
    ).expect("insert foreign cos row");

    // Foreign row 2: farm='TTDVKH', owner=NULL (unattributed foreign incident)
    bus.conn_for_test().execute(
        "INSERT OR IGNORE INTO threads (id, owner, state, last_trigger_ms) VALUES ('agent:w7:p25', 'unknown', 'open', ?1)",
        rusqlite::params![now as i64],
    ).unwrap();
    bus.conn_for_test().execute(
        r#"
        INSERT INTO triggers (ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm)
        VALUES (?1, 'herdr', 'blocked', 'herdr', NULL, 'agent:w7:p25', 'blocked w7:p25', NULL, '{}', 'blocked:w7:p25:foreign_null', 'TTDVKH')
        "#,
        rusqlite::params![now as i64],
    ).expect("insert foreign null row");

    // 1. cos digest must NOT see foreign-farm row
    let cos_digest = bus.inbox_digest(&runs_dir, "cos").expect("cos digest");
    assert_eq!(
        cos_digest.incidents.len(),
        0,
        "foreign farm row (farm='TTDVKH') must not reach cos inbox digest"
    );

    // 2. mu-lead digest must NOT see owner=NULL row or foreign-farm row
    let lead_digest = bus.inbox_digest(&runs_dir, "mu-lead").expect("lead digest");
    assert_eq!(
        lead_digest.incidents.len(),
        0,
        "foreign rows with owner=NULL or foreign farm must not reach mu-lead inbox digest"
    );
}

#[test]
fn test_inbox_digest_interleaved_mail_incident_no_redelivery() {
    let runs_dir = hermetic_temp_dir("interleaved-cursor");
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");
    let herdr = Herdr::new();

    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    guard.remove("HERDR_PANE_ID");

    // Trigger 1: incident
    let ev1 = quaqua::bus::FarmEvent::new(
        "blocked",
        Some("herdr"),
        Some("w1"),
        Some("cos"),
        Some("p1"),
    )
    .with_incident_key("k1");
    bus.record(&ev1).unwrap();

    // Trigger 2: mail
    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "directive".to_string(),
        body: "do work".to_string(),
        project: None,
        reply_to: None,
        client_id: None,
    };
    let send_res = mail::send_mail(&runs_dir, &opts, &herdr).unwrap();
    assert_eq!(send_res.id, 2);

    // Trigger 3: incident after mail
    let ev2 = quaqua::bus::FarmEvent::new(
        "stalled",
        Some("watchdog"),
        Some("w2"),
        Some("cos"),
        Some("p1"),
    )
    .with_incident_key("k2");
    bus.record(&ev2).unwrap();

    // First read: must see both incidents and mail, advancing cursor to 3
    let digest1 = bus.inbox_digest(&runs_dir, "cos").unwrap();
    assert_eq!(digest1.unacked_mail.len(), 1);
    assert_eq!(digest1.incidents.len(), 2);
    assert_eq!(digest1.new_cursor, 3);
    assert_eq!(bus.get_cursor("cos").unwrap(), Some(3));

    // Second read: must be completely empty (F1: no redelivery of trigger 3)
    let digest2 = bus.inbox_digest(&runs_dir, "cos").unwrap();
    assert_eq!(digest2.unacked_mail.len(), 0);
    assert_eq!(digest2.incidents.len(), 0);
    assert_eq!(digest2.new_cursor, 3);
    assert_eq!(bus.get_cursor("cos").unwrap(), Some(3));
}

#[test]
fn test_finding_f5_follow_tick_preserves_project_and_reply_to() {
    let runs_dir = hermetic_temp_dir("f5-follow-ref");
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");
    let herdr = Herdr::new();

    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "alpha-lead");
    guard.remove("HERDR_PANE_ID");

    let opts = SendMailOpts {
        to: "cos".to_string(),
        kind: "accept".to_string(),
        body: "finished unit".to_string(),
        project: Some("my-proj".to_string()),
        reply_to: Some(42),
        client_id: None,
    };
    let send_res = mail::send_mail(&runs_dir, &opts, &herdr).expect("send mail");
    assert!(send_res.id > 0);

    let mut last_seen_mail = 0;
    let digest = bus
        .follow_tick(&runs_dir, "cos", &mut last_seen_mail)
        .expect("follow tick");

    assert_eq!(digest.unacked_mail.len(), 1);
    let msg = &digest.unacked_mail[0];

    assert_eq!(
        msg.project.as_deref(),
        Some("my-proj"),
        "F5: project must be decoded from ref"
    );
    assert_eq!(
        msg.reply_to,
        Some(42),
        "F5: reply_to must be decoded from ref"
    );
}

#[test]
fn test_finding_m1_mail_ack_never_drops_unread_incidents_or_unacked_lower_mail() {
    let runs_dir = hermetic_temp_dir("m1-mail-ack");
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");
    let herdr = Herdr::new();

    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    guard.remove("HERDR_PANE_ID");

    // 1. Initial event to establish cos cursor at 1
    let ev0 = quaqua::bus::FarmEvent::new(
        "spawned",
        Some("cos"),
        Some("w0"),
        Some("cos"),
        Some("p1"),
    )
    .with_incident_key("k0");
    bus.record(&ev0).unwrap();
    let d0 = bus.inbox_digest(&runs_dir, "cos").unwrap();
    assert_eq!(d0.new_cursor, 1);
    assert_eq!(bus.get_cursor("cos").unwrap(), Some(1));

    // 2. Trigger 2: lower mail to cos
    let opts1 = SendMailOpts {
        to: "cos".to_string(),
        kind: "directive".to_string(),
        body: "lower mail 1".to_string(),
        project: None,
        reply_to: None,
        client_id: None,
    };
    let res_mail1 = mail::send_mail(&runs_dir, &opts1, &herdr).unwrap();
    assert_eq!(res_mail1.id, 2);

    // 3. Trigger 3: non-mail incident
    let ev_inc = quaqua::bus::FarmEvent::new(
        "blocked",
        Some("herdr"),
        Some("w1"),
        Some("cos"),
        Some("p1"),
    )
    .with_incident_key("k_blocked");
    bus.record(&ev_inc).unwrap();

    // 4. Trigger 4: higher mail to cos
    let opts2 = SendMailOpts {
        to: "cos".to_string(),
        kind: "directive".to_string(),
        body: "higher mail 2".to_string(),
        project: None,
        reply_to: None,
        client_id: None,
    };
    let res_mail2 = mail::send_mail(&runs_dir, &opts2, &herdr).unwrap();
    assert_eq!(res_mail2.id, 4);

    // 5. cos acks ONLY higher mail 4
    let acked = mail::ack_mail(&runs_dir, "cos", &[res_mail2.id]).expect("ack mail 4");
    assert_eq!(acked, vec![res_mail2.id]);

    // Part A: Lower mail 2 must NOT disappear from mail inbox
    let inbox_after = mail::render_inbox(&runs_dir, "cos").expect("render inbox");
    assert!(
        inbox_after.contains(&format!("#{}", res_mail1.id)),
        "lower mail 2 must NOT disappear from mail inbox; got:\n{inbox_after}"
    );

    let mail_list = mail::list_mail(&runs_dir, false, None, None, None).expect("list mail");
    assert!(
        mail_list.contains("#2    mu-lead      -> cos          directive  [unacked]"),
        "lower mail 2 must remain unacked in mail ls; got:\n{mail_list}"
    );
    assert!(
        mail_list.contains("#4    mu-lead      -> cos          directive  [acked]"),
        "higher mail 4 must be acked in mail ls; got:\n{mail_list}"
    );

    // Part B: Incident trigger 3 must NOT be skipped by inbox_digest
    let digest = bus.inbox_digest(&runs_dir, "cos").expect("inbox digest");
    assert_eq!(
        digest.incidents.len(),
        1,
        "incident trigger 3 must NOT be lost after mail ack; incidents found: {:?}",
        digest.incidents
    );
    assert_eq!(digest.incidents[0].event_type, "blocked");
    assert_eq!(digest.incidents[0].seq, 3);
}
