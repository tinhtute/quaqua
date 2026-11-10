//! Tests for `quaqua source github-ci poll` (spec §9, R7).
//!
//! Every case drives the built binary with a fake `gh` on PATH and reads the
//! farm bus afterwards: what the caller observes is the trigger rows, the
//! `source_state` watermark, and the `gh` invocation itself.

use quaqua::bus::{now_ms, FarmBus, NewTrigger};
use quaqua::herdr::Herdr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const FOURTEEN_DAYS_MS: u64 = 14 * 24 * 60 * 60 * 1000;
/// The repo every poll passes to `gh`; the flag, not the cwd, selects it.
const REPO: &str = "tinhtute/quaqua";

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

/// One `source_state` row, by key.
fn state_value(runs_dir: &Path, key: &str) -> Option<String> {
    let bus = open_bus(runs_dir);
    let value = bus
        .conn_for_test()
        .query_row(
            "SELECT value FROM source_state WHERE source = ?1",
            [key],
            |row| row.get(0),
        )
        .ok();
    value
}

fn position(runs_dir: &Path) -> Option<String> {
    state_value(runs_dir, "github-ci")
}

/// Highest trigger id already nudged for `consumer`, as the wake path records it.
fn wake_watermark(runs_dir: &Path, consumer: &str) -> u64 {
    state_value(runs_dir, &format!("wake:{consumer}"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
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

/// Writes an executable `herdr` stub into `dir` and returns its path.
fn herdr_stub(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("herdr");
    fs::write(&path, body).expect("write fake herdr");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod fake herdr");
    path
}

/// A `herdr` that reports nothing: `agent get` does not parse, so no nudge.
fn quiet_herdr(dir: &Path) -> PathBuf {
    herdr_stub(dir, "#!/bin/sh\nexit 0\n")
}

/// A `herdr` whose agent is idle and which logs every invocation to `$HERDR_LOG`.
fn nudging_herdr(dir: &Path) -> PathBuf {
    herdr_stub(
        dir,
        "#!/bin/sh\n\
         if [ -n \"$HERDR_LOG\" ]; then printf '%s\\n' \"$*\" >> \"$HERDR_LOG\"; fi\n\
         if [ \"$1 $2\" = \"agent get\" ]; then\n\
           echo '{\"result\":{\"agent\":{\"agent_status\":\"idle\",\"pane_id\":\"pane-7\"}}}'\n\
         fi\n\
         exit 0\n",
    )
}

/// A `herdr` whose `agent get <name>` reports the status stored in
/// `<status_dir>/<name>` (default `idle`), names that agent's pane `pane-<name>`,
/// and appends every invocation to `log`.
///
/// The status is read on every call, so a test can flip one agent between polls;
/// the log path is baked in because a test may also drive this stub in-process.
fn status_herdr(dir: &Path, status_dir: &Path, log: &Path) -> PathBuf {
    herdr_stub(
        dir,
        &format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> '{log}'\n\
             if [ \"$1 $2\" = \"agent get\" ]; then\n\
             status=$(cat '{status_dir}/'\"$3\" 2>/dev/null || echo idle)\n\
             printf '{{\"result\":{{\"agent\":{{\"agent_status\":\"%s\",\"pane_id\":\"pane-%s\"}}}}}}\\n' \"$status\" \"$3\"\n\
             fi\n\
             exit 0\n",
            log = log.display(),
            status_dir = status_dir.display(),
        ),
    )
}

/// Sets the status `status_herdr` reports for `agent`.
fn set_status(status_dir: &Path, agent: &str, status: &str) {
    fs::write(status_dir.join(agent), status).expect("write agent status");
}

/// One nudge is one `pane send-text` invocation.
fn nudge_count(log: &Path) -> usize {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("pane send-text"))
        .count()
}

fn poll(
    runs_dir: &Path,
    bin_dir: &Path,
    herdr_bin: &Path,
    herdr_log: &Path,
    fixture: &Path,
    argv_file: &Path,
) -> Output {
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .args(["source", "github-ci", "poll", "--repo", REPO])
        .env("QUAQUA_RUNS_DIR", runs_dir)
        .env("QUAQUA_TEST", "1")
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_ROOT")
        .env("PATH", path_env)
        .env("HERDR_BIN_PATH", herdr_bin)
        .env("HERDR_LOG", herdr_log)
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
    let herdr_bin = quiet_herdr(&dir);
    let herdr_log = dir.join("herdr.log");

    let out = poll(
        &runs_dir, &bin_dir, &herdr_bin, &herdr_log, &fixture, &argv_file,
    );
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

    // The gh contract: the named repo, not the cwd, 50 entries, these fields.
    let argv = fs::read_to_string(&argv_file).expect("fake gh recorded argv");
    let args: Vec<&str> = argv.lines().collect();
    assert_eq!(args[0], "run");
    assert_eq!(args[1], "list");
    let repo_at = args
        .iter()
        .position(|a| *a == "--repo")
        .unwrap_or_else(|| panic!("gh must be told the repo explicitly, argv: {argv}"));
    assert_eq!(
        args.get(repo_at + 1).copied(),
        Some(REPO),
        "gh must poll the requested repo, argv: {argv}"
    );
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
    let herdr_bin = quiet_herdr(&dir);
    let herdr_log = dir.join("herdr.log");

    let out = poll(
        &runs_dir, &bin_dir, &herdr_bin, &herdr_log, &fixture, &argv_file,
    );
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
    let herdr_bin = quiet_herdr(&dir);
    let herdr_log = dir.join("herdr.log");

    for attempt in 1..=2 {
        let out = poll(
            &runs_dir, &bin_dir, &herdr_bin, &herdr_log, &fixture, &argv_file,
        );
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
    let herdr_bin = quiet_herdr(&dir);
    let herdr_log = dir.join("herdr.log");

    let out = poll(
        &runs_dir, &bin_dir, &herdr_bin, &herdr_log, &fixture, &argv_file,
    );
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

#[test]
fn missing_gh_exits_one_with_a_clear_error() {
    let dir = temp_dir("no-gh");
    let runs_dir = dir.join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    seed_thread(&runs_dir, "quaqua/alpha", "tieude", "tieude");

    // A PATH with no `gh` at all: the binary is invoked by absolute path, so
    // only the source's own lookup is affected.
    let empty_bin = dir.join("empty-bin");
    fs::create_dir_all(&empty_bin).unwrap();
    let herdr_bin = quiet_herdr(&dir);

    let out = Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .args(["source", "github-ci", "poll", "--repo", REPO])
        .env("QUAQUA_RUNS_DIR", &runs_dir)
        .env("QUAQUA_TEST", "1")
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_ROOT")
        .env("PATH", &empty_bin)
        .env("HERDR_BIN_PATH", &herdr_bin)
        .output()
        .expect("spawn quaqua source github-ci poll");

    assert_eq!(
        out.status.code(),
        Some(1),
        "a missing gh is a permanent misconfiguration, not a retryable read"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("gh"), "error must name gh: {stderr}");
    assert!(
        stderr.contains("PATH"),
        "error must point at PATH: {stderr}"
    );
    assert!(
        triggers(&runs_dir).is_empty(),
        "a missing gh must record nothing"
    );
    assert_eq!(
        position(&runs_dir),
        None,
        "a missing gh must not write a position"
    );
}

#[test]
fn new_trigger_wakes_each_live_subscriber_once() {
    let dir = temp_dir("wake");
    let runs_dir = dir.join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    seed_thread(&runs_dir, "quaqua/alpha", "tieude", "tieude");
    seed_thread(&runs_dir, "quaqua/alpha", "tieude", "reviewer");

    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fake_gh(&bin_dir);
    let herdr_bin = nudging_herdr(&dir);
    let herdr_log = dir.join("herdr.log");
    let fixture = write_fixture(
        &dir,
        r#"[{"databaseId":900,"headBranch":"quaqua/alpha","conclusion":"failure",
             "url":"https://example.test/900","workflowName":"test"}]"#,
    );
    let argv_file = dir.join("argv.txt");

    let out = poll(
        &runs_dir, &bin_dir, &herdr_bin, &herdr_log, &fixture, &argv_file,
    );
    assert!(
        out.status.success(),
        "poll must exit 0: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = fs::read_to_string(&herdr_log).unwrap_or_default();
    assert_eq!(
        nudge_count(&herdr_log),
        2,
        "each live subscriber must be nudged once, log:\n{log}"
    );
    assert!(
        log.lines()
            .filter(|l| l.starts_with("pane send-text"))
            .all(|l| l.contains("ci_failed")),
        "the nudge must name the trigger kind, log:\n{log}"
    );

    // A duplicate run records nothing, so it wakes nobody.
    let out = poll(
        &runs_dir, &bin_dir, &herdr_bin, &herdr_log, &fixture, &argv_file,
    );
    assert!(out.status.success(), "repeat poll must exit 0");
    assert_eq!(
        nudge_count(&herdr_log),
        2,
        "a duplicate run must not nudge again, log:\n{}",
        fs::read_to_string(&herdr_log).unwrap_or_default()
    );
}

/// A duplicate run records nothing, so the poll must not wake anybody — not even a
/// subscriber the wake watermark does not cover.
///
/// The watermark alone would hide a poll that woke on every run: after the first
/// nudge it sits at the CI trigger's id, so a second wake for the same batch is a
/// no-op whatever the poll does. The direct trigger below moves it past that id,
/// and `late` (busy when the CI trigger landed, idle now) is still behind it, so a
/// poll that woke on a duplicate run would reach `late` and be caught here.
#[test]
fn duplicate_poll_wakes_nobody_even_after_the_watermark_moves() {
    let dir = temp_dir("wake-duplicate");
    let runs_dir = dir.join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    seed_thread(&runs_dir, "quaqua/alpha", "tieude", "tieude");
    seed_thread(&runs_dir, "quaqua/alpha", "tieude", "late");

    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fake_gh(&bin_dir);
    let status_dir = dir.join("status");
    fs::create_dir_all(&status_dir).unwrap();
    set_status(&status_dir, "late", "working");
    let herdr_log = dir.join("herdr.log");
    let herdr_bin = status_herdr(&dir, &status_dir, &herdr_log);
    let fixture = write_fixture(
        &dir,
        r#"[{"databaseId":900,"headBranch":"quaqua/alpha","conclusion":"failure",
             "url":"https://example.test/900","workflowName":"test"}]"#,
    );
    let argv_file = dir.join("argv.txt");

    let out = poll(
        &runs_dir, &bin_dir, &herdr_bin, &herdr_log, &fixture, &argv_file,
    );
    assert!(
        out.status.success(),
        "poll must exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        nudge_count(&herdr_log),
        1,
        "only the idle subscriber is nudged, log:\n{}",
        fs::read_to_string(&herdr_log).unwrap_or_default()
    );
    let after_first = wake_watermark(&runs_dir, "tieude");
    assert!(
        after_first > 0,
        "the first nudge must claim the wake watermark"
    );

    // An unrelated direct trigger for the same consumer, announced like any other
    // wake: it moves the watermark past the CI trigger.
    let bus = open_bus(&runs_dir);
    bus.record_trigger(
        &NewTrigger::new("mail", "handoff", "cos", "agent:tieude", "direct handoff")
            .with_to_name("tieude"),
    )
    .expect("record direct trigger");
    let woke = quaqua::bus::events::wake_idle_consumer(
        &bus,
        "tieude",
        &Herdr {
            bin: herdr_bin.display().to_string(),
        },
    )
    .expect("wake the direct trigger");
    assert!(woke, "the direct trigger must nudge the idle consumer");
    assert_eq!(
        nudge_count(&herdr_log),
        2,
        "the direct trigger is a new batch, log:\n{}",
        fs::read_to_string(&herdr_log).unwrap_or_default()
    );
    assert!(
        wake_watermark(&runs_dir, "tieude") > after_first,
        "the direct trigger must move the watermark"
    );
    drop(bus);

    // `late` is idle now: if the duplicate poll woke anyone, this is who it reaches.
    set_status(&status_dir, "late", "idle");
    let out = poll(
        &runs_dir, &bin_dir, &herdr_bin, &herdr_log, &fixture, &argv_file,
    );
    assert!(out.status.success(), "repeat poll must exit 0");
    let log = fs::read_to_string(&herdr_log).unwrap_or_default();
    assert_eq!(
        nudge_count(&herdr_log),
        2,
        "a duplicate run records nothing, so it wakes nobody, log:\n{log}"
    );
    assert!(
        !log.contains("pane send-text pane-late"),
        "the duplicate poll must not reach the subscriber the watermark misses, log:\n{log}"
    );
    assert_eq!(
        triggers(&runs_dir).len(),
        1,
        "the duplicate run must not record a second trigger"
    );
}

/// The CI path routes through `wake_idle_consumer`, so it inherits both of its
/// guards: the CoS pane is never nudged, and an agent that is not idle/done is not
/// nudged whatever the trigger says.
#[test]
fn ci_poll_never_nudges_cos_or_a_busy_subscriber() {
    let dir = temp_dir("wake-guards");
    let runs_dir = dir.join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    seed_thread(&runs_dir, "quaqua/alpha", "tieude", "cos");
    seed_thread(&runs_dir, "quaqua/alpha", "tieude", "busy");

    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fake_gh(&bin_dir);
    let status_dir = dir.join("status");
    fs::create_dir_all(&status_dir).unwrap();
    // `cos` would be nudged if only its live status decided, `busy` if only the
    // consumer name did: each guard has to hold on its own.
    set_status(&status_dir, "cos", "idle");
    set_status(&status_dir, "busy", "working");
    let herdr_log = dir.join("herdr.log");
    let herdr_bin = status_herdr(&dir, &status_dir, &herdr_log);
    let fixture = write_fixture(
        &dir,
        r#"[{"databaseId":900,"headBranch":"quaqua/alpha","conclusion":"failure",
             "url":"https://example.test/900","workflowName":"test"}]"#,
    );
    let argv_file = dir.join("argv.txt");

    let out = poll(
        &runs_dir, &bin_dir, &herdr_bin, &herdr_log, &fixture, &argv_file,
    );
    assert!(
        out.status.success(),
        "poll must exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        triggers(&runs_dir).len(),
        1,
        "the failure must be recorded, so a wake was attempted"
    );

    let log = fs::read_to_string(&herdr_log).unwrap_or_default();
    assert_eq!(
        nudge_count(&herdr_log),
        0,
        "neither the CoS pane nor a working agent may be nudged, log:\n{log}"
    );
    assert!(
        log.contains("agent get busy"),
        "the CI path must consult the busy subscriber's live status, log:\n{log}"
    );
}

