//! T55b: the spawn verb of a Lead enforces the protocol worker cap beside
//! `roles.lead.max_peers` (the stricter of the two refuses).

use std::path::{Path, PathBuf};
use std::process::Command;

use qq_core::config;
use qq_core::db;
use qq_core::driver::Registry;
use qq_core::herdr::FakeHerdr;
use qq_core::task::worker_flow::spawn_cmd::{run_worker, Ctx};
use qq_core::testkit::TestWorkspace;
use rusqlite::Connection;

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
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

struct Fx {
    ws: TestWorkspace,
    repo: PathBuf,
    conn: Connection,
    herdr: FakeHerdr,
    anchor: String,
    drivers: Registry,
}

impl Fx {
    fn new(tag: &str, protocol: &str, max_peers: &str) -> Self {
        let ws = TestWorkspace::new(tag);
        let repo = ws.root().join("code").join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        if !protocol.is_empty() {
            std::fs::write(repo.join("WORKSPACE_PROTOCOL.md"), protocol).unwrap();
        }
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        let mut conn = db::open(&ws.db_path()).expect("open db");
        let team = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
        )
        .unwrap();
        config::seed(&mut conn, &team).expect("seed");
        conn.execute(
            "INSERT INTO setting (key, value_json) VALUES ('roles.lead.max_peers', ?1) \
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
            [max_peers],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project (slug, repo_path, created_ms) VALUES ('alpha', ?1, 1)",
            [std::fs::canonicalize(&repo)
                .unwrap()
                .to_string_lossy()
                .to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent (name, role, project_id, started_ms) \
             VALUES ('alpha-lead', 'lead', (SELECT id FROM project WHERE slug='alpha'), 1)",
            [],
        )
        .unwrap();
        let herdr = FakeHerdr::new();
        let anchor = herdr.seed_pane("home", None, &[]);
        let harnesses: Vec<_> = config::roster(&conn)
            .unwrap()
            .iter()
            .filter_map(|k| config::harness(&conn, &k.harness).unwrap())
            .collect();
        let drivers = Registry::from_config(&harnesses);
        Fx {
            ws,
            repo,
            conn,
            herdr,
            anchor,
            drivers,
        }
    }

    /// Drives the Lead's spawn verb for peer `name`.
    fn spawn(&self, name: &str) -> Result<String, String> {
        self.spawn_as(Some("alpha-lead"), name)
    }

    /// Drives the spawn verb as `caller` (None: the CoS).
    fn spawn_as(&self, caller: Option<&str>, name: &str) -> Result<String, String> {
        let ctx = Ctx {
            conn: &self.conn,
            herdr: &self.herdr,
            drivers: &self.drivers,
            runs_dir: self.ws.runs_dir(),
            caller,
            anchor: Some(&self.anchor),
            now_ms: 1_000,
        };
        let a = vec![
            name.to_string(),
            "tieude".to_string(),
            self.repo.to_string_lossy().to_string(),
        ];
        run_worker(&ctx, &a).map(|s| s.name).map_err(|e| e.message)
    }

    /// The reasons of every journaled refusal, oldest first.
    fn refusal_reasons(&self) -> Vec<String> {
        let has: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'events'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        if has == 0 {
            return Vec::new();
        }
        let mut st = self
            .conn
            .prepare("SELECT payload_json FROM events WHERE kind = 'refusal' ORDER BY seq")
            .unwrap();
        let rows = st.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.map(|p| {
            let v: serde_json::Value = serde_json::from_str(&p.unwrap()).unwrap();
            v["reason"].as_str().unwrap().to_string()
        })
        .collect()
    }

    fn agent_rows(&self, name: &str) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM agent WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn refusals(&self) -> i64 {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind = 'refusal'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }
}

const CAP2: &str = "```toml\nworker_cap = 2\n```\n";

#[test]
fn spawn_verb_refuses_over_protocol_cap() {
    // Config cap 6 is looser than the protocol cap 2: spawn 3 is refused.
    let fx = Fx::new("protocap", CAP2, "6");
    fx.spawn("tieude-a").unwrap();
    fx.spawn("tieude-b").unwrap();
    let e = fx.spawn("tieude-c").unwrap_err();
    assert!(e.contains("worker cap 2 reached"), "{e}");
    assert_eq!(fx.refusals(), 1);
    let n: i64 = fx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM agent WHERE name = 'tieude-c'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn spawn_verb_stricter_config_cap_wins_over_protocol() {
    // Config cap 1 is stricter than the protocol cap 2.
    let fx = Fx::new("cfgstrict", CAP2, "1");
    fx.spawn("tieude-a").unwrap();
    let e = fx.spawn("tieude-b").unwrap_err();
    assert!(e.contains("roles.lead.max_peers"), "{e}");
}

#[test]
fn spawn_verb_default_protocol_cap_is_four() {
    let fx = Fx::new("protodef", "", "8");
    for n in ["a", "b", "c", "d"] {
        fx.spawn(&format!("tieude-{n}")).unwrap();
    }
    let e = fx.spawn("tieude-e").unwrap_err();
    assert!(e.contains("worker cap 4 reached"), "{e}");
}

#[test]
fn worker_cap_applies_to_cos_spawn() {
    // The CoS (no caller) is held to the project cap too: 2 peers, then refused.
    let fx = Fx::new("capcos", CAP2, "6");
    fx.spawn_as(None, "tieude-a").unwrap();
    fx.spawn_as(None, "tieude-b").unwrap();
    let e = fx.spawn_as(None, "tieude-c").unwrap_err();
    assert!(e.contains("worker cap 2 reached"), "{e}");
    let reasons = fx.refusal_reasons();
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].starts_with("worker cap"), "{reasons:?}");
    assert_eq!(fx.agent_rows("tieude-c"), 0);
}

#[test]
fn worker_cap_counts_peers_across_spawners() {
    // One Lead peer plus one CoS peer fill a cap of 2, whoever asks next.
    let fx = Fx::new("capmix", CAP2, "6");
    fx.spawn("tieude-a").unwrap();
    fx.spawn_as(None, "tieude-b").unwrap();
    assert!(fx
        .spawn_as(None, "tieude-c")
        .unwrap_err()
        .contains("worker cap 2"));
    assert!(fx.spawn("tieude-d").unwrap_err().contains("worker cap 2"));
}

#[test]
fn worker_cap_reopen_is_not_a_new_spawn() {
    let fx = Fx::new("capreopen", CAP2, "6");
    fx.spawn_as(None, "tieude-a").unwrap();
    fx.spawn_as(None, "tieude-b").unwrap();
    // At the cap, reopening an open agent still goes through and journals nothing.
    fx.spawn_as(None, "tieude-a").unwrap();
    assert!(fx.refusal_reasons().is_empty());
}

#[test]
fn worker_cap_cos_spawn_fails_on_broken_protocol() {
    // Same as a Lead: a broken protocol block refuses the spawn, no agent row.
    let fx = Fx::new("capbroken", "```toml\nworker_cap = \"x\"\n```\n", "6");
    assert!(fx.spawn_as(None, "tieude-a").is_err());
    assert_eq!(fx.agent_rows("tieude-a"), 0);
}
