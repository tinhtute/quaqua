//! R14 write half and Q15 tier source: the caller-checked `grant.*` verbs write
//! daemon-owned rows, each change raises one herdr notification, and the tier
//! reader reads those rows (unlisted repo and unknown lane are high).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use qq_core::authority::tier::{self, Action, Decision, Tier};
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::herdr::{FakeHerdr, HerdrClient, Verb};
use qq_core::identity::peer::HerdrPeers;
use qq_core::rpc::{self, Request};
use qq_core::testkit::TestWorkspace;
use rusqlite::Connection;
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq)]
enum Who {
    Cos,
    Lead,
}

struct Rig {
    ws: TestWorkspace,
    herdr: Arc<FakeHerdr>,
    daemon: Option<DaemonHandle>,
}

fn me() -> u32 {
    std::process::id()
}

fn rig(tag: &str, who: Who) -> Rig {
    let ws = TestWorkspace::new(tag);
    ws.seal_herdr();
    let herdr = Arc::new(FakeHerdr::new());
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone() as Arc<dyn HerdrClient>,
    });
    d.require_peer(Arc::new(HerdrPeers::new(herdr.clone())));
    let daemon = d.spawn().unwrap();
    let (name, role) = match who {
        Who::Cos => ("cos", "cos"),
        Who::Lead => ("lead-1", "lead"),
    };
    let conn = db(&ws.runs_dir());
    conn.execute(
        "INSERT INTO agent (name, role, started_ms) VALUES (?1, ?2, 1)",
        rusqlite::params![name, role],
    )
    .unwrap();
    let p = if who == Who::Cos {
        herdr.seed_pane("c", Some("claude"), &[("grid_role", "cos")])
    } else {
        let p = herdr.seed_pane("w", None, &[]);
        herdr.agent_start(name, "claude", &p, 0, &[]).unwrap();
        p
    };
    herdr.set_pane_pids(&p, me(), &[]);
    Rig {
        ws,
        herdr,
        daemon: Some(daemon),
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            self.herdr.clear(Verb::PaneList);
            let p = self.herdr.seed_pane("stop", None, &[]);
            self.herdr
                .agent_start("stopper", "claude", &p, 0, &[])
                .unwrap();
            self.herdr.set_pane_pids(&p, me(), &[]);
            let _ = d.stop();
        }
    }
}

fn db(runs_dir: &Path) -> Connection {
    qq_core::db::open(&runs_dir.join("qq.db")).expect("open qq db")
}

impl Rig {
    fn runs(&self) -> PathBuf {
        self.ws.runs_dir()
    }

