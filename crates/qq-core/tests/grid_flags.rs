//! `grid reconcile --only <names>` / `--tab <label>`: which panes of ended
//! agents the command closes, observed on FakeHerdr through the CLI command
//! and a live daemon.

use std::path::Path;
use std::sync::Arc;

use qq_core::cli::{Env, Registry as CliRegistry};
use qq_core::config;
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::db;
use qq_core::grid;
use qq_core::herdr::FakeHerdr;
use qq_core::task::cleanup;
use qq_core::testkit::TestWorkspace;

/// Three ended agents, one pane each: `wa` and `wb` in `repo-workers`, `wc` in
/// `repo-workers-2`; plus `live`, whose agent is still open.
struct World {
    _ws: TestWorkspace,
    herdr: Arc<FakeHerdr>,
    daemon: Option<DaemonHandle>,
    reg: CliRegistry,
    env: Env,
    panes: [String; 4],
}

const TOKENS: &[(&str, &str)] = &[("grid_role", "worker")];

impl World {
    fn new(tag: &str) -> Self {
        let ws = TestWorkspace::new(tag);
        let herdr = Arc::new(FakeHerdr::new());
        let panes = [
            herdr.seed_pane("repo-workers", None, TOKENS),
            herdr.seed_pane("repo-workers", None, TOKENS),
            herdr.seed_pane("repo-workers-2", None, TOKENS),
            herdr.seed_pane("repo-workers", None, TOKENS),
        ];
        let mut conn = db::open(&ws.db_path()).expect("open qq db");
        let team = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
        )
        .unwrap()
        .replace("harness = \"omp\"", "harness = \"opencode\"");
        config::seed(&mut conn, &team).expect("seed");
        for (name, pane, ended) in [
            ("wa", &panes[0], true),
            ("wb", &panes[1], true),
            ("wc", &panes[2], true),
            ("live", &panes[3], false),
        ] {
            conn.execute(
                "INSERT INTO agent (name, role, kind, started_ms, ended_ms, pane_id) \
                 VALUES (?1, 'peer', 'tieude', 1, ?2, ?3)",
                rusqlite::params![name, ended.then_some(2), pane],
            )
            .unwrap();
        }
        let mut d = Daemon::new(Config {
            runs_dir: ws.runs_dir(),
            herdr: herdr.clone(),
        });
        let mut reg = CliRegistry::new();
        cleanup::register(&mut d).expect("worker verbs");
        (grid::INSTALL)(&mut reg, &mut d).expect("install grid");
        let env = Env {
            runs_dir: ws.runs_dir(),
        };
        World {
            daemon: Some(d.spawn().expect("daemon starts")),
            _ws: ws,
            herdr,
            reg,
            env,
            panes,
        }
    }

    /// Runs `grid reconcile <flags>`; returns the exit status.
    fn reconcile(&self, flags: &[&str]) -> i32 {
        let mut args = vec!["grid".to_string(), "reconcile".to_string()];
        args.extend(flags.iter().map(|f| f.to_string()));
        self.reg.dispatch(&self.env, &args)
    }

    /// Which of wa, wb, wc, live still have their pane.
    fn alive(&self) -> [bool; 4] {
        std::array::from_fn(|i| self.herdr.has_pane(&self.panes[i]))
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            let _ = d.stop();
        }
    }
}

#[test]
fn without_flags_reconcile_closes_every_ended_agents_pane_and_repeats_to_nothing() {
    let w = World::new("gflags-all");
    assert_eq!(w.reconcile(&[]), 0);
    assert_eq!(w.alive(), [false, false, false, true]);
    assert_eq!(w.reconcile(&[]), 0);
    assert_eq!(w.alive(), [false, false, false, true]);
}

#[test]
fn only_narrows_to_the_named_agents() {
    let w = World::new("gflags-only");
    assert_eq!(w.reconcile(&["--only", "wa"]), 0);
    assert_eq!(w.alive(), [false, true, true, true]);

    // a comma list closes each name, blanks are dropped, an open agent is never closed
    assert_eq!(w.reconcile(&["--only=wb, ,live"]), 0);
    assert_eq!(w.alive(), [false, false, true, true]);
}

#[test]
fn tab_narrows_to_that_exact_tab_not_its_overflow() {
    let w = World::new("gflags-tab");
    assert_eq!(w.reconcile(&["--tab", "repo-workers"]), 0);
    assert_eq!(
        w.alive(),
        [false, false, true, true],
        "repo-workers-2 is left"
    );
    assert_eq!(w.reconcile(&["--tab=repo-workers-2"]), 0);
    assert_eq!(w.alive(), [false, false, false, true]);
}

#[test]
fn tab_and_only_together_close_the_intersection() {
    let w = World::new("gflags-both");
    assert_eq!(
        w.reconcile(&["--tab", "repo-workers", "--only", "wc,wb"]),
        0
    );
    assert_eq!(w.alive(), [true, false, true, true], "wc is in another tab");
}

#[test]
fn a_bad_flag_exits_1_and_closes_nothing() {
    let w = World::new("gflags-bad");
    for bad in [
        &["--tab", ""][..],
        &["--tab="],
        &["--tab"],
        &["--only"],
        &["--bogus"],
        &["stray"],
    ] {
        assert_eq!(w.reconcile(bad), 1, "{bad:?}");
        assert_eq!(w.alive(), [true; 4], "{bad:?}");
    }
}
