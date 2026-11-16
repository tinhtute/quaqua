//! `worker spawn` and `lead spawn`, driven through their library entry points
//! against a real temp git repo, a real database and the fake herdr. Every case
//! asserts what a caller observes: the result or error, the agent row, the
//! worktree and branch on disk, the sidecar files and the herdr effects.
//! Legacy contract: `crates/quaqua/src/worker/spawn.rs` (grammar, name rule,
//! `--base`, `--review`, `--solo`, metadata), `src/cap.rs` (owner cap) and
//! `src/worker/leadgate.rs` (two signals block, one warns); `lead/spawn.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use qq_core::config;
use qq_core::db;
use qq_core::driver::Registry;
use qq_core::herdr::FakeHerdr;
use qq_core::cli::Env;
use qq_core::herdr::{HerdrClient, Reply, Verb};
use qq_core::task::worker_flow::spawn_cmd::{run_lead, run_worker, worker_spawn_with, Ctx};
use qq_core::testkit::TestWorkspace;
use rusqlite::Connection;

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&o.stdout).trim().to_string()
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
    fn new(tag: &str) -> Self {
        let ws = TestWorkspace::new(tag);
        // The repo's parent is not the workspace root: worktrees go beside the repo.
        let repo = ws.root().join("code").join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        git(&repo, &["branch", "side"]);
        let mut conn = db::open(&ws.db_path()).expect("open db");
        let team = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/workspace/team.toml"),
        )
        .unwrap()
        .replace("harness = \"omp\"", "harness = \"opencode\"");
        config::seed(&mut conn, &team).expect("seed");
        let herdr = FakeHerdr::new();
        let anchor = herdr.seed_pane("home", None, &[]);
        let harnesses: Vec<_> = config::roster(&conn)
            .unwrap()
            .iter()
            .filter_map(|k| config::harness(&conn, &k.harness).unwrap())
            .collect();
        Fx {
            ws,
            repo,
            conn,
            herdr,
            anchor,
            drivers: Registry::from_config(&harnesses),
        }
    }

    fn ctx<'a>(&'a self, caller: Option<&'a str>) -> Ctx<'a> {
        Ctx {
            conn: &self.conn,
            herdr: &self.herdr,
            drivers: &self.drivers,
            runs_dir: self.ws.runs_dir(),
            caller,
            anchor: Some(&self.anchor),
            now_ms: 1_000,
        }
    }

    fn worker(&self, caller: Option<&str>, args: &[&str]) -> Result<String, String> {
        let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        run_worker(&self.ctx(caller), &a)
            .map(|s| s.name)
            .map_err(|e| e.message)
    }

    /// `worker spawn <name> <kind> <repo>` with extra leading flags.
    fn spawn(&self, flags: &[&str], name: &str, kind: &str) -> Result<String, String> {
        let repo = self.repo.to_string_lossy().to_string();
        let mut a: Vec<&str> = flags.to_vec();
        a.extend([name, kind, repo.as_str()]);
        self.worker(None, &a)
    }

    fn wt(&self, name: &str) -> PathBuf {
        self.repo.parent().unwrap().join(".wt").join(name)
    }

    fn setting(&self, key: &str, json: &str) {
        self.conn
            .execute(
                "INSERT INTO setting (key, value_json) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
                [key, json],
            )
            .unwrap();
    }

    fn agents(&self, name: &str) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM agent WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn project(&self, slug: &str) {
        self.conn
            .execute(
                "INSERT INTO project (slug, repo_path, created_ms) VALUES (?1, ?2, 1)",
                [slug, &self.repo.to_string_lossy()],
            )
            .unwrap();
    }
}

// ---- grammar -----------------------------------------------------------------

