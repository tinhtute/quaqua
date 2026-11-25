//! The daemon half of `quaqua up` (R4): make sure the farm daemon answers before the
//! CoS is started, or say why it cannot. Every outside effect — the ping, the service
//! manager (systemd or launchd, through `daemon::service`), the TTY prompt, the clock and
//! the printed lines — goes through [`DaemonHost`], so the decision is unit-tested against
//! fakes and this module never touches a real unit, a real socket or a real terminal in a test.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cli::{CliError, Env, EXIT_REFUSED, EXIT_UNREACHABLE};
pub use crate::daemon::service::Names;
use crate::daemon::service::{HerdrEnv, ProcessRunner, Runner, Service};

/// The command a human runs to install the unit by hand: exactly what `up` runs for them.
pub const INSTALL_CMD: &str = "quaqua daemon install";

/// One `quaqua ping`, as the exit codes #368/#370 gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ping {
    /// Exit 0: the daemon answers.
    Answers,
    /// Exit 77: it answers and refuses this caller (a herdr outage is one refusal);
    /// the text says why.
    Refused(String),
    /// Exit 69: nothing listens on the daemon socket.
    Unreachable,
    /// Any other status: the socket was reached but the call did not finish. The daemon
    /// may be alive and may have acted, so it is never started a second time.
    Unclear(String),
}

/// The y/N answer when no unit is installed; `NoTty` is a human who cannot be asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    NoTty,
}

/// How long the unit gets to answer and how often to look. Both come from config: the
/// deadline is `notify.daemon_start_sec` through [`crate::config::daemon_start_sec`], the
/// cadence is the daemon's own report poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait {
    pub deadline: Duration,
    pub poll: Duration,
}

/// The check is over. Every variant is a `up` that carries on: a refusal and an
/// unsupported platform do not stop the CoS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ready {
    /// Ping answered 0 on the first look; nothing was run.
    Answers,
    /// It answered with a status that is not "unreachable" (77 refused, or a call that
    /// did not finish): the daemon is up, so it is never started again.
    Refused,
    /// `up` started the unit — installing it first when the human said yes — and it
    /// answers now.
    Started,
    /// No service backend here (a target that is neither Linux nor macOS, or no HOME):
    /// nothing can be started from this host.
    Unsupported,
}

/// The outside world, one method per effect. The real one is [`ServiceHost`]; a test
/// implements this and never runs systemd or launchd, waits, or asks a terminal.
pub trait DaemonHost {
    /// The unit (or launchd label) and the status command to show. `Err(why)` means
    /// nothing can be started here, for example `NO_BACKEND` or "HOME is not set".
    fn service(&self) -> Result<Names, String>;
    /// One `quaqua ping`.
    fn ping(&mut self) -> Ping;
    /// Whether the unit (systemd) or the plist (launchd) exists.
    fn unit_installed(&mut self) -> bool;
    /// Starts the unit or agent; the error names the failing command and its stderr.
    fn start_unit(&mut self) -> Result<(), String>;
    /// The same install `quaqua daemon install` runs, output printed through [`line`].
    ///
    /// [`line`]: DaemonHost::line
    fn install_unit(&mut self) -> Result<(), String>;
    /// Ask y/N on the terminal.
    fn ask_install(&mut self) -> Answer;
    /// The wait budget, from config.
    fn wait(&mut self) -> Wait;
    /// One line for the human.
    fn line(&mut self, s: &str);
    /// Sleep between two pings.
    fn sleep(&mut self, d: Duration);
    /// The clock the deadline is measured on.
    fn now(&self) -> Instant;
}

