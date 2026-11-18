//! Gap G1 of the walking skeleton: a verb that writes a task row, and a daemon
//! `push` verb that runs `authority::check_orchestration` before any git call.
//! Every daemon here lives in a temp workspace; the remote is a local bare repo.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use qq_core::authority;
use qq_core::config;
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::db;
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::identity::peer::HerdrPeers;
use qq_core::rpc::{self, Request};
use qq_core::run::{self, NewRun};
use qq_core::testkit::TestWorkspace;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

struct Rig {
    ws: TestWorkspace,
    herdr: Arc<FakeHerdr>,
    daemon: DaemonHandle,
}

fn template() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml");
    std::fs::read_to_string(p)
        .expect("read template team.toml")
        .replace("harness = \"omp\"", "harness = \"claude\"")
}

fn agent(conn: &Connection, name: &str, role: &str, kind: Option<&str>, project: Option<i64>) {
    conn.execute(
        "INSERT INTO agent (name, role, kind, project_id, started_ms) VALUES (?1, ?2, ?3, ?4, 1)",
        params![name, role, kind, project],
    )
    .unwrap();
}

/// Seeded db: project 1 `alpha`; `lead-f` (claude, fenced), `lead-u` (opencode,
/// generic so unfenced), `p1` (a Peer).
fn rig(tag: &str, peers: bool) -> Rig {
    let ws = TestWorkspace::new(tag);
    ws.seal_herdr();
    {
        let mut conn = db::open(&ws.db_path()).unwrap();
        config::seed(&mut conn, &template()).unwrap();
        conn.execute(
            "INSERT INTO project (slug, repo_path, created_ms) VALUES ('alpha', '/r', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project (slug, repo_path, created_ms) VALUES ('beta', '/r2', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO kind (name, harness, model) VALUES ('gen', 'opencode', 'm')",
            [],
        )
        .unwrap();
        agent(&conn, "lead-f", "lead", Some("ni"), Some(1));
        agent(&conn, "lead-u", "lead", Some("gen"), Some(2));
        agent(&conn, "p1", "peer", Some("ni"), Some(1));
    }
    let herdr = Arc::new(FakeHerdr::new());
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone() as Arc<dyn HerdrClient>,
    });
    qq_core::task::create::register(&mut d, ws.runs_dir()).unwrap();
    authority::push::register(&mut d, ws.runs_dir()).unwrap();
    if peers {
        d.require_peer(Arc::new(HerdrPeers::new(herdr.clone())));
    }
    let daemon = d.spawn().unwrap();
    Rig { ws, herdr, daemon }
}

fn call(r: &Rig, verb: &str, caller: Option<&str>, args: Value) -> Result<Value, rpc::RpcError> {
    let mut req = Request::new(verb, args);
    req.caller = caller.map(String::from);
    rpc::call(&r.daemon.socket, &req).unwrap().into_result()
}

