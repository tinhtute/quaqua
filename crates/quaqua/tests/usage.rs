//! Integration and unit tests for `quaqua usage`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURES: &str = "tests/fixtures/usage";
const UNTIL_MS: &str = "1792148400000"; // 2026-10-16T18:00:00+07:00

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_quaqua")
}

fn fixture_source_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURES)
}

fn build_test_db(path: &Path) {
    let sql = r#"
CREATE TABLE session_v2 (
    id TEXT PRIMARY KEY, directory TEXT, title TEXT,
    cost REAL, tokens_input INTEGER, tokens_output INTEGER, tokens_reasoning INTEGER,
    tokens_cache_read INTEGER, tokens_cache_write INTEGER, model TEXT,
    time_created INTEGER, time_updated INTEGER, idle_outcome TEXT
);
CREATE TABLE session_message (
    id TEXT, session_id TEXT, type TEXT, seq INTEGER,
    time_created INTEGER, time_updated INTEGER, data TEXT
);
INSERT INTO session_v2 VALUES ('ses_a', 'proj/.wt/tieude-alpha', 'alpha work', 0.5, 34000, 3300, 1360, 330000, 0, 'deepseek-v4.1-flash', 1791946800000, 1792141200000, 'succeeded');
INSERT INTO session_v2 VALUES ('ses_b', 'proj/.wt/tieude-beta', 'beta work', 0.2, 8000, 600, 150, 60000, 0, 'deepseek-v4.1-flash', 1792058400000, 1792116000000, 'failed');
INSERT INTO session_v2 VALUES ('ses_c', 'proj/.wt/tieude-gamma', 'no traffic', 0.0, 0, 0, 0, 0, 0, NULL, 0, 0, NULL);
INSERT INTO session_message VALUES (
    'm0', 'ses_a', 'assistant', 0, 1792116000000, 1792116000000,
    '{"time":{"created":1792116000000},"type":"assistant","content":[{"type":"tool","name":"shell","state":{"input":{"command":"cargo test --locked -p quaqua"}}}]}'
);
INSERT INTO session_message VALUES (
    'm1', 'ses_a', 'assistant', 1, 1792116300000, 1792116300000,
    '{"time":{"created":1792116300000},"type":"assistant","content":[{"type":"tool","name":"read","state":{"input":{"filePath":"crates/quaqua/src/lib.rs"}}}]}'
);
INSERT INTO session_message VALUES (
    'm2', 'ses_a', 'assistant', 2, 1792118700000, 1792118700000,
    '{"time":{"created":1792118700000},"type":"assistant","content":[{"type":"tool","name":"edit","state":{"input":{"filePath":"~/quaqua/runs/tieude-alpha.md"}}}]}'
);
INSERT INTO session_message VALUES (
    'm3', 'ses_b', 'assistant', 3, 1792058400000, 1792058400000,
    '{"time":{"created":1792058400000},"type":"assistant","content":[{"type":"tool","name":"shell","state":{"input":{"command":"git status"}}}]}'
);
INSERT INTO session_message VALUES (
    'm4', 'ses_b', 'assistant', 4, 1792059000000, 1792059000000,
    '{"time":{"created":1792059000000},"type":"assistant","content":[{"type":"tool","name":"read","state":{"input":{"filePath":"rules/_all/brief.md"}}}]}'
);
"#;

    let out = Command::new("sqlite3")
        .arg(path)
        .arg(sql)
        .output()
        .expect("failed to run sqlite3 to setup test db");
    assert!(out.status.success(), "sqlite3 setup failed: {:?}", out);
}

