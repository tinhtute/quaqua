//! `quaqua up` (R1-R4): the whole flow against one herdr fake and one daemon fake,
//! the R2 error verbatim, and the outside-herdr install path (R4's acceptance).
//!
//! No test here runs herdr, systemd, a TTY, or a real clock: every outside effect is
//! behind [`UpHost`] or [`DaemonHost`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use qq_core::cli::{CliError, Env};
use qq_core::config::{self, CosLaunch};
use qq_core::db;
use qq_core::testkit::TestWorkspace;
use qq_core::up::daemon::{Answer, DaemonHost, Ping, Platform, Ready, Wait};
use qq_core::up::herdr::{Agent, UpHost, Workspace};
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
    /// One entry per live agent, keyed by pane: the shape `agent list` reports.
    agents: Vec<Agent>,
    /// Each `start_agent` as `kind|pane|arg arg ...`.
    starts: Vec<String>,
    /// The first N `start_agent` calls fail (a failed resume).
    fail_starts: usize,
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
            workspace: Some(Workspace {
                id: "w1".into(),
                root_pane: "p1".into(),
            }),
            agents: Vec::new(),
            starts: Vec::new(),
            fail_starts: 0,
            log: Vec::new(),
            lines: Vec::new(),
            focused: Vec::new(),
            attached: 0,
        }
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
        let ws = Workspace {
            id: "w-new".into(),
            root_pane: "p-new".into(),
        };
        self.workspace = Some(ws.clone());
        Ok(ws)
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
    ) -> Result<(), String> {
        self.log.push(format!("start_agent:{name}:{pane}"));
        if self.fail_starts > 0 {
            self.fail_starts -= 1;
            self.starts
                .push(format!("{kind}|{pane}|{}", args.join(" ")));
            return Err("agent_not_ready".into());
        }
        self.starts
            .push(format!("{kind}|{pane}|{}", args.join(" ")));
        Ok(())
    }

    fn focus(&mut self, pane: &str) -> Result<(), String> {
        self.log.push(format!("focus:{pane}"));
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

/// The daemon world: ping answers, the unit is installed, the human says yes.
struct FakeDaemon {
    platform: Platform,
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
            platform: Platform::Linux,
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
    fn platform(&self) -> Platform {
        self.platform
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
/// launched from `roles.cos` alone, into the workspace's root pane.
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
    assert_eq!(
        fake.starts,
        vec!["claude|p-new|--model claude-opus-5-5 --effort low".to_string()]
    );
    assert_eq!(fake.focused, 1);
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
    let mut fake = FakeUp::new(ws.root());
    fake.agents = vec![Agent {
        name: None,
        pane: "p1".into(),
        status: "working".into(),
    }];
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

// ---- R2: the missing-key error, verbatim ------------------------------------------

/// One or all `roles.cos.*` keys missing: up exits non-zero with `cos_launch`'s own
/// message, and only after herdr is up, so the human can reach a pane to set them.
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

    let e = run(&mut fake).unwrap_err();
    assert_ne!(e.code, 0);
    assert_eq!(e.message, expected);
    // herdr first: the human must be able to run `config set` from a pane.
    assert_eq!(fake.log[0], "session_running");
    assert_eq!(fake.log[1], "start_session");
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
/// comes up. `service::install` needs the session the herdr half resolved, so this is
/// the branch that fails from a bare terminal without R4's env wiring.
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
    assert!(!fake.log.contains(&"launch".to_string()), "{:?}", fake.log);
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

/// The key comes from `team.toml`: `[notify] daemon_start_sec` is parsed like every
/// other setting section, so a farm that sets it is obeyed.
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
    let _ = (
        Env {
            runs_dir: ws.runs_dir(),
        },
        Ready::Answers,
        Arc::new(()),
    );
}

// ---- the live CoS, found by pane --------------------------------------------------

/// The real `agent list` shape (ids replaced): only agents started with a name carry
/// `name`, and the CoS started by the launcher carries none.
const ROOT_AGENT_UNNAMED: &str = r#"{"id":"cli:agent:list","result":{"agents":[
  {"agent":"claude","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"00000000-aaaa-bbbb-cccc-000000000001"},"agent_status":"working","cwd":"/farm","focused":true,"pane_id":"w1:pEZ","tab_id":"w1:t1Z7","tokens":{"grid_farm":"quaqua","grid_owner":"cos","grid_role":"cos","logo":""},"workspace_id":"w1"},
  {"agent":"pi","name":"sude-upint","agent_status":"working","pane_id":"w1:pHP","workspace_id":"w1"}
]},"type":"agent_list"}"#;