fn conn(r: &Rig) -> Connection {
    db::open(&r.ws.db_path()).unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A work repo with one commit on `feat` and a local bare `origin` that lacks it.
fn repo_with_origin(r: &Rig) -> (std::path::PathBuf, std::path::PathBuf) {
    let work = r.ws.root().join("work");
    let origin = r.ws.root().join("origin.git");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "feat"]);
    git(&work, &["config", "user.email", "t@example.invalid"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("f"), "x").unwrap();
    git(&work, &["add", "f"]);
    git(&work, &["commit", "-qm", "c"]);
    git(
        origin.parent().unwrap(),
        &["init", "-q", "--bare", "origin.git"],
    );
    git(
        &work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    // The caller's project (alpha) owns this repo.
    conn(r)
        .execute(
            "UPDATE project SET repo_path = ?1 WHERE slug = 'alpha'",
            [work.to_str().unwrap()],
        )
        .unwrap();
    (work, origin)
}

fn origin_has_feat(origin: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(origin)
        .args(["rev-parse", "--verify", "-q", "refs/heads/feat"])
        .status()
        .unwrap()
        .success()
}

// ---- gap 1: task.create ----------------------------------------------------------------

#[test]
fn task_create_writes_a_task_row_a_run_can_carry() {
    let r = rig("g1-task", false);
    let a = call(
        &r,
        "task.create",
        Some("lead-f"),
        json!({"project": "alpha", "title": "add x", "kind": "ni"}),
    )
    .unwrap();
    let id = a["task_id"].as_i64().unwrap();
    assert_eq!(a["created"], true);

    let c = conn(&r);
    let row: (i64, String, Option<String>, String, String) = c
        .query_row(
            "SELECT project_id, title, kind, disposition, state FROM task WHERE id = ?1",
            [id],
            |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            1,
            "add x".into(),
            Some("ni".into()),
            "engineer".into(),
            "open".into()
        )
    );

    // A run opened with that id satisfies the run.task_id foreign key.
    let mut c = c;
    c.execute(
        "INSERT INTO workflow (id, kind, state, started_ms, updated_ms) VALUES ('worker:w', 'k', 's', 0, 0)",
        [],
    )
    .unwrap();
    let run_id = run::open(
        &mut c,
        &NewRun {
            workflow_id: "worker:w",
            agent_id: 1,
            task_id: Some(id),
            kind: "ni",
            harness: "claude",
            model: "m",
            effort: None,
            base: None,
            now_ms: 1,
        },
    )
    .unwrap();
    let carried: Option<i64> = c
        .query_row("SELECT task_id FROM run WHERE id = ?1", [run_id], |x| {
            x.get(0)
        })
        .unwrap();
    assert_eq!(carried, Some(id));
}

#[test]
fn task_create_repeat_converges_and_bad_input_is_refused() {
    let r = rig("g1-task-idem", false);
    let args = json!({"project": "alpha", "title": "same"});
    let a = call(&r, "task.create", Some("lead-f"), args.clone()).unwrap();
    let b = call(&r, "task.create", Some("lead-f"), args).unwrap();
    assert_eq!(a["task_id"], b["task_id"]);
    assert_eq!(b["created"], false);
    let n: i64 = conn(&r)
        .query_row("SELECT COUNT(*) FROM task", [], |x| x.get(0))
        .unwrap();
    assert_eq!(n, 1);

    let no_project = call(
        &r,
        "task.create",
        Some("lead-f"),
        json!({"project": "nope", "title": "t"}),
    );
    assert_eq!(no_project.unwrap_err().code, "no_such_project");
    let no_kind = call(
        &r,
        "task.create",
        Some("lead-f"),
        json!({"project": "alpha", "title": "k", "kind": "ghost"}),
    );
    assert_eq!(no_kind.unwrap_err().code, "no_such_kind");
}

