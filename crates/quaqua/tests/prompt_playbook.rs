//! Brief contract + playbook injection acceptance tests (§ S4).
//!
//! Acceptance cases:
//! 1. unknown playbook rejected
//! 2. missing `Playbook:` / `Context:` warns, exit 0
//! 3. submitted prompt carries Worker text, no Orchestrator text, each cited leaf once
//! 4. pre-filled `## Playbook` block present
//! 5. `runs/.<name>.playbook` holds name + step ids; a follow-up prompt overwrites it
//! 6. a brief without the headers is submitted byte-identical to today
//!
//! Verify command: `cargo test --locked -p quaqua prompt`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

use quaqua::git::Git;
use quaqua::herdr::Herdr;
use quaqua::worker::prompt::brief::{
    brief_header_warnings, compose_prompt_body, named_playbook, playbook_header, prepare_injection,
    prepare_injection_in_workspace, PlaybookHeader,
};
use quaqua::worker::prompt::{brief_headers_ok, prompt_worker};

/// Helper RAII temporary directory for test isolation.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("quaqua-prompt-{tag}-{}-{id}", std::process::id()));
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

/// A brief with neither of the two advisory headers (§ S4: it must still pass).
const OLD_BRIEF: &str = "\
Topology: solo — one engine task
Goal: make the thing work
Hard constraints: none
Current design: the approach as it stands
Unknowns: how to check it
Evidence: command + result
Time budget: 5 min
";

const ORCHESTRATOR_MARKER: &str = "ORCHESTRATOR-ONLY-MARKER";
const LEAF_FIXTURE_LINE: &str = "FIXTURE-LEAF-BODY-LINE";
const LEAF_OTHER_LINE: &str = "OTHER-LEAF-BODY-LINE";

/// One fixture playbook that cites `principle-fixture` twice and
/// `principle-other` once, plus the two leaves it resolves.
fn fixture_index(sandbox: &TempDir) -> (PathBuf, PathBuf) {
    let pb_dir = sandbox.sub("playbooks");
    let pr_dir = sandbox.sub("principles");
    sandbox.write_file(
        "playbooks/feature.md",
        &format!(
            "\
---
name: feature
when: New behavior from a named data shape.
---

## Orchestrator

1. {ORCHESTRATOR_MARKER}: define the interfaces before implementation.

## Worker

1. Clarify the domain model, citing principle-fixture.
2. Implement the feature, citing principle-fixture once more.
3. Write the check, citing principle-other.
"
        ),
    );
    sandbox.write_file(
        "playbooks/bug-fix.md",
        "\
---
name: bug-fix
when: A defect with a reproducible failure.
---

## Orchestrator

1. Reproduce before fixing.

## Worker

1. Build the failing check first.
2. Fix the root cause.
",
    );
    sandbox.write_file(
        "principles/fixture.md",
        &format!("# Fixture\n\n{LEAF_FIXTURE_LINE}\n"),
    );
    sandbox.write_file(
        "principles/other.md",
        &format!("# Other\n\n{LEAF_OTHER_LINE}\n"),
    );
    (pb_dir, pr_dir)
}

fn brief_with(header: &str) -> String {
    format!("{OLD_BRIEF}{header}\n")
}

// ─── 2. missing Playbook / Context warns, old briefs still pass ─────────────

#[test]
fn prompt_brief_header_warnings_are_advisory() {
    // Old brief: no Playbook / Context. It must keep passing the hard check.
    assert!(brief_headers_ok(OLD_BRIEF));
    let warnings = brief_header_warnings(OLD_BRIEF);
    assert_eq!(warnings.len(), 2, "want two warnings: {warnings:?}");
    assert!(warnings[0].contains("'Playbook:'"), "{warnings:?}");
    assert!(warnings[1].contains("'Context:'"), "{warnings:?}");

    // Both present with real values: no warning.
    assert!(brief_header_warnings(&brief_with(
        "Playbook: feature\nContext: crates/quaqua/src/worker/prompt/mod.rs"
    ))
    .is_empty());

    // `none` without a reason warns; with a reason it does not.
    let bare = brief_header_warnings(&brief_with("Playbook: none\nContext: none"));
    assert_eq!(bare.len(), 2, "bare none must warn twice: {bare:?}");
    assert!(bare[0].contains("needs a reason"), "{bare:?}");
    assert!(bare[1].contains("needs a reason"), "{bare:?}");

    let reasoned = brief_header_warnings(&brief_with(
        "Playbook: none, one-command unit\nContext: none, single file already read",
    ));
    assert!(reasoned.is_empty(), "{reasoned:?}");
}

