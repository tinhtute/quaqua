//! `grid reconcile` repairs the layout from the panes' grid tokens: workers go
//! to `<grid_tab>` (4 per tab, overflow `-2`), a Lead and its Peers to the
//! workspace `<slug>`, the CoS tab keeps only the CoS; a `working` pane is
//! deferred. Observed on FakeHerdr through the CLI command and a live daemon.

use std::sync::Arc;

use qq_core::cli::{Env, Registry as CliRegistry};
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::grid;
use qq_core::herdr::{FakeHerdr, MoveTo};
use qq_core::task::cleanup;
use qq_core::testkit::TestWorkspace;

struct World {
    _ws: TestWorkspace,
    herdr: Arc<FakeHerdr>,
    daemon: Option<DaemonHandle>,
    reg: CliRegistry,
    env: Env,
}

impl World {
    fn new(tag: &str) -> Self {
        let ws = TestWorkspace::new(tag);
        let herdr = Arc::new(FakeHerdr::new());
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
        }
    }

    fn reconcile(&self, flags: &[&str]) -> i32 {
        let mut args = vec!["grid".to_string(), "reconcile".to_string()];
        args.extend(flags.iter().map(|f| f.to_string()));
        self.reg.dispatch(&self.env, &args)
    }

    /// A pane of the CoS tab `quaqua` (harness `pi`, idle) with the tokens
    /// `worker spawn` / `lead spawn` record.
    fn seed(&self, tokens: &[(&str, &str)]) -> String {
        let id = self.herdr.seed_pane("quaqua", Some("pi"), tokens);
        self.herdr.seed_status(&id, "idle");
        id
    }

    fn worker(&self, owner: &str, tab: &str) -> String {
        self.seed(&[
            ("grid_role", "worker"),
            ("grid_group", "quaqua"),
            ("grid_owner", owner),
            ("grid_tab", tab),
            ("grid_ready", "1"),
        ])
    }

    fn splits(&self) -> Vec<(String, String)> {
        self.herdr
            .moves()
            .into_iter()
            .filter_map(|(_, to)| match to {
                MoveTo::Split { anchor, dir, .. } => Some((anchor, dir)),
                _ => None,
            })
            .collect()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            let _ = d.stop();
        }
    }
}

type Layout = Vec<(String, String, Vec<String>)>;

fn tab(ws: &str, tab: &str, panes: &[&String]) -> (String, String, Vec<String>) {
    (
        ws.to_string(),
        tab.to_string(),
        panes.iter().map(|p| p.to_string()).collect(),
    )
}

/// Today's five panes, all in the CoS tab.
fn five(w: &World) -> [String; 5] {
    let cos = w.seed(&[("grid_role", "cos"), ("grid_owner", "cos")]);
    let exit6 = w.worker("cos", "quaqua-workers");
    let pend = w.worker("cos", "quaqua-workers");
    let lead = w.seed(&[
        ("grid_role", "lead"),
        ("grid_group", "quaqua-rs"),
        ("grid_owner", "cos"),
        ("grid_tab", "quaqua-rs-lead"),
        ("grid_ready", "1"),
    ]);
    let runner = w.worker("quaqua-rs-lead", "quaqua-rs-workers");
    [cos, exit6, pend, lead, runner]
}

#[test]
fn five_panes_in_the_cos_tab_are_repaired_to_the_rule() {
    let w = World::new("gl-five");
    let [cos, exit6, pend, lead, runner] = five(&w);
    assert_eq!(w.reconcile(&[]), 0);
    let want: Layout = vec![
        tab("main", "quaqua", &[&cos]),
        tab("main", "quaqua-workers", &[&exit6, &pend]),
        tab("quaqua-rs", "quaqua-rs-lead", &[&lead]),
        tab("quaqua-rs", "quaqua-rs-workers", &[&runner]),
    ];
    assert_eq!(w.herdr.layout(), want);

    // a repeat finds nothing to move
    let moves = w.herdr.moves().len();
    assert_eq!(w.reconcile(&[]), 0);
    assert_eq!(w.herdr.moves().len(), moves);
    assert_eq!(w.herdr.layout(), want);
}

