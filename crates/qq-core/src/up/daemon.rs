//! The daemon half of `quaqua up` (R4): make sure the farm daemon answers before the
//! CoS is started, or say why it cannot. Every outside effect — the ping, `systemctl`,
//! the unit-file check, the TTY prompt, the platform, the clock and the printed lines —
//! goes through [`DaemonHost`], so the decision is unit-tested against fakes and this
//! module never touches a real unit, a real socket or a real terminal in a test.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cli::{CliError, Env, EXIT_REFUSED, EXIT_UNREACHABLE};
use crate::daemon::service::UNIT_NAME;

/// NG2 (issue #369): the launchd LaunchAgent that would let `up` start the daemon on
/// macOS. Until it exists, `up` names it and carries on.
pub const MACOS_ISSUE: u32 = 369;

/// The command a human runs to install the unit by hand: exactly what `up` runs for them.
pub const INSTALL_CMD: &str = "quaqua daemon install";

/// Whether `up` can start anything here at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
}

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
    /// macOS: nothing can be started from here (#369).
    Unsupported,
}

/// The outside world, one method per effect. The real one is [`SystemdHost`]; a test
/// implements this and never runs systemd, waits, or asks a terminal.
pub trait DaemonHost {
    /// `cfg!(target_os = "macos")` in the real host.
    fn platform(&self) -> Platform;
    /// One `quaqua ping`.
    fn ping(&mut self) -> Ping;
    /// Whether `quaqua-daemon.service` exists for the user manager.
    fn unit_installed(&mut self) -> bool;
    /// `systemctl --user start quaqua-daemon.service`; the error text is systemd's own.
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
/// started here, or cannot be started on this platform (#369).
/// `Err` is a daemon that is not running and will not be: no unit and no install (the
/// message names [`INSTALL_CMD`]), a failed install or start, or a unit that did not
/// answer within the config's budget (the message names [`UNIT_NAME`]). Exit status is
/// [`EXIT_UNREACHABLE`]: the daemon is unreachable, so sending anything again cannot help.
pub fn ensure_answers(host: &mut dyn DaemonHost) -> Result<Ready, CliError> {
    if host.platform() == Platform::MacOs {
        host.line(&format!(
            "quaqua up: starting the daemon is not supported on macOS yet (issue #{MACOS_ISSUE}); start it yourself, then run up again"
        ));
        return Ok(Ready::Unsupported);
    }
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
    let Some(installed) = installed_or_asked(host)? else {
        return Err(no_unit_error());
    };
    if !installed {
        host.install_unit()
            .map_err(|e| CliError::new(EXIT_UNREACHABLE, format!("`{INSTALL_CMD}` failed: {e}")))?;
    }
    host.start_unit().map_err(|e| {
        CliError::new(
            EXIT_UNREACHABLE,
            format!("`systemctl --user start {UNIT_NAME}` failed: {e}"),
        )
    })?;
    let wait = host.wait();
    host.line(&format!(
        "quaqua up: started {UNIT_NAME}; waiting up to {}s for it to answer (notify.daemon_start_sec)",
        wait.deadline.as_secs()
    ));
    wait_until_up(host, wait)
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

fn no_unit_error() -> CliError {
    CliError::new(
        EXIT_UNREACHABLE,
        format!("the daemon is not running and {UNIT_NAME} is not installed; install it with `{INSTALL_CMD}`"),
    )
}

/// Pings until the daemon answers or the budget runs out. A refusal counts as answered:
/// the unit is up, it just does not want this caller.
fn wait_until_up(host: &mut dyn DaemonHost, wait: Wait) -> Result<Ready, CliError> {
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
                    "{UNIT_NAME} did not answer within {}s of `systemctl --user start`; see `systemctl --user status {UNIT_NAME}`",
                    wait.deadline.as_secs()
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

/// The real host: `quaqua ping` through the CLI client, systemd through `systemctl`, the
/// install through [`crate::daemon::service::install`] (the same call `daemon install`
/// makes), config through the store, the terminal through stdio.
pub struct SystemdHost {
    runs_dir: PathBuf,
}

impl SystemdHost {
    pub fn new(runs_dir: impl Into<PathBuf>) -> Self {
        SystemdHost {
            runs_dir: runs_dir.into(),
        }
    }

    fn systemctl(args: &[&str]) -> std::io::Result<std::process::Output> {
        std::process::Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()
    }

    /// stderr when systemd said something, else its exit status.
    fn systemctl_error(out: &std::process::Output) -> String {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if err.is_empty() {
            format!("systemctl exited {}", out.status)
        } else {
            err
        }
    }
}

impl DaemonHost for SystemdHost {
    fn platform(&self) -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            // The daemon unit is systemd, and `daemon install` already refuses anything
            // that is not Linux; every other target lands here and fails loudly there.
            Platform::Linux
        }
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
        // `show` succeeds even for a unit that is not there, so the answer is its
        // LoadState: `not-found` (or nothing at all) means no unit. `masked` and
        // `bad-setting` are installed, and `start` names them when it refuses.
        match Self::systemctl(&["show", "--property=LoadState", "--value", UNIT_NAME]) {
            Ok(out) if out.status.success() => {
                let load = String::from_utf8_lossy(&out.stdout).trim().to_string();
                !matches!(load.as_str(), "" | "not-found")
            }
            // No user manager, or no systemd: nothing is installed for this user.
            _ => false,
        }
    }

    fn start_unit(&mut self) -> Result<(), String> {
        let out = Self::systemctl(&["start", UNIT_NAME]).map_err(|e| format!("{e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(Self::systemctl_error(&out))
    }

    fn install_unit(&mut self) -> Result<(), String> {
        let text = crate::daemon::service::install(&self.runs_dir, &[String::from("install")])?;
        self.line(text.trim_end());
        Ok(())
    }

    fn ask_install(&mut self) -> Answer {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Answer::NoTty;
        }
        self.line(&format!(
            "quaqua up: {UNIT_NAME} is not installed; install it now? [y/N]"
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

    const POLL_MS: u64 = 250;

    /// A host that records every effect and answers from a list of pings; the last one
    /// repeats. `sleep` only moves its own clock, so a test never waits.
    struct Fake {
        platform: Platform,
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
                platform: Platform::Linux,
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

        fn macos(mut self) -> Self {
            self.platform = Platform::MacOs;
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
        fn platform(&self) -> Platform {
            self.platform
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
    fn up_daemon_macos_says_unsupported_and_starts_nothing() {
        let mut h = Fake::new().macos().pings(&[Ping::Unreachable]);
        assert_eq!(ensure_answers(&mut h).unwrap(), Ready::Unsupported);
        assert_eq!(h.lines.len(), 1, "{:?}", h.lines);
        assert!(h.lines[0].contains("not supported"), "{}", h.lines[0]);
        assert!(
            h.lines[0].contains(&format!("#{MACOS_ISSUE}")),
            "{}",
            h.lines[0]
        );
        assert_eq!((h.starts, h.installs), (0, 0));
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