/// An expired lease is skipped: a new CI trigger on that thread must not wake the
/// consumer, even though the consumer has unread work that a nudge would carry.
#[test]
fn ci_poll_skips_an_expired_subscription() {
    let dir = temp_dir("wake-expired");
    let runs_dir = dir.join("runs");
    fs::create_dir_all(&runs_dir).unwrap();
    let expired_at = now_ms() - 60_000;
    {
        let bus = open_bus(&runs_dir);
        bus.open_thread_and_subscribe("quaqua/alpha", "tieude", "stale", expired_at)
            .expect("seed expired subscription");
        // Unread work the consumer already has: a nudge is possible, so the expired
        // lease on this thread is the only reason it stays silent.
        bus.record_trigger(
            &NewTrigger::new("mail", "handoff", "cos", "agent:stale", "direct handoff")
                .with_to_name("stale"),
        )
        .expect("record direct trigger");
    }

    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fake_gh(&bin_dir);
    let status_dir = dir.join("status");
    fs::create_dir_all(&status_dir).unwrap();
    let herdr_log = dir.join("herdr.log");
    let herdr_bin = status_herdr(&dir, &status_dir, &herdr_log);
    let fixture = write_fixture(
        &dir,
        r#"[{"databaseId":900,"headBranch":"quaqua/alpha","conclusion":"failure",
             "url":"https://example.test/900","workflowName":"test"}]"#,
    );
    let argv_file = dir.join("argv.txt");

    let out = poll(
        &runs_dir, &bin_dir, &herdr_bin, &herdr_log, &fixture, &argv_file,
    );
    assert!(
        out.status.success(),
        "poll must exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        triggers(&runs_dir).len(),
        1,
        "the CI failure is recorded for the thread regardless of the lease"
    );
    assert_eq!(
        nudge_count(&herdr_log),
        0,
        "an expired subscription must not be woken, log:\n{}",
        fs::read_to_string(&herdr_log).unwrap_or_default()
    );

    let bus = open_bus(&runs_dir);
    assert_eq!(
        bus.get_subscriptions("quaqua/alpha")
            .expect("subscriptions"),
        vec![("stale".to_string(), expired_at)],
        "a new trigger must not revive an expired lease"
    );
}