fn setup_fixture_env(dir: &Path) {
    let src = fixture_source_dir();
    fs::copy(src.join("request_logs.json"), dir.join("request_logs.json"))
        .expect("copy request_logs.json");
    fs::copy(src.join("meter.json"), dir.join("meter.json")).expect("copy meter.json");

    let runs_dir = dir.join("runs");
    fs::create_dir_all(&runs_dir).expect("create runs dir");
    fs::write(runs_dir.join("tieude-alpha.md"), "# alpha report\n").unwrap();
    fs::write(runs_dir.join("tieude-beta.md"), "# beta report\n").unwrap();
    fs::write(runs_dir.join("tieude-beta2.md"), "# beta2 report\n").unwrap();
    fs::write(runs_dir.join("suhuynh-alphar1.md"), "# review round 1\n").unwrap();

    build_test_db(&dir.join("opencode.db"));
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let unique = format!(
            "{}-{}-{}",
            prefix,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn test_text_report() {
    let tmp = TempDir::new("quaqua-usage-test");
    setup_fixture_env(&tmp.path);

    let out = Command::new(bin())
        .args([
            "usage",
            "--fixture-dir",
            &tmp.path.to_string_lossy(),
            "--local-version",
            "2.0.32",
            "--until",
            UNTIL_MS,
            "--days",
            "3",
        ])
        .output()
        .expect("spawn quaqua usage");

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("5 requests"),
        "expected 5 requests in: {stdout}"
    );
    assert!(stdout.contains("$0.7000"), "expected $0.7000 in: {stdout}");
    assert!(stdout.contains("14/10"), "expected 14/10 in: {stdout}");
    assert!(stdout.contains("16/10"), "expected 16/10 in: {stdout}");
    assert!(stdout.contains("[WARN]"), "expected [WARN] in: {stdout}");
    assert!(stdout.contains("9.9.9"), "expected 9.9.9 in: {stdout}");
    assert!(
        stdout.contains("tieude-alpha"),
        "expected tieude-alpha in: {stdout}"
    );
    assert!(
        stdout.contains("tieude-beta"),
        "expected tieude-beta in: {stdout}"
    );
    assert!(
        stdout.contains("respawn     tieude-beta → tieude-beta2"),
        "expected respawn flag in: {stdout}"
    );
    assert!(
        stdout.contains("killed      tieude-beta"),
        "expected killed flag in: {stdout}"
    );
    assert!(
        stdout.contains("idle>30m    tieude-alpha (40m)"),
        "expected idle>30m in: {stdout}"
    );
    assert!(
        stdout.contains("review      tieude-alpha (1 round(s))"),
        "expected review in: {stdout}"
    );
    assert!(
        stdout.contains("40% calls"),
        "expected 40% calls in: {stdout}"
    );
    assert!(stdout.contains("$0.3333"), "expected $0.3333 in: {stdout}");
    assert!(
        stdout.contains("rolling  7%"),
        "expected rolling 7% in: {stdout}"
    );
    assert!(
        stdout.contains("log $ in window: $0.0300"),
        "expected log $ in window in: {stdout}"
    );

    // Tripwire: never emit tokens
    assert!(
        !quaqua::usage::contains_token(&stdout),
        "tripwire: token pattern found in output!"
    );
}

#[test]
fn test_json_report() {
    let tmp = TempDir::new("quaqua-usage-json");
    setup_fixture_env(&tmp.path);

    let out = Command::new(bin())
        .args([
            "usage",
            "--fixture-dir",
            &tmp.path.to_string_lossy(),
            "--local-version",
            "2.0.32",
            "--until",
            UNTIL_MS,
            "--days",
            "3",
            "--json",
        ])
        .output()
        .expect("spawn quaqua usage --json");

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !quaqua::usage::contains_token(&stdout),
        "tripwire: token pattern found in json!"
    );

    let rep: serde_json::Value =
        serde_json::from_str(&stdout).expect("valid json report from quaqua usage");
    assert_eq!(rep["totals"]["requests"], 5);
    let cost = rep["totals"]["cost"].as_f64().unwrap();
    assert!((cost - 0.70).abs() < 1e-4);
    assert_eq!(rep["warn_other_machine"], true);
    assert_eq!(rep["local_version"], "2.0.32");

    let tasks = rep["tasks"].as_array().expect("tasks array");
    let alpha = tasks
        .iter()
        .find(|t| t["task"] == "tieude-alpha")
        .expect("tieude-alpha found");
    let beta = tasks
        .iter()
        .find(|t| t["task"] == "tieude-beta")
        .expect("tieude-beta found");

    let alpha_cost = alpha["cost"].as_f64().unwrap();
    let beta_cost = beta["cost"].as_f64().unwrap();
    assert!((alpha_cost - 0.50).abs() < 1e-4);
    assert_eq!(alpha["spend_src"], "session");
    assert!((beta_cost - 0.20).abs() < 1e-4);
    assert_eq!(beta["spend_src"], "session");
    assert_eq!(beta["outcome"], "killed(failed); report");

    let prod_cost = rep["split"]["product"]["cost"].as_f64().unwrap();
    let prod_calls_pct = rep["split"]["product"]["callsPct"].as_f64().unwrap();
    let proc_cost = rep["split"]["process"]["cost"].as_f64().unwrap();
    assert!((prod_cost - 0.3333).abs() < 1e-3);
    assert!((prod_calls_pct - 40.0).abs() < 0.1);
    assert!((proc_cost - 0.2667).abs() < 1e-3);
    let rolling_pct = rep["meter"]["rolling"]["percent"].as_i64().unwrap();
    let rolling_cost = rep["meter"]["rolling"]["log_cost"].as_f64().unwrap();
    assert_eq!(rolling_pct, 7);
    assert!((rolling_cost - 0.03).abs() < 1e-4);
}