// ─── 3+4. Worker text only, each leaf once, pre-filled block ────────────────

#[test]
fn prompt_injection_carries_worker_text_and_each_leaf_once() {
    let sandbox = TempDir::new("inject");
    let (pb_dir, pr_dir) = fixture_index(&sandbox);

    let injection = prepare_injection(&brief_with("Playbook: feature"), &pb_dir, &pr_dir)
        .expect("known playbook resolves")
        .expect("a named playbook injects");

    assert_eq!(injection.name, "feature");
    assert_eq!(injection.ids, vec![1, 2, 3]);

    // Worker steps, with their ids.
    assert!(injection.text.contains("1. Clarify the domain model"));
    assert!(injection.text.contains("2. Implement the feature"));
    assert!(injection.text.contains("3. Write the check"));

    // No Orchestrator text.
    assert!(
        !injection.text.contains(ORCHESTRATOR_MARKER),
        "Orchestrator text leaked into the worker prompt:\n{}",
        injection.text
    );

    // Each cited leaf exactly once, even though step 1 and 2 both cite it.
    assert_eq!(
        injection.text.matches(LEAF_FIXTURE_LINE).count(),
        1,
        "cited leaf repeated:\n{}",
        injection.text
    );
    assert_eq!(injection.text.matches(LEAF_OTHER_LINE).count(), 1);
    assert!(injection.text.contains("### principle-fixture"));
    assert!(injection.text.contains("### principle-other"));

    // Pre-filled report block the worker copies: `1: `, `2: `, `3: `.
    let prefilled = injection
        .text
        .rfind("## Playbook\n")
        .map(|at| &injection.text[at..])
        .expect("pre-filled ## Playbook block");
    assert_eq!(prefilled, "## Playbook\n1: \n2: \n3: \n");

    // `Playbook: none, <reason>` and a missing header inject nothing.
    assert!(prepare_injection(
        &brief_with("Playbook: none, one-command unit"),
        &pb_dir,
        &pr_dir
    )
    .unwrap()
    .is_none());
    assert!(prepare_injection(OLD_BRIEF, &pb_dir, &pr_dir)
        .unwrap()
        .is_none());
}

// ─── 1. unknown playbook rejected ───────────────────────────────────────────

#[test]
fn prompt_unknown_playbook_is_rejected() {
    let sandbox = TempDir::new("unknown");
    let (pb_dir, pr_dir) = fixture_index(&sandbox);

    let err = prepare_injection(&brief_with("Playbook: nope"), &pb_dir, &pr_dir)
        .expect_err("an unknown playbook is rejected");
    assert_eq!(err.0, 1, "unknown playbook keeps the brief-error exit code");
    assert!(err.1.contains("unknown playbook 'nope'"), "{}", err.1);

    // The explicit-index path reports an absent index as a load failure; the
    // workspace variant downgrades that to a warning, since the brief is not
    // at fault (`prompt_missing_index_only_warns` pins that split).
    let missing = prepare_injection(
        &brief_with("Playbook: feature"),
        &sandbox.0.join("absent/playbooks"),
        &sandbox.0.join("absent/principles"),
    );
    assert!(
        missing.is_err(),
        "the explicit path surfaces an absent index"
    );
}

// ─── 1b. an absent or incomplete index only warns, never rejects ────────────

