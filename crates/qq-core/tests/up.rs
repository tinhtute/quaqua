//! `quaqua up` (R1-R4) against one herdr fake and one daemon fake, the R2 error
//! verbatim, the pure CoS-pane selector on a pane list shaped like the live one, and the
//! real `CliUp` seam against a scripted herdr (process group, log, session addressing).
//!
//! No test here runs herdr, systemd, a TTY, or a real clock.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use qq_core::cli::CliError;
use qq_core::config::{self, CosLaunch};
use qq_core::daemon::service::{Platform, Ran, Runner, Service, NO_BACKEND};
use qq_core::db;
use qq_core::testkit::TestWorkspace;
use qq_core::up::daemon::{Answer, DaemonHost, Names, Ping, ServiceHost, Wait};
use qq_core::up::herdr::{
    agent_in_pane_json, cos_pane, Agent, CosPane, PaneLists, UpHost, Workspace,
};
use qq_core::up::{self, Cos};
use rusqlite::Connection;

fn template() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml");
    std::fs::read_to_string(p).expect("read template team.toml")
}

fn launch() -> CosLaunch {
    CosLaunch {
        harness: "claude".into(),
        model: "opus-5.5".into(),
        effort: "low".into(),
        herdr_kind: "claude".into(),
        argv: vec![
            "claude".into(),
            "--model".into(),
            "claude-opus-5-5".into(),
            "--effort".into(),
            "low".into(),
        ],
    }
}

/// The herdr world, one recorded effect per call. It never runs herdr.
struct FakeUp {
    /// `HERDR_ENV` set: the caller is already inside herdr.
    inside: bool,
    running: bool,
    start_session: Result<(), String>,
    launch: Result<CosLaunch, CliError>,
    root: PathBuf,
    recorded: Option<(String, String)>,
    workspace: Option<Workspace>,
    /// What the pane selector answers for the workspace.
    cos: Result<CosPane, String>,
    /// One entry per pane that hosts an agent: what `agent list` reports.
    agents: Vec<Agent>,
    /// Each `start_agent` as `kind|pane|arg arg ...`.
    starts: Vec<String>,
    /// The first N `start_agent` calls fail (a failed resume).
    fail_starts: usize,
    /// Whether a failed start leaves an agent in the pane (it came up anyway).
    fail_live: bool,
    /// Whether `focus` fails.
    focus_fails: bool,
    log: Vec<String>,
    lines: Vec<String>,
    focused: Vec<String>,
    attached: usize,
}

impl FakeUp {
    fn new(root: &Path) -> Self {
        FakeUp {
            inside: true,
            running: true,
            start_session: Ok(()),
            launch: Ok(launch()),
            root: root.to_path_buf(),
            recorded: None,
            workspace: Some(Workspace { id: "w1".into() }),
            cos: Ok(CosPane {
                pane: "p1".into(),
                live: false,
            }),
            agents: Vec::new(),
            starts: Vec::new(),
            fail_starts: 0,
            fail_live: false,
            focus_fails: false,
            log: Vec::new(),
            lines: Vec::new(),
            focused: Vec::new(),
            attached: 0,
        }
    }

    /// The pane a live CoS already occupies.
    fn live_in(mut self, pane: &str) -> Self {
        self.cos = Ok(CosPane {
            pane: pane.to_string(),
            live: true,
        });
        self
    }
}

impl UpHost for FakeUp {
    fn name(&self) -> String {
        qq_core::identity::farm_of(&self.root)
    }

    fn inside_herdr(&self) -> bool {
        self.inside
    }

    fn session_running(&mut self) -> bool {
        self.log.push("session_running".into());
        self.running
    }

    fn start_session(&mut self) -> Result<(), String> {
        self.log.push("start_session".into());
        self.running = true;
        self.start_session.clone()
    }

    fn launch(&mut self) -> Result<CosLaunch, CliError> {
        self.log.push("launch".into());
        match &self.launch {
            Ok(l) => Ok(l.clone()),
            Err(e) => Err(CliError::new(e.code, e.message.clone())),
        }
    }

    fn workspace_root(&self) -> PathBuf {
        self.root.clone()
    }

    fn recorded(&mut self) -> Option<(String, String)> {
        self.log.push("recorded".into());
        self.recorded.clone()
    }

    fn workspace(&mut self, label: &str) -> Option<Workspace> {
        self.log.push(format!("workspace:{label}"));
        self.workspace.clone()
    }

    fn create_workspace(&mut self, label: &str, cwd: &Path) -> Result<Workspace, String> {
        self.log
            .push(format!("create_workspace:{label}:{}", cwd.display()));
        let ws = Workspace { id: "w-new".into() };
        self.workspace = Some(ws.clone());
        Ok(ws)
    }

    fn cos_pane(
        &mut self,
        workspace: &str,
        _recorded: Option<&(String, String)>,
    ) -> Result<CosPane, String> {
        self.log.push(format!("cos_pane:{workspace}"));
        self.cos.clone()
    }