/// Makes sure the farm daemon answers.
///
/// `Ok` means `up` carries on: the daemon answers, answers and refuses this caller, was
/// started here, or cannot be started on this target (no backend).
/// `Err` is a daemon that is not running and will not be: no unit and no install (the
/// message names [`INSTALL_CMD`]), a failed install or start, or a unit that did not
/// answer within the config's budget (the message names the unit or label). Exit status is
/// [`EXIT_UNREACHABLE`]: the daemon is unreachable, so sending anything again cannot help.
pub fn ensure_answers(host: &mut dyn DaemonHost) -> Result<Ready, CliError> {
    match host.ping() {
        Ping::Answers => return Ok(Ready::Answers),
        Ping::Refused(why) => {
            host.line(&format!(
                "quaqua up: the daemon is up but refused this caller: {why}"
            ));
            return Ok(Ready::Refused);
        }
        Ping::Unclear(why) => {
            host.line(&format!(
                "quaqua up: the daemon answered but the ping did not finish ({why}); not starting a second one"
            ));
            return Ok(Ready::Refused);
        }
        Ping::Unreachable => {}
    }
    let names = match host.service() {
        Ok(n) => n,
        Err(why) => {
            host.line(&format!(
                "quaqua up: cannot start the daemon here ({why}); start it yourself, then run up again"
            ));
            return Ok(Ready::Unsupported);
        }
    };
    let Some(installed) = installed_or_asked(host)? else {
        return Err(no_unit_error(&names));
    };
    if !installed {
        host.install_unit()
            .map_err(|e| CliError::new(EXIT_UNREACHABLE, format!("`{INSTALL_CMD}` failed: {e}")))?;
    }
    host.start_unit().map_err(|e| {
        CliError::new(
            EXIT_UNREACHABLE,
            format!("starting {} failed: {e}", names.unit),
        )
    })?;
    let wait = host.wait();
    host.line(&format!(
        "quaqua up: started {}; waiting up to {}s for it to answer (notify.daemon_start_sec)",
        names.unit,
        wait.deadline.as_secs()
    ));
    wait_until_up(host, wait, &names)
}

/// `true` when the unit is already installed, `false` when the human agreed to install
/// it, `None` when nobody can be asked and nothing was done.
fn installed_or_asked(host: &mut dyn DaemonHost) -> Result<Option<bool>, CliError> {
    if host.unit_installed() {
        return Ok(Some(true));
    }
    match host.ask_install() {
        Answer::Yes => Ok(Some(false)),
        Answer::No | Answer::NoTty => Ok(None),
    }
}

fn no_unit_error(names: &Names) -> CliError {
    CliError::new(
        EXIT_UNREACHABLE,
        format!(
            "the daemon is not running and {} is not installed; install it with `{INSTALL_CMD}`",
            names.unit
        ),
    )
}

/// Pings until the daemon answers or the budget runs out. A refusal counts as answered:
/// the unit is up, it just does not want this caller.
fn wait_until_up(host: &mut dyn DaemonHost, wait: Wait, names: &Names) -> Result<Ready, CliError> {
    let begin = host.now();
    loop {
        match host.ping() {
            Ping::Unreachable => {}
            Ping::Answers => return Ok(Ready::Started),
            Ping::Refused(why) => {
                host.line(&format!(
                    "quaqua up: the daemon is up but refused this caller: {why}"
                ));
                return Ok(Ready::Started);
            }
            Ping::Unclear(why) => {
                host.line(&format!(
                    "quaqua up: the daemon answered but the ping did not finish ({why})"
                ));
                return Ok(Ready::Started);
            }
        }
        let left = wait
            .deadline
            .saturating_sub(host.now().saturating_duration_since(begin));
        if left.is_zero() {
            return Err(CliError::new(
                EXIT_UNREACHABLE,
                format!(
                    "{} did not answer within {}s of the start; see `{}`",
                    names.unit,
                    wait.deadline.as_secs(),
                    names.status
                ),
            ));
        }
        host.sleep(wait.poll.min(left));
    }
}

/// The wait budget: the deadline is `notify.daemon_start_sec` (read by
/// [`crate::config::daemon_start_sec`], default [`crate::config::DAEMON_START_SEC`], well
/// under the 600 s stall), the cadence is the daemon's own report poll.
pub fn wait_budget(conn: &rusqlite::Connection) -> Wait {
    Wait {
        deadline: crate::config::daemon_start_sec(conn),
        poll: crate::daemon::watch::REPORT_POLL,
    }
}

/// The real host: `quaqua ping` through the CLI client, the unit or agent through
/// [`Service`] (systemd or launchd) and its [`Runner`], the install through
/// [`crate::daemon::service::install_with`] on the host's own service (the spec `daemon install`
/// writes), config
/// through the store, the terminal through stdio.
pub struct ServiceHost {
    runs_dir: PathBuf,
    svc: Result<Service, String>,
    runner: Box<dyn Runner>,
    herdr: Result<HerdrEnv, String>,
}