#[test]
fn prompt_missing_index_only_warns() {
    let _lock = env_lock();
    let sandbox = TempDir::new("noindex");

    // No index at all: `Playbook: nope` cannot be checked against anything, so
    // the workspace variant warns and injects nothing instead of failing a
    // brief the workspace cannot judge.
    {
        let _env = EnvGuard::set(&[
            (
                "QUAQUA_HOME",
                sandbox.0.join("absent-ws").display().to_string(),
            ),
            (
                "QUAQUA_PLAYBOOKS_DIR",
                sandbox.0.join("absent-playbooks").display().to_string(),
            ),
            (
                "QUAQUA_PRINCIPLES_DIR",
                sandbox.0.join("absent-principles").display().to_string(),
            ),
        ]);
        assert_eq!(
            prepare_injection_in_workspace(&brief_with("Playbook: nope")),
            Ok(None)
        );
    }

    // Incomplete index (`playbooks/` present, `principles/` absent): nothing
    // resolves, so the name is left unchecked rather than failing every named
    // brief in a workspace whose index is incomplete.
    let partial = TempDir::new("partial");
    fs::create_dir_all(partial.0.join("playbooks")).unwrap();
    {
        let _env = EnvGuard::set(&[
            (
                "QUAQUA_PLAYBOOKS_DIR",
                partial.0.join("playbooks").display().to_string(),
            ),
            (
                "QUAQUA_PRINCIPLES_DIR",
                partial.0.join("absent-principles").display().to_string(),
            ),
        ]);
        assert_eq!(
            prepare_injection_in_workspace(&brief_with("Playbook: nope")),
            Ok(None)
        );
    }
}

// ─── 1c. a playbook the index cannot render is rejected, not ignored ────────

#[test]
fn prompt_unloadable_playbook_is_rejected_with_the_brief_code() {
    let sandbox = TempDir::new("broken");
    let pb_dir = sandbox.sub("playbooks");
    let pr_dir = sandbox.sub("principles");
    sandbox.write_file(
        "playbooks/broken.md",
        "---\nname: broken\nwhen: Cites a leaf that does not exist.\n---\n\n## Orchestrator\n\n1. Plan.\n\n## Worker\n\n1. Do it, citing principle-absent.\n",
    );

    let err = prepare_injection(&brief_with("Playbook: broken"), &pb_dir, &pr_dir)
        .expect_err("a citation that cannot be read is rejected");
    assert_eq!(err.0, 1, "keeps the brief-error exit code");
    assert!(err.1.contains("cannot load playbook 'broken'"), "{}", err.1);
}

// ─── 2b. the header scan stops at the header block ─────────────────────────

#[test]
fn prompt_header_scan_stops_at_the_header_block() {
    let sandbox = TempDir::new("relay");
    let (pb_dir, pr_dir) = fixture_index(&sandbox);

    // A brief with no `Playbook:` of its own that relays an upstream brief
    // after `Context:` must not adopt the relayed header.
    let relayed = format!(
        "{OLD_BRIEF}Context: upstream brief pasted below\n\nPlaybook: feature\nGoal: the upstream unit\n"
    );
    assert_eq!(playbook_header(&relayed), PlaybookHeader::Missing);
    assert_eq!(named_playbook(&relayed), None);
    assert_eq!(prepare_injection(&relayed, &pb_dir, &pr_dir), Ok(None));
    let warnings = brief_header_warnings(&relayed);
    assert!(
        warnings.iter().any(|w| w.contains("no 'Playbook:'")),
        "{warnings:?}"
    );

    // Its own header still counts when it is not the first line, and leading
    // blank lines do not hide the header block.
    let own = "\nGoal: x\nPlaybook: feature\nContext: crates/quaqua/src/lib.rs\n";
    assert_eq!(named_playbook(own).as_deref(), Some("feature"));
    assert!(brief_header_warnings(own).is_empty(), "{own}");

    // The documented skeleton copied unfilled warns instead of rejecting:
    // `<name …>` is a placeholder, not a playbook name.
    let skeleton = brief_with(
        "Playbook: <name from `quaqua playbook list`, or `none, <reason>` for a trivial one-command unit>\nContext: <files / PR pointers>",
    );
    assert_eq!(playbook_header(&skeleton), PlaybookHeader::Placeholder);
    assert_eq!(prepare_injection(&skeleton, &pb_dir, &pr_dir), Ok(None));
    let warnings = brief_header_warnings(&skeleton);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("placeholder"), "{warnings:?}");
    assert!(warnings[1].contains("placeholder"), "{warnings:?}");
}

