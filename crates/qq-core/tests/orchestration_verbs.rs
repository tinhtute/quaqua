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
    std::fs::read_to_string(p).expect("read template team.toml")
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
    qq_core::merge::register(&mut d).unwrap();
    qq_core::config::INSTALL(&mut qq_core::cli::Registry::new(), &mut d).unwrap();
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

/// A work repo with one commit on `feat` and a local bare `origin` whose default
/// branch is `trunk` (seeded) and which lacks `feat`.
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
        &["init", "-q", "--bare", "-b", "trunk", "origin.git"],
    );
    git(
        &work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    // A real default branch: a bare remote whose HEAD names an unborn branch
    // has no provable default, and the push guard refuses it (fail closed).
    git(&work, &["push", "-q", "origin", "feat:refs/heads/trunk"]);
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
    let c = c;
    c.execute(
        "INSERT INTO workflow (id, kind, state, started_ms, updated_ms) VALUES ('worker:w', 'k', 's', 0, 0)",
        [],
    )
    .unwrap();
    let run_id = run::open(
        &c,
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

#[test]
fn push_refuses_main_master_and_the_remote_default_branch() {
    let r = rig("g1-push-mainguard", false);
    let (work, origin) = repo_with_origin(&r);
    // A default branch named neither main nor master, known only via the remote HEAD.
    git(&work, &["branch", "trunk"]);
    git(&work, &["branch", "main"]);
    git(&work, &["branch", "master"]);
    git(
        &work,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    let protected = ["main", "master", "trunk"];
    // `trunk` is the remote's seeded default: it must stay exactly where it is.
    let trunk0 = origin_ref(&origin, "refs/heads/trunk");
    assert!(trunk0.is_some());
    for branch in protected {
        let e = call(
            &r,
            "push",
            Some("lead-f"),
            json!({"repo": work, "remote": "origin", "branch": branch}),
        )
        .unwrap_err();
        assert_eq!(e.code, "refused", "{branch}");
        assert_eq!(
            origin_ref(&origin, &format!("refs/heads/{branch}")),
            if branch == "trunk" {
                trunk0.clone()
            } else {
                None
            },
            "{branch} must not reach the remote"
        );
    }
    let rows = authority::refusals(&conn(&r)).unwrap();
    assert_eq!(rows.len(), protected.len(), "each refusal is journaled");
    // Every other allowed push behaves as before.
    call(&r, "push", Some("lead-f"), push_args(&work)).unwrap();
    assert!(origin_has_feat(&origin));
}

/// The Lead verb refuses a remote with no refs: its unborn HEAD is invisible,
/// so a first push could create the remote's default branch.
#[test]
fn push_refuses_an_empty_remote() {
    let r = rig("g1-push-empty", false);
    let (work, _origin) = repo_with_origin(&r);
    git(
        r.ws.root(),
        &["init", "-q", "--bare", "-b", "trunk", "empty.git"],
    );
    git(
        &work,
        &[
            "remote",
            "add",
            "empty",
            r.ws.root().join("empty.git").to_str().unwrap(),
        ],
    );
    let e = call(
        &r,
        "push",
        Some("lead-f"),
        json!({"repo": work, "remote": "empty", "branch": "feat"}),
    )
    .unwrap_err();
    assert_eq!(e.code, "refused", "{e:?}");
    assert!(origin_ref(&r.ws.root().join("empty.git"), "refs/heads/feat").is_none());
}

/// The push URL flips between a decoy (HEAD `other`) and the real origin (HEAD
/// `trunk`) while the Lead pushes `trunk`: the real trunk must never move.
#[test]
fn push_pushurl_flip_race_never_moves_default() {
    let r = rig("g1-push-race", false);
    let (work, origin) = repo_with_origin(&r);
    git(&work, &["checkout", "-q", "-b", "trunk"]);
    std::fs::write(work.join("f"), "y").unwrap();
    git(&work, &["commit", "-qam", "ahead"]);
    git(&work, &["checkout", "-q", "feat"]);
    let decoy = r.ws.root().join("decoy.git");
    git(
        r.ws.root(),
        &["init", "-q", "--bare", "-b", "other", "decoy.git"],
    );
    git(
        &work,
        &[
            "push",
            "-q",
            decoy.to_str().unwrap(),
            "feat:refs/heads/other",
        ],
    );
    let before = origin_ref(&origin, "refs/heads/trunk");
    let (o, d) = (
        origin.to_str().unwrap().to_string(),
        decoy.to_str().unwrap().to_string(),
    );
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flipper = {
        let (stop, work) = (stop.clone(), work.clone());
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let u = if n.is_multiple_of(2) { &d } else { &o };
                let _ = Command::new("git")
                    .arg("-C")
                    .arg(&work)
                    .args(["config", "remote.origin.pushurl", u])
                    .output();
                n += 1;
            }
        })
    };
    let mut moved = None;
    for i in 0..150 {
        let _ = call(
            &r,
            "push",
            Some("lead-f"),
            json!({"repo": work, "remote": "origin", "branch": "trunk"}),
        );
        if origin_ref(&origin, "refs/heads/trunk") != before {
            moved = Some(i);
            break;
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    flipper.join().unwrap();
    assert_eq!(
        moved, None,
        "the real default branch moved on call {moved:?}"
    );
}

/// `repo_with_origin`'s work repo plus `integ` (at the base) and `t1` (one commit ahead).
fn merge_repo(r: &Rig) -> (std::path::PathBuf, String) {
    let (work, _origin) = repo_with_origin(r);
    git(&work, &["branch", "integ"]);
    git(&work, &["checkout", "-q", "-b", "t1"]);
    std::fs::write(work.join("t1.txt"), "t1").unwrap();
    git(&work, &["add", "t1.txt"]);
    git(&work, &["commit", "-qm", "t1"]);
    let tip = git(&work, &["rev-parse", "t1"]);
    git(&work, &["checkout", "-q", "feat"]);
    (work, tip)
}

fn merge_args(work: &Path, integ: &str, tip: &str) -> Value {
    json!({"repo": work, "integ": integ, "entries": [{"branch": "t1", "sha": tip}]})
}

#[test]
fn merge_refuses_a_peer_and_no_caller_and_journals_it() {
    let r = rig("g3-merge-caller", false);
    let (work, tip) = merge_repo(&r);
    let before = git(&work, &["rev-parse", "integ"]);
    let peer = call(&r, "merge", Some("p1"), merge_args(&work, "integ", &tip)).unwrap_err();
    let anon = call(&r, "merge", None, merge_args(&work, "integ", &tip)).unwrap_err();
    assert_eq!(
        (peer.code.as_str(), anon.code.as_str()),
        ("refused", "refused")
    );
    assert_eq!(
        git(&work, &["rev-parse", "integ"]),
        before,
        "nothing merged"
    );
    let rows = authority::refusals(&conn(&r)).unwrap();
    assert_eq!(
        rows.iter()
            .map(|x| (x.caller.as_str(), x.edge.as_str()))
            .collect::<Vec<_>>(),
        [("p1", "orchestration")]
    );
    // A Lead still merges into a plain integration branch.
    let ok = call(
        &r,
        "merge",
        Some("lead-f"),
        merge_args(&work, "integ", &tip),
    )
    .unwrap();
    assert_eq!(ok["merged"][0]["merged_now"], true);
}

#[test]
fn merge_never_runs_a_shell_string_from_the_request() {
    let r = rig("g3-merge-check", false);
    let (work, tip) = merge_repo(&r);
    let marker = r.ws.root().join("pwned");
    let mut args = merge_args(&work, "integ", &tip);
    args["check"] = json!(format!("touch {}", marker.display()));
    let e = call(&r, "merge", Some("lead-f"), args).unwrap_err();
    assert_eq!(e.code, "bad_request");
    assert!(!marker.exists(), "the request's check must never run");
    // The check is the daemon's: the `merge.check` setting.
    conn(&r)
        .execute(
            "INSERT OR REPLACE INTO setting (key, value_json) VALUES ('merge.check', '\"false\"')",
            [],
        )
        .unwrap();
    let e = call(
        &r,
        "merge",
        Some("lead-f"),
        merge_args(&work, "integ", &tip),
    )
    .unwrap_err();
    assert_eq!(e.code, "base_broken", "the configured check ran and failed");
}

#[test]
fn merge_refuses_main_master_and_the_remote_default_branch() {
    let r = rig("g3-merge-protected", false);
    let (work, tip) = merge_repo(&r);
    for b in ["main", "master", "trunk"] {
        git(&work, &["branch", b, "feat"]);
    }
    git(
        &work,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    let heads: Vec<String> = ["main", "master", "trunk"]
        .iter()
        .map(|b| git(&work, &["rev-parse", b]))
        .collect();
    for b in ["main", "master", "trunk"] {
        let e = call(&r, "merge", Some("lead-f"), merge_args(&work, b, &tip)).unwrap_err();
        assert_eq!(e.code, "refused", "{b}");
    }
    let after: Vec<String> = ["main", "master", "trunk"]
        .iter()
        .map(|b| git(&work, &["rev-parse", b]))
        .collect();
    assert_eq!(heads, after, "no protected branch moved");
    assert_eq!(
        authority::refusals(&conn(&r)).unwrap().len(),
        3,
        "each refusal is journaled"
    );
}

fn cfg(r: &Rig, caller: Option<&str>, argv: &[&str]) -> Result<Value, rpc::RpcError> {
    call(r, "config", caller, json!({ "argv": argv }))
}

fn setting_of(r: &Rig, key: &str) -> Option<String> {
    conn(r)
        .query_row(
            "SELECT value_json FROM setting WHERE key = ?1",
            [key],
            |x| x.get(0),
        )
        .ok()
}

#[test]
fn config_writes_refuse_a_peer_and_no_caller_but_reads_stay_open() {
    let r = rig("g3-config-write", false);
    let toml = r.ws.root().join("t.toml");
    std::fs::write(&toml, template()).unwrap();
    let toml = toml.to_str().unwrap();
    for argv in [vec!["set", "limits.depth", "9"], vec!["import", toml]] {
        assert_eq!(
            cfg(&r, Some("p1"), &argv).unwrap_err().code,
            "refused",
            "{argv:?}"
        );
        assert_eq!(
            cfg(&r, None, &argv).unwrap_err().code,
            "refused",
            "{argv:?}"
        );
    }
    assert_ne!(setting_of(&r, "limits.depth").as_deref(), Some("9"));
    let rows = authority::refusals(&conn(&r)).unwrap();
    assert!(
        rows.iter()
            .any(|x| x.caller == "p1" && x.edge == "orchestration"),
        "{rows:?}"
    );
    // reads stay open to a Peer
    assert!(cfg(&r, Some("p1"), &["get", "limits.depth"]).is_ok());
    assert!(cfg(&r, Some("p1"), &["validate"]).is_ok());
    assert!(call(&r, "roster", Some("p1"), json!({"argv": []})).is_ok());
    // a Lead still writes
    cfg(&r, Some("lead-f"), &["set", "limits.depth", "9"]).unwrap();
    assert_eq!(setting_of(&r, "limits.depth").as_deref(), Some("9"));
}

#[test]
fn a_peer_cannot_plant_a_merge_check_for_the_next_lead_merge() {
    let r = rig("g3-config-merge", false);
    let (work, tip) = merge_repo(&r);
    let marker = r.ws.root().join("pwned");
    let planted = format!("touch {}", marker.display());
    let _ = cfg(&r, Some("p1"), &["set", "merge.check", &planted]);
    let ok = call(
        &r,
        "merge",
        Some("lead-f"),
        merge_args(&work, "integ", &tip),
    );
    assert!(ok.is_ok(), "{ok:?}");
    assert!(!marker.exists(), "a Peer's planted check must never run");
}

// ---- pr_merge_* / fence_path_tier_*: the one merge door (R10, R13) ----

mod pr_merge_door {
    use super::*;
    use qq_core::authority::tier::Tier;
    use qq_core::daemon::pr_merge;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba98";

    #[derive(Clone, Copy, PartialEq)]
    enum Who {
        Cos,
        Worker,
    }

    struct Door {
        r: Rig,
        log: std::path::PathBuf,
    }

    /// A rig whose peer (this test process) is `who`, a fake `gh` that logs its
    /// argv and answers from `diff`/`head`/`checks`, and a workflow `wf` whose
    /// journal says `reviewed` passed review at stage `stage`.
    #[allow(clippy::too_many_arguments)]
    fn door(
        tag: &str,
        who: Who,
        base: Tier,
        diff: &str,
        head: &str,
        checks: &str,
        reviewed: &str,
        stage: &str,
    ) -> Door {
        let r = rig(tag, true);
        let pid = std::process::id();
        match who {
            Who::Cos => {
                let p = r
                    .herdr
                    .seed_pane("c", Some("claude"), &[("grid_role", "cos")]);
                r.herdr.set_pane_pids(&p, pid, &[]);
            }
            Who::Worker => {
                let p = r.herdr.seed_pane("w", None, &[]);
                r.herdr.agent_start("w-1", "claude", &p, 0, &[]).unwrap();
                r.herdr.set_pane_pids(&p, pid, &[]);
            }
        }
        std::fs::create_dir_all(r.ws.root().join("work")).unwrap();
        let log = r.ws.root().join("gh.log");
        let gh = r.ws.root().join("fake-gh");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{log}'\ncase \"$1 $2\" in\n\
             'pr diff') {diff} ;;\n\
             'pr view') echo {head} ;;\n\
             'pr checks') {checks} ;;\n\
             'pr merge') exit 0 ;;\n\
             *) exit 9 ;;\nesac\n",
            log = log.display()
        );
        std::fs::write(&gh, script).unwrap();
        std::fs::set_permissions(&gh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        pr_merge::set_test_hooks(&r.ws.runs_dir(), gh, base, base);
        let state = json!({
            "input": { "repo": r.ws.root().join("work") },
            "stage": stage, "reviewed": reviewed, "findings": 0,
        });
        conn(&r)
            .execute(
                "INSERT INTO workflow (id, kind, state, started_ms, updated_ms) \
                 VALUES ('wf-1', 'pipeline', 'running', 1, 1)",
                [],
            )
            .unwrap();
        conn(&r)
            .execute(
                "INSERT INTO snapshots (workflow_id, seq, state_json) VALUES ('wf-1', 1, ?1)",
                params![state.to_string()],
            )
            .unwrap();
        Door { r, log }
    }

    /// A shell snippet that prints `diff --git` headers for the named files.
    fn names(files: &str) -> String {
        format!("for f in {files}; do printf 'diff --git a/%s b/%s\\n' \"$f\" \"$f\"; done")
    }

    fn green(tag: &str, who: Who, base: Tier) -> Door {
        door(
            tag,
            who,
            base,
            &names("src/lib.rs"),
            SHA,
            "echo '[{\"bucket\":\"pass\"}]'",
            SHA,
            "gate",
        )
    }

    fn merge(d: &Door, sha: &str) -> Result<Value, rpc::RpcError> {
        call(
            &d.r,
            "pr.merge",
            None,
            json!({"workflow": "wf-1", "pr": "7", "sha": sha}),
        )
    }

    fn gh_log(d: &Door) -> String {
        std::fs::read_to_string(&d.log).unwrap_or_default()
    }

    fn assert_refused_no_merge(d: &Door, res: Result<Value, rpc::RpcError>, why: &str) {
        let e = res.unwrap_err();
        assert_eq!(e.code, "refused");
        assert!(e.message.contains(why), "want '{why}' in: {}", e.message);
        assert!(
            !gh_log(d).contains("pr merge"),
            "gh merge ran: {}",
            gh_log(d)
        );
    }

    #[test]
    fn pr_merge_refuses_a_non_cos_caller() {
        let d = green("prm-worker", Who::Worker, Tier::Medium);
        let res = merge(&d, SHA);
        assert_refused_no_merge(&d, res, "only the CoS");
    }

    #[test]
    fn pr_merge_refuses_a_claimed_cos_without_a_peer_gate() {
        // No resolver: the request's own claim of `cos` must not stand.
        let r = rig("prm-claim", false);
        let res = call(
            &r,
            "pr.merge",
            Some("cos"),
            json!({"workflow": "wf-1", "pr": "7", "sha": SHA}),
        );
        assert_eq!(res.unwrap_err().code, "refused");
    }

    #[test]
    fn pr_merge_refuses_a_sha_that_is_not_the_reviewed_sha() {
        let d = green("prm-sha", Who::Cos, Tier::Medium);
        let res = merge(&d, OTHER);
        assert_refused_no_merge(&d, res, "not the reviewed sha");
    }

    #[test]
    fn pr_merge_refuses_a_sha_still_under_review() {
        let d = door(
            "prm-stage",
            Who::Cos,
            Tier::Medium,
            &names("src/lib.rs"),
            SHA,
            "echo '[{\"bucket\":\"pass\"}]'",
            SHA,
            "review",
        );
        let res = merge(&d, SHA);
        assert_refused_no_merge(&d, res, "no clean reviewed sha");
    }

    #[test]
    fn pr_merge_refuses_a_red_check() {
        let d = door(
            "prm-red",
            Who::Cos,
            Tier::Medium,
            &names("src/lib.rs"),
            SHA,
            "echo '[{\"bucket\":\"fail\"}]'; exit 1",
            SHA,
            "gate",
        );
        let res = merge(&d, SHA);
        assert_refused_no_merge(&d, res, "checks are not green");
    }

    #[test]
    fn pr_merge_refuses_when_the_pr_head_is_not_the_sha() {
        let d = door(
            "prm-head",
            Who::Cos,
            Tier::Medium,
            &names("src/lib.rs"),
            OTHER,
            "echo '[{\"bucket\":\"pass\"}]'",
            SHA,
            "gate",
        );
        let res = merge(&d, SHA);
        assert_refused_no_merge(&d, res, "PR head");
    }

    #[test]
    fn pr_merge_refuses_a_high_tier_repo() {
        let d = green("prm-high", Who::Cos, Tier::High);
        let res = merge(&d, SHA);
        assert_refused_no_merge(&d, res, "tier High");
    }

    #[test]
    fn pr_merge_happy_path_runs_gh_with_match_head_commit() {
        let d = green("prm-ok", Who::Cos, Tier::Medium);
        let v = merge(&d, SHA).unwrap();
        assert_eq!(v["merged"], true, "{v}");
        let log = gh_log(&d);
        assert!(
            log.lines().any(|l| l.starts_with("pr merge 7")
                && l.contains(&format!("--match-head-commit {SHA}"))),
            "{log}"
        );
        assert!(!log.contains("--admin") && !log.contains("--auto"), "{log}");
    }

    /// Rewrites the fake gh so `pr merge` hangs and `pr view --json state,...`
    /// answers `view_state`; a 400 ms limit stands in for the production minute.
    fn hang_merge(d: &Door, view_state: &str) {
        let gh = d.r.ws.root().join("fake-gh");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{log}'\ncase \"$1 $2\" in\n\
             'pr diff') {diff} ;;\n\
             'pr view') case \"$*\" in *state*) {view_state} ;; *) echo {SHA} ;; esac ;;\n\
             'pr checks') echo '[{{\"bucket\":\"pass\"}}]' ;;\n\
             'pr merge') exec sleep 30 ;;\n\
             *) exit 9 ;;\nesac\n",
            log = d.log.display(),
            diff = names("src/lib.rs"),
        );
        std::fs::write(&gh, script).unwrap();
        pr_merge::set_test_gh_limit(&d.r.ws.runs_dir(), std::time::Duration::from_millis(400));
    }

    #[test]
    fn pr_merge_timeout_with_the_pr_merged_reports_merged() {
        let d = green("prm-to-merged", Who::Cos, Tier::Medium);
        hang_merge(&d, "echo '{\"state\":\"MERGED\",\"mergeCommit\":{\"oid\":\"abc\"}}'");
        let v = merge(&d, SHA).unwrap();
        assert_eq!(v["merged"], true, "{v}");
        assert!(gh_log(&d).contains("state,mergeCommit"), "{}", gh_log(&d));
    }

    #[test]
    fn pr_merge_timeout_with_the_pr_open_is_merge_unknown() {
        let d = green("prm-to-open", Who::Cos, Tier::Medium);
        hang_merge(&d, "echo '{\"state\":\"OPEN\",\"mergeCommit\":null}'");
        let e = merge(&d, SHA).unwrap_err();
        assert_eq!(e.code, "merge_unknown", "{}", e.message);
    }

    #[test]
    fn pr_merge_timeout_with_an_unreadable_recheck_is_merge_unknown() {
        let d = green("prm-to-unread", Who::Cos, Tier::Medium);
        hang_merge(&d, "exit 1");
        let e = merge(&d, SHA).unwrap_err();
        assert_eq!(e.code, "merge_unknown", "{}", e.message);
    }

    #[test]
    fn pr_merge_never_runs_a_request_string_through_a_shell() {
        let d = green("prm-inject", Who::Cos, Tier::Medium);
        let marker = d.r.ws.root().join("pwned");
        let bad = format!("7; touch {}", marker.display());
        let res = call(
            &d.r,
            "pr.merge",
            None,
            json!({"workflow": "wf-1", "pr": bad, "sha": SHA}),
        );
        assert_refused_no_merge(&d, res, "PR number");
        assert!(!marker.exists());
    }

    #[test]
    fn fence_path_tier_names_every_fence_path_and_no_neighbour() {
        for p in [
            "crates/qq-core/src/authority/gate.rs",
            "crates/qq-core/src/identity/peer.rs",
            "crates/qq-core/src/merge/mod.rs",
            "crates/qq-core/src/daemon/pr_merge.rs",
            "crates/qq-core/templates/workspace/scripts/hooks/herdr-gate.sh",
            ".claude/settings.json",
            "crates/qq-core/templates/workspace/AGENTS.md",
            "crates/qq-core/templates/workspace/.claude/settings.json",
        ] {
            assert!(pr_merge::fence_path(p), "{p}");
        }
        for p in [
            "crates/qq-core/src/authority_x/a.rs",
            "crates/qq-core/src/task/spawn.rs",
            "crates/qq-core/templates/workspace/rules/_all/a.md",
            "crates/qq-core/templates/seed/AGENTS.md",
            "docs/AGENTS.md",
            "README.md",
        ] {
            assert!(!pr_merge::fence_path(p), "{p}");
        }
    }

    #[test]
    fn fence_path_tier_is_high_whatever_the_base_tier() {
        let fence = vec![
            "README.md".to_string(),
            "crates/qq-core/src/authority/tier.rs".to_string(),
        ];
        let plain = vec!["README.md".to_string()];
        assert_eq!(pr_merge::pr_tier(Tier::Low, Tier::Low, &fence), Tier::High);
        assert_eq!(pr_merge::pr_tier(Tier::Low, Tier::Low, &plain), Tier::Low);
    }

    #[test]
    fn fence_path_tier_refuses_a_fence_diff_under_a_medium_grant() {
        let d = door(
            "prm-fence",
            Who::Cos,
            Tier::Medium,
            &names("src/lib.rs crates/qq-core/src/daemon/mod.rs"),
            SHA,
            "echo '[{\"bucket\":\"pass\"}]'",
            SHA,
            "gate",
        );
        let res = merge(&d, SHA);
        assert_refused_no_merge(&d, res, "tier High");
    }

    #[test]
    fn fence_path_tier_treats_an_unreadable_diff_as_high() {
        let d = door(
            "prm-nodiff",
            Who::Cos,
            Tier::Medium,
            "x; exit 1",
            SHA,
            "echo '[{\"bucket\":\"pass\"}]'",
            SHA,
            "gate",
        );
        let res = merge(&d, SHA);
        assert_refused_no_merge(&d, res, "tier High");
    }

    #[test]
    fn fence_path_tier_sees_a_rename_out_of_a_fence_dir() {
        // `--name-only` lists only the new side; the old side is the fence.
        let rename = "printf 'diff --git a/crates/qq-core/src/authority/gate.rs b/elsewhere/gate.rs\\nsimilarity index 100%%\\nrename from crates/qq-core/src/authority/gate.rs\\nrename to elsewhere/gate.rs\\n'";
        let d = door(
            "prm-rename",
            Who::Cos,
            Tier::Medium,
            rename,
            SHA,
            "echo '[{\"bucket\":\"pass\"}]'",
            SHA,
            "gate",
        );
        let res = merge(&d, SHA);
        assert_refused_no_merge(&d, res, "tier High");
    }

    #[test]
    fn fence_path_tier_sees_a_quoted_fence_path() {
        let quoted =
            "printf 'diff --git \"a/crates/qq-core/src/daemon/\\\\303\\\\251.rs\" \"b/x\"\\n'";
        let d = door(
            "prm-quoted",
            Who::Cos,
            Tier::Medium,
            quoted,
            SHA,
            "echo '[{\"bucket\":\"pass\"}]'",
            SHA,
            "gate",
        );
        let res = merge(&d, SHA);
        assert_refused_no_merge(&d, res, "tier High");
    }
}
