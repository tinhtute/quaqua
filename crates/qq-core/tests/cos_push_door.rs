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
    let (work, origin) = repo_with_trunk_remote(&r);
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

fn rev(dir: &Path, rf: &str) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", rf])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A work repo plus a bare `origin` whose HEAD is `trunk` and holds `trunk`.
/// The work repo has no `refs/remotes/origin/HEAD`, and `trunk` is one commit
/// ahead of the remote's, so pushing it is a fast-forward.
fn repo_with_trunk_remote(r: &Rig) -> (PathBuf, PathBuf) {
    let (work, origin) = repo_with_origin(r);
    git(
        r.ws.root(),
        &["init", "-q", "--bare", "-b", "trunk", "o2.git"],
    );
    std::fs::remove_dir_all(&origin).unwrap();
    std::fs::rename(r.ws.root().join("o2.git"), &origin).unwrap();
    git(&work, &["branch", "trunk"]);
    git(&work, &["push", "-q", "origin", "trunk"]);
    git(&work, &["checkout", "-q", "trunk"]);
    std::fs::write(work.join("f"), "y").unwrap();
    git(&work, &["commit", "-qam", "ahead"]);
    git(&work, &["checkout", "-q", "feat"]);
    (work, origin)
}

#[test]
fn cos_push_door_remote_default_refused_without_local_head() {
    let r = rig("cpd-rdef", Who::Cos);
    let (work, origin) = repo_with_trunk_remote(&r);
    let before = rev(&origin, "refs/heads/trunk");
    let e = push(&r, args(&work, "trunk")).unwrap_err();
    assert_eq!(e.code, "refused");
    assert_eq!(rev(&origin, "refs/heads/trunk"), before, "trunk moved");
}

#[test]
fn cos_push_door_remote_default_refused_after_set_head_delete() {
    let r = rig("cpd-sethead", Who::Cos);
    let (work, origin) = repo_with_trunk_remote(&r);
    git(&work, &["fetch", "-q", "origin"]);
    git(&work, &["remote", "set-head", "origin", "trunk"]);
    git(&work, &["remote", "set-head", "origin", "-d"]);
    let before = rev(&origin, "refs/heads/trunk");
    assert_eq!(push(&r, args(&work, "trunk")).unwrap_err().code, "refused");
    assert_eq!(rev(&origin, "refs/heads/trunk"), before, "trunk moved");
}

#[test]
fn cos_push_door_remote_default_refused_via_second_remote() {
    let r = rig("cpd-o2", Who::Cos);
    let (work, origin) = repo_with_trunk_remote(&r);
    git(&work, &["remote", "add", "o2", origin.to_str().unwrap()]);
    let before = rev(&origin, "refs/heads/trunk");
    let mut a = args(&work, "trunk");
    a["remote"] = json!("o2");
    assert_eq!(push(&r, a).unwrap_err().code, "refused");
    assert_eq!(rev(&origin, "refs/heads/trunk"), before, "trunk moved");
}

#[test]
fn cos_push_door_case_variants_refused() {
    let r = rig("cpd-case", Who::Cos);
    let (work, origin) = repo_with_trunk_remote(&r);
    for b in ["Main", "MAIN", "Master", "MASTER", "Trunk", "TRUNK"] {
        let e = push(&r, args(&work, b)).unwrap_err();
        assert_eq!(e.code, "refused", "{b}: {e:?}");
        assert!(
            !has_ref(&origin, b) || b.eq_ignore_ascii_case("trunk"),
            "{b}"
        );
    }
}

#[test]
fn cos_push_door_unreadable_remote_refused() {
    let r = rig("cpd-unread", Who::Cos);
    let (work, _origin) = repo_with_trunk_remote(&r);
    let gone = r.ws.root().join("gone.git");
    git(
        &work,
        &["remote", "set-url", "origin", gone.to_str().unwrap()],
    );
    let e = push(&r, args(&work, "feat")).unwrap_err();
    assert_eq!(e.code, "refused", "{e:?}");
}

/// An empty bare remote has no readable default branch (an unborn HEAD is
/// invisible to `ls-remote`), so the first push to it needs the owner.
#[test]
fn cos_push_door_empty_remote_refused() {
    let r = rig("cpd-empty", Who::Cos);
    let (work, origin) = repo_with_origin(&r);
    assert_eq!(push(&r, args(&work, "feat")).unwrap_err().code, "refused");
    assert!(!has_ref(&origin, "feat"));
}

/// A bare repo `name` under the rig root with HEAD `head`, optionally holding a
/// `decoy`-style branch so it is not empty.
fn decoy_bare(r: &Rig, work: &Path, name: &str, head: &str, seed: bool) -> PathBuf {
    let p = r.ws.root().join(name);
    git(
        r.ws.root(),
        &["init", "-q", "--bare", "-b", head, p.to_str().unwrap()],
    );
    if seed {
        git(
            work,
            &[
                "push",
                "-q",
                p.to_str().unwrap(),
                &format!("HEAD:refs/heads/{head}"),
            ],
        );
    }
    p
}