// ─── 6. old brief unchanged ────────────────────────────────────────────────

#[test]
fn prompt_body_without_playbook_is_byte_identical() {
    let body = compose_prompt_body("sude-oldbrief", OLD_BRIEF, None);
    let today = format!(
        "{OLD_BRIEF}. When done, write summary (changes, test cmd+result, risks) to a temp file and rename it to ~/quaqua/runs/sude-oldbrief.md (publish it atomically, never a partial file); reply only the path."
    );
    assert_eq!(body, today);

    // With an injection the brief and the block come before the same tail.
    let sandbox = TempDir::new("compose");
    let (pb_dir, pr_dir) = fixture_index(&sandbox);
    let injection = prepare_injection(&brief_with("Playbook: feature"), &pb_dir, &pr_dir)
        .unwrap()
        .unwrap();
    let injected = compose_prompt_body("sude-oldbrief", OLD_BRIEF, Some(injection.text.as_str()));
    assert!(injected.starts_with(OLD_BRIEF));
    assert!(injected.contains("## Worker (playbook: feature)"));
    assert!(injected.ends_with("reply only the path."));
}

// ─── CLI: --check-brief warns (exit 0) and rejects an unknown name ─────────

fn run_check_brief(sandbox: &TempDir, dirs: &(PathBuf, PathBuf), brief: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.args(["worker", "prompt", "--check-brief", brief])
        .env("HOME", &sandbox.0)
        .env("QUAQUA_HOME", sandbox.0.join("ws"))
        .env("QUAQUA_PLAYBOOKS_DIR", &dirs.0)
        .env("QUAQUA_PRINCIPLES_DIR", &dirs.1)
        .env_remove("PROMPT_WORKER_BUDGET_SEC")
        .env_remove("HERDR_PANE_ID");
    for (k, _) in std::env::vars() {
        if k.starts_with("HERDR_") && k != "HERDR_BIN_PATH" {
            cmd.env_remove(&k);
        }
    }
    cmd.output().expect("run quaqua")
}

#[test]
fn prompt_check_brief_cli_warns_and_rejects_unknown_playbook() {
    let sandbox = TempDir::new("cli");
    let dirs = fixture_index(&sandbox);

    // Old brief: exit 0, both warnings on stderr, stdout keeps only the budget.
    let out = run_check_brief(&sandbox, &dirs, OLD_BRIEF);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "budget=300s source=brief"
    );
    assert!(
        stderr.contains("warning: brief has no 'Playbook:'"),
        "{stderr}"
    );
    assert!(
        stderr.contains("warning: brief has no 'Context:'"),
        "{stderr}"
    );

    // Headers present: exit 0 and quiet.
    let out = run_check_brief(
        &sandbox,
        &dirs,
        &brief_with("Playbook: feature\nContext: crates/quaqua/src/lib.rs"),
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stderr).trim().is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Bare `none`: warns, still exit 0.
    let out = run_check_brief(
        &sandbox,
        &dirs,
        &brief_with("Playbook: none\nContext: none"),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stderr.matches("needs a reason").count(), 2, "{stderr}");

    // Unknown name: exit 1 with the name in the message.
    let out = run_check_brief(&sandbox, &dirs, &brief_with("Playbook: nope"));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown playbook 'nope'"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Missing index: a name cannot be checked against anything, so even
    // `Playbook: nope` only warns (exit 0) — the reading rules/_all/brief.md
    // states and `prompt_missing_index_only_warns` pins.
    let absent = (
        sandbox.0.join("absent/playbooks"),
        sandbox.0.join("absent/principles"),
    );
    let out = run_check_brief(
        &sandbox,
        &absent,
        &brief_with("Playbook: nope\nContext: crates/quaqua/src/lib.rs"),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.contains("playbook index missing"), "{stderr}");
    assert!(!stderr.contains("unknown playbook"), "{stderr}");

    // The documented skeleton copied unfilled: the `<…>` placeholders warn and
    // the check stays at exit 0 instead of reading `<name` as a playbook.
    let out = run_check_brief(
        &sandbox,
        &dirs,
        &brief_with("Playbook: <name from `quaqua playbook list`>\nContext: <files / PR pointers>"),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stderr.matches("placeholder").count(), 2, "{stderr}");
}