    fn agent_in_pane(&mut self, pane: &str) -> Option<Agent> {
        self.log.push(format!("agent_in_pane:{pane}"));
        self.agents.iter().find(|a| a.pane == pane).cloned()
    }

    fn start_agent(
        &mut self,
        name: &str,
        kind: &str,
        pane: &str,
        args: &[String],
        _dialog: bool,
    ) -> Result<(), String> {
        self.log.push(format!("start_agent:{name}:{pane}"));
        self.starts
            .push(format!("{kind}|{pane}|{}", args.join(" ")));
        if self.fail_starts > 0 {
            self.fail_starts -= 1;
            if self.fail_live {
                self.agents.push(Agent {
                    name: None,
                    pane: pane.to_string(),
                    status: "working".into(),
                    session: None,
                });
            }
            return Err("agent_not_ready".into());
        }
        self.agents.push(Agent {
            name: None,
            pane: pane.to_string(),
            status: "working".into(),
            session: None,
        });
        Ok(())
    }

    fn focus(&mut self, pane: &str) -> Result<(), String> {
        self.log.push(format!("focus:{pane}"));
        if self.focus_fails {
            return Err("herdr refused".into());
        }
        self.focused.push(pane.to_string());
        Ok(())
    }

    fn attach(&mut self) -> Result<(), String> {
        self.log.push("attach".into());
        self.attached += 1;
        Ok(())
    }

    fn line(&mut self, s: &str) {
        self.lines.push(s.to_string());
    }
}

fn systemd_names() -> Names {
    Names {
        unit: "quaqua-daemon.service".into(),
        status: "systemctl --user status quaqua-daemon.service".into(),
    }
}

fn launchd_names() -> Names {
    Names {
        unit: "com.quaqua.daemon".into(),
        status: "launchctl print gui/501/com.quaqua.daemon".into(),
    }
}

/// The daemon world: ping answers, the unit is installed, the human says no.
struct FakeDaemon {
    service: Result<Names, String>,
    pings: Vec<Ping>,
    unit_installed: bool,
    answer: Answer,
    start: Result<(), String>,
    install: Result<(), String>,
    starts: usize,
    installs: usize,
    lines: Vec<String>,
    base: Instant,
    at: Duration,
}

impl FakeDaemon {
    fn answers() -> Self {
        FakeDaemon {
            service: Ok(systemd_names()),
            pings: vec![Ping::Answers],
            unit_installed: true,
            answer: Answer::No,
            start: Ok(()),
            install: Ok(()),
            starts: 0,
            installs: 0,
            lines: Vec::new(),
            base: Instant::now(),
            at: Duration::ZERO,
        }
    }
}

impl DaemonHost for FakeDaemon {
    fn service(&self) -> Result<Names, String> {
        self.service.clone()
    }

    fn ping(&mut self) -> Ping {
        if self.pings.len() > 1 {
            self.pings.remove(0)
        } else {
            self.pings.first().cloned().unwrap_or(Ping::Answers)
        }
    }

    fn unit_installed(&mut self) -> bool {
        self.unit_installed
    }

    fn start_unit(&mut self) -> Result<(), String> {
        self.starts += 1;
        self.start.clone()
    }

    fn install_unit(&mut self) -> Result<(), String> {
        self.installs += 1;
        self.install.clone()
    }

    fn ask_install(&mut self) -> Answer {
        self.answer
    }

    fn wait(&mut self) -> Wait {
        Wait {
            deadline: Duration::from_secs(1),
            poll: Duration::from_millis(10),
        }
    }

    fn line(&mut self, s: &str) {
        self.lines.push(s.to_string());
    }

    fn sleep(&mut self, d: Duration) {
        self.at += d;
    }

    fn now(&self) -> Instant {
        self.base + self.at
    }
}

fn run(fake: &mut FakeUp) -> Result<Cos, CliError> {
    let mut daemon = FakeDaemon::answers();
    up::up(fake, &mut daemon)
}

// ---- R1: the order and the launch -------------------------------------------------

/// A stopped session is started before anything that needs herdr, and the CoS is
/// launched from `roles.cos` alone, into the workspace's CoS pane.
#[test]
fn up_starts_the_session_then_launches_the_cos_from_the_config_keys() {
    let ws = TestWorkspace::new("up-launch");
    let mut fake = FakeUp::new(ws.root());
    fake.running = false;
    fake.workspace = None;

    let cos = run(&mut fake).expect("up carries on");
    assert_eq!(cos, Cos::Fresh);
    assert_eq!(fake.log[0], "session_running");
    assert_eq!(fake.log[1], "start_session");
    assert!(fake.log.contains(&"launch".to_string()), "{:?}", fake.log);
    // The label and the cwd come from the farm root, never a literal.
    let label = qq_core::identity::farm_of(ws.root());
    assert!(
        fake.log
            .contains(&format!("create_workspace:{label}:{}", ws.root().display())),
        "{:?}",
        fake.log
    );
    assert!(
        fake.log.contains(&"cos_pane:w-new".to_string()),
        "{:?}",
        fake.log
    );
    assert_eq!(
        fake.starts,
        vec!["claude|p1|--model claude-opus-5-5 --effort low".to_string()]
    );
    assert_eq!(fake.focused, vec!["p1".to_string()]);
    assert!(
        fake.lines.iter().any(|l| l.contains(&label)),
        "{:?}",
        fake.lines
    );
}

