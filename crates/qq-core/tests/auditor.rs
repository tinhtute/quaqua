//! `quaqua auditor run`: report shape, read-only workspace, failure stand-in.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use qq_core::auditor::{self, AuditorRunOptions};

const TEAM: &str = r#"
[purposes]
review = "rev"

[kinds.rev]
harness = "claude"
model = "opus-5.5"
effort = "medium"
"#;

const REPORT: &str = "## Drift\n- none\n\n## Stale docs\n- none\n\n## Unhandled\n- none\n\n## Pipeline\n- none\n\n## Workers\n- none\n\nVerdict: clean\n";

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("qq-auditor-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A workspace with a team.toml and one blocked pipeline run no memory names.
fn workspace(tag: &str) -> PathBuf {
    let root = tmp(tag);
    std::fs::write(root.join("team.toml"), TEAM).unwrap();
    std::fs::create_dir_all(root.join("runs")).unwrap();
    std::fs::create_dir_all(root.join("memory")).unwrap();
    std::fs::write(
        root.join("runs/pipeline-x.md"),
        "- started: 2999-01-01\n- status: blocked\n",
    )
    .unwrap();
    root
}

/// A stub harness: records its argv beside itself, prints `body` as the report.
fn stub(dir: &Path, body: &str, code: i32) -> PathBuf {
    let p = dir.join("stub.sh");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}/argv.txt'\ncat <<'EOT'\n{body}EOT\nexit {code}\n",
        dir.display()
    );
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn files(root: &Path) -> Vec<String> {
    let mut v = Vec::new();
    fn walk(d: &Path, root: &Path, v: &mut Vec<String>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, root, v);
            } else {
                let rel = p.strip_prefix(root).unwrap().display().to_string();
                v.push(format!("{rel}:{}", std::fs::read(&p).unwrap().len()));
            }
        }
    }
    walk(root, root, &mut v);
    v.sort();
    v
}

#[test]
fn auditor_run_writes_report() {
    let root = workspace("run");
    let bin = tmp("run-bin");
    let out = root.join("reports/audit-test.md");
    let before = files(&root);
    let got = auditor::run_auditor(AuditorRunOptions {
        repo: Some(root.clone()),
        out: Some(out.clone()),
        exec: Some(stub(&bin, REPORT, 0)),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(got, out);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), REPORT);
    // Only the report is new; nothing else in the workspace moved.
    let mut after = files(&root);
    after.retain(|f| !f.starts_with("reports/"));
    assert_eq!(before, after);
    // Model and effort come from team.toml through the driver, read-only tools only.
    let argv = std::fs::read_to_string(bin.join("argv.txt")).unwrap();
    for want in [
        "--model",
        "claude-opus-5-5",
        "--effort",
        "medium",
        "dontAsk",
    ] {
        assert!(argv.lines().any(|l| l == want), "{want} missing in {argv}");
    }
    assert!(
        argv.contains("pipeline-x"),
        "pre-check block reaches the prompt"
    );
}

#[test]
fn auditor_failure_writes_stand_in() {
    let root = workspace("fail");
    let bin = tmp("fail-bin");
    let out = root.join("reports/audit-test.md");
    let e = auditor::run_auditor(AuditorRunOptions {
        repo: Some(root.clone()),
        out: Some(out.clone()),
        exec: Some(stub(&bin, "partial\n", 3)),
        ..Default::default()
    })
    .unwrap_err();
    assert_eq!(e.code, 3);
    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.contains("FAILED") && !body.contains("partial"));
}

#[test]
fn auditor_dry_run_has_all_sections_and_flags() {
    let root = workspace("dry");
    let out = root.join("reports/audit-dry.md");
    auditor::run_auditor(AuditorRunOptions {
        dry_run: true,
        repo: Some(root.clone()),
        out: Some(out.clone()),
        ..Default::default()
    })
    .unwrap();
    let body = std::fs::read_to_string(&out).unwrap();
    let heads: Vec<&str> = body.lines().filter(|l| l.starts_with("## ")).collect();
    assert_eq!(
        heads,
        [
            "## Drift",
            "## Stale docs",
            "## Unhandled",
            "## Pipeline",
            "## Workers"
        ]
    );
    assert!(body.contains("runs/pipeline-x.md"));
    let line = body.lines().find(|l| l.contains("pipeline-x")).unwrap();
    assert!(
        line.contains("[ack id: "),
        "dry-run line lacks ack tag: {line}"
    );
    assert!(body.trim_end().ends_with("Verdict: 1 findings"));
}

#[test]
fn auditor_mail_checks_read_qq_db_without_side_files() {
    let root = workspace("mail");
    let db = root.join("runs/qq.db");
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE mail (id INTEGER PRIMARY KEY, sender TEXT, recipient TEXT, kind TEXT, body TEXT, created_ms INTEGER);
             CREATE TABLE agent (id INTEGER PRIMARY KEY, name TEXT, role TEXT, spawner_id INTEGER, ended_ms INTEGER);
             INSERT INTO mail VALUES (1,'a-lead','cos','handoff','see runs/ghost.md',1000);
             INSERT INTO agent VALUES (1,'a-lead','lead',NULL,NULL),(2,'w1','peer',1,NULL);",
        )
        .unwrap();
        c.pragma_update(None, "journal_mode", "DELETE").unwrap();
    }
    let flags = auditor::run_deterministic_prechecks(&root);
    assert!(
        flags
            .iter()
            .any(|f| f.contains("mail #1") && f.contains("runs/ghost.md")),
        "{flags:?}"
    );
    assert!(
        flags
            .iter()
            .any(|f| f.contains("lost momentum") && f.contains("w1")),
        "{flags:?}"
    );
    let names: Vec<String> = std::fs::read_dir(root.join("runs"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n.ends_with("-wal") || n.ends_with("-shm")),
        "{names:?}"
    );
}

#[test]
fn auditor_args_parse() {
    let a = |s: &[&str]| auditor::parse_args(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
    let o = a(&[
        "run",
        "--dry-run",
        "--out=/x",
        "--model",
        "m",
        "--kind",
        "k",
        "--repo",
        "/r",
    ])
    .unwrap()
    .unwrap();
    assert!(o.dry_run);
    assert_eq!(o.out, Some(PathBuf::from("/x")));
    assert_eq!(
        (o.model.as_deref(), o.kind.as_deref()),
        (Some("m"), Some("k"))
    );
    assert_eq!(o.repo, Some(PathBuf::from("/r")));
    assert!(a(&["run", "--out"]).is_err());
    assert!(a(&["run", "--harness", "x"]).is_err());
    assert!(a(&["bogus"]).is_err());
    assert!(a(&["--help"]).unwrap().is_none());
}

#[test]
fn auditor_date_math_roundtrips() {
    // Pre-check expiry relies on this: a rule dated yesterday is expired, today is not.
    let root = tmp("expiry");
    std::fs::create_dir_all(root.join("rules/x")).unwrap();
    std::fs::write(
        root.join("rules/x/a.md"),
        "- old [expires: 2020-02-29]\n- bad [expires: 2021-02-30]\n- far [expires: 2999-12-31]\n",
    )
    .unwrap();
    let flags = auditor::run_deterministic_prechecks(&root);
    assert!(flags
        .iter()
        .any(|f| f.starts_with("- expired rule:") && f.contains("a.md:1")));
    assert!(flags
        .iter()
        .any(|f| f.starts_with("- malformed expiry:") && f.contains("a.md:2")));
    assert!(!flags.iter().any(|f| f.contains("a.md:3")));
}