// ─── 5. `.playbook` holds name + ids; a follow-up prompt overwrites it ─────

/// Serializes the two tests that write the process env: `set_var` is global, so
/// a parallel test would otherwise read another test's playbook index.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Process-env guard: the herdr stub reads these from the inherited env.
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
    # A compliant worker: feature pins 1,2,3 and bug-fix pins 1,2, so this
    # block accounts for either (S8 rejects a report that skips a pinned step).
    printf 'stub report\n\n## Playbook\n1: done\n2: done\n3: done\n\n## Lessons\nnone\n' > "$STUB_RUNS/$3.md"
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

/// A stub whose prompt never lands: `agent prompt` accepts the call but stays
/// `idle`, and the recovery `agent wait` never reaches `working` either, so
/// `prompt_worker` gives up with exit 3.
fn stub_herdr_not_landing(dir: &Path) -> PathBuf {
    let path = dir.join("herdr-stub-noland");
    fs::write(
        &path,
        r#"#!/bin/sh
echo "herdr $*" >> "$CALLS"
case "${1:-} ${2:-}" in
  "agent get")
    printf '{"result":{"agent":{"agent_status":"working","cwd":"%s"}}}\n' "$STUB_CWD"
    exit 0 ;;
  "agent prompt")
    printf '{"result":{"agent":{"agent_status":"idle"}}}\n'
    exit 0 ;;
  "agent wait")
    printf '{"result":{"agent":{"agent_status":"idle"}}}\n'
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

/// The body the stub received for the last `agent prompt <name> ...` call.
fn recorded_prompt(sandbox: &Path, name: &str) -> String {
    fs::read_to_string(sandbox.join(format!("prompt-{name}.txt")))
        .unwrap_or_else(|e| panic!("no recorded prompt for {name}: {e}"))
}

#[test]
fn prompt_state_pins_name_and_ids_and_followup_overwrites() {
    let _lock = env_lock();
    let sandbox = TempDir::new("state");
    let dirs = fixture_index(&sandbox);
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

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs.display().to_string()),
        ("QUAQUA_HOME", sandbox.0.join("ws").display().to_string()),
        ("QUAQUA_PLAYBOOKS_DIR", dirs.0.display().to_string()),
        ("QUAQUA_PRINCIPLES_DIR", dirs.1.display().to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("CALLS", calls.display().to_string()),
        ("STUB_RUNS", runs.display().to_string()),
        ("STUB_CWD", sandbox.0.display().to_string()),
        ("HERDR_BIN_PATH", stub.display().to_string()),
        ("PROMPT_WORKER_CLOCK_FILE", clock.display().to_string()),
        ("PROMPT_WORKER_SLEEP_CMD", advance.display().to_string()),
        ("PROMPT_WORKER_POLL_SEC", "1".to_string()),
        ("PROMPT_WORKER_MAX_SEC", "60".to_string()),
        ("PROMPT_WORKER_HEARTBEAT_SEC", "3600".to_string()),
    ]);

    let herdr = Herdr {
        bin: stub.display().to_string(),
    };
    let git = Git::new();
    let state = runs.join(".sude-s4state.playbook");

    // First prompt: `Playbook: feature` → name + the ids the worker was given.
    let res = prompt_worker(
        "sude-s4state",
        Some(&brief_with("Playbook: feature")),
        false,
        None,
        &git,
        &herdr,
    );
    assert_eq!(res, Ok(()), "first prompt must be accepted");
    assert_eq!(
        fs::read_to_string(&state).unwrap(),
        "name=feature\nids=1,2,3\n"
    );
    let body = recorded_prompt(&sandbox.0, "sude-s4state");
    assert!(body.contains("## Worker (playbook: feature)"), "{body}");
    assert!(!body.contains(ORCHESTRATOR_MARKER), "{body}");

    // Follow-up prompt with another playbook overwrites the pin.
    let res = prompt_worker(
        "sude-s4state",
        Some(&brief_with("Playbook: bug-fix")),
        false,
        None,
        &git,
        &herdr,
    );
    assert_eq!(res, Ok(()), "follow-up prompt must be accepted");
    assert_eq!(
        fs::read_to_string(&state).unwrap(),
        "name=bug-fix\nids=1,2\n"
    );

    // A brief without the header keeps today's body and drops the stale pin.
    let res = prompt_worker("sude-s4state", Some(OLD_BRIEF), false, None, &git, &herdr);
    assert_eq!(res, Ok(()), "old brief must be accepted");
    assert_eq!(
        recorded_prompt(&sandbox.0, "sude-s4state"),
        format!("{OLD_BRIEF}. When done, write summary (changes, test cmd+result, risks) to a temp file and rename it to ~/quaqua/runs/sude-s4state.md (publish it atomically, never a partial file); reply only the path.")
    );
    assert!(!state.exists(), "stale .playbook must not survive");
}