/// A live CoS is found, not started again; a created workspace is never resumed.
#[test]
fn up_finds_a_live_cos_and_starts_nothing() {
    let ws = TestWorkspace::new("up-live");
    let mut fake = FakeUp::new(ws.root()).live_in("p1");
    fake.recorded = Some(("s-old".into(), "p1".into()));

    let cos = run(&mut fake).expect("up carries on");
    assert_eq!(cos, Cos::Live);
    assert!(fake.starts.is_empty(), "{:?}", fake.starts);
    assert_eq!(fake.focused, vec!["p1".to_string()]);
}

// ---- R3: resume, no record, failed resume -----------------------------------------

/// The workspace exists and the CoS has exited: resume the recorded id.
#[test]
fn up_resumes_the_recorded_session_of_an_exited_cos() {
    let ws = TestWorkspace::new("up-resume");
    let mut fake = FakeUp::new(ws.root());
    fake.recorded = Some(("sess-1234".into(), "p1".into()));

    let cos = run(&mut fake).expect("up carries on");
    assert_eq!(cos, Cos::Resumed);
    assert_eq!(
        fake.starts,
        vec!["claude|p1|--model claude-opus-5-5 --effort low --resume sess-1234".to_string()]
    );
}

/// No recorded session: one line says so, and a fresh CoS starts (no resume flag).
#[test]
fn up_without_a_recorded_session_says_so_and_starts_fresh() {
    let ws = TestWorkspace::new("up-norec");
    let mut fake = FakeUp::new(ws.root());

    let cos = run(&mut fake).expect("up carries on");
    assert_eq!(cos, Cos::Fresh);
    assert!(
        fake.lines.iter().any(|l| l.contains("no recorded session")),
        "{:?}",
        fake.lines
    );
    assert_eq!(
        fake.starts,
        vec!["claude|p1|--model claude-opus-5-5 --effort low".to_string()]
    );
}

/// A failed resume names the id, then starts a fresh CoS: up never leaves a dead farm.
#[test]
fn up_failed_resume_names_the_id_and_starts_fresh() {
    let ws = TestWorkspace::new("up-badresume");
    let mut fake = FakeUp::new(ws.root());
    fake.recorded = Some(("sess-bogus".into(), "p1".into()));
    fake.fail_starts = 1;

    let cos = run(&mut fake).expect("up carries on");
    assert_eq!(cos, Cos::Fresh);
    assert!(
        fake.lines.iter().any(|l| l.contains("sess-bogus")),
        "{:?}",
        fake.lines
    );
    assert_eq!(fake.starts.len(), 2, "{:?}", fake.starts);
    assert!(fake.starts[0].contains("--resume sess-bogus"));
    assert!(!fake.starts[1].contains("--resume"), "{:?}", fake.starts);
}

/// M2: `agent start` can fail while the agent IS running (not ready inside the timeout,
/// or held by a dialog). An agent in the pane afterwards is the resume, not a failure,
/// and never a second start.
#[test]
fn up_failed_resume_with_the_agent_present_is_a_resume() {
    let ws = TestWorkspace::new("up-resume-late");
    let mut fake = FakeUp::new(ws.root());
    fake.recorded = Some(("sess-slow".into(), "p1".into()));
    fake.fail_starts = 1;
    fake.fail_live = true;

    let cos = run(&mut fake).expect("up carries on");
    assert_eq!(cos, Cos::Resumed);
    assert_eq!(fake.starts.len(), 1, "no second start: {:?}", fake.starts);
    assert_eq!(fake.focused, vec!["p1".to_string()]);
}

/// The safety net the review asked for: an agent found in the pane before `agent start`
/// means nothing is started, whatever herdr would do with a busy pane.
#[test]
fn up_never_starts_a_second_cos_in_a_busy_pane() {
    let ws = TestWorkspace::new("up-busy");
    let mut fake = FakeUp::new(ws.root());
    // The selector looked just before the pane was tagged, then the agent appeared.
    fake.recorded = Some(("sess-busy".into(), "p1".into()));
    fake.agents = vec![Agent {
        name: None,
        pane: "p1".into(),
        status: "working".into(),
        session: Some("sess-busy".into()),
    }];

    let cos = run(&mut fake).expect("up carries on");
    assert_eq!(cos, Cos::Live);
    assert!(fake.starts.is_empty(), "{:?}", fake.starts);
}

// ---- R2: the missing-key error, verbatim ------------------------------------------