#[test]
fn purpose_stands_in_for_the_kind() {
    let fx = Fx::new("purpose");
    let repo = fx.repo.to_string_lossy().to_string();
    // `quick` resolves to `tieude` in team.toml [purposes].
    let name = fx
        .worker(None, &["--purpose", "quick", "tieude-a", &repo])
        .unwrap();
    assert_eq!(name, "tieude-a");
    let kind: String = fx
        .conn
        .query_row("SELECT kind FROM agent WHERE name = 'tieude-a'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kind, "tieude");
}

#[test]
fn purpose_and_kind_that_disagree_are_refused() {
    let fx = Fx::new("mismatch");
    let repo = fx.repo.to_string_lossy().to_string();
    let e = fx
        .worker(None, &["--purpose", "quick", "ni-a", "ni", &repo])
        .unwrap_err();
    assert!(e.contains("resolves to kind 'tieude'"), "{e}");
    assert_eq!(fx.agents("ni-a"), 0);
}

#[test]
fn a_missing_argument_is_named() {
    let fx = Fx::new("missing");
    let e = fx.worker(None, &["tieude-a"]).unwrap_err();
    assert!(e.contains("missing <kind> and <repo-path>"), "{e}");
    let e = fx.worker(None, &["tieude-a", "tieude"]).unwrap_err();
    assert!(e.contains("missing <repo-path>"), "{e}");
}

// ---- name rule ---------------------------------------------------------------

#[test]
fn a_name_must_be_kind_dash_task() {
    let fx = Fx::new("names");
    for bad in [
        "other-task",
        "tieude-",
        "tieude-Upper",
        "tieude-a-very-long-task-name",
        "tieude-x-lead",
    ] {
        assert!(fx.spawn(&[], bad, "tieude").is_err(), "{bad} was accepted");
        assert_eq!(fx.agents(bad), 0, "{bad}");
    }
    assert_eq!(
        fx.spawn(&[], "tieude-ok-1", "tieude").unwrap(),
        "tieude-ok-1"
    );
}

// ---- --base ------------------------------------------------------------------

#[test]
fn a_bad_base_is_refused_before_any_worktree() {
    let fx = Fx::new("badbase");
    let e = fx
        .spawn(&["--base", "no-such-ref"], "tieude-a", "tieude")
        .unwrap_err();
    assert!(e.contains("not a known commit-ish"), "{e}");
    assert!(!fx.wt("tieude-a").exists());
}

#[test]
fn base_starts_the_branch_at_the_ref_and_is_recorded() {
    let fx = Fx::new("base");
    git(&fx.repo, &["checkout", "-q", "side"]);
    std::fs::write(fx.repo.join("s.txt"), "s\n").unwrap();
    git(&fx.repo, &["add", "."]);
    git(&fx.repo, &["commit", "-q", "-m", "side"]);
    let side = git(&fx.repo, &["rev-parse", "side"]);
    git(&fx.repo, &["checkout", "-q", "main"]);
    fx.spawn(&["--base", "side"], "tieude-a", "tieude").unwrap();
    assert_eq!(git(&fx.repo, &["rev-parse", "quaqua/tieude-a"]), side);
    assert_eq!(git(&fx.wt("tieude-a"), &["rev-parse", "HEAD"]), side);
    let rec = std::fs::read_to_string(fx.ws.runs_dir().join("tieude-a.base")).unwrap();
    assert_eq!(rec.trim(), "side");
    // A spawn without --base leaves no stale record under another name.
    fx.spawn(&[], "tieude-b", "tieude").unwrap();
    assert!(!fx.ws.runs_dir().join("tieude-b.base").exists());
}

// ---- --review ------------------------------------------------------------------

#[test]
fn review_needs_a_kind_that_can_review() {
    let fx = Fx::new("review");
    let e = fx.spawn(&["--review"], "tieude-r", "tieude").unwrap_err();
    assert!(e.contains("can_review"), "{e}");
    assert_eq!(fx.agents("tieude-r"), 0);
    assert_eq!(fx.spawn(&["--review"], "ni-r", "ni").unwrap(), "ni-r");
}

// ---- spawn identity: worktree, owner, grid, repo ---------------------------------------

#[test]
fn a_spawn_leaves_a_worktree_a_row_and_pane_tokens() {
    let fx = Fx::new("meta");
    fx.spawn(&[], "tieude-a", "tieude").unwrap();
    let wt = fx.wt("tieude-a");
    assert!(wt.join("a.txt").is_file());
    assert_eq!(
        git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "quaqua/tieude-a"
    );
    let (role, kind, repo, pane): (String, String, String, String) = fx
        .conn
        .query_row(
            "SELECT a.role, a.kind, p.repo_path, a.pane_id FROM agent a \
             JOIN project p ON p.id = a.project_id WHERE a.name = 'tieude-a'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!((role.as_str(), kind.as_str()), ("peer", "tieude"));
    assert_eq!(
        std::fs::canonicalize(repo).unwrap(),
        std::fs::canonicalize(&fx.repo).unwrap()
    );
    let tokens = fx.herdr.tokens(&pane);
    let tok = |k: &str| tokens.get(k).map(String::as_str);
    assert_eq!(tok("grid_role"), Some("worker"));
    assert_eq!(tok("grid_group"), Some("repo"));
    assert_eq!(tok("grid_owner"), Some("cos"));
    assert_eq!(tok("grid_tab"), Some("repo-workers"));
    assert_eq!(tok("grid_ready"), Some("1"));
    // The pane's shell starts in the worktree, split from the caller's anchor.
    let cwds = fx.herdr.split_cwds();
    assert_eq!(cwds.len(), 1);
    assert_eq!(cwds[0].0, pane);
    assert_eq!(
        std::fs::canonicalize(&cwds[0].1).unwrap(),
        std::fs::canonicalize(&wt).unwrap()
    );
}

#[test]
fn a_leads_peer_belongs_to_the_leads_workers_tab() {
    let fx = Fx::new("leadtab");
    fx.project("alpha");
    fx.conn
        .execute(
            "INSERT INTO agent (name, role, project_id, started_ms) \
             VALUES ('alpha-lead', 'lead', (SELECT id FROM project WHERE slug='alpha'), 1)",
            [],
        )
        .unwrap();
    let repo = fx.repo.to_string_lossy().to_string();
    fx.worker(Some("alpha-lead"), &["tieude-a", "tieude", &repo])
        .unwrap();
    let pane: String = fx
        .conn
        .query_row("SELECT pane_id FROM agent WHERE name='tieude-a'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let tokens = fx.herdr.tokens(&pane);
    assert_eq!(tokens["grid_owner"], "alpha-lead");
    assert_eq!(tokens["grid_tab"], "alpha-workers");
    let spawner: String = fx
        .conn
        .query_row(
            "SELECT s.name FROM agent a JOIN agent s ON s.id = a.spawner_id WHERE a.name='tieude-a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(spawner, "alpha-lead");
}

#[test]
fn a_peer_cannot_spawn() {
    let fx = Fx::new("peerspawn");
    fx.spawn(&[], "tieude-a", "tieude").unwrap();
    let repo = fx.repo.to_string_lossy().to_string();
    let e = fx
        .worker(Some("tieude-a"), &["tieude-b", "tieude", &repo])
        .unwrap_err();
    assert!(e.contains("Peer can spawn nothing"), "{e}");
    assert_eq!(fx.agents("tieude-b"), 0);
}

#[test]
fn a_repeated_spawn_converges() {
    let fx = Fx::new("again");
    fx.spawn(&[], "tieude-a", "tieude").unwrap();
    let id: i64 = fx
        .conn
        .query_row("SELECT id FROM agent WHERE name='tieude-a'", [], |r| {
            r.get(0)
        })
        .unwrap();
    fx.spawn(&[], "tieude-a", "tieude").unwrap();
    assert_eq!(fx.agents("tieude-a"), 1);
    let id2: i64 = fx
        .conn
        .query_row("SELECT id FROM agent WHERE name='tieude-a'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(id, id2);
    assert_eq!(fx.herdr.splits().len(), 1, "no second pane");
    assert_eq!(
        git(&fx.repo, &["branch", "--list", "quaqua/tieude-a"])
            .lines()
            .count(),
        1
    );
}

// ---- cap gate ------------------------------------------------------------------------

#[test]
fn the_owner_cap_refuses_one_spawn_too_many() {
    let fx = Fx::new("cap");
    fx.setting("limits.max_workers_per_orchestrator", "1");
    fx.spawn(&[], "tieude-a", "tieude").unwrap();
    let e = fx.spawn(&[], "tieude-b", "tieude").unwrap_err();
    assert!(e.contains("worker cap exceeded"), "{e}");
    assert_eq!(fx.agents("tieude-b"), 0);
    assert!(!fx.wt("tieude-b").exists());
    // The cap is per owner and the open worker may be spawned again.
    assert!(fx.spawn(&[], "tieude-a", "tieude").is_ok());
}

#[test]
fn a_lead_is_held_to_max_peers() {
    let fx = Fx::new("leadcap");
    fx.setting("roles.lead.max_peers", "1");
    fx.project("alpha");
    fx.conn
        .execute(
            "INSERT INTO agent (name, role, project_id, started_ms) \
             VALUES ('alpha-lead', 'lead', (SELECT id FROM project WHERE slug='alpha'), 1)",
            [],
        )
        .unwrap();
    let repo = fx.repo.to_string_lossy().to_string();
    fx.worker(Some("alpha-lead"), &["tieude-a", "tieude", &repo])
        .unwrap();
    let e = fx
        .worker(Some("alpha-lead"), &["tieude-b", "tieude", &repo])
        .unwrap_err();
    assert!(e.contains("roles.lead.max_peers"), "{e}");
}

// ---- lead gate -----------------------------------------------------------------------

fn plan(fx: &Fx, left: u32) {
    let dir = fx.ws.root().join("docs/plans/active");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("repo.md"),
        format!("---\nphases_left: {left}\n---\nplan\n"),
    )
    .unwrap();
}

#[test]
fn two_topology_signals_block_a_cos_owned_spawn() {
    let fx = Fx::new("gate");
    plan(&fx, 3);
    fx.spawn(&[], "tieude-a", "tieude").unwrap();
    fx.spawn(&[], "tieude-b", "tieude").unwrap();
    // Third live build worker (signal 1) plus phases_left 3 (signal 2).
    let e = fx.spawn(&[], "tieude-c", "tieude").unwrap_err();
    assert!(e.contains("needs a Lead"), "{e}");
    assert!(e.contains("lead spawn repo"), "{e}");
    assert_eq!(fx.agents("tieude-c"), 0);
    assert!(!fx.wt("tieude-c").exists());
}

#[test]
fn one_signal_only_warns() {
    let fx = Fx::new("gate1");
    fx.spawn(&[], "tieude-a", "tieude").unwrap();
    fx.spawn(&[], "tieude-b", "tieude").unwrap();
    assert!(fx.spawn(&[], "tieude-c", "tieude").is_ok());
}

#[test]
fn solo_with_a_reason_passes_the_gate_and_is_recorded() {
    let fx = Fx::new("solo");
    plan(&fx, 3);
    fx.spawn(&[], "tieude-a", "tieude").unwrap();
    fx.spawn(&[], "tieude-b", "tieude").unwrap();
    let reason = "one-off migration, no Lead needed";
    fx.spawn(&["--solo", reason], "tieude-c", "tieude").unwrap();
    let mem: String = std::fs::read_dir(fx.ws.root().join("memory"))
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert!(
        mem.contains(&format!("- [repo] solo override: {reason}")),
        "{mem}"
    );
}

#[test]
fn a_short_solo_reason_is_refused() {
    let fx = Fx::new("solo-short");
    let e = fx
        .spawn(&["--solo", "short"], "tieude-a", "tieude")
        .unwrap_err();
    assert!(e.contains("at least 10"), "{e}");
    assert_eq!(fx.agents("tieude-a"), 0);
}

#[test]
fn a_project_with_a_lead_is_not_gated() {
    let fx = Fx::new("gate-lead");
    plan(&fx, 3);
    fx.project("repo");
    fx.conn
        .execute(
            "INSERT INTO agent (name, role, project_id, started_ms) \
             VALUES ('repo-lead', 'lead', (SELECT id FROM project WHERE slug='repo'), 1)",
            [],
        )
        .unwrap();
    for n in ["tieude-a", "tieude-b", "tieude-c"] {
        fx.spawn(&[], n, "tieude").unwrap();
    }
}

// ---- lead spawn ----------------------------------------------------------------------

fn lead(
    fx: &Fx,
    args: &[&str],
) -> Result<qq_core::task::worker_flow::spawn_cmd::LeadSpawned, String> {
    let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    run_lead(&fx.ctx(None), &a).map_err(|e| e.message)
}

#[test]
fn lead_spawn_registers_the_project_lead_and_its_integ_branch() {
    let fx = Fx::new("lead");
    fx.project("alpha");
    let s = lead(&fx, &["alpha"]).unwrap();
    assert_eq!(s.name, "alpha-lead");
    // strategy.lead_kind in team.toml is `ni`.
    assert_eq!(s.kind, "ni");
    let (role, project): (String, String) = fx
        .conn
        .query_row(
            "SELECT a.role, p.slug FROM agent a JOIN project p ON p.id = a.project_id \
             WHERE a.name = 'alpha-lead' AND a.ended_ms IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((role.as_str(), project.as_str()), ("lead", "alpha"));
    assert_eq!(
        git(&fx.repo, &["rev-parse", "quaqua/alpha-integ"]),
        git(&fx.repo, &["rev-parse", "main"])
    );
    let integ: String = fx
        .conn
        .query_row(
            "SELECT integ_branch FROM project WHERE slug='alpha'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(integ, "quaqua/alpha-integ");
}

#[test]
fn lead_spawn_refuses_what_legacy_refused() {
    let fx = Fx::new("lead-bad");
    fx.project("alpha");
    assert!(lead(&fx, &["nope"])
        .unwrap_err()
        .contains("unknown project"));
    assert!(lead(&fx, &["Bad_Slug"]).is_err());
    let long = "a".repeat(30);
    fx.project(&long);
    assert!(lead(&fx, &[&long]).unwrap_err().contains("32"));
    lead(&fx, &["alpha"]).unwrap();
    let e = lead(&fx, &["alpha"]).unwrap_err();
    assert!(e.contains("already exists"), "{e}");
}

#[test]
fn lead_task_is_delivered_after_the_brief_and_without_it_the_next_step_is_printed() {
    let fx = Fx::new("lead-task");
    fx.project("alpha");
    fx.project("beta");
    let s = lead(&fx, &["alpha", "--task", "KICKOFF-TEXT"]).unwrap();
    assert!(s.kickoff_sent);
    let prompts = fx.herdr.prompts();
    assert_eq!(prompts.len(), 2, "brief then kickoff: {prompts:?}");
    assert!(prompts[0].1.contains("alpha"), "{prompts:?}");
    assert_eq!(
        prompts[1],
        ("alpha-lead".to_string(), "KICKOFF-TEXT".to_string())
    );
    let s = lead(&fx, &["beta"]).unwrap();
    assert!(!s.kickoff_sent);
    assert_eq!(
        s.next.as_deref(),
        Some("NEXT: herdr agent prompt beta-lead \"<kickoff>\"")
    );
    assert_eq!(fx.herdr.prompts().len(), 3, "only the brief for beta");
}

// ---- spawn fixes (sude-spawnfix) ---------------------------------------------------------

#[test]
fn worktrees_live_beside_the_repo_where_cleanup_looks() {
    let fx = Fx::new("wtpath");
    fx.project("alpha");
    let s = fx.spawn(&[], "tieude-a", "tieude").unwrap();
    assert_eq!(s, "tieude-a");
    assert!(fx.wt("tieude-a").join("a.txt").exists());
    assert!(!fx.ws.root().join(".wt").exists(), "nothing under the workspace");
    let l = lead(&fx, &["alpha"]).unwrap();
    assert!(fx.wt(&l.name).exists());
    assert!(!fx.ws.root().join(".wt").exists());
}

fn pid() -> u32 {
    std::process::id()
}

fn cli(fx: &Fx, herdr: &Arc<FakeHerdr>, args: &[&str]) -> Result<String, String> {
    let repo = fx.repo.to_string_lossy().to_string();
    let mut a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    a.push(repo);
    let env = Env {
        runs_dir: fx.ws.runs_dir(),
    };
    worker_spawn_with(
        &env,
        herdr.clone() as Arc<dyn HerdrClient>,
        pid() as i32,
        Some(fx.anchor.clone()),
        &a,
    )
    .map(|s| s.name)
    .map_err(|e| e.message)
}

#[test]
fn a_spawn_from_an_unmapped_pane_is_refused_not_taken_for_the_cos() {
    let fx = Fx::new("unmapped");
    let herdr = Arc::new(FakeHerdr::new());
    herdr.seed_pane("home", None, &[]);
    // No pane runs this process.
    let e = cli(&fx, &herdr, &["tieude-a", "tieude"]).unwrap_err();
    assert!(e.contains("caller not identified"), "{e}");
    assert_eq!(fx.agents("tieude-a"), 0);
    assert!(!fx.wt("tieude-a").exists());
}

#[test]
fn a_spawn_from_the_cos_pane_is_cos_owned_and_from_a_lead_pane_lead_owned() {
    let fx = Fx::new("mapped");
    fx.project("alpha");
    let herdr = Arc::new(FakeHerdr::new());
    let cos = herdr.seed_pane("home", None, &[("grid_role", "cos")]);
    herdr.set_pane_pids(&cos, pid(), &[]);
    // The anchor pane belongs to the test's own herdr.
    let anchor = herdr.seed_pane("anchor", None, &[]);
    let repo = fx.repo.to_string_lossy().to_string();
    let env = Env {
        runs_dir: fx.ws.runs_dir(),
    };
    let run = |h: &Arc<FakeHerdr>, name: &str| {
        worker_spawn_with(
            &env,
            h.clone() as Arc<dyn HerdrClient>,
            pid() as i32,
            Some(anchor.clone()),
            &[name.to_string(), "tieude".into(), repo.clone()],
        )
        .map_err(|e| e.message)
    };
    run(&herdr, "tieude-a").unwrap();
    let spawner: Option<i64> = fx
        .conn
        .query_row("SELECT spawner_id FROM agent WHERE name='tieude-a'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(spawner, None);

    // Now the same process sits in a Lead's pane.
    fx.conn
        .execute(
            "INSERT INTO agent (name, role, project_id, started_ms) \
             VALUES ('alpha-lead', 'lead', (SELECT id FROM project WHERE slug='alpha'), 1)",
            [],
        )
        .unwrap();
    let h2 = Arc::new(FakeHerdr::new());
    let lp = h2.seed_pane("lead", None, &[]);
    h2.agent_start("alpha-lead", "claude", &lp, 0, &[]).unwrap();
    h2.set_pane_pids(&lp, pid(), &[]);
    let anchor2 = h2.seed_pane("anchor", None, &[]);
    worker_spawn_with(
        &env,
        h2.clone() as Arc<dyn HerdrClient>,
        pid() as i32,
        Some(anchor2),
        &["tieude-b".to_string(), "tieude".into(), repo.clone()],
    )
    .unwrap();
    let owner: String = fx
        .conn
        .query_row(
            "SELECT s.name FROM agent a JOIN agent s ON s.id = a.spawner_id WHERE a.name='tieude-b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(owner, "alpha-lead");
}

#[test]
fn the_gate_blocks_at_two_phases_left_like_legacy() {
    let fx = Fx::new("gate2");
    plan(&fx, 2);
    fx.spawn(&[], "tieude-a", "tieude").unwrap();
    fx.spawn(&[], "tieude-b", "tieude").unwrap();
    let e = fx.spawn(&[], "tieude-c", "tieude").unwrap_err();
    assert!(e.contains("needs a Lead"), "{e}");
}

fn memory_days(fx: &Fx, days: &[&str]) {
    let dir = fx.ws.root().join("memory");
    std::fs::create_dir_all(&dir).unwrap();
    for d in days {
        std::fs::write(dir.join(format!("{d}.md")), "- [repo] work\n").unwrap();
    }
}

#[test]
fn two_consecutive_memory_days_are_a_gate_signal() {
    let fx = Fx::new("memdays");
    memory_days(&fx, &["2026-11-14", "2026-11-15"]);
    plan(&fx, 2);
    // memory (signal) + phases_left 2 (signal) block with no live worker.
    let e = fx.spawn(&[], "tieude-a", "tieude").unwrap_err();
    assert!(e.contains("needs a Lead"), "{e}");
    assert!(e.contains("memory"), "{e}");
}

#[test]
fn memory_days_with_a_gap_are_not_a_run() {
    let fx = Fx::new("memgap");
    memory_days(&fx, &["2026-11-10", "2026-11-15"]);
    plan(&fx, 2);
    assert!(fx.spawn(&[], "tieude-a", "tieude").is_ok());
}

#[test]
fn a_lead_cap_is_the_smaller_of_max_peers_and_the_global_cap() {
    let fx = Fx::new("minleadcap");
    fx.setting("roles.lead.max_peers", "4");
    fx.setting("limits.max_workers_per_orchestrator", "1");
    fx.project("alpha");
    fx.conn
        .execute(
            "INSERT INTO agent (name, role, project_id, started_ms) \
             VALUES ('alpha-lead', 'lead', (SELECT id FROM project WHERE slug='alpha'), 1)",
            [],
        )
        .unwrap();
    let repo = fx.repo.to_string_lossy().to_string();
    fx.worker(Some("alpha-lead"), &["tieude-a", "tieude", &repo]).unwrap();
    let e = fx
        .worker(Some("alpha-lead"), &["tieude-b", "tieude", &repo])
        .unwrap_err();
    assert!(e.contains("worker cap exceeded"), "{e}");
}

#[test]
fn the_cap_counts_weighted_slots() {
    let fx = Fx::new("weights");
    fx.setting("limits.max_workers_per_orchestrator", "3");
    fx.setting("limits.weights", r#"{"default":1,"tieude":2}"#);
    fx.spawn(&[], "tieude-a", "tieude").unwrap(); // 2 slots
    fx.spawn(&[], "tieude-b", "tieude").unwrap_err(); // 2 + 2 > 3
    assert_eq!(fx.agents("tieude-b"), 0);
}

#[test]
fn a_second_repo_with_the_same_basename_keeps_its_own_project() {
    let fx = Fx::new("slugclash");
    fx.spawn(&[], "tieude-a", "tieude").unwrap();
    let other = fx.ws.root().join("elsewhere").join("repo");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q", "-b", "main"]);
    std::fs::write(other.join("b.txt"), "b\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "base"]);
    fx.worker(None, &["tieude-b", "tieude", &other.to_string_lossy()])
        .unwrap();
    let pa: Option<i64> = fx
        .conn
        .query_row("SELECT project_id FROM agent WHERE name='tieude-a'", [], |r| r.get(0))
        .unwrap();
    let pb: Option<i64> = fx
        .conn
        .query_row("SELECT project_id FROM agent WHERE name='tieude-b'", [], |r| r.get(0))
        .unwrap();
    assert!(pb.is_some(), "the second repo has a project");
    assert_ne!(pa, pb);
}

#[test]
fn lead_spawn_prompts_nothing_until_the_lead_is_ready() {
    let fx = Fx::new("leadready");
    fx.project("alpha");
    fx.herdr.set_reply(Verb::Wait, Reply::err("timeout"));
    let e = lead(&fx, &["alpha", "--task", "GO"]).unwrap_err();
    assert!(e.contains("ready"), "{e}");
    assert!(fx.herdr.prompts().is_empty(), "{:?}", fx.herdr.prompts());
}
