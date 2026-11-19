//! `push.cos` (R10, Q16, R7): the CoS pushes a non-main branch through a
//! caller-checked daemon door. Every remote here is a local bare repo.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use qq_core::authority;
use qq_core::daemon::{push_door, Config, Daemon, DaemonHandle};
use qq_core::db;
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::identity::peer::HerdrPeers;
use qq_core::rpc::{self, Request};
use qq_core::testkit::TestWorkspace;
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq)]
enum Who {
    Cos,
    Worker,
}

struct Rig {
    ws: TestWorkspace,
    daemon: DaemonHandle,
}

/// A rig whose socket peer (this test process) is `who`.
fn rig(tag: &str, who: Who) -> Rig {
    let ws = TestWorkspace::new(tag);
    ws.seal_herdr();
    drop(db::open(&ws.db_path()).unwrap());
    let herdr = Arc::new(FakeHerdr::new());
    let pid = std::process::id();
    match who {
        Who::Cos => {
            let p = herdr.seed_pane("c", Some("claude"), &[("grid_role", "cos")]);
            herdr.set_pane_pids(&p, pid, &[]);
        }
        Who::Worker => {
            let p = herdr.seed_pane("w", None, &[]);
            herdr.agent_start("w-1", "claude", &p, 0, &[]).unwrap();
            herdr.set_pane_pids(&p, pid, &[]);
        }
    }
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone() as Arc<dyn HerdrClient>,
    });
    d.require_peer(Arc::new(HerdrPeers::new(herdr)));
    let daemon = d.spawn().unwrap();
    Rig { ws, daemon }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A work repo with branches `feat`, `main` and a local bare `origin` lacking them.
fn repo_with_origin(r: &Rig) -> (PathBuf, PathBuf) {
    let work = r.ws.root().join("work");
    let origin = r.ws.root().join("origin.git");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "feat"]);
    git(&work, &["config", "user.email", "t@example.invalid"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("f"), "x").unwrap();
    git(&work, &["add", "f"]);
    git(&work, &["commit", "-qm", "c"]);
    git(&work, &["branch", "main"]);
    git(r.ws.root(), &["init", "-q", "--bare", "origin.git"]);
    git(
        &work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    (work, origin)
}

fn has_ref(origin: &Path, branch: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(origin)
        .args([
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}"),
        ])
        .status()
        .unwrap()
        .success()
}

fn push(r: &Rig, args: Value) -> Result<Value, rpc::RpcError> {
    let req = Request::new(push_door::VERB, args);
    rpc::call(&r.daemon.socket, &req).unwrap().into_result()
}

fn args(work: &Path, branch: &str) -> Value {
    json!({"repo": work, "remote": "origin", "branch": branch})
}

fn refusals(r: &Rig) -> usize {
    authority::refusals(&db::open(&r.ws.db_path()).unwrap())
        .unwrap()
        .len()
}

#[test]
fn cos_push_door_cos_non_main_allow() {
    let r = rig("cpd-allow", Who::Cos);
    let (work, origin) = repo_with_origin(&r);
    let out = push(&r, args(&work, "feat")).unwrap();
    assert_eq!(out["pushed"], true);
    assert!(has_ref(&origin, "feat"));
    assert_eq!(refusals(&r), 0);
}

#[test]
fn cos_push_door_cos_main_refuse() {
    let r = rig("cpd-main", Who::Cos);
    let (work, origin) = repo_with_origin(&r);
    git(&work, &["branch", "master"]);
    git(&work, &["branch", "trunk"]);
    git(
        &work,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    for b in ["main", "master", "trunk"] {
        let e = push(&r, args(&work, b)).unwrap_err();
        assert_eq!(e.code, "refused", "{b}");
        assert!(!has_ref(&origin, b), "{b} reached the remote");
    }
    assert_eq!(refusals(&r), 3, "each refusal is journaled");
}

#[test]
fn cos_push_door_cos_force_push_refuse() {
    let r = rig("cpd-force", Who::Cos);
    let (work, origin) = repo_with_origin(&r);
    let mut forced = args(&work, "feat");
    forced["force"] = json!(true);
    assert_eq!(push(&r, forced).unwrap_err().code, "refused");
    // A forcing refspec smuggled through the branch is refused too.
    for b in ["+feat", "feat:main", ":feat"] {
        assert_eq!(push(&r, args(&work, b)).unwrap_err().code, "refused", "{b}");
    }
    assert!(!has_ref(&origin, "feat"));
    assert!(!has_ref(&origin, "main"));
}

#[test]
fn cos_push_door_worker_refuse() {
    let r = rig("cpd-worker", Who::Worker);
    let (work, origin) = repo_with_origin(&r);
    let e = push(&r, args(&work, "feat")).unwrap_err();
    assert_eq!(e.code, "refused");
    assert!(!has_ref(&origin, "feat"));
    assert_eq!(refusals(&r), 1);
}

#[test]
fn cos_push_door_claimed_cos_without_peer_gate_refuse() {
    let ws = TestWorkspace::new("cpd-claim");
    ws.seal_herdr();
    drop(db::open(&ws.db_path()).unwrap());
    let herdr = Arc::new(FakeHerdr::new());
    let d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr as Arc<dyn HerdrClient>,
    });
    let daemon = d.spawn().unwrap();
    let r = Rig { ws, daemon };
    let (work, origin) = repo_with_origin(&r);
    let mut req = Request::new(push_door::VERB, args(&work, "feat"));
    req.caller = Some("cos".into());
    let e = rpc::call(&r.daemon.socket, &req)
        .unwrap()
        .into_result()
        .unwrap_err();
    assert_eq!(e.code, "refused");
    assert!(!has_ref(&origin, "feat"));
}

#[test]
fn cos_push_door_daemon_down_refuse() {
    let ws = TestWorkspace::new("cpd-down");
    ws.seal_herdr();
    let err = push_door::push(&ws.runs_dir(), Path::new("/nowhere"), "origin", "feat").unwrap_err();
    assert!(err.contains("no daemon answer"), "{err}");
}