/// One `roles.cos.*` key missing: up exits non-zero with `cos_launch`'s own message, and
/// only after herdr is up, so the human can reach a pane to set it — and before any
/// daemon side effect (finding 4).
#[test]
fn up_missing_cos_keys_prints_the_error_verbatim() {
    let ws = TestWorkspace::new("up-missing");
    let mut conn = db::open(&ws.db_path()).unwrap();
    config::seed(&mut conn, &template()).unwrap();
    conn.execute("DELETE FROM setting WHERE key = 'roles.cos.model'", [])
        .unwrap();
    let expected = config::cos_launch(&conn).unwrap_err().to_string();
    assert!(expected.contains("roles.cos.model"), "{expected}");
    let bridged = up::launch_from(&conn).unwrap_err();
    assert_eq!(bridged.message, expected, "the message is cos_launch's own");
    assert_ne!(bridged.code, 0);

    let mut fake = FakeUp::new(ws.root());
    fake.running = false;
    fake.launch = Err(bridged);
    let mut daemon = FakeDaemon::answers();
    daemon.pings = vec![Ping::Unreachable];
    daemon.unit_installed = false;
    daemon.answer = Answer::Yes;

    let e = up::up(&mut fake, &mut daemon).unwrap_err();
    assert_eq!(e.code, 1);
    assert_eq!(e.message, expected);
    // herdr first: the human must be able to run `config set` from a pane.
    assert_eq!(fake.log[0], "session_running");
    assert_eq!(fake.log[1], "start_session");
    // and the config error costs no daemon side effect.
    assert_eq!(
        daemon.installs, 0,
        "nothing is installed for a config error"
    );
    assert_eq!(daemon.starts, 0);
    assert!(!fake.log.iter().any(|l| l.starts_with("create_workspace")));
    assert!(fake.starts.is_empty());
}

// ---- R1: attach only outside herdr; R4: the install path from a plain terminal ---

/// Outside herdr the TUI is attached at the end; inside, up focuses and returns.
#[test]
fn up_attaches_only_outside_herdr() {
    let ws = TestWorkspace::new("up-attach");
    let mut inside = FakeUp::new(ws.root());
    run(&mut inside).expect("up carries on");
    assert_eq!(inside.attached, 0);
    assert_eq!(inside.focused, vec!["p1".to_string()]);

    let mut outside = FakeUp::new(ws.root());
    outside.inside = false;
    run(&mut outside).expect("up carries on");
    assert_eq!(outside.attached, 1);
}

/// Outside herdr, no unit and a yes: the daemon is installed and started, then the CoS
/// comes up. `service::install` needs the session the herdr half resolved.
#[test]
fn up_no_unit_yes_installs_and_starts_outside_herdr() {
    let ws = TestWorkspace::new("up-install");
    let mut fake = FakeUp::new(ws.root());
    fake.inside = false;
    let mut daemon = FakeDaemon::answers();
    daemon.pings = vec![Ping::Unreachable, Ping::Answers];
    daemon.unit_installed = false;
    daemon.answer = Answer::Yes;

    let cos = up::up(&mut fake, &mut daemon).expect("up carries on");
    assert_eq!(cos, Cos::Fresh);
    assert_eq!(daemon.installs, 1);
    assert_eq!(daemon.starts, 1);
    assert_eq!(fake.attached, 1);
    assert!(!fake.starts.is_empty());
}

/// The daemon half can still refuse: up stops there and starts no CoS.
#[test]
fn up_daemon_refusal_stops_before_the_cos() {
    let ws = TestWorkspace::new("up-refused");
    let mut fake = FakeUp::new(ws.root());
    let mut daemon = FakeDaemon::answers();
    daemon.pings = vec![Ping::Unreachable];
    daemon.unit_installed = false;
    daemon.answer = Answer::NoTty;

    let e = up::up(&mut fake, &mut daemon).unwrap_err();
    assert_eq!(e.code, qq_core::cli::EXIT_UNREACHABLE);
    assert!(fake.starts.is_empty());
    // The config read comes first (finding 4), the daemon next: no workspace either way.
    assert!(fake.log.contains(&"launch".to_string()), "{:?}", fake.log);
    assert!(
        !fake.log.iter().any(|l| l.starts_with("workspace")),
        "{:?}",
        fake.log
    );
    assert!(
        !fake.log.contains(&"cos_pane:w1".to_string()),
        "{:?}",
        fake.log
    );
}

/// `Ready` from the daemon half is a carry-on, never a stop.
#[test]
fn up_carries_on_after_a_refused_daemon() {
    let ws = TestWorkspace::new("up-daemon-refused");
    let mut fake = FakeUp::new(ws.root());
    let mut daemon = FakeDaemon::answers();
    daemon.pings = vec![Ping::Refused("outside herdr".into())];

    let cos = up::up(&mut fake, &mut daemon).expect("a refusal is not a stop");
    assert_eq!(cos, Cos::Fresh);
    assert_eq!(fake.starts.len(), 1);
}

