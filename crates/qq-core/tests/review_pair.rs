//! The review pair rule: same model refused and journaled, another model allowed.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use qq_core::authority;
use qq_core::config;
use qq_core::daemon::{Config, Daemon};
use qq_core::db;
use qq_core::driver::Registry;
use qq_core::herdr::FakeHerdr;
use qq_core::rpc::{self, Request};
use qq_core::task::pipeline_io::{self, Deps};
use qq_core::task::review_pair;
use qq_core::testkit::TestWorkspace;
use serde_json::{json, Value};

fn conn(tag: &str) -> rusqlite::Connection {
    let dir = std::env::temp_dir().join(format!("qq-review-pair-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = db::open(&dir.join(db::path::DB_FILE)).expect("open qq db");
    let team = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
    )
    .unwrap();
    config::seed(&mut c, &team).expect("seed");
    c
}

fn add_kind(c: &rusqlite::Connection, name: &str, harness: &str, model: &str) {
    c.execute(
        "INSERT INTO kind (name, harness, model, can_review) VALUES (?1, ?2, ?3, 1)",
        [name, harness, model],
    )
    .unwrap();
}

#[test]
fn review_refused_on_same_model() {
    let c = conn("refused");
    // ni runs on claude, other on opencode: two harnesses, one model.
    add_kind(&c, "other", "opencode", "opus-5.5");
    let cause = review_pair::check(&c, "ni", "other").expect_err("same model is refused");
    assert!(cause.contains("'ni'") && cause.contains("'other'") && cause.contains("opus-5.5"));
    let rows = authority::refusals(&c).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].edge, "review");
    for part in ["'ni'", "'other'", "'opus-5.5'"] {
        assert!(rows[0].reason.contains(part), "{part} in {}", rows[0].reason);
    }
    // Same harness, same model, different spelling of case: refused too.
    add_kind(&c, "loud", "claude", " OPUS-5.5 ");
    assert!(review_pair::check(&c, "ni", "loud").is_err());
    assert_eq!(authority::refusals(&c).unwrap().len(), 2);
}

#[test]
fn review_allowed_same_harness_other_model() {
    let c = conn("allowed");
    // sude and tieude both run on pi, on different models.
    review_pair::check(&c, "sude", "tieude").expect("another model is allowed");
    assert!(authority::refusals(&c).unwrap().is_empty());
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@local")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@local")
        .output()
        .expect("git runs");
    assert!(o.status.success(), "git {args:?}");
}

/// The start verb's kind check: a same-model pair is refused before the build
/// (code same_model, journaled); a same-harness pair on another model starts.
#[test]
fn review_start_verb_checks_models_not_harnesses() {
    let ws = TestWorkspace::new("review-pair-verb");
    let repo = std::env::temp_dir().join(format!("qq-review-pair-repo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["branch", "integ"]);
    git(&repo, &["branch", "t1"]);
    let mut c = db::open(&ws.db_path()).expect("open qq db");
    let team = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
    )
    .unwrap();
    config::seed(&mut c, &team).expect("seed");
    c.execute(
        "INSERT INTO agent (name, role, started_ms) VALUES ('cos', 'cos', 1)",
        [],
    )
    .unwrap();
    c.execute("INSERT INTO project(slug, created_ms) VALUES ('p', 1)", [])
        .unwrap();
    add_kind(&c, "other", "opencode", "opus-5.5"); // another harness, builder's model
    add_kind(&c, "claude2", "claude", "other-model"); // builder's harness, another model
    drop(c);

    let herdr = Arc::new(FakeHerdr::new());
    let anchor = herdr.seed_pane("work", None, &[]);
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone(),
    });
    let watcher = d.report_watcher();
    let deps = Deps::new(herdr.clone(), ws.runs_dir(), Registry::native(), watcher);
    pipeline_io::register(&mut d, deps).expect("register pipeline");
    let daemon = d.spawn().expect("daemon starts");
    let call = |reviewer: &str| {
        let args: Value = json!({
            "task": "t1", "builder_kind": "ni", "reviewer_kind": reviewer,
            "brief": "Goal: x\n", "repo": repo.to_string_lossy(), "branch": "t1",
            "integ": "integ", "project_id": 1, "plan": false, "pane": anchor,
        });
        let mut req = Request::new("pipeline", args);
        req.caller = Some("cos".into());
        rpc::call(&rpc::socket_path(&ws.runs_dir()), &req).expect("rpc transport")
    };

    for reviewer in ["other", "suhuynh"] {
        let r = call(reviewer);
        assert!(!r.ok, "{reviewer} shares the model");
        assert_eq!(r.error.unwrap().code, "same_model");
    }
    assert!(herdr.prompts().is_empty(), "a refusal starts nothing");
    let rows = authority::refusals(&db::open(&ws.db_path()).unwrap()).unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].reason.contains("'ni'") && rows[0].reason.contains("'other'"));

    let ok = call("claude2");
    assert!(ok.ok, "{:?}", ok.error);
    let t0 = Instant::now();
    while herdr.prompts().is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(10), "no build prompt");
        std::thread::sleep(Duration::from_millis(20));
    }
    daemon.stop().unwrap();
}
