//! Blocking playbook gap check acceptance tests (§ S5 check, § S8 exit).
//!
//! Acceptance cases:
//! 1. the four step line shapes are parsed (unit test in `playbook_check`)
//! 2. missing section / step / reason → `playbook_gap` + exit 6 (builder)
//! 3. `skip: unsupported on <harness>` is a valid skip
//! 4. a report without `.playbook` behaves as today
//! 5. `--resume` accepts the report once the block is added
//!
//! Verify command: `cargo test --locked -p quaqua playbook_check`

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use quaqua::engine::journal::Journal;
use quaqua::engine::journal_sqlite::SqliteJournal;
use quaqua::git::Git;
use quaqua::herdr::Herdr;
use quaqua::worker::prompt::prompt_worker;

/// Helper RAII temporary directory for test isolation.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("quaqua-s5-{tag}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn sub(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_file(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `prompt_worker` reads process-global env, so the tests in this binary run
/// one at a time.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct EnvGuard(Vec<(&'static str, Option<String>)>);

impl EnvGuard {
    fn set(pairs: &[(&'static str, String)]) -> Self {
        let mut saved = Vec::new();
        for (key, val) in pairs {
            saved.push((*key, std::env::var(key).ok()));
            std::env::set_var(key, val);
        }
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

const BRIEF: &str = "\
Goal: make the thing work
Hard constraints: none
Current design: the approach as it stands
Unknowns: how to check it
Evidence: command + result
Time budget: 5 min
";

/// A three-step playbook with no cited leaves, so the fixture index needs no
/// `principles/` file.
fn fixture_index(sandbox: &TempDir) -> (PathBuf, PathBuf) {
    let pb_dir = sandbox.sub("playbooks");
    let pr_dir = sandbox.sub("principles");
    sandbox.write_file(
        "playbooks/feature.md",
        "\
---
name: feature
when: New behavior from a named data shape.
---

## Orchestrator

1. Define the interfaces before implementation.

## Worker

1. Clarify the domain model.
2. Implement the feature.
3. Write the check.
",
    );
    (pb_dir, pr_dir)
}

/// `agent prompt` records the body and copies `$STUB_REPORT` into the run
/// report, so acceptance sees the exact report a test wants to check.
fn stub_herdr(dir: &Path) -> PathBuf {
    let path = dir.join("herdr-stub");
    fs::write(
        &path,
        r#"#!/bin/sh
echo "herdr $*" >> "$CALLS"
case "${1:-} ${2:-}" in
  "agent get")
    printf '{"result":{"agent":{"agent_status":"working","cwd":"%s"}}}\n' "$STUB_CWD"
    exit 0 ;;
  "agent prompt")
    printf '%s' "$4" > "$STUB_CWD/prompt-$3.txt"
    cat "$STUB_REPORT" > "$STUB_RUNS/$3.md"
    printf '{"result":{"agent":{"agent_status":"working"}}}\n'
    exit 0 ;;
  "agent wait")
    printf '{"result":{"agent":{"agent_status":"working"}}}\n'
    exit 0 ;;
esac
printf '{"result":{}}\n'
exit 0
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// One sandbox with every env `prompt_worker` needs, plus the stub. The caller
/// sets the report fixture at `sandbox.0/report.md` before prompting.
struct Harness {
    _sandbox: TempDir,
    runs: PathBuf,
    report: PathBuf,
    herdr: Herdr,
    _env: EnvGuard,
}

impl Harness {
    fn new(tag: &str) -> Self {
        let sandbox = TempDir::new(tag);
        let (pb_dir, pr_dir) = fixture_index(&sandbox);
        let runs = sandbox.sub("runs");
        let calls = sandbox.0.join("calls");
        let clock = sandbox.0.join("clock");
        fs::write(&clock, "1700000000\n").unwrap();
        let advance = sandbox.0.join("advance-clock");
        fs::write(
            &advance,
            "#!/bin/sh\nn=$(cat \"$PROMPT_WORKER_CLOCK_FILE\")\nprintf '%s\\n' \"$(( n + ${1:-1} ))\" > \"$PROMPT_WORKER_CLOCK_FILE\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&advance, fs::Permissions::from_mode(0o755)).unwrap();
        let stub = stub_herdr(&sandbox.0);
        let report = sandbox.0.join("report.md");

        let env = EnvGuard::set(&[
            ("QUAQUA_RUNS_DIR", runs.display().to_string()),
            ("QUAQUA_HOME", sandbox.0.join("ws").display().to_string()),
            ("QUAQUA_PLAYBOOKS_DIR", pb_dir.display().to_string()),
            ("QUAQUA_PRINCIPLES_DIR", pr_dir.display().to_string()),
            ("GRID_SKIP", "1".to_string()),
            ("QUAQUA_TEST", "1".to_string()),
            ("CALLS", calls.display().to_string()),
            ("STUB_RUNS", runs.display().to_string()),
            ("STUB_CWD", sandbox.0.display().to_string()),
            ("STUB_REPORT", report.display().to_string()),
            ("HERDR_BIN_PATH", stub.display().to_string()),
            ("PROMPT_WORKER_CLOCK_FILE", clock.display().to_string()),
            ("PROMPT_WORKER_SLEEP_CMD", advance.display().to_string()),
            ("PROMPT_WORKER_POLL_SEC", "1".to_string()),
            ("PROMPT_WORKER_MAX_SEC", "60".to_string()),
            ("PROMPT_WORKER_HEARTBEAT_SEC", "3600".to_string()),
        ]);

        Harness {
            _sandbox: sandbox,
            runs,
            report,
            herdr: Herdr {
                bin: stub.display().to_string(),
            },
            _env: env,
        }
    }

    fn set_report(&self, content: &str) {
        fs::write(&self.report, content).unwrap();
    }

    fn prompt(&self, name: &str, brief: Option<&str>, resume: bool) -> Result<(), (i32, String)> {
        let git = Git::new();
        prompt_worker(name, brief, resume, None, &git, &self.herdr)
    }

    fn gap_events(&self, name: &str) -> Vec<serde_json::Value> {
        let journal = match SqliteJournal::open(&self.runs.join("quaqua.db")) {
            Ok(j) => j,
            Err(_) => return Vec::new(),
        };
        journal
            .load_events(&format!("worker:{name}"), 1)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.kind == "playbook_gap")
            .map(|e| serde_json::from_str(&e.payload_json).unwrap())
            .collect()
    }

    fn pin(&self, name: &str) -> PathBuf {
        self.runs.join(format!(".{name}.playbook"))
    }
}

fn brief_with_playbook() -> String {
    format!("{BRIEF}Playbook: feature\nContext: tests/prompt_playbook_check.rs\n")
}

/// Line 2: the report has no `## Playbook` section at all — every pinned id is
/// missing, one event lands, and the builder's report is rejected with exit 6.
#[test]
fn playbook_check_missing_section_rejects_the_report() {
    let _lock = env_lock();
    let h = Harness::new("section");
    h.set_report("# Report\n\n## Changes\n\ndid things\n\n## Lessons\nnone\n");
    assert_eq!(
        fs::read_to_string(h.runs.join(".sude-s5a.playbook")).ok(),
        None,
        "no pin before the prompt"
    );

    let res = h.prompt("sude-s5a", Some(&brief_with_playbook()), false);

    assert_eq!(
        res,
        Err((6, String::new())),
        "a gap rejects the report like a malformed one"
    );
    assert!(
        !h.runs.join(".sude-s5a.read").exists(),
        "a rejected report is not accepted"
    );
    assert_eq!(
        fs::read_to_string(h.pin("sude-s5a")).unwrap(),
        "name=feature\nids=1,2,3\n",
        "the pin written by worker prompt is what the check reads"
    );
    let events = h.gap_events("sude-s5a");
    assert_eq!(events.len(), 1, "one playbook_gap event: {events:?}");
    assert_eq!(events[0]["name"], "feature");
    assert_eq!(events[0]["missing"], serde_json::json!([1, 2, 3]));
    assert_eq!(events[0]["unreasoned"], serde_json::json!([]));
}

/// Line 2: a step that is absent and a `skip:` without a reason are reported
/// as separate facts, and the report is rejected.
#[test]
fn playbook_check_missing_step_and_unreasoned_skip_report_a_gap() {
    let _lock = env_lock();
    let h = Harness::new("steps");
    h.set_report("# Report\n\n## Playbook\n\n1: done\n3: skip:\n\n## Lessons\nnone\n");

    let res = h.prompt("sude-s5b", Some(&brief_with_playbook()), false);

    assert_eq!(res, Err((6, String::new())));
    let events = h.gap_events("sude-s5b");
    assert_eq!(events.len(), 1, "one playbook_gap event: {events:?}");
    assert_eq!(events[0]["missing"], serde_json::json!([2]));
    assert_eq!(events[0]["unreasoned"], serde_json::json!([3]));
}

/// Line 3: `skip: unsupported on <harness>` is a valid skip, so a report that
/// uses it for every step it cannot run is silent.
#[test]
fn playbook_check_unsupported_harness_skip_is_accepted() {
    let _lock = env_lock();
    let h = Harness::new("harness");
    h.set_report(
        "# Report\n\n## Playbook\n\n1: done\n2: skip: unsupported on omp, no browser\n3: skip: unsupported on omp, read-only\n\n## Lessons\nnone\n",
    );

    let res = h.prompt("sude-s5c", Some(&brief_with_playbook()), false);

    assert_eq!(res, Ok(()));
    assert!(
        h.gap_events("sude-s5c").is_empty(),
        "an unsupported-on-harness skip is not a gap"
    );
}

/// Line 4: a brief that names no playbook pins nothing, so a report with no
/// `## Playbook` block is accepted exactly as today — no warning, no event.
#[test]
fn playbook_check_report_without_a_pin_behaves_as_today() {
    let _lock = env_lock();
    let h = Harness::new("nopin");
    h.set_report("# Report\n\n## Changes\n\ndid things\n\n## Lessons\nnone\n");

    let res = h.prompt("sude-s5d", Some(BRIEF), false);

    assert_eq!(res, Ok(()));
    assert!(!h.pin("sude-s5d").exists(), "no playbook, no pin");
    assert!(h.gap_events("sude-s5d").is_empty());
}

/// Line 5: a rejected report is accepted by `--resume` once it accounts for
/// every pinned step; the gap is journaled once, at the rejection.
#[test]
fn playbook_check_resume_accepts_the_fixed_report() {
    let _lock = env_lock();
    let h = Harness::new("resume");
    h.set_report("# Report\n\n## Changes\n\ndid things\n\n## Lessons\nnone\n");

    let res = h.prompt("sude-s5e", Some(&brief_with_playbook()), false);
    assert_eq!(res, Err((6, String::new())));
    assert_eq!(h.gap_events("sude-s5e").len(), 1);

    // The worker rewrites the run report atomically (temp file + rename).
    let run_report = h.runs.join("sude-s5e.md");
    let pending = h.runs.join(".sude-s5e.md.tmp");
    fs::write(
        &pending,
        "# Report\n\n## Playbook\n\n1: done\n2: done\n3: done\n\n## Lessons\nnone\n",
    )
    .unwrap();
    fs::rename(&pending, &run_report).unwrap();
    let res = h.prompt("sude-s5e", None, true);

    assert_eq!(res, Ok(()), "--resume accepts the fixed report");
    assert_eq!(
        h.gap_events("sude-s5e").len(),
        1,
        "the accepted resume journals no second gap"
    );
}

/// Line 2 through the real CLI: a builder gap exits 6, prints the WATCHDOG
/// line, and prints no report path.
#[test]
fn playbook_check_cli_exits_6_for_a_builder_gap() {
    let _lock = env_lock();
    let h = Harness::new("cli");
    h.set_report("# Report\n\n## Changes\n\ndid things\n\n## Lessons\nnone\n");

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.args(["worker", "prompt", "sude-s5f", &brief_with_playbook()])
        .env_remove("HERDR_PANE_ID");
    for (k, _) in std::env::vars() {
        if k.starts_with("HERDR_") && k != "HERDR_BIN_PATH" {
            cmd.env_remove(&k);
        }
    }
    let out = cmd.output().unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(6), "stderr: {stderr}");
    assert!(
        stderr.contains(
            "WATCHDOG playbook-gap | name=sude-s5f playbook=feature missing=1,2,3 unreasoned=none"
        ),
        "{stderr}"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "",
        "a rejected report prints no path"
    );
    assert_eq!(h.gap_events("sude-s5f").len(), 1);
}