/// M1/finding 9: a workspace whose panes are all workers has nowhere to put the CoS.
/// `up` says so and exits non-zero, rather than calling a worker the CoS.
#[test]
fn up_reports_a_workspace_with_no_cos_pane() {
    let ws = TestWorkspace::new("up-worker-only");
    let mut fake = FakeUp::new(ws.root());
    fake.cos = Err("workspace w1 has no pane that can host the CoS".into());

    let e = run(&mut fake).unwrap_err();
    assert_ne!(e.code, 0);
    assert!(e.message.contains("w1"), "{}", e.message);
    assert!(fake.starts.is_empty(), "{:?}", fake.starts);
    assert!(fake.focused.is_empty());
}

/// finding 5: a focus miss must not turn a working farm into a failed `up`.
#[test]
fn up_focus_failure_warns_and_still_exits_zero() {
    let ws = TestWorkspace::new("up-focus-warn");
    let mut fake = FakeUp::new(ws.root()).live_in("p1");
    fake.focus_fails = true;

    let cos = run(&mut fake).expect("a focus miss is not a failure");
    assert_eq!(cos, Cos::Live);
    assert!(
        fake.lines.iter().any(|l| l.contains("focusing it failed")),
        "{:?}",
        fake.lines
    );
}

// ---- the one new config key -------------------------------------------------------

/// `up`'s daemon wait reads `notify.daemon_start_sec`, seeded well under the 600 s stall.
#[test]
fn up_wait_budget_reads_notify_daemon_start_sec() {
    let ws = TestWorkspace::new("up-budget");
    let mut conn = db::open(&ws.db_path()).unwrap();
    config::seed(&mut conn, &template()).unwrap();
    let w = up::daemon::wait_budget(&conn);
    assert!(w.deadline < Duration::from_secs(600), "{:?}", w.deadline);
    assert_eq!(w.deadline, Duration::from_secs(config::DAEMON_START_SEC));

    conn.execute(
        "INSERT INTO setting (key, value_json) VALUES ('notify.daemon_start_sec', '5') \
         ON CONFLICT(key) DO UPDATE SET value_json = '5'",
        [],
    )
    .unwrap();
    assert_eq!(
        up::daemon::wait_budget(&conn).deadline,
        Duration::from_secs(5)
    );
}

/// The key comes from `team.toml`: `[notify] daemon_start_sec` is parsed like every other
/// setting section.
#[test]
fn up_daemon_start_sec_is_parsed_from_team_toml() {
    let ws = TestWorkspace::new("up-budget-file");
    let mut conn: Connection = db::open(&ws.db_path()).unwrap();
    let text = template().replace("daemon_start_sec = 90", "daemon_start_sec = 7");
    config::seed(&mut conn, &text).expect("a [notify] section is a known section");
    assert_eq!(
        up::daemon::wait_budget(&conn).deadline,
        Duration::from_secs(7)
    );
}

/// finding 8: the herdr-session wait reads the same key, not the const.
#[test]
fn up_session_wait_reads_notify_daemon_start_sec() {
    let ws = TestWorkspace::new("up-session-budget");
    let mut conn: Connection = db::open(&ws.db_path()).unwrap();
    config::seed(&mut conn, &template()).unwrap();
    conn.execute(
        "INSERT INTO setting (key, value_json) VALUES ('notify.daemon_start_sec', '7') \
         ON CONFLICT(key) DO UPDATE SET value_json = '7'",
        [],
    )
    .unwrap();
    drop(conn);
    let host = qq_core::up::herdr::CliUp::new(ws.runs_dir());
    assert_eq!(host.budget(), Duration::from_secs(7));
}

// ---- the CoS pane, from a real pane list ------------------------------------------

/// A workspace shaped like the live one: a CoS tab and a workers tab, the worker listed
/// first. Ids are fake.
const PANES: &str = r#"{"result":{"panes":[
 {"pane_id":"w1:pWW","tab_id":"w1:tB","workspace_id":"w1","cwd":"/farm","agent":"pi","agent_status":"working","tokens":{"grid_role":"worker","grid_tab":"quaqua-workers"}},
 {"pane_id":"w1:pAA","tab_id":"w1:tA","workspace_id":"w1","cwd":"/farm","agent":"claude","agent_status":"done","tokens":{"grid_role":"cos","grid_owner":"cos"}},
 {"pane_id":"w7:pZZ","tab_id":"w7:tA","workspace_id":"w7","cwd":"/other","agent":"claude","agent_status":"idle","tokens":{"grid_role":"cos"}}
]}}"#;

const TABS: &str = r#"{"result":{"tabs":[
 {"tab_id":"w1:tB","label":"quaqua-workers","number":12,"pane_count":1},
 {"tab_id":"w1:tA","label":"qq","number":7,"pane_count":1},
 {"tab_id":"w7:tA","label":"cos","number":1,"pane_count":1}
]}}"#;

/// Only the workers tab: a CoS tab that was closed.
const PANES_WORKERS_ONLY: &str = r#"{"result":{"panes":[
 {"pane_id":"w1:pWW","tab_id":"w1:tB","workspace_id":"w1","agent":"pi","agent_status":"working","tokens":{"grid_role":"worker","grid_tab":"quaqua-workers"}},
 {"pane_id":"w1:pWW2","tab_id":"w1:tC","workspace_id":"w1","agent":"pi","agent_status":"idle","tokens":{"grid_role":"worker"}}
]}}"#;