#[test]
fn test_days_window() {
    let tmp = TempDir::new("quaqua-usage-days1");
    setup_fixture_env(&tmp.path);

    let out = Command::new(bin())
        .args([
            "usage",
            "--fixture-dir",
            &tmp.path.to_string_lossy(),
            "--local-version",
            "2.0.32",
            "--until",
            UNTIL_MS,
            "--days",
            "1",
        ])
        .output()
        .expect("spawn quaqua usage --days 1");

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("2 requests"),
        "expected 2 requests in: {stdout}"
    );
    assert!(
        stdout.contains("16/10       2    0.0500"),
        "expected day row $0.0500 in: {stdout}"
    );
    assert!(
        stdout.contains("$0.7000"),
        "expected whole-session task total $0.7000 in: {stdout}"
    );
}

#[test]
fn test_meter_unavailable_keeps_report() {
    let tmp = TempDir::new("quaqua-usage-metermiss");
    setup_fixture_env(&tmp.path);
    fs::write(tmp.path.join("meter.json"), "not json {{{").unwrap();

    let out_json = Command::new(bin())
        .args([
            "usage",
            "--fixture-dir",
            &tmp.path.to_string_lossy(),
            "--local-version",
            "2.0.32",
            "--until",
            UNTIL_MS,
            "--days",
            "3",
            "--json",
        ])
        .output()
        .expect("spawn quaqua usage --json with bad meter");

    assert_eq!(out_json.status.code(), Some(0));
    let rep: serde_json::Value =
        serde_json::from_slice(&out_json.stdout).expect("valid json report");
    let meter_unavail = rep["meterUnavailable"].as_str().unwrap();
    assert!(
        meter_unavail.contains("bad JSON"),
        "expected bad JSON in meterUnavailable: {meter_unavail}"
    );
    let cost = rep["totals"]["cost"].as_f64().unwrap();
    assert!((cost - 0.70).abs() < 1e-4);

    let out_text = Command::new(bin())
        .args([
            "usage",
            "--fixture-dir",
            &tmp.path.to_string_lossy(),
            "--local-version",
            "2.0.32",
            "--until",
            UNTIL_MS,
            "--days",
            "3",
        ])
        .output()
        .expect("spawn quaqua usage with bad meter");

    assert_eq!(out_text.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out_text.stdout);
    assert!(text.contains("$0.7000"), "expected $0.7000 in: {text}");
    assert!(
        text.contains("unavailable: bad JSON"),
        "expected unavailable: bad JSON in: {text}"
    );
}