// ─── 5b. the pin is written only after the prompt lands ────────────────────

#[test]
fn prompt_state_is_written_only_after_the_prompt_lands() {
    let _lock = env_lock();
    let sandbox = TempDir::new("noland");
    let dirs = fixture_index(&sandbox);
    let runs = sandbox.sub("runs");
    let calls = sandbox.0.join("calls");
    let clock = sandbox.0.join("clock");
    fs::write(&clock, "1700000000\n").unwrap();
    let stub = stub_herdr_not_landing(&sandbox.0);

    let _env = EnvGuard::set(&[
        ("QUAQUA_RUNS_DIR", runs.display().to_string()),
        ("QUAQUA_HOME", sandbox.0.join("ws").display().to_string()),
        ("QUAQUA_PLAYBOOKS_DIR", dirs.0.display().to_string()),
        ("QUAQUA_PRINCIPLES_DIR", dirs.1.display().to_string()),
        ("GRID_SKIP", "1".to_string()),
        ("QUAQUA_TEST", "1".to_string()),
        ("CALLS", calls.display().to_string()),
        ("STUB_RUNS", runs.display().to_string()),
        ("STUB_CWD", sandbox.0.display().to_string()),
        ("HERDR_BIN_PATH", stub.display().to_string()),
        ("PROMPT_WORKER_CLOCK_FILE", clock.display().to_string()),
        ("PROMPT_WORKER_MAX_SEC", "600".to_string()),
        ("PROMPT_WORKER_HEARTBEAT_SEC", "3600".to_string()),
    ]);

    let herdr = Herdr {
        bin: stub.display().to_string(),
    };
    let git = Git::new();
    let state = runs.join(".sude-s4noland.playbook");

    // A pin left by an earlier task: a prompt that never lands must not keep it
    // for S5 to check against the new report.
    fs::write(&state, "name=feature\nids=1,2,3\n").unwrap();

    let res = prompt_worker(
        "sude-s4noland",
        Some(&brief_with("Playbook: feature")),
        false,
        None,
        &git,
        &herdr,
    );
    assert_eq!(
        res,
        Err((3, String::new())),
        "a prompt that never lands exits 3"
    );
    assert!(!state.exists(), "no pin may survive a non-landed prompt");
}

// Keep the workspace resolver honest: a brief without a Playbook header never
// touches the workspace index (so `--check-brief` works outside a workspace).
#[test]
fn prompt_workspace_lookup_is_skipped_without_a_playbook_header() {
    let _lock = env_lock();
    let sandbox = TempDir::new("noplaybook");
    let _env = EnvGuard::set(&[
        (
            "QUAQUA_HOME",
            sandbox.0.join("absent-workspace").display().to_string(),
        ),
        (
            "QUAQUA_PLAYBOOKS_DIR",
            sandbox.0.join("absent-playbooks").display().to_string(),
        ),
        (
            "QUAQUA_PRINCIPLES_DIR",
            sandbox.0.join("absent-principles").display().to_string(),
        ),
    ]);
    assert_eq!(prepare_injection_in_workspace(OLD_BRIEF), Ok(None));
    assert_eq!(
        prepare_injection_in_workspace(&brief_with("Playbook: none, trivial")),
        Ok(None)
    );
}