/// Nothing in the root pane: the entry is in another workspace.
const ROOT_AGENT_ABSENT: &str = r#"{"id":"cli:agent:list","result":{"agents":[
  {"agent":"pi","name":"sude-upint","agent_status":"working","pane_id":"w1:pHP","workspace_id":"w1"}
]},"type":"agent_list"}"#;

/// A CoS that was started with the name `cos` in the root pane.
const ROOT_AGENT_NAMED: &str = r#"{"id":"cli:agent:list","result":{"agents":[
  {"agent":"claude","name":"cos","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"00000000-aaaa-bbbb-cccc-000000000002"},"agent_status":"idle","pane_id":"w2:pQA","workspace_id":"w2"}
]},"type":"agent_list"}"#;

/// The parser behind the seam, on the real payload: the root pane decides, never a name.
#[test]
fn up_agent_list_marks_the_root_pane_live() {
    let live = qq_core::up::herdr::agent_in_pane_json(ROOT_AGENT_UNNAMED, "w1:pEZ")
        .expect("an unnamed agent in the root pane is the CoS");
    assert_eq!(live.pane, "w1:pEZ");
    assert_eq!(live.name, None);
    assert_eq!(live.status, "working");
    assert_eq!(live.session.as_deref(), Some("00000000-aaaa-bbbb-cccc-000000000001"));

    assert!(
        qq_core::up::herdr::agent_in_pane_json(ROOT_AGENT_ABSENT, "w1:pEZ").is_none(),
        "an agent in another pane is not the CoS"
    );

    let named = qq_core::up::herdr::agent_in_pane_json(ROOT_AGENT_NAMED, "w2:pQA")
        .expect("a named cos in the root pane is the CoS too");
    assert_eq!(named.name.as_deref(), Some("cos"));
    // A name is never the key: `cos` is not found in a pane it does not occupy.
    assert!(qq_core::up::herdr::agent_in_pane_json(ROOT_AGENT_NAMED, "w2:pOTHER").is_none());
}

/// Start state (b): the session is up and the CoS runs, unnamed, in the root pane. `up`
/// starts nothing and focuses that pane.
#[test]
fn up_live_cos_in_the_root_pane_is_found_by_pane_not_name() {
    let ws = TestWorkspace::new("up-live-pane");
    let mut fake = FakeUp::new(ws.root());
    fake.agents = vec![Agent {
        name: None,
        pane: "p1".into(),
        status: "working".into(),
    }];

    let cos = run(&mut fake).expect("up carries on");
    assert_eq!(cos, Cos::Live);
    assert!(fake.starts.is_empty(), "{:?}", fake.starts);
    assert_eq!(fake.focused, vec!["p1".to_string()]);
    assert!(
        !fake.log.iter().any(|l| l.starts_with("recorded")),
        "a live CoS needs no record: {:?}",
        fake.log
    );
}

/// A CoS restarted by hand in its recorded pane is still the CoS: found there, focused
/// there, and never started a second time in the root pane.
#[test]
fn up_live_cos_in_its_recorded_pane_is_found() {
    let ws = TestWorkspace::new("up-live-recorded-pane");
    let mut fake = FakeUp::new(ws.root());
    fake.workspace = Some(Workspace {
        id: "w1".into(),
        root_pane: "p1".into(),
    });
    fake.recorded = Some((
        "00000000-aaaa-bbbb-cccc-000000000003".into(),
        "p7".into(),
    ));
    fake.agents = vec![Agent {
        name: None,
        pane: "p7".into(),
        status: "idle".into(),
    }];

    let cos = run(&mut fake).expect("up carries on");
    assert_eq!(cos, Cos::Live);
    assert!(fake.starts.is_empty(), "{:?}", fake.starts);
    assert_eq!(fake.focused, vec!["p7".to_string()]);
}
