//! Tests for `quaqua source github-ci poll` (spec §9, R7).
//!
//! Every case drives the built binary with a fake `gh` on PATH and reads the
//! farm bus afterwards: what the caller observes is the trigger rows, the
//! `source_state` watermark, and the `gh` invocation itself.

use quaqua::bus::{now_ms, FarmBus};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FOURTEEN_DAYS_MS: u64 = 14 * 24 * 60 * 60 * 1000;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "quaqua-github-ci-{tag}-{}-{id}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create hermetic temp dir");
    dir
}

/// Opens the bus at `runs_dir` and returns it (schema is created on open).
fn open_bus(runs_dir: &Path) -> FarmBus {
    FarmBus::open(&runs_dir.join("quaqua.db")).expect("open bus")
}

fn seed_thread(runs_dir: &Path, thread_id: &str, owner: &str, consumer: &str) {
    let bus = open_bus(runs_dir);
    bus.open_thread_and_subscribe(thread_id, owner, consumer, now_ms() + FOURTEEN_DAYS_MS)
        .expect("seed thread");
}

fn seed_position(runs_dir: &Path, value: &str) {
    let bus = open_bus(runs_dir);
    bus.conn_for_test()
        .execute(
            "INSERT INTO source_state (source, value) VALUES ('github-ci', ?1)",
            rusqlite::params![value],
        )
        .expect("seed source_state");
}

fn position(runs_dir: &Path) -> Option<String> {
    let bus = open_bus(runs_dir);
    let value = bus
        .conn_for_test()
        .query_row(
            "SELECT value FROM source_state WHERE source = 'github-ci'",
            [],
            |row| row.get(0),
        )
        .ok();
    value
}

/// (source, kind, actor, thread_id, summary, ref, dedupe_key) per trigger row.
type TriggerRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

fn triggers(runs_dir: &Path) -> Vec<TriggerRow> {
    let bus = open_bus(runs_dir);
    let conn = bus.conn_for_test();
    let mut stmt = conn
        .prepare(
            "SELECT source, kind, actor, thread_id, summary, ref, dedupe_key \
             FROM triggers WHERE source = 'github-ci' ORDER BY id ASC",
        )
        .expect("prepare trigger query");
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .expect("query triggers");
    rows.map(|r| r.expect("trigger row")).collect()
}

/// Writes an executable `gh` stub into `dir` that records its argv and prints
/// the JSON at `$GH_FIXTURE`.
fn fake_gh(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("gh");
    fs::write(
        &path,
        "#!/bin/sh\n\
         if [ -n \"$GH_ARGV_FILE\" ]; then printf '%s\\n' \"$@\" > \"$GH_ARGV_FILE\"; fi\n\
         cat \"$GH_FIXTURE\"\n",
    )
    .expect("write fake gh");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod fake gh");
    path
}

/// Writes an executable `gh` stub that always fails, like a network outage.
fn failing_gh(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("gh");
    fs::write(&path, "#!/bin/sh\nexit 1\n").expect("write failing gh");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod failing gh");
    path
}

fn poll(runs_dir: &Path, bin_dir: &Path, fixture: &Path, argv_file: &Path) -> Output {
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .args(["source", "github-ci", "poll"])
        .env("QUAQUA_RUNS_DIR", runs_dir)
        .env("QUAQUA_TEST", "1")
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_ROOT")
        .env("PATH", path_env)
        .env("GH_FIXTURE", fixture)
        .env("GH_ARGV_FILE", argv_file)
        .output()
        .expect("spawn quaqua source github-ci poll")
}

fn write_fixture(dir: &Path, json: &str) -> PathBuf {
    let path = dir.join("runs.json");
    fs::write(&path, json).expect("write gh fixture");
    path
}