impl ServiceHost {
    /// The process's own service (HOME, XDG, uid, config, target) and the real runner.
    /// Only `up_cmd` calls this.
    pub fn new(runs_dir: impl Into<PathBuf>) -> Self {
        let runs_dir = runs_dir.into();
        let svc = Service::for_process(&runs_dir);
        ServiceHost::with_service(runs_dir, svc, Box::new(ProcessRunner))
    }

    /// A host on a given service and runner: what a test builds with a recording fake.
    pub fn with_service(
        runs_dir: impl Into<PathBuf>,
        svc: Result<Service, String>,
        runner: Box<dyn Runner>,
    ) -> Self {
        ServiceHost {
            runs_dir: runs_dir.into(),
            svc,
            runner,
            herdr: HerdrEnv::from_process(),
        }
    }

    /// The herdr session the installed unit carries, in place of the process's.
    pub fn with_herdr(mut self, herdr: HerdrEnv) -> Self {
        self.herdr = Ok(herdr);
        self
    }
}

impl DaemonHost for ServiceHost {
    fn service(&self) -> Result<Names, String> {
        self.svc.as_ref().map(Service::names).map_err(Clone::clone)
    }

    fn ping(&mut self) -> Ping {
        let env = Env {
            runs_dir: self.runs_dir.clone(),
        };
        // The same call the `ping` command makes, so the codes cannot drift apart.
        match crate::cli::rpc_call(&env, "ping", Value::Null) {
            Ok(_) => Ping::Answers,
            Err(e) if e.code == EXIT_REFUSED => Ping::Refused(e.message),
            Err(e) if e.code == EXIT_UNREACHABLE => Ping::Unreachable,
            Err(e) => Ping::Unclear(e.message),
        }
    }

    fn unit_installed(&mut self) -> bool {
        // Systemd: `show`'s LoadState; launchd: the plist on disk (see `Service::installed`).
        self.svc
            .as_ref()
            .is_ok_and(|svc| svc.installed(self.runner.as_mut()))
    }

    fn start_unit(&mut self) -> Result<(), String> {
        match &self.svc {
            Ok(svc) => svc.start(self.runner.as_mut()),
            Err(why) => Err(why.clone()),
        }
    }

    fn install_unit(&mut self) -> Result<(), String> {
        // The host's own service, never `Service::for_process`: the file written is the
        // `svc.file` that `start_unit` loads, and a test host never reads the real HOME.
        let svc = self.svc.as_ref().map_err(Clone::clone)?;
        let herdr = self.herdr.as_ref().map_err(Clone::clone)?;
        let text = crate::daemon::service::install_with(
            svc,
            &self.runs_dir,
            &[String::from("install")],
            herdr,
        )?;
        self.line(text.trim_end());
        Ok(())
    }