#[test]
fn task_create_by_a_peer_is_refused() {
    let r = rig("g1-task-peer", false);
    let e = call(
        &r,
        "task.create",
        Some("p1"),
        json!({"project": "alpha", "title": "t"}),
    )
    .unwrap_err();
    assert_eq!(e.code, "refused");
    let n: i64 = conn(&r)
        .query_row("SELECT COUNT(*) FROM task", [], |x| x.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

// ---- gap 2: push ------------------------------------------------------------------------

fn push_args(work: &Path) -> Value {
    json!({"repo": work, "remote": "origin", "branch": "feat"})
}

#[test]
fn fenced_lead_push_reaches_the_bare_remote() {
    let r = rig("g1-push-ok", false);
    let (work, origin) = repo_with_origin(&r);
    assert!(!origin_has_feat(&origin));
    let out = call(&r, "push", Some("lead-f"), push_args(&work)).unwrap();
    assert_eq!(out["pushed"], true);
    assert!(origin_has_feat(&origin));
    // A repeat converges: nothing new to push, still a success.
    assert_eq!(
        call(&r, "push", Some("lead-f"), push_args(&work)).unwrap()["pushed"],
        true
    );
    assert!(authority::refusals(&conn(&r)).unwrap().is_empty());
}

#[test]
fn peer_and_unfenced_pushes_are_refused_and_journaled() {
    let r = rig("g1-push-refused", false);
    let (work, origin) = repo_with_origin(&r);

    let peer = call(&r, "push", Some("p1"), push_args(&work)).unwrap_err();
    let unfenced = call(&r, "push", Some("lead-u"), push_args(&work)).unwrap_err();
    assert_eq!(
        (peer.code.as_str(), unfenced.code.as_str()),
        ("refused", "refused")
    );
    assert!(
        !origin_has_feat(&origin),
        "a refused push must not reach the remote"
    );

    let rows = authority::refusals(&conn(&r)).unwrap();
    let seen: Vec<(&str, &str)> = rows
        .iter()
        .map(|x| (x.caller.as_str(), x.edge.as_str()))
        .collect();
    assert_eq!(seen, [("p1", "orchestration"), ("lead-u", "push")]);
}

#[test]
fn push_without_a_caller_is_refused() {
    let r = rig("g1-push-anon", false);
    let (work, origin) = repo_with_origin(&r);
    let e = call(&r, "push", None, push_args(&work)).unwrap_err();
    assert_eq!(e.code, "refused");
    assert!(!origin_has_feat(&origin));
}

#[test]
fn unmapped_peer_is_still_refused_on_both_verbs() {
    let r = rig("g1-unmapped", true);
    let (work, origin) = repo_with_origin(&r);
    // The test process maps to no pane: the daemon refuses before any handler.
    let t = call(
        &r,
        "task.create",
        Some("lead-f"),
        json!({"project": "alpha", "title": "t"}),
    );
    let p = call(&r, "push", Some("lead-f"), push_args(&work));
    for e in [t.unwrap_err(), p.unwrap_err()] {
        assert_eq!(e.code, "unmapped_peer");
    }
    assert!(!origin_has_feat(&origin));
    let n: i64 = conn(&r)
        .query_row("SELECT COUNT(*) FROM task", [], |x| x.get(0))
        .unwrap();
    assert_eq!(n, 0);
    let _ = &r.herdr;
}

fn origin_ref(origin: &Path, r: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(origin)
        .args(["rev-parse", "--verify", "-q", r])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap().trim().to_string())
}

#[test]
fn push_cannot_force_delete_overwrite_or_leave_the_configured_remotes() {
    let r = rig("g1-push-escape", false);
    let (work, origin) = repo_with_origin(&r);
    // origin already has a `main` the Lead must not touch.
    git(&work, &["branch", "main"]);
    git(&work, &["push", "-q", "origin", "main"]);
    let main_before = origin_ref(&origin, "refs/heads/main");
    assert!(main_before.is_some());
    let elsewhere = r.ws.root().join("elsewhere.git");
    git(r.ws.root(), &["init", "-q", "--bare", "elsewhere.git"]);

    let cases = [
        ("origin", "+feat:main"),
        ("origin", ":main"),
        ("origin", "feat:refs/heads/main"),
        ("origin", "feat:main"),
        ("origin", "^feat"),
        ("origin", "refs/../x"),
        ("https://example.invalid/x.git", "feat"),
        (elsewhere.to_str().unwrap(), "feat"),
        ("nosuchremote", "feat"),
    ];
    for (remote, branch) in cases {
        let e = call(
            &r,
            "push",
            Some("lead-f"),
            json!({"repo": work, "remote": remote, "branch": branch}),
        )
        .unwrap_err();
        assert_eq!(e.code, "refused", "{remote} {branch}");
    }
    assert_eq!(origin_ref(&origin, "refs/heads/main"), main_before);
    assert!(!origin_has_feat(&origin));
    assert!(origin_ref(&elsewhere, "refs/heads/feat").is_none());
    let rows = authority::refusals(&conn(&r)).unwrap();
    assert_eq!(rows.len(), cases.len(), "each refusal is journaled");
    assert!(rows
        .iter()
        .all(|x| x.caller == "lead-f" && x.edge == "push"));

    // The same Lead still pushes a plain branch to the configured remote.
    call(&r, "push", Some("lead-f"), push_args(&work)).unwrap();
    assert!(origin_has_feat(&origin));
}

#[test]
fn push_of_a_repo_outside_the_callers_project_is_refused() {
    let r = rig("g1-push-repo", false);
    let (work, origin) = repo_with_origin(&r);
    conn(&r)
        .execute(
            "UPDATE project SET repo_path = '/elsewhere' WHERE slug = 'alpha'",
            [],
        )
        .unwrap();
    let e = call(&r, "push", Some("lead-f"), push_args(&work)).unwrap_err();
    assert_eq!(e.code, "refused");
    assert!(!origin_has_feat(&origin));
}