#[test]
fn failed_run_on_known_branch_emits_one_trigger() {
    let dir = temp_dir("failed");
    let runs_dir = dir.join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    seed_thread(&runs_dir, "quaqua/alpha", "tieude", "tieude");

    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fake_gh(&bin_dir);
    let fixture = write_fixture(
        &dir,
        r#"[{"databaseId":900,"headBranch":"quaqua/alpha","conclusion":"failure",
            "url":"https://github.com/tinhtute/quaqua/actions/runs/900","workflowName":"test"}]"#,
    );
    let argv_file = dir.join("argv.txt");

    let out = poll(&runs_dir, &bin_dir, &fixture, &argv_file);
    assert!(
        out.status.success(),
        "poll must exit 0: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let rows = triggers(&runs_dir);
    assert_eq!(
        rows.len(),
        1,
        "one failed run must record one trigger: {rows:?}"
    );
    let (source, kind, actor, thread_id, summary, ref_val, dedupe_key) = &rows[0];
    assert_eq!(source, "github-ci");
    assert_eq!(kind, "ci_failed");
    assert_eq!(actor, "github");
    assert_eq!(thread_id, "quaqua/alpha");
    assert_eq!(summary, "test failed on quaqua/alpha");
    assert_eq!(
        ref_val.as_deref(),
        Some("https://github.com/tinhtute/quaqua/actions/runs/900")
    );
    assert_eq!(dedupe_key.as_deref(), Some("gh-run:900"));

    // The thread's subscriber sees it: that is how the owner is reached.
    let bus = open_bus(&runs_dir);
    let unread = bus.load_unread_triggers("tieude", 0).expect("load unread");
    assert_eq!(unread.len(), 1, "subscriber must see the ci_failed trigger");
    assert_eq!(unread[0].kind, "ci_failed");
    assert_eq!(unread[0].thread_id, "quaqua/alpha");

    assert_eq!(position(&runs_dir).as_deref(), Some("900"));

    // The gh contract: the run list of the engine repo, 50 entries, these fields.
    let argv = fs::read_to_string(&argv_file).expect("fake gh recorded argv");
    let args: Vec<&str> = argv.lines().collect();
    assert_eq!(args[0], "run");
    assert_eq!(args[1], "list");
    assert!(args.contains(&"--json"), "argv: {argv}");
    assert!(
        args.contains(&"databaseId,headBranch,conclusion,url,workflowName"),
        "argv: {argv}"
    );
    assert!(
        args.contains(&"--limit") && args.contains(&"50"),
        "argv: {argv}"
    );
}

#[test]
fn success_unknown_branch_and_foreign_branch_are_ignored() {
    let dir = temp_dir("filtered");
    let runs_dir = dir.join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    seed_thread(&runs_dir, "quaqua/alpha", "tieude", "tieude");
    seed_thread(&runs_dir, "main", "tieude", "tieude");

    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fake_gh(&bin_dir);
    let fixture = write_fixture(
        &dir,
        r#"[{"databaseId":901,"headBranch":"quaqua/alpha","conclusion":"success",
             "url":"https://example.test/901","workflowName":"test"},
            {"databaseId":902,"headBranch":"quaqua/unknown","conclusion":"failure",
             "url":"https://example.test/902","workflowName":"test"},
            {"databaseId":903,"headBranch":"main","conclusion":"failure",
             "url":"https://example.test/903","workflowName":"test"},
            {"databaseId":904,"headBranch":"quaqua/alpha","conclusion":null,
             "url":"https://example.test/904","workflowName":"test"}]"#,
    );
    let argv_file = dir.join("argv.txt");

    let out = poll(&runs_dir, &bin_dir, &fixture, &argv_file);
    assert!(out.status.success(), "poll must exit 0");

    assert!(
        triggers(&runs_dir).is_empty(),
        "only failures on quaqua/* with a thread may be recorded: {:?}",
        triggers(&runs_dir)
    );
    assert_eq!(
        position(&runs_dir).as_deref(),
        Some("904"),
        "the watermark is the largest run id seen, emitted or not"
    );
}

#[test]
fn repeat_poll_records_each_run_once() {
    let dir = temp_dir("dedupe");
    let runs_dir = dir.join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    seed_thread(&runs_dir, "quaqua/alpha", "tieude", "tieude");

    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fake_gh(&bin_dir);
    let fixture = write_fixture(
        &dir,
        r#"[{"databaseId":900,"headBranch":"quaqua/alpha","conclusion":"failure",
             "url":"https://example.test/900","workflowName":"test"}]"#,
    );
    let argv_file = dir.join("argv.txt");

    for attempt in 1..=2 {
        let out = poll(&runs_dir, &bin_dir, &fixture, &argv_file);
        assert!(out.status.success(), "poll #{attempt} must exit 0");
    }

    let rows = triggers(&runs_dir);
    assert_eq!(
        rows.len(),
        1,
        "a repeated poll must not duplicate the trigger: {rows:?}"
    );
    assert_eq!(rows[0].6.as_deref(), Some("gh-run:900"));
}

#[test]
fn failed_gh_leaves_position_unchanged() {
    let dir = temp_dir("network");
    let runs_dir = dir.join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    seed_thread(&runs_dir, "quaqua/alpha", "tieude", "tieude");
    seed_position(&runs_dir, "100");

    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    failing_gh(&bin_dir);
    let fixture = write_fixture(&dir, "[]");
    let argv_file = dir.join("argv.txt");

    let out = poll(&runs_dir, &bin_dir, &fixture, &argv_file);
    assert!(
        out.status.success(),
        "a failed read is not an error: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        triggers(&runs_dir).is_empty(),
        "a failed read must record nothing"
    );
    assert_eq!(
        position(&runs_dir).as_deref(),
        Some("100"),
        "a failed read must not advance the watermark"
    );
}