#[test]
fn test_logs_missing_exits_nonzero_but_renders() {
    let tmp = TempDir::new("quaqua-usage-logmiss");
    setup_fixture_env(&tmp.path);
    fs::remove_file(tmp.path.join("request_logs.json")).unwrap();

    let out = Command::new(bin())
        .args([
            "usage",
            "--fixture-dir",
            &tmp.path.to_string_lossy(),
            "--local-version",
            "2.0.32",
            "--until",
            UNTIL_MS,
            "--days",
            "3",
        ])
        .output()
        .expect("spawn quaqua usage with missing request_logs");

    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit code 2 when request-logs are missing entirely"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[UNAVAILABLE] request-logs"),
        "expected [UNAVAILABLE] marker in: {stdout}"
    );
    assert!(
        stdout.contains("== Meter"),
        "expected meter section still rendered in: {stdout}"
    );
}

#[test]
fn test_retry_semantics() {
    let mut sleeps = Vec::new();
    let res = quaqua::usage::http_json_with_sleep(
        "http://127.0.0.1:9", // unreachable port
        "fake-token",
        "ua/test",
        5,
        |s| sleeps.push(s),
    );
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(err.reason.contains("network error"));
    // 5 attempts -> 4 sleeps: 1s, 2s, 4s, 4s (capped)
    assert_eq!(sleeps, vec![1, 2, 4, 4]);
}

#[test]
fn test_token_tripwire_and_matching() {
    assert!(quaqua::usage::contains_token(
        "my_token: st_81980e6d-2382-4800-a2d3-dd65d3442eed"
    ));
    assert!(quaqua::usage::contains_token(
        "rt_20cfc6a6-3c0b-4f6a-aeaf-ab9a83bfc9c2"
    ));
    assert!(!quaqua::usage::contains_token(
        "regular text without tokens"
    ));
    assert!(!quaqua::usage::contains_token("st_short")); // length < 10

    // Civil date round-trip
    let days = quaqua::usage::days_from_civil(2026, 10, 18);
    let (y, m, d) = quaqua::usage::civil_from_days(days);
    assert_eq!((y, m, d), (2026, 10, 18));
}

#[test]
fn test_usage_and_watchdog_share_spend_source() {
    let tmp = TempDir::new("quaqua-usage-parity");
    setup_fixture_env(&tmp.path);

    let out = Command::new(bin())
        .args([
            "usage",
            "--fixture-dir",
            &tmp.path.to_string_lossy(),
            "--local-version",
            "2.0.32",
            "--until",
            UNTIL_MS,
            "--days",
            "3",
            "--json",
        ])
        .output()
        .expect("spawn quaqua usage --json");

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rep: serde_json::Value =
        serde_json::from_str(&stdout).expect("valid json report from quaqua usage");

    let tasks = rep["tasks"].as_array().expect("tasks array");
    let alpha = tasks
        .iter()
        .find(|t| t["task"] == "tieude-alpha")
        .expect("tieude-alpha found");
    let beta = tasks
        .iter()
        .find(|t| t["task"] == "tieude-beta")
        .expect("tieude-beta found");

    let alpha_usage_cost = alpha["cost"].as_f64().unwrap();
    let beta_usage_cost = beta["cost"].as_f64().unwrap();

    // Watchdog spend path on the same fixture run
    let runs_dir = tmp.path.join("runs");
    let db_path = tmp.path.join("opencode.db");
    let alpha_watchdog = quaqua::worker::prompt::cost::read_worker_spend_with(
        "tieude-alpha",
        "ses_a",
        "proj/.wt/tieude-alpha",
        "opencode",
        None,
        Some(&db_path),
        Some(&runs_dir),
        false,
    );
    let beta_watchdog = quaqua::worker::prompt::cost::read_worker_spend_with(
        "tieude-beta",
        "ses_b",
        "proj/.wt/tieude-beta",
        "opencode",
        None,
        Some(&db_path),
        Some(&runs_dir),
        false,
    );

    let alpha_watchdog_cost = alpha_watchdog.usd.expect("alpha usd");
    let beta_watchdog_cost = beta_watchdog.usd.expect("beta usd");

    // Both paths MUST report the same number for the same run
    assert_eq!(
        alpha_usage_cost, alpha_watchdog_cost,
        "tieude-alpha spend mismatch: usage={alpha_usage_cost}, watchdog={alpha_watchdog_cost}"
    );
    assert_eq!(
        beta_usage_cost, beta_watchdog_cost,
        "tieude-beta spend mismatch: usage={beta_usage_cost}, watchdog={beta_watchdog_cost}"
    );
    // Fixture db values (0.50, 0.20) must differ from request-log sums (0.33, 0.07)
    assert_ne!(alpha_usage_cost, 0.33, "must not match request log sum");
    assert_ne!(beta_usage_cost, 0.07, "must not match request log sum");
    assert!((alpha_usage_cost - 0.50).abs() < 1e-4);
    assert!((beta_usage_cost - 0.20).abs() < 1e-4);
    assert_eq!(alpha["spend_src"], "session");
    assert_eq!(beta["spend_src"], "session");
}

