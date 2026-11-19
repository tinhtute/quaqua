//! R7/R10/R14/Q15/Q17 wiring: the gate and `pr.merge` read the daemon-owned
//! tier rows (repo tier, plan lane keyed by repo key) and a full grant lowers
//! the context tier to low for the CoS only. Floor, push to main and fence
//! paths keep their verdicts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use qq_core::authority::tier;
use qq_core::authority::tier::Tier;
use qq_core::authority::{gate_evaluate_in, GateVerdict};
use qq_core::cli::Env;
use qq_core::daemon::{pr_merge, Config, Daemon, DaemonHandle};
use qq_core::herdr::{FakeHerdr, HerdrClient, Verb};
use qq_core::identity::peer::HerdrPeers;
use qq_core::rpc::{self, Request};
use qq_core::testkit::TestWorkspace;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const SPEND: &str = "mcp__plugin_tinyfish_tinyfish__top_up_wallet";

#[derive(Clone, Copy, PartialEq)]
enum Who {
    Cos,
    Worker,
}

struct Rig {
    ws: TestWorkspace,
    herdr: Arc<FakeHerdr>,
    daemon: Option<DaemonHandle>,
    repo: PathBuf,
}

fn rig(tag: &str, who: Who, diff: &str) -> Rig {
    let ws = TestWorkspace::new(tag);
    ws.seal_herdr();
    let herdr = Arc::new(FakeHerdr::new());
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: herdr.clone() as Arc<dyn HerdrClient>,
    });
    qq_core::merge::register(&mut d).unwrap();
    d.require_peer(Arc::new(HerdrPeers::new(herdr.clone())));
    let daemon = d.spawn().unwrap();
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
    let repo = ws.root().join("work");
    std::fs::create_dir_all(&repo).unwrap();
    let gh = ws.root().join("fake-gh");
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{log}'\ncase \"$1 $2\" in\n\
         'pr diff') for f in {diff}; do printf 'diff --git a/%s b/%s\\n' \"$f\" \"$f\"; done ;;\n\
         'pr view') echo {SHA} ;;\n\
         'pr checks') echo '[{{\"bucket\":\"pass\"}}]' ;;\n\
         'pr merge') exit 0 ;;\n\
         *) exit 9 ;;\nesac\n",
        log = ws.root().join("gh.log").display()
    );
    std::fs::write(&gh, script).unwrap();
    std::fs::set_permissions(&gh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    // The seam stands in for the unlisted repo: high, as production reads it.
    pr_merge::set_test_hooks(&ws.runs_dir(), gh, Tier::High, Tier::High);
    let state = json!({
        "input": { "repo": repo }, "stage": "gate", "reviewed": SHA, "findings": 0,
    });
    let c = db(&ws);
    c.execute(
        "INSERT INTO workflow (id, kind, state, started_ms, updated_ms) \
         VALUES ('wf-1', 'pipeline', 'running', 1, 1)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO snapshots (workflow_id, seq, state_json) VALUES ('wf-1', 1, ?1)",
        params![state.to_string()],
    )
    .unwrap();
    Rig {
        ws,
        herdr,
        daemon: Some(daemon),
        repo,
    }
}

fn db(ws: &TestWorkspace) -> Connection {
    qq_core::db::open(&ws.db_path()).unwrap()
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            self.herdr.clear(Verb::PaneList);
            let p = self.herdr.seed_pane("stop", None, &[]);
            self.herdr
                .agent_start("stopper", "claude", &p, 0, &[])
                .unwrap();
            self.herdr.set_pane_pids(&p, std::process::id(), &[]);
            let _ = d.stop();
        }
    }
}

impl Rig {
    fn key(&self) -> String {
        tier::repo_key(&self.repo)
    }

    fn tier_row(&self, table: &str, col: &str, key: &str, t: &str) {
        db(&self.ws)
            .execute(
                &format!(
                    "INSERT INTO {table} ({col}, tier, set_ms, set_by) VALUES (?1, ?2, 1, 'cos')"
                ),
                params![key, t],
            )
            .unwrap();
    }

    fn low_repo_and_lane(&self) {
        self.tier_row("repo_tier", "repo", &self.key(), "low");
        self.tier_row("plan_lane", "lane", &self.key(), "low");
    }