const TABS_WORKERS_ONLY: &str = r#"{"result":{"tabs":[
 {"tab_id":"w1:tC","label":"workers2","number":20,"pane_count":1},
 {"tab_id":"w1:tB","label":"quaqua-workers","number":12,"pane_count":1}
]}}"#;

/// No tags at all: the CoS pane was never tagged, or the tokens were dropped by a herdr
/// restart. Two panes, the CoS in the first tab, a stale record.
const PANES_UNTAGGED: &str = r#"{"result":{"panes":[
 {"pane_id":"w1:pX","tab_id":"w1:tA","workspace_id":"w1","agent":null,"agent_status":"unknown","tokens":{"logo":""}},
 {"pane_id":"w1:pY","tab_id":"w1:tB","workspace_id":"w1","agent":"claude","agent_status":"done","tokens":{"logo":""}}
]}}"#;

const TABS_UNTAGGED: &str = r#"{"result":{"tabs":[
 {"tab_id":"w1:tA","label":"qq","number":7,"pane_count":1},
 {"tab_id":"w1:tB","label":"cos-extra","number":9,"pane_count":1}
]}}"#;

const AGENTS_NEW: &str = r#"{"result":{"agents":[
 {"agent":"claude","agent_status":"done","pane_id":"w1:pAA","tokens":{"grid_role":"cos"}},
 {"agent":"claude","agent_session":{"agent":"claude","kind":"id","value":"00000000-aaaa-bbbb-cccc-0000000000aa"},"agent_status":"done","pane_id":"w1:pY","tokens":{"grid_role":""}},
 {"agent":"pi","name":"sude-upint","agent_status":"working","pane_id":"w1:pWW"}
]}}"#;

fn lists(panes: &str, tabs: &str, agents: &str) -> PaneLists {
    PaneLists {
        panes: panes.into(),
        tabs: tabs.into(),
        agents: agents.into(),
    }
}

/// A tagged CoS wins over list order, and only its own workspace is considered.
#[test]
fn up_cos_pane_prefers_the_cos_tagged_pane() {
    let l = lists(PANES, TABS, AGENTS_NEW);
    let p = cos_pane(&l, "w1", None).expect("a CoS pane");
    assert_eq!(p.pane, "w1:pAA");
    assert!(p.live, "an agent is in it");
}

/// M1: a worker pane is never the CoS, however the pane list or the tab order is sorted.
#[test]
fn up_cos_pane_never_returns_a_worker_pane() {
    let l = lists(PANES_WORKERS_ONLY, TABS_WORKERS_ONLY, AGENTS_NEW);
    let e = cos_pane(&l, "w1", None).expect_err("workers only");
    assert!(e.contains("w1"), "{e}");
    // A record pointing at a worker does not make it the CoS either.
    let rec = (
        "00000000-aaaa-bbbb-cccc-0000000000aa".to_string(),
        "w1:pWW".to_string(),
    );
    assert!(cos_pane(&l, "w1", Some(&rec)).is_err());
}

/// The recorded pane is used when its agent session is the recorded one.
#[test]
fn up_cos_pane_uses_the_recorded_pane_when_its_session_matches() {
    let l = lists(PANES_UNTAGGED, TABS_UNTAGGED, AGENTS_NEW);
    let rec = (
        "00000000-aaaa-bbbb-cccc-0000000000aa".to_string(),
        "w1:pY".to_string(),
    );
    let p = cos_pane(&l, "w1", Some(&rec)).expect("a CoS pane");
    assert_eq!(p.pane, "w1:pY", "the record, not the first tab");
    assert!(p.live);
}

/// A record whose session is not the one in the pane is stale: fall back to the first tab.
#[test]
fn up_cos_pane_ignores_a_recorded_pane_whose_session_differs() {
    let l = lists(PANES_UNTAGGED, TABS_UNTAGGED, AGENTS_NEW);
    let rec = (
        "00000000-aaaa-bbbb-cccc-0000000000ff".to_string(),
        "w1:pY".to_string(),
    );
    let p = cos_pane(&l, "w1", Some(&rec)).expect("a CoS pane");
    assert_eq!(p.pane, "w1:pX", "the first pane of the first tab");
    assert!(!p.live);
}

/// finding 9: no pane at all is a clear error, not an empty `--pane`.
#[test]
fn up_cos_pane_errors_when_the_workspace_has_no_pane() {
    let l = lists(PANES, TABS, AGENTS_NEW);
    let e = cos_pane(&l, "w9", None).expect_err("no such workspace");
    assert!(e.contains("w9"), "{e}");
    let e = cos_pane(&lists("{}", "{}", AGENTS_NEW), "w1", None).expect_err("no panes");
    assert!(e.contains("w1"), "{e}");
}

