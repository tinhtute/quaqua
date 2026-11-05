//! Tests for `quaqua inbox` consumer digest and per-reader cursor idempotence.

use quaqua::bus::events::{render_triggers, TriggerRecord};
use quaqua::bus::mail::{self, SendMailOpts};
use quaqua::bus::{FarmBus, NewTrigger};
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
    bus.open_thread_and_subscribe(
        "agent:sude-worker",
        "cos",
        "cos",
        quaqua::bus::now_ms() + 86_400_000,
    )
    .expect("open thread and subscribe");
    bus.record(&ev).expect("record incident");

    // Read inbox digest
    let digest = bus.inbox_digest(&runs_dir, "cos").expect("digest");
    assert!(
        digest.triggers.iter().any(|t| t.id == send_res.id),
        "must have 1 mail trigger"
    );
    assert_eq!(digest.incidents.len(), 1, "must have 1 incident");

    let rendered = digest.render("cos");
    assert!(
        rendered.contains(&format!("#{} accept mu-lead", send_res.id)),
        "rendered mail header"
    );
    assert!(
        rendered.contains("stalled watchdog agent:sude-worker"),
        "rendered incident: {rendered}"
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
        .matches(&format!("#{} accept mu-lead", send_res.id))
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
    assert_eq!(digest1.triggers.len(), 1, "first digest must see mail 1");
    assert_eq!(digest1.triggers[0].id, res1.id);
    assert_eq!(
        bus.get_mail_cursor("cos").expect("get cursor"),
        Some(res1.id)
    );

    // Second inbox read: returns 0 mail (high-water mark prevents re-delivering)
    let digest2 = bus.inbox_digest(&runs_dir, "cos").expect("digest 2");
    assert_eq!(
        digest2.triggers.len(),
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
        digest3.triggers.len(),
        1,
        "third digest must see only new mail"
    );
    assert_eq!(digest3.triggers[0].id, res2.id);
    assert_eq!(
        bus.get_mail_cursor("cos").expect("get cursor"),
        Some(res2.id)
    );
}