    fn ask_install(&mut self) -> Answer {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Answer::NoTty;
        }
        let unit = self
            .service()
            .map_or_else(|_| "the daemon".to_string(), |n| n.unit);
        self.line(&format!(
            "quaqua up: {unit} is not installed; install it now? [y/N]"
        ));
        let mut buf = String::new();
        if std::io::stdin().read_line(&mut buf).is_err() {
            return Answer::No;
        }
        match buf.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => Answer::Yes,
            _ => Answer::No,
        }
    }

    fn wait(&mut self) -> Wait {
        let path = self.runs_dir.join(crate::db::path::DB_FILE);
        crate::db::open(&path).map_or_else(
            // No store to read: the stall default `config::stall_sec` itself falls back to.
            |_| Wait {
                deadline: Duration::from_secs(crate::config::DAEMON_START_SEC),
                poll: crate::daemon::watch::REPORT_POLL,
            },
            |c| wait_budget(&c),
        )
    }

    fn line(&mut self, s: &str) {
        println!("{s}");
    }

    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::daemon::service::{Platform, Ran, NO_BACKEND, UNIT_NAME};
    use std::cell::RefCell;
    use std::rc::Rc;

    const POLL_MS: u64 = 250;
    const LABEL: &str = "com.quaqua.daemon";

    fn systemd_names() -> Names {
        Names {
            unit: UNIT_NAME.into(),
            status: format!("systemctl --user status {UNIT_NAME}"),
        }
    }

    fn launchd_names() -> Names {
        Names {
            unit: LABEL.into(),
            status: format!("launchctl print gui/501/{LABEL}"),
        }
    }

    /// A host that records every effect and answers from a list of pings; the last one
    /// repeats. `sleep` only moves its own clock, so a test never waits.
    struct Fake {
        service: Result<Names, String>,
        pings: Vec<Ping>,
        unit_installed: bool,
        start: Result<(), String>,
        install: Result<(), String>,
        answer: Answer,
        wait: Wait,
        lines: Vec<String>,
        starts: usize,
        installs: usize,
        sleeps: Vec<Duration>,
        base: Instant,
        at: Duration,
    }

    impl Fake {
        fn new() -> Self {
            Fake {
                service: Ok(systemd_names()),
                pings: vec![Ping::Answers],
                unit_installed: true,
                start: Ok(()),
                install: Ok(()),
                answer: Answer::No,
                wait: Wait {
                    deadline: Duration::from_secs(1),
                    poll: Duration::from_millis(POLL_MS),
                },
                lines: Vec::new(),
                starts: 0,
                installs: 0,
                sleeps: Vec::new(),
                base: Instant::now(),
                at: Duration::ZERO,
            }
        }

        fn pings(mut self, p: &[Ping]) -> Self {
            self.pings = p.to_vec();
            self
        }

        fn no_unit(mut self) -> Self {
            self.unit_installed = false;
            self
        }

        fn answer(mut self, a: Answer) -> Self {
            self.answer = a;
            self
        }

        fn no_backend(mut self, why: &str) -> Self {
            self.service = Err(why.to_string());
            self
        }

        fn names(mut self, n: Names) -> Self {
            self.service = Ok(n);
            self
        }

        fn deadline_ms(mut self, ms: u64) -> Self {
            self.wait.deadline = Duration::from_millis(ms);
            self
        }

        fn start(mut self, r: Result<(), String>) -> Self {
            self.start = r;
            self
        }

        fn install(mut self, r: Result<(), String>) -> Self {
            self.install = r;
            self
        }

        fn slept(&self) -> Duration {
            self.sleeps.iter().copied().sum()
        }
    }

    impl DaemonHost for Fake {
        fn service(&self) -> Result<Names, String> {
            self.service.clone()
        }
        fn ping(&mut self) -> Ping {
            if self.pings.len() > 1 {
                self.pings.remove(0)
            } else {
                self.pings.first().cloned().unwrap_or(Ping::Unreachable)
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
            self.wait
        }
        fn line(&mut self, s: &str) {
            self.lines.push(s.to_string());
        }
        fn sleep(&mut self, d: Duration) {
            self.sleeps.push(d);
            self.at += d;
        }
        fn now(&self) -> Instant {
            self.base + self.at
        }
    }

    #[test]
    fn up_daemon_ping_ok_runs_nothing() {
        let mut h = Fake::new();
        assert_eq!(ensure_answers(&mut h).unwrap(), Ready::Answers);
        assert!(h.lines.is_empty(), "a working daemon prints nothing");
        assert_eq!((h.starts, h.installs), (0, 0));
        assert!(h.sleeps.is_empty());
    }

    #[test]
    fn up_daemon_refused_says_why_and_starts_nothing() {
        let mut h = Fake::new().pings(&[Ping::Refused("peer unmapped".into())]);
        assert_eq!(ensure_answers(&mut h).unwrap(), Ready::Refused);
        assert_eq!(h.lines.len(), 1, "one line: {:?}", h.lines);
        assert!(h.lines[0].contains("refused") && h.lines[0].contains("peer unmapped"));
        assert_eq!(h.starts, 0, "the daemon is up; a second one must not start");
    }

    #[test]
    fn up_daemon_unclear_ping_is_still_up() {
        let mut h = Fake::new().pings(&[Ping::Unclear("rpc failed: timed out".into())]);
        assert_eq!(ensure_answers(&mut h).unwrap(), Ready::Refused);
        assert!(h.lines[0].contains("rpc failed: timed out"));
        assert_eq!(h.starts, 0, "it may be alive, so it is never started twice");
    }

    #[test]
    fn up_daemon_starts_the_unit_when_the_daemon_is_down() {
        let mut h = Fake::new().pings(&[Ping::Unreachable, Ping::Answers]);
        assert_eq!(ensure_answers(&mut h).unwrap(), Ready::Started);
        assert_eq!(h.starts, 1);
        assert!(
            h.lines.iter().any(|l| l.contains(UNIT_NAME)),
            "{:?}",
            h.lines
        );
        assert!(h.sleeps.is_empty(), "the first ping after start answers");
    }

    #[test]
    fn up_daemon_waits_for_the_daemon_to_answer() {
        let mut h = Fake::new().pings(&[
            Ping::Unreachable,
            Ping::Unreachable,
            Ping::Unreachable,
            Ping::Answers,
        ]);
        assert_eq!(ensure_answers(&mut h).unwrap(), Ready::Started);
        assert_eq!(h.starts, 1);
        assert_eq!(h.sleeps.len(), 2, "two rounds of polling before the answer");
        assert!(h.slept() <= Duration::from_secs(1), "inside the budget");
    }

    #[test]
    fn up_daemon_started_unit_that_refuses_counts_as_up() {
        let mut h = Fake::new().pings(&[Ping::Unreachable, Ping::Refused("herdr down".into())]);
        assert_eq!(ensure_answers(&mut h).unwrap(), Ready::Started);
        assert!(
            h.lines.iter().any(|l| l.contains("herdr down")),
            "{:?}",
            h.lines
        );
    }

    #[test]
    fn up_daemon_timeout_names_the_unit() {
        let mut h = Fake::new().pings(&[Ping::Unreachable]).deadline_ms(500);
        let e = ensure_answers(&mut h).unwrap_err();
        assert_eq!(e.code, EXIT_UNREACHABLE);
        assert!(e.message.contains(UNIT_NAME), "{}", e.message);
        assert!(
            e.message.contains("systemctl --user status"),
            "{}",
            e.message
        );
        assert_eq!(h.slept(), Duration::from_millis(500), "stops at the budget");
    }

    #[test]
    fn up_daemon_missing_unit_yes_installs_then_starts() {
        let mut h = Fake::new()
            .no_unit()
            .answer(Answer::Yes)
            .pings(&[Ping::Unreachable, Ping::Answers]);
        assert_eq!(ensure_answers(&mut h).unwrap(), Ready::Started);
        assert_eq!((h.installs, h.starts), (1, 1));
    }

    #[test]
    fn up_daemon_missing_unit_no_exits_with_the_install_command() {
        let mut h = Fake::new()
            .no_unit()
            .answer(Answer::No)
            .pings(&[Ping::Unreachable]);
        let e = ensure_answers(&mut h).unwrap_err();
        assert_eq!(e.code, EXIT_UNREACHABLE);
        assert!(e.message.contains(INSTALL_CMD), "{}", e.message);
        assert_eq!((h.installs, h.starts), (0, 0));
    }

    #[test]
    fn up_daemon_missing_unit_without_a_tty_prints_the_install_command() {
        let mut h = Fake::new()
            .no_unit()
            .answer(Answer::NoTty)
            .pings(&[Ping::Unreachable]);
        let e = ensure_answers(&mut h).unwrap_err();
        assert_eq!(e.code, EXIT_UNREACHABLE);
        assert!(e.message.contains(INSTALL_CMD), "{}", e.message);
        assert_eq!((h.installs, h.starts), (0, 0));
    }

    #[test]
    fn up_daemon_without_a_backend_says_why_and_starts_nothing() {
        let mut h = Fake::new()
            .no_backend(NO_BACKEND)
            .pings(&[Ping::Unreachable]);
        assert_eq!(ensure_answers(&mut h).unwrap(), Ready::Unsupported);
        assert_eq!(h.lines.len(), 1, "{:?}", h.lines);
        assert!(h.lines[0].contains(NO_BACKEND), "{}", h.lines[0]);
        assert_eq!((h.starts, h.installs), (0, 0));
    }

    #[test]
    fn up_daemon_without_a_backend_still_accepts_a_live_daemon() {
        let mut h = Fake::new().no_backend(NO_BACKEND);
        assert_eq!(ensure_answers(&mut h).unwrap(), Ready::Answers);
        assert!(h.lines.is_empty(), "{:?}", h.lines);
    }

    #[test]
    fn up_daemon_launchd_start_names_the_label() {
        let mut h = Fake::new()
            .names(launchd_names())
            .pings(&[Ping::Unreachable, Ping::Answers]);
        assert_eq!(ensure_answers(&mut h).unwrap(), Ready::Started);
        assert_eq!(h.starts, 1);
        assert!(h.lines[0].contains(LABEL), "{}", h.lines[0]);
    }

    #[test]
    fn up_daemon_launchd_start_failure_wraps_the_command_error() {
        let mut h = Fake::new()
            .names(launchd_names())
            .pings(&[Ping::Unreachable])
            .start(Err(
                "`launchctl bootstrap gui/501 /x.plist` failed: Bootstrap failed: 5".into(),
            ));
        let e = ensure_answers(&mut h).unwrap_err();
        assert_eq!(e.code, EXIT_UNREACHABLE);
        assert!(
            e.message
                .starts_with(&format!("starting {LABEL} failed: `launchctl bootstrap")),
            "{}",
            e.message
        );
    }

    #[test]
    fn up_daemon_launchd_timeout_names_the_label_and_print() {
        let mut h = Fake::new()
            .names(launchd_names())
            .pings(&[Ping::Unreachable])
            .deadline_ms(500);
        let e = ensure_answers(&mut h).unwrap_err();
        assert_eq!(e.code, EXIT_UNREACHABLE);
        assert!(e.message.contains(LABEL), "{}", e.message);
        assert!(
            e.message
                .contains(&format!("launchctl print gui/501/{LABEL}")),
            "{}",
            e.message
        );
    }

    /// Records every command and answers from a list; no process runs.
    #[derive(Clone, Default)]
    struct FakeRunner {
        calls: Rc<RefCell<Vec<Vec<String>>>>,
        answers: Rc<RefCell<Vec<std::io::Result<Ran>>>>,
    }

    impl FakeRunner {
        fn answering(a: Vec<std::io::Result<Ran>>) -> Self {
            FakeRunner {
                calls: Rc::default(),
                answers: Rc::new(RefCell::new(a)),
            }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl Runner for FakeRunner {
        fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<Ran> {
            self.calls.borrow_mut().push(
                std::iter::once(program.to_string())
                    .chain(args.iter().cloned())
                    .collect(),
            );
            self.answers.borrow_mut().remove(0)
        }
    }

    fn ran(code: i32, stdout: &str, stderr: &str) -> std::io::Result<Ran> {
        Ok(Ran {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        })
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn service(platform: Option<Platform>, home: &std::path::Path) -> Result<Service, String> {
        Service::resolve(platform, home, None, &Default::default(), 501)
    }

    #[test]
    fn service_host_launchd_checks_the_plist_and_starts_through_launchctl() {
        let ws = crate::testkit::TestWorkspace::new("up-host-launchd");
        let runner = FakeRunner::answering(vec![ran(113, "", ""), ran(0, "", "")]);
        let mut h = ServiceHost::with_service(
            ws.runs_dir(),
            service(Some(Platform::Launchd), ws.root()),
            Box::new(runner.clone()),
        );
        assert_eq!(h.service().unwrap().unit, LABEL);
        assert!(!h.unit_installed(), "no plist yet");
        let plist = ws
            .root()
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"));
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, "<plist/>").unwrap();
        assert!(h.unit_installed());
        assert!(
            runner.calls().is_empty(),
            "launchd installed runs no command"
        );
        h.start_unit().unwrap();
        assert_eq!(
            runner.calls(),
            vec![
                argv(&["launchctl", "print", "gui/501/com.quaqua.daemon"]),
                argv(&[
                    "launchctl",
                    "bootstrap",
                    "gui/501",
                    &plist.display().to_string()
                ]),
            ]
        );
    }

    #[test]
    fn service_host_systemd_reads_load_state_and_starts_the_unit() {
        let ws = crate::testkit::TestWorkspace::new("up-host-systemd");
        let runner = FakeRunner::answering(vec![
            ran(0, "loaded\n", ""),
            ran(0, "not-found\n", ""),
            ran(0, "", ""),
            ran(1, "", "Unit quaqua-daemon.service is masked."),
        ]);
        let mut h = ServiceHost::with_service(
            ws.runs_dir(),
            service(Some(Platform::Systemd), ws.root()),
            Box::new(runner.clone()),
        );
        assert_eq!(h.service().unwrap(), systemd_names());
        assert!(h.unit_installed());
        assert!(!h.unit_installed());
        h.start_unit().unwrap();
        let e = h.start_unit().unwrap_err();
        assert!(e.contains("masked") && e.contains("systemctl"), "{e}");
        let calls = runner.calls();
        assert_eq!(
            calls[0],
            argv(&[
                "systemctl",
                "--user",
                "show",
                "--property=LoadState",
                "--value",
                UNIT_NAME
            ])
        );
        assert_eq!(calls[2], argv(&["systemctl", "--user", "start", UNIT_NAME]));
    }

    #[test]
    fn service_host_installs_through_its_own_service_under_the_scratch_home() {
        for platform in [Platform::Launchd, Platform::Systemd] {
            let ws = crate::testkit::TestWorkspace::new("up-host-install");
            let runner = FakeRunner::default();
            let svc = service(Some(platform), ws.root());
            let file = svc.as_ref().unwrap().file.clone();
            let mut h = ServiceHost::with_service(ws.runs_dir(), svc, Box::new(runner.clone()))
                .with_herdr(HerdrEnv {
                    session: "s1".into(),
                    socket: "/h/herdr.sock".into(),
                });
            h.install_unit().unwrap();
            assert!(file.starts_with(ws.root()), "{}", file.display());
            assert!(file.is_file(), "{} not written", file.display());
            assert!(runner.calls().is_empty(), "install runs no command");
        }
    }

    #[test]
    fn service_host_without_a_backend_runs_nothing() {
        let ws = crate::testkit::TestWorkspace::new("up-host-none");
        let runner = FakeRunner::default();
        let mut h = ServiceHost::with_service(
            ws.runs_dir(),
            service(None, ws.root()),
            Box::new(runner.clone()),
        );
        assert_eq!(h.service().unwrap_err(), NO_BACKEND);
        assert!(!h.unit_installed());
        assert_eq!(h.start_unit().unwrap_err(), NO_BACKEND);
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn up_daemon_start_failure_names_the_unit_and_systemd() {
        let mut h = Fake::new()
            .pings(&[Ping::Unreachable])
            .start(Err("Unit quaqua-daemon.service is masked.".into()));
        let e = ensure_answers(&mut h).unwrap_err();
        assert_eq!(e.code, EXIT_UNREACHABLE);
        assert!(
            e.message.contains(UNIT_NAME) && e.message.contains("masked"),
            "{}",
            e.message
        );
    }

    #[test]
    fn up_daemon_install_failure_names_the_install_command() {
        let mut h = Fake::new()
            .no_unit()
            .answer(Answer::Yes)
            .pings(&[Ping::Unreachable])
            .install(Err("HERDR_SESSION not set".into()));
        let e = ensure_answers(&mut h).unwrap_err();
        assert_eq!(e.code, EXIT_UNREACHABLE);
        assert!(e.message.contains(INSTALL_CMD), "{}", e.message);
        assert!(e.message.contains("HERDR_SESSION"), "{}", e.message);
        assert_eq!(h.starts, 0, "a failed install starts nothing");
    }

    #[test]
    fn up_daemon_wait_budget_is_the_daemon_start_reader_and_the_report_poll() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate::migrate(&mut c).unwrap();
        let w = wait_budget(&c);
        assert_eq!(w.poll, crate::daemon::watch::REPORT_POLL);
        // no key: the module's own default, well under the 600 s stall
        assert_eq!(
            w.deadline,
            Duration::from_secs(crate::config::DAEMON_START_SEC)
        );
        c.execute(
            "INSERT INTO setting (key, value_json) VALUES ('notify.daemon_start_sec', '42')",
            [],
        )
        .unwrap();
        // the deadline is whatever the start reader says, never a number of its own
        let w = wait_budget(&c);
        assert_eq!(w.deadline, crate::config::daemon_start_sec(&c));
        assert_eq!(w.deadline, Duration::from_secs(42));
    }
}