/// The fetch URL names a decoy whose HEAD is not `trunk`, while the push URL
/// is the real origin (HEAD `trunk`). The door must check what git pushes to.
#[test]
fn cos_push_door_pushurl_decoy_fetch_url_refused() {
    let r = rig("cpd-pushurl-a", Who::Cos);
    let (work, origin) = repo_with_trunk_remote(&r);
    let decoy = decoy_bare(&r, &work, "decoy.git", "decoy", true);
    git(
        &work,
        &["config", "remote.origin.url", decoy.to_str().unwrap()],
    );
    git(
        &work,
        &["config", "remote.origin.pushurl", origin.to_str().unwrap()],
    );
    let before = rev(&origin, "refs/heads/trunk");
    assert_eq!(push(&r, args(&work, "trunk")).unwrap_err().code, "refused");
    assert_eq!(rev(&origin, "refs/heads/trunk"), before, "trunk moved");
}

/// An empty fetch URL reads as "no default"; the push URL is the real origin.
#[test]
fn cos_push_door_pushurl_empty_fetch_url_refused() {
    let r = rig("cpd-pushurl-b", Who::Cos);
    let (work, origin) = repo_with_trunk_remote(&r);
    let empty = decoy_bare(&r, &work, "empty.git", "decoy", false);
    git(
        &work,
        &["config", "remote.origin.url", empty.to_str().unwrap()],
    );
    git(
        &work,
        &["config", "remote.origin.pushurl", origin.to_str().unwrap()],
    );
    let before = rev(&origin, "refs/heads/trunk");
    assert_eq!(push(&r, args(&work, "trunk")).unwrap_err().code, "refused");
    assert_eq!(rev(&origin, "refs/heads/trunk"), before, "trunk moved");
}

/// `url.<real>.pushInsteadOf=<decoy>` redirects the push, not the fetch.
#[test]
fn cos_push_door_push_insteadof_refused() {
    let r = rig("cpd-pushurl-c", Who::Cos);
    let (work, origin) = repo_with_trunk_remote(&r);
    let empty = decoy_bare(&r, &work, "empty3.git", "decoy", false);
    git(
        &work,
        &["config", "remote.origin.url", empty.to_str().unwrap()],
    );
    git(
        &work,
        &[
            "config",
            &format!("url.{}.pushInsteadOf", origin.display()),
            empty.to_str().unwrap(),
        ],
    );
    let before = rev(&origin, "refs/heads/trunk");
    assert_eq!(push(&r, args(&work, "trunk")).unwrap_err().code, "refused");
    assert_eq!(rev(&origin, "refs/heads/trunk"), before, "trunk moved");
}

/// Several push URLs: every one must be proven, and any default refuses.
#[test]
fn cos_push_door_multiple_pushurls_each_checked() {
    let r = rig("cpd-multi", Who::Cos);
    let (work, origin) = repo_with_trunk_remote(&r);
    let other = decoy_bare(&r, &work, "other.git", "decoy", true);
    git(
        &work,
        &["config", "remote.origin.pushurl", other.to_str().unwrap()],
    );
    git(
        &work,
        &[
            "config",
            "--add",
            "remote.origin.pushurl",
            origin.to_str().unwrap(),
        ],
    );
    let before = rev(&origin, "refs/heads/trunk");
    assert_eq!(push(&r, args(&work, "trunk")).unwrap_err().code, "refused");
    assert_eq!(rev(&origin, "refs/heads/trunk"), before, "trunk moved");
    // An unreadable second push URL refuses even a harmless branch.
    let gone = r.ws.root().join("gone.git");
    git(
        &work,
        &[
            "config",
            "--add",
            "remote.origin.pushurl",
            gone.to_str().unwrap(),
        ],
    );
    assert_eq!(push(&r, args(&work, "feat")).unwrap_err().code, "refused");
    assert!(!has_ref(&origin, "feat"));
}

/// Chained `insteadOf` (alias -> origin, origin -> decoy): `get-url --push`
/// says origin, but `ls-remote -- origin` would read the decoy. A URL git
/// would rewrite a second time is unproven, so the push refuses.
#[test]
fn cos_push_door_chained_insteadof_refused() {
    let r = rig("cpd-chain", Who::Cos);
    let (work, origin) = repo_with_trunk_remote(&r);
    let decoy = decoy_bare(&r, &work, "decoy.git", "decoy", true);
    let alias = r.ws.root().join("alias.git");
    git(
        &work,
        &["config", "remote.origin.url", alias.to_str().unwrap()],
    );
    git(
        &work,
        &[
            "config",
            &format!("url.{}.insteadOf", origin.display()),
            alias.to_str().unwrap(),
        ],
    );
    git(
        &work,
        &[
            "config",
            &format!("url.{}.insteadOf", decoy.display()),
            origin.to_str().unwrap(),
        ],
    );
    let before = rev(&origin, "refs/heads/trunk");
    assert_eq!(push(&r, args(&work, "trunk")).unwrap_err().code, "refused");
    assert_eq!(rev(&origin, "refs/heads/trunk"), before, "trunk moved");
}

/// A URL in the `remote` slot is not a configured remote name.
#[test]
fn cos_push_door_url_as_remote_refused() {
    let r = rig("cpd-urlremote", Who::Cos);
    let (work, origin) = repo_with_trunk_remote(&r);
    let mut a = args(&work, "feat");
    a["remote"] = json!(origin.to_str().unwrap());
    assert_eq!(push(&r, a).unwrap_err().code, "refused");
    assert!(!has_ref(&origin, "feat"));
}