#[test]
fn test_task_cost_whole_session_across_days_windows() {
    let tmp = TempDir::new("quaqua-usage-windows");
    setup_fixture_env(&tmp.path);

    // 1. Report for --days 3 (covers the whole session)
    let out3 = Command::new(bin())
        .args([
            "usage",
            "--fixture-dir",
            &tmp.path.to_string_lossy(),
            "--local-version",
            "2.0.32",
            "--until",
            UNTIL_MS,
            "--days",
            "3",
            "--json",
        ])
        .output()
        .expect("spawn quaqua usage --days 3");
    assert_eq!(out3.status.code(), Some(0));
    let rep3: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out3.stdout)).unwrap();
    let alpha3 = rep3["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task"] == "tieude-alpha")
        .unwrap();
    let beta3 = rep3["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task"] == "tieude-beta")
        .unwrap();

    // 2. Report for --days 1 (window cutoff is 16/10, after session time_created 14/10 and 15/10)
    let out1 = Command::new(bin())
        .args([
            "usage",
            "--fixture-dir",
            &tmp.path.to_string_lossy(),
            "--local-version",
            "2.0.32",
            "--until",
            UNTIL_MS,
            "--days",
            "1",
            "--json",
        ])
        .output()
        .expect("spawn quaqua usage --days 1");
    assert_eq!(out1.status.code(), Some(0));
    let rep1: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out1.stdout)).unwrap();
    let alpha1 = rep1["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task"] == "tieude-alpha")
        .unwrap();
    let beta1 = rep1["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task"] == "tieude-beta")
        .unwrap();

    // Verify task spend is WHOLE-SESSION spend regardless of --days window: NO FLIP!
    assert_eq!(alpha3["cost"].as_f64().unwrap(), 0.50);
    assert_eq!(alpha1["cost"].as_f64().unwrap(), 0.50);
    assert_eq!(alpha3["spend_src"], "session");
    assert_eq!(alpha1["spend_src"], "session");

    assert_eq!(beta3["cost"].as_f64().unwrap(), 0.20);
    assert_eq!(beta1["cost"].as_f64().unwrap(), 0.20);
    assert_eq!(beta3["spend_src"], "session");
    assert_eq!(beta1["spend_src"], "session");

    // Meanwhile, day rows stay request-log windowed
    let days1 = rep1["days_table"].as_array().unwrap();
    assert_eq!(days1.len(), 1);
    assert_eq!(days1[0]["date"], "16/10");
    assert_eq!(days1[0]["requests"], 2);
    assert_eq!(days1[0]["cost"], 0.05);
}