    fn grant(&self, repo: &Path) {
        db(&self.ws)
            .execute(
                "INSERT INTO grant_full (repo, granted_ms, granted_by) VALUES (?1, 1, 'cos')",
                [tier::repo_key(repo)],
            )
            .unwrap();
    }

    fn gate(&self, tool: &str, input: Value) -> GateVerdict {
        let hook = json!({"tool_name": tool, "tool_input": input, "cwd": self.repo});
        let env = Env {
            runs_dir: self.ws.runs_dir(),
        };
        gate_evaluate_in(&env, &hook.to_string())
    }

    fn bash(&self, cmd: &str) -> GateVerdict {
        self.gate("Bash", json!({ "command": cmd }))
    }

    fn merge(&self) -> Result<Value, rpc::RpcError> {
        let req = Request::new(
            "pr.merge",
            json!({"workflow": "wf-1", "pr": "7", "sha": SHA}),
        );
        rpc::call(&rpc::socket_path(&self.ws.runs_dir()), &req)
            .unwrap()
            .into_result()
    }

    fn merged(&self) -> bool {
        std::fs::read_to_string(self.ws.root().join("gh.log"))
            .unwrap_or_default()
            .contains("pr merge")
    }
}

fn is_ask(v: &GateVerdict) -> bool {
    matches!(v, GateVerdict::Ask(_))
}

fn is_block(v: &GateVerdict) -> bool {
    matches!(v, GateVerdict::Block(_))
}

#[test]
fn tier_source_wired_low_repo_and_low_lane_let_cos_tag() {
    let r = rig("tw-low", Who::Cos, "src/lib.rs");
    // Nothing listed: high, so the tag asks.
    assert!(is_ask(&r.bash("git tag v1")), "unlisted must ask");
    // A low repo row alone is not enough: the lane is still unknown (high).
    r.tier_row("repo_tier", "repo", &r.key(), "low");
    assert!(is_ask(&r.bash("git tag v1")), "unknown lane must ask");
    // Low repo + low lane (keyed by the repo key): the cos tags alone.
    r.tier_row("plan_lane", "lane", &r.key(), "low");
    assert_eq!(r.bash("git tag v1"), GateVerdict::Allow);
    // The action class still binds: a history rewrite is high on its own.
    assert!(is_ask(&r.bash("git filter-repo --force")));
}

#[test]
fn tier_source_wired_full_grant_lowers_merge_not_the_floor() {
    let r = rig("tw-grant", Who::Cos, "src/lib.rs");
    // Before the grant the high base refuses the merge.
    let e = r.merge().unwrap_err();
    assert_eq!(e.code, "refused");
    assert!(!r.merged());
    r.grant(&r.repo);
    let v = r.merge().unwrap();
    assert_eq!(v["merged"], true, "{v}");
    assert!(r.merged());
    // Floor still asks; main pushes still deny; a tag is now the cos's alone.
    assert!(is_ask(&r.gate(SPEND, json!({}))), "spend must ask");
    assert!(is_block(&r.bash("git push origin main")));
    assert!(is_block(&r.bash("git push --force origin feat")));
    assert_eq!(r.bash("git tag v1"), GateVerdict::Allow);
}

#[test]
fn tier_source_wired_grant_for_another_repo_lowers_nothing() {
    let r = rig("tw-other", Who::Cos, "src/lib.rs");
    let other = r.ws.root().join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    r.grant(&other);
    assert!(is_ask(&r.bash("git tag v1")));
    assert!(r.merge().is_err());
    assert!(!r.merged());
}

#[test]
fn tier_source_wired_fence_path_pr_under_grant_is_refused() {
    let r = rig(
        "tw-fence",
        Who::Cos,
        "src/lib.rs crates/qq-core/src/authority/gate.rs",
    );
    r.grant(&r.repo);
    let e = r.merge().unwrap_err();
    assert_eq!(e.code, "refused");
    assert!(e.message.contains("tier High"), "{}", e.message);
    assert!(!r.merged());
}

#[test]
fn tier_source_wired_worker_under_full_grant_is_denied() {
    let r = rig("tw-worker", Who::Worker, "src/lib.rs");
    r.grant(&r.repo);
    r.low_repo_and_lane();
    assert!(is_block(&r.bash("git tag v1")));
    assert!(is_block(&r.gate(SPEND, json!({}))));
    let e = r.merge().unwrap_err();
    assert_eq!(e.code, "refused");
    assert!(!r.merged());
}