/// The parser behind the seam, on the real payload shape: the pane decides, never a name.
#[test]
fn up_agent_list_marks_the_root_pane_live() {
    let unnamed = r#"{"id":"cli:agent:list","result":{"agents":[
      {"agent":"claude","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"00000000-aaaa-bbbb-cccc-000000000001"},"agent_status":"working","cwd":"/farm","focused":true,"pane_id":"w1:pEZ","tab_id":"w1:t1Z7","tokens":{"grid_farm":"quaqua","grid_owner":"cos","grid_role":"cos","logo":""},"workspace_id":"w1"},
      {"agent":"pi","name":"sude-upint","agent_status":"working","pane_id":"w1:pHP","workspace_id":"w1"}
    ]},"type":"agent_list"}"#;
    let live = agent_in_pane_json(unnamed, "w1:pEZ").expect("an unnamed CoS is live");
    assert_eq!(live.pane, "w1:pEZ");
    assert_eq!(live.name, None);
    assert_eq!(live.status, "working");
    assert_eq!(
        live.session.as_deref(),
        Some("00000000-aaaa-bbbb-cccc-000000000001")
    );
    assert!(agent_in_pane_json(unnamed, "w1:pOTHER").is_none());
}

// ---- the real host, against a scripted herdr --------------------------------------

/// A herdr that records its argv and answers `status server` from a file, so the real
/// `CliUp` seam can be exercised without herdr. The server branch writes `ready` last, so
/// its existence means `pids` and the log line are already there.
fn fake_herdr(dir: &Path) -> String {
    let bin = dir.join("herdr");
    let script = format!(
        r#"#!/bin/sh
dir={dir}
printf '%s\n' "$*" >> "$dir/argv"
[ "$1" = "--session" ] && shift 2
case "$1 $2" in
  "status server")
    sleep "$(cat "$dir/status_sleep" 2>/dev/null || echo 0)"
    printf '{{"running":true,"session":"fake","socket":"{dir}/herdr.sock"}}'
    ;;
  "server ")
    echo $$ >> "$dir/pids"
    echo "server was here" >&2
    touch "$dir/ready"
    sleep 30
    ;;
esac
"#,
        dir = dir.display()
    );
    std::fs::write(&bin, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin.to_string_lossy().into_owned()
}

/// Wait, bounded, for a marker a child process writes when (and only when) everything
/// the test then reads is already on disk.
fn wait_for(path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// The process group of `pid`, from the kernel: `/proc/<pid>/stat` field 5. The `comm`
/// field is parenthesised and may itself hold spaces or a `)`, so the split starts after
/// the last `)`. `ps` is only the fallback where there is no `/proc`; a CI image without
/// it would otherwise answer "" and every comparison against it would pass for free.
fn pgid_of(pid: &str) -> String {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let rest = stat.rsplit_once(')').map_or(stat.as_str(), |(_, r)| r);
        if let Some(pgid) = rest.split_whitespace().nth(2) {
            return pgid.to_string();
        }
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "pgid=", "-p", pid])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn own_pgid() -> String {
    pgid_of(&std::process::id().to_string())
}

/// M3: the server `up` starts must not share `up`'s process group (a Ctrl-C at the y/N
/// prompt would otherwise take the server, the CoS and every worker with it), and its
/// stderr must go to a log rather than a pipe nobody drains.
#[test]
fn up_start_session_detaches_the_server_and_logs_it() {
    let ws = TestWorkspace::new("up-detach");
    let dir = ws.root().join("herdr-fake");
    std::fs::create_dir_all(&dir).unwrap();
    let mut host = qq_core::up::herdr::CliUp::new(ws.runs_dir());
    host.bin = fake_herdr(&dir);
    host.set_root(ws.root());

    host.start_session().expect("the fake session comes up");
    // `start_session` returns once `status server` answers, which the fake does at once:
    // the server must be given until its last write, or its files are read half-made.
    assert!(
        wait_for(&dir.join("ready")),
        "the fake server never reached its last marker"
    );
    let pids = std::fs::read_to_string(dir.join("pids")).expect("the fake server ran");
    let pid = pids
        .lines()
        .next()
        .expect("the fake wrote a pid")
        .trim()
        .to_string();
    let pgid = pgid_of(&pid);

    assert!(!pgid.is_empty(), "no pgid for pid {pid}");
    assert_ne!(pgid, own_pgid(), "the server shares up's process group");
    let log = std::fs::read_to_string(ws.runs_dir().join("herdr-server.log")).unwrap_or_default();
    assert!(log.contains("server was here"), "stderr went to {log:?}");
    let _ = std::process::Command::new("kill").arg(&pid).status();
}

/// finding 6: even inside herdr, `up` addresses the configured session by name, so a run
/// from another session's pane cannot land in the ambient one.
#[test]
fn up_addresses_the_configured_session_even_inside_herdr() {
    let ws = TestWorkspace::new("up-session-addr");
    let dir = ws.root().join("herdr-fake");
    std::fs::create_dir_all(&dir).unwrap();
    let mut host = qq_core::up::herdr::CliUp::new(ws.runs_dir());
    host.bin = fake_herdr(&dir);
    host.set_root(ws.root());
    std::env::set_var("HERDR_ENV", "1");
    let running = host.session_running();
    std::env::remove_var("HERDR_ENV");

    assert!(running, "the fake reports running");
    let argv = std::fs::read_to_string(dir.join("argv")).unwrap();
    let first = argv.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        format!("--session {} status server --json", host.name()),
        "every call names the session"
    );
}