#[test]
fn a_working_pane_is_deferred_until_it_idles() {
    let w = World::new("gl-defer");
    let [cos, exit6, pend, lead, runner] = five(&w);
    w.herdr.seed_status(&exit6, "working");
    assert_eq!(w.reconcile(&[]), 0);
    assert_eq!(
        w.herdr.layout(),
        vec![
            tab("main", "quaqua", &[&cos, &exit6]),
            tab("main", "quaqua-workers", &[&pend]),
            tab("quaqua-rs", "quaqua-rs-lead", &[&lead]),
            tab("quaqua-rs", "quaqua-rs-workers", &[&runner]),
        ],
        "the working pane stays, the rest move"
    );
    w.herdr.seed_status(&exit6, "idle");
    assert_eq!(w.reconcile(&[]), 0);
    assert_eq!(
        w.herdr.layout(),
        vec![
            tab("main", "quaqua", &[&cos]),
            tab("main", "quaqua-workers", &[&pend, &exit6]),
            tab("quaqua-rs", "quaqua-rs-lead", &[&lead]),
            tab("quaqua-rs", "quaqua-rs-workers", &[&runner]),
        ]
    );
}

#[test]
fn a_peer_follows_its_lead_into_the_lead_workspace() {
    let w = World::new("gl-lead-working");
    let [_, _, _, lead, runner] = five(&w);
    w.herdr.seed_status(&lead, "working");
    assert_eq!(w.reconcile(&[]), 0);
    let layout = w.herdr.layout();
    assert!(
        layout.contains(&tab("quaqua-rs", "quaqua-rs-workers", &[&runner])),
        "{layout:?}"
    );
    assert!(
        layout
            .iter()
            .any(|(ws, t, p)| ws == "main" && t == "quaqua" && p.contains(&lead)),
        "the working Lead stays: {layout:?}"
    );
    w.herdr.seed_status(&lead, "idle");
    assert_eq!(w.reconcile(&[]), 0);
    assert!(w
        .herdr
        .layout()
        .contains(&tab("quaqua-rs", "quaqua-rs-lead", &[&lead])));
}

#[test]
fn tab_narrows_the_repair_to_one_grid_tab() {
    let w = World::new("gl-narrow");
    let [cos, exit6, pend, lead, runner] = five(&w);
    assert_eq!(w.reconcile(&["--tab", "quaqua-workers"]), 0);
    assert_eq!(
        w.herdr.layout(),
        vec![
            tab("main", "quaqua", &[&cos, &lead, &runner]),
            tab("main", "quaqua-workers", &[&exit6, &pend]),
        ]
    );
}

#[test]
fn one_to_four_workers_take_the_target_shapes_and_a_fifth_overflows() {
    for n in 1..=5usize {
        let w = World::new(&format!("gl-shape{n}"));
        let ids: Vec<String> = (0..n).map(|_| w.worker("cos", "quaqua-workers")).collect();
        assert_eq!(w.reconcile(&[]), 0);
        let layout = w.herdr.layout();
        let first: Vec<&String> = ids.iter().take(4).collect();
        assert!(
            layout.contains(&tab("main", "quaqua-workers", &first)),
            "n={n}: {layout:?}"
        );
        if n == 5 {
            assert!(
                layout.contains(&tab("main", "quaqua-workers-2", &[&ids[4]])),
                "{layout:?}"
            );
        }
        // 2 = split the first right, 3 = split the second down, 4 = split the first down
        let want: Vec<(String, String)> = [(0, "right"), (1, "down"), (0, "down")]
            .iter()
            .take(n.min(4) - 1)
            .map(|(i, d)| (ids[*i].clone(), d.to_string()))
            .collect();
        assert_eq!(w.splits(), want, "n={n}");
    }
}