#[test]
fn test_e2e_smoke_mail_send_hook_digest_idempotent_resend() {
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
        hook_stdout.contains(&format!("#{} handoff mu-lead", res1.id)),
        "hook digest must show the new mail"
    );
    assert!(
        hook_stdout.contains(body_text),
        "hook digest must include the mail summary"
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
        stdout.contains("escalate arch-lead"),
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
    assert!(digest1.triggers.iter().any(|t| t.id == send_res.id));
    assert_eq!(digest1.incidents.len(), 2);
    assert_eq!(digest1.new_cursor, 3);
    assert_eq!(bus.get_cursor("cos").unwrap(), Some(3));

    // Second read: must be completely empty (F1: no redelivery of trigger 3)
    let digest2 = bus.inbox_digest(&runs_dir, "cos").unwrap();
    assert_eq!(digest2.triggers.len(), 0);
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
fn test_superagent_trigger_digest_golden_format_and_ref_omission() {
    let triggers = vec![
        TriggerRecord {
            id: 1,
            ts_ms: 100,
            source: "mail".to_string(),
            kind: "handoff".to_string(),
            actor: "mu-lead".to_string(),
            to_name: Some("cos".to_string()),
            thread_id: "quaqua/mu-lead".to_string(),
            summary: "Task completed successfully".to_string(),
            ref_val: Some("runs/mu-lead.md".to_string()),
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
        TriggerRecord {
            id: 2,
            ts_ms: 200,
            source: "worker".to_string(),
            kind: "progress".to_string(),
            actor: "mu-lead".to_string(),
            to_name: None,
            thread_id: "quaqua/mu-lead".to_string(),
            summary: "running targeted tests".to_string(),
            ref_val: None,
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
        TriggerRecord {
            id: 3,
            ts_ms: 300,
            source: "watchdog".to_string(),
            kind: "notice".to_string(),
            actor: "watchdog".to_string(),
            to_name: Some("cos".to_string()),
            thread_id: "quaqua/other".to_string(),
            summary: "budget check ok".to_string(),
            ref_val: Some("   ".to_string()),
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
    ];

    let out = render_triggers(&triggers, false);

    // Thread "quaqua/other" has max trigger #3, so it appears first
    // Thread "quaqua/mu-lead" has max trigger #2, so it appears second
    let expected = "\
#3 notice watchdog quaqua/other budget check ok
#1 handoff mu-lead quaqua/mu-lead Task completed successfully → runs/mu-lead.md
#2 progress mu-lead quaqua/mu-lead running targeted tests
";
    assert_eq!(out, expected);
}

#[test]
fn test_superagent_trigger_digest_thread_grouping_newest_first() {
    let triggers = vec![
        TriggerRecord {
            id: 10,
            ts_ms: 100,
            source: "worker".to_string(),
            kind: "k1".to_string(),
            actor: "a1".to_string(),
            to_name: None,
            thread_id: "thread-alpha".to_string(),
            summary: "alpha first".to_string(),
            ref_val: None,
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
        TriggerRecord {
            id: 20,
            ts_ms: 200,
            source: "worker".to_string(),
            kind: "k2".to_string(),
            actor: "a1".to_string(),
            to_name: None,
            thread_id: "thread-alpha".to_string(),
            summary: "alpha second".to_string(),
            ref_val: None,
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
        TriggerRecord {
            id: 5,
            ts_ms: 150,
            source: "worker".to_string(),
            kind: "k1".to_string(),
            actor: "b1".to_string(),
            to_name: None,
            thread_id: "thread-beta".to_string(),
            summary: "beta first".to_string(),
            ref_val: None,
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
        TriggerRecord {
            id: 50,
            ts_ms: 500,
            source: "worker".to_string(),
            kind: "k2".to_string(),
            actor: "b1".to_string(),
            to_name: None,
            thread_id: "thread-beta".to_string(),
            summary: "beta second".to_string(),
            ref_val: None,
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
        TriggerRecord {
            id: 30,
            ts_ms: 300,
            source: "worker".to_string(),
            kind: "k1".to_string(),
            actor: "c1".to_string(),
            to_name: None,
            thread_id: "thread-gamma".to_string(),
            summary: "gamma only".to_string(),
            ref_val: None,
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
    ];

    let out = render_triggers(&triggers, false);

    // Order: thread-beta (max 50), then thread-gamma (max 30), then thread-alpha (max 20)
    let expected = "\
#5 k1 b1 thread-beta beta first
#50 k2 b1 thread-beta beta second
#30 k1 c1 thread-gamma gamma only
#10 k1 a1 thread-alpha alpha first
#20 k2 a1 thread-alpha alpha second
";
    assert_eq!(out, expected);
}

#[test]
fn test_superagent_trigger_digest_same_kind_collapsing() {
    let triggers = vec![
        TriggerRecord {
            id: 1,
            ts_ms: 10,
            source: "worker".to_string(),
            kind: "progress".to_string(),
            actor: "worker-1".to_string(),
            to_name: None,
            thread_id: "quaqua/worker-1".to_string(),
            summary: "step 1".to_string(),
            ref_val: None,
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
        TriggerRecord {
            id: 2,
            ts_ms: 20,
            source: "worker".to_string(),
            kind: "progress".to_string(),
            actor: "worker-1".to_string(),
            to_name: None,
            thread_id: "quaqua/worker-1".to_string(),
            summary: "step 2".to_string(),
            ref_val: None,
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
        TriggerRecord {
            id: 3,
            ts_ms: 30,
            source: "worker".to_string(),
            kind: "progress".to_string(),
            actor: "worker-1".to_string(),
            to_name: None,
            thread_id: "quaqua/worker-1".to_string(),
            summary: "step 3".to_string(),
            ref_val: Some("pr/1".to_string()),
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
        TriggerRecord {
            id: 4,
            ts_ms: 40,
            source: "reviewer".to_string(),
            kind: "review".to_string(),
            actor: "reviewer-1".to_string(),
            to_name: None,
            thread_id: "quaqua/worker-1".to_string(),
            summary: "review clean".to_string(),
            ref_val: None,
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
        TriggerRecord {
            id: 5,
            ts_ms: 50,
            source: "worker".to_string(),
            kind: "progress".to_string(),
            actor: "worker-1".to_string(),
            to_name: None,
            thread_id: "quaqua/worker-1".to_string(),
            summary: "step 4".to_string(),
            ref_val: None,
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
        TriggerRecord {
            id: 6,
            ts_ms: 60,
            source: "worker".to_string(),
            kind: "progress".to_string(),
            actor: "worker-1".to_string(),
            to_name: None,
            thread_id: "quaqua/worker-1".to_string(),
            summary: "step 5".to_string(),
            ref_val: Some("pr/2".to_string()),
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        },
    ];

    let out = render_triggers(&triggers, false);
    let expected = "\
#1..#3 progress ×3 worker-1 quaqua/worker-1 step 3 → pr/1
#4 review reviewer-1 quaqua/worker-1 review clean
#5..#6 progress ×2 worker-1 quaqua/worker-1 step 5 → pr/2
";
    assert_eq!(out, expected);
}

#[test]
fn test_superagent_trigger_digest_overflow_truncation_and_all() {
    let mut triggers = Vec::new();
    for i in 1..=25 {
        triggers.push(TriggerRecord {
            id: i,
            ts_ms: i * 10,
            source: "worker".to_string(),
            kind: "progress".to_string(),
            actor: format!("worker-{i}"),
            to_name: None,
            thread_id: format!("thread-{i}"),
            summary: format!("summary {i}"),
            ref_val: None,
            payload_json: "{}".to_string(),
            dedupe_key: None,
            farm: "local".to_string(),
        });
    }

    // 1. Without --all: capped at 20 lines, ends with overflow note
    let out_capped = render_triggers(&triggers, false);
    let lines_capped: Vec<&str> = out_capped.lines().collect();
    assert_eq!(lines_capped.len(), 20, "capped output must have 20 lines");
    assert_eq!(
        lines_capped[19], "… +6 more: quaqua mail inbox --all",
        "20th line must be overflow line with +6 more"
    );

    // 2. With --all: all 25 lines printed without overflow truncation
    let out_all = render_triggers(&triggers, true);
    let lines_all: Vec<&str> = out_all.lines().collect();
    assert_eq!(lines_all.len(), 25, "--all must show all 25 lines");
    assert!(
        !out_all.contains("more: quaqua mail inbox --all"),
        "--all must not contain overflow note"
    );
}

#[test]
fn test_mail_inbox_cli_caller_resolution_no_for_unknown_exits_1() {
    let guard = EnvGuard::new(&["QUAQUA_OWNER", "HERDR_PANE_ID", "QUAQUA_RUNS_DIR"]);
    guard.remove("QUAQUA_OWNER");
    guard.remove("HERDR_PANE_ID");

    let runs_dir = hermetic_temp_dir("caller-unknown");
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    let bus = FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");

    let tr =
        NewTrigger::new("mail", "handoff", "lead", "thread-1", "test summary").with_to_name("cos");
    bus.record_trigger(&tr).expect("record trigger");

    let herdr = Herdr::new();
    let res = mail::handle_mail(&["inbox".to_string()], &herdr);
    assert!(
        res.is_err(),
        "inbox without --for must error when caller identity is unknown"
    );
    let err = res.err().unwrap();
    assert!(
        err.contains("unresolved caller"),
        "error must mention unresolved caller: {err}"
    );

    // Ensure cursor was NOT created or advanced
    assert_eq!(bus.get_cursor("cos").unwrap(), None);
}

#[test]
fn test_mail_inbox_cli_for_other_peeks_with_warning() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    guard.remove("HERDR_PANE_ID");
    let runs_dir = hermetic_temp_dir("for-other-peek");
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    let bus = FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");

    let tr =
        NewTrigger::new("mail", "handoff", "lead", "thread-1", "test summary").with_to_name("cos");
    bus.record_trigger(&tr).expect("record trigger");

    let herdr = Herdr::new();
    let res = mail::handle_mail(
        &["inbox".to_string(), "--for".to_string(), "cos".to_string()],
        &herdr,
    );
    assert_eq!(res.expect("inbox for other succeeds with peek"), 0);

    // Recipient 'cos' cursor must NOT be advanced because caller is mu-lead
    assert_eq!(
        bus.get_cursor("cos").unwrap(),
        None,
        "cos cursor must not advance when inspected by another caller"
    );
}

#[test]
fn test_mail_inbox_cli_peek_leaves_cursor_unchanged() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "cos");
    guard.remove("HERDR_PANE_ID");
    let runs_dir = hermetic_temp_dir("peek-no-advance");
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    let bus = FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");

    let tr =
        NewTrigger::new("mail", "handoff", "lead", "thread-1", "test summary").with_to_name("cos");
    bus.record_trigger(&tr).expect("record trigger");

    let herdr = Herdr::new();
    let res = mail::handle_mail(&["inbox".to_string(), "--peek".to_string()], &herdr);
    assert_eq!(res.expect("inbox --peek succeeds"), 0);

    // cos cursor must remain None
    assert_eq!(
        bus.get_cursor("cos").unwrap(),
        None,
        "--peek must not advance cursor"
    );
}

#[test]
fn test_mail_inbox_cli_matching_caller_advances_cursor() {
    let guard = EnvGuard::new(&[
        "QUAQUA_TEST",
        "QUAQUA_OWNER",
        "HERDR_PANE_ID",
        "QUAQUA_RUNS_DIR",
    ]);
    guard.set("QUAQUA_TEST", "1");
    guard.set("QUAQUA_OWNER", "mu-lead");
    guard.remove("HERDR_PANE_ID");
    let runs_dir = hermetic_temp_dir("matching-advances");
    guard.set("QUAQUA_RUNS_DIR", runs_dir.to_str().unwrap());
    let bus = FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus");

    let tr1 =
        NewTrigger::new("mail", "handoff", "cos", "thread-1", "summary 1").with_to_name("mu-lead");
    let tr2 =
        NewTrigger::new("mail", "handoff", "cos", "thread-1", "summary 2").with_to_name("mu-lead");
    bus.record_trigger(&tr1).expect("record trigger 1");
    bus.record_trigger(&tr2).expect("record trigger 2");

    let herdr = Herdr::new();

    // First run: matching caller mu-lead advances cursor
    let res = mail::handle_mail(&["inbox".to_string()], &herdr);
    assert_eq!(res.expect("matching caller inbox succeeds"), 0);

    let cursor_after = bus.get_cursor("mu-lead").unwrap();
    assert_eq!(
        cursor_after,
        Some(2),
        "cursor must advance to max trigger id 2"
    );

    // Second run: no unread triggers, cursor remains 2
    let res2 = mail::handle_mail(&["inbox".to_string()], &herdr);
    assert_eq!(res2.expect("second run succeeds"), 0);
    assert_eq!(bus.get_cursor("mu-lead").unwrap(), Some(2));
}