/// A missing `status server` answer is "not running", never a panic.
#[test]
fn up_session_running_is_false_without_a_herdr_answer() {
    let ws = TestWorkspace::new("up-session-missing");
    let mut host = qq_core::up::herdr::CliUp::new(ws.runs_dir());
    host.bin = ws
        .root()
        .join("no-such-herdr")
        .to_string_lossy()
        .into_owned();
    host.set_root(ws.root());
    assert!(!host.session_running());
}

// ---- the service seam: both backends, a recording runner, a scratch home ------------

/// Records every command and answers from a list; no process runs.
#[derive(Clone, Default)]
struct FakeRunner {
    calls: std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>>,
    answers: std::rc::Rc<std::cell::RefCell<Vec<std::io::Result<Ran>>>>,
}

impl Runner for FakeRunner {
    fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<Ran> {
        let call = std::iter::once(program.to_string())
            .chain(args.iter().cloned())
            .collect();
        self.calls.borrow_mut().push(call);
        self.answers.borrow_mut().remove(0)
    }
}

fn ran(code: i32, stdout: &str) -> std::io::Result<Ran> {
    Ok(Ran {
        code: Some(code),
        stdout: stdout.into(),
        stderr: String::new(),
    })
}

/// With no daemon and no backend, `up` says why and carries on to the CoS.
#[test]
fn up_without_a_service_backend_carries_on() {
    let ws = TestWorkspace::new("up-no-backend");
    let mut fake = FakeUp::new(ws.root());
    let mut daemon = FakeDaemon::answers();
    daemon.service = Err(NO_BACKEND.into());
    daemon.pings = vec![Ping::Unreachable];
    up::up(&mut fake, &mut daemon).expect("no backend is not a stop");
    assert_eq!((daemon.starts, daemon.installs), (0, 0));
    assert!(
        daemon.lines.iter().any(|l| l.contains(NO_BACKEND)),
        "{:?}",
        daemon.lines
    );
}

/// On launchd `up` starts the installed agent and waits on the label.
#[test]
fn up_starts_the_launchd_agent_and_names_the_label() {
    let ws = TestWorkspace::new("up-launchd-start");
    let mut fake = FakeUp::new(ws.root());
    let mut daemon = FakeDaemon::answers();
    daemon.service = Ok(launchd_names());
    daemon.pings = vec![Ping::Unreachable, Ping::Answers];
    up::up(&mut fake, &mut daemon).expect("up carries on");
    assert_eq!(daemon.starts, 1);
    assert!(
        daemon.lines.iter().any(|l| l.contains("com.quaqua.daemon")),
        "{:?}",
        daemon.lines
    );
}

/// The real host on launchd: plist check and start go through the runner, in a scratch home.
#[test]
fn service_host_launchd_uses_the_plist_and_launchctl() {
    let ws = TestWorkspace::new("up-host-launchd");
    let runner = FakeRunner::default();
    *runner.answers.borrow_mut() = vec![ran(0, ""), ran(0, "")];
    let svc = Service::resolve(
        Some(Platform::Launchd),
        ws.root(),
        None,
        &Default::default(),
        501,
    );
    let mut host = ServiceHost::with_service(ws.runs_dir(), svc, Box::new(runner.clone()));
    assert_eq!(host.service().unwrap(), launchd_names());
    assert!(!host.unit_installed());
    let plist = ws
        .root()
        .join("Library/LaunchAgents/com.quaqua.daemon.plist");
    std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
    std::fs::write(&plist, "<plist/>").unwrap();
    assert!(host.unit_installed());
    host.start_unit().unwrap();
    let calls = runner.calls.borrow().clone();
    let expect = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        calls,
        vec![
            expect(&["launchctl", "print", "gui/501/com.quaqua.daemon"]),
            expect(&["launchctl", "kickstart", "gui/501/com.quaqua.daemon"]),
        ]
    );
}

/// The real host on systemd: LoadState and start go through the runner.
#[test]
fn service_host_systemd_uses_systemctl() {
    let ws = TestWorkspace::new("up-host-systemd");
    let runner = FakeRunner::default();
    *runner.answers.borrow_mut() = vec![ran(0, "loaded\n"), ran(0, "")];
    let svc = Service::resolve(
        Some(Platform::Systemd),
        ws.root(),
        None,
        &Default::default(),
        501,
    );
    let mut host = ServiceHost::with_service(ws.runs_dir(), svc, Box::new(runner.clone()));
    assert_eq!(host.service().unwrap(), systemd_names());
    assert!(host.unit_installed());
    host.start_unit().unwrap();
    let calls = runner.calls.borrow().clone();
    assert_eq!(calls[1][..3], ["systemctl", "--user", "start"]);
}