    fn repo(&self, name: &str) -> PathBuf {
        let p = self.ws.root().join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn call(&self, verb: &str, args: Value) -> Result<Value, String> {
        let resp = rpc::call(&rpc::socket_path(&self.runs()), &Request::new(verb, args))
            .map_err(|e| e.to_string())?;
        resp.into_result()
            .map_err(|e| format!("{}: {}", e.code, e.message))
    }

    fn count(&self, sql: &str) -> i64 {
        db(&self.runs()).query_row(sql, [], |r| r.get(0)).unwrap()
    }
}

// ---- grant_write_* ----

#[test]
fn grant_write_non_cos_refused() {
    let r = rig("grant-lead", Who::Lead);
    let repo = r.repo("proj");
    let args = json!({ "repo": repo });
    let e = r.call("grant.full", args).unwrap_err();
    assert!(e.starts_with("refused"), "{e}");
    let e = r
        .call("grant.tier", json!({ "repo": repo, "tier": "low" }))
        .unwrap_err();
    assert!(e.starts_with("refused"), "{e}");
    let e = r
        .call("grant.lane", json!({ "lane": "docs", "tier": "low" }))
        .unwrap_err();
    assert!(e.starts_with("refused"), "{e}");
    assert_eq!(r.count("SELECT COUNT(*) FROM grant_full"), 0);
    assert_eq!(r.count("SELECT COUNT(*) FROM repo_tier"), 0);
    assert_eq!(r.count("SELECT COUNT(*) FROM plan_lane"), 0);
    assert!(r.herdr.notifications().is_empty());
}

#[test]
fn grant_write_cos_ok() {
    let r = rig("grant-cos", Who::Cos);
    let repo = r.repo("proj");
    r.call("grant.full", json!({ "repo": repo })).unwrap();
    r.call("grant.tier", json!({ "repo": repo, "tier": "low" }))
        .unwrap();
    r.call("grant.lane", json!({ "lane": "docs", "tier": "medium" }))
        .unwrap();
    assert_eq!(r.count("SELECT COUNT(*) FROM grant_full"), 1);
    assert_eq!(tier::repo_tier(&r.runs(), &repo), Tier::Low);
    assert_eq!(tier::lane_tier_of(&r.runs(), "docs"), Tier::Medium);
}

#[test]
fn grant_write_rejects_bad_tier() {
    let r = rig("grant-bad", Who::Cos);
    let repo = r.repo("proj");
    let e = r
        .call("grant.tier", json!({ "repo": repo, "tier": "none" }))
        .unwrap_err();
    assert!(e.starts_with("bad_request"), "{e}");
    assert_eq!(r.count("SELECT COUNT(*) FROM repo_tier"), 0);
    assert!(r.herdr.notifications().is_empty());
}

#[test]
fn grant_write_notification_row_per_change() {
    let r = rig("grant-note", Who::Cos);
    let repo = r.repo("proj");
    r.call("grant.full", json!({ "repo": repo })).unwrap();
    assert_eq!(r.herdr.notifications().len(), 1);
    r.call("grant.tier", json!({ "repo": repo, "tier": "medium" }))
        .unwrap();
    assert_eq!(r.herdr.notifications().len(), 2);
    r.call("grant.lane", json!({ "lane": "docs", "tier": "low" }))
        .unwrap();
    assert_eq!(r.herdr.notifications().len(), 3);
    assert_eq!(r.count("SELECT COUNT(*) FROM grant_note"), 3);
    assert_eq!(
        r.count("SELECT COUNT(*) FROM grant_note WHERE notified = 1"),
        3
    );
}

// ---- tier_source_* ----

#[test]
fn tier_source_unlisted_repo_and_lane_are_high() {
    let r = rig("tier-unlisted", Who::Cos);
    let repo = r.repo("proj");
    assert_eq!(tier::repo_tier(&r.runs(), &repo), Tier::High);
    assert_eq!(tier::lane_tier_of(&r.runs(), "nope"), Tier::High);
    // No database at all reads as unlisted too.
    let nodb = r.ws.root().join("no-runs");
    assert_eq!(tier::repo_tier(&nodb, &repo), Tier::High);
    assert_eq!(tier::lane_tier_of(&nodb, "l"), Tier::High);
    assert!(!nodb.exists(), "a read never creates the database");
}

#[test]
fn tier_source_row_read_through() {
    let r = rig("tier-row", Who::Cos);
    let repo = r.repo("proj");
    for t in ["low", "medium", "high"] {
        r.call("grant.tier", json!({ "repo": repo, "tier": t }))
            .unwrap();
        let want = match t {
            "low" => Tier::Low,
            "medium" => Tier::Medium,
            _ => Tier::High,
        };
        assert_eq!(tier::repo_tier(&r.runs(), &repo), want);
    }
    // A subdirectory of a listed non-git path is its own key, so unlisted.
    let other = r.repo("other");
    assert_eq!(tier::repo_tier(&r.runs(), &other), Tier::High);
}

#[test]
fn tier_source_strictest_of_repo_lane_and_action() {
    let r = rig("tier-strict", Who::Cos);
    let repo = r.repo("proj");
    let set = |repo_t: &str, lane_t: &str| {
        r.call("grant.tier", json!({ "repo": repo, "tier": repo_t }))
            .unwrap();
        r.call("grant.lane", json!({ "lane": "l", "tier": lane_t }))
            .unwrap();
        (
            tier::repo_tier(&r.runs(), &repo),
            tier::lane_tier_of(&r.runs(), "l"),
        )
    };
    let (rt, lt) = set("low", "low");
    assert_eq!(tier::decide(true, Action::Tag, rt, lt), Decision::Allow);
    // Action class decides: history rewrite is high on its own.
    assert_eq!(
        tier::decide(true, Action::HistoryRewrite, rt, lt),
        Decision::Ask
    );
    let (rt, lt) = set("high", "low");
    assert_eq!(tier::decide(true, Action::Tag, rt, lt), Decision::Ask);
    let (rt, lt) = set("low", "high");
    assert_eq!(tier::decide(true, Action::Tag, rt, lt), Decision::Ask);
}
