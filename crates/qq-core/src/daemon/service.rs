//! Service glue: the systemd user unit, the sleep inhibitor and wake detection.
//! Pure decisions live in free functions; the daemon thread and `install` are thin wrappers.

use std::path::{Path, PathBuf};

/// Unit file name under `systemd/user/`.
pub const UNIT_NAME: &str = "quaqua-daemon.service";

/// Quotes one unit-file word: double quotes, with `\\`, `"` and `%` escaped.
fn quote(s: &str) -> String {
    let body = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{body}\"")
}

/// The herdr session the daemon must reach: what the installing environment carries, then
/// what herdr says — a herdr pane exports `HERDR_SOCKET_PATH` and never the session name (#376).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrEnv {
    pub session: String,
    pub socket: String,
}

/// What herdr says about itself, through the caller's own seam: [`ProcessFacts`] runs the
/// binary, `up` answers from its session-scoped calls, a test from a literal.
pub trait HerdrFacts {
    /// The JSON of `herdr status server --json`, asking about `session` by name when set.
    fn server(&self, session: Option<&str>) -> Option<serde_json::Value>;
    /// The JSON of `herdr session list --json`: every session, its name and its socket.
    fn sessions(&self) -> Option<serde_json::Value>;
}

/// A herdr call that has not answered in this long is hung, not slow.
const HERDR_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// The real herdr: `HERDR_BIN_PATH` when set and non-empty, else `herdr`, the same lookup
/// [`crate::herdr::CliHerdr`] makes.
#[derive(Debug, Clone)]
pub struct ProcessFacts {
    pub bin: String,
}

impl ProcessFacts {
    pub fn new() -> Self {
        let bin = std::env::var("HERDR_BIN_PATH")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "herdr".to_string());
        ProcessFacts { bin }
    }

    /// One verb's JSON: a failed, hung or unparseable call says nothing.
    fn json(&self, args: &[&str]) -> Option<serde_json::Value> {
        let mut cmd = std::process::Command::new(&self.bin);
        cmd.args(args);
        let out = crate::proc::output_within(&mut cmd, HERDR_LIMIT).ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }
}

impl Default for ProcessFacts {
    fn default() -> Self {
        Self::new()
    }
}

impl HerdrFacts for ProcessFacts {
    fn server(&self, session: Option<&str>) -> Option<serde_json::Value> {
        let mut args: Vec<&str> = Vec::new();
        if let Some(session) = session {
            args.extend_from_slice(&["--session", session]);
        }
        args.extend_from_slice(&["status", "server", "--json"]);
        self.json(&args)
    }

    fn sessions(&self) -> Option<serde_json::Value> {
        self.json(&["session", "list", "--json"])
    }
}

/// A present, non-blank value: an empty env var is an unset one.
fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// One non-blank string field of a JSON object.
fn field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// The name of the running session whose socket is `socket`, from `herdr session list --json`.
/// Only a running session counts: a stopped one's socket says nothing about which session this
/// process is in, and herdr reports `"session": null` when it is not asked by name.
pub fn session_of_socket(list: &serde_json::Value, socket: &str) -> Option<String> {
    list.get("sessions")?
        .as_array()?
        .iter()
        .find(|s| {
            s.get("running").and_then(serde_json::Value::as_bool) == Some(true)
                && s.get("socket_path")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|p| same_path(p, socket))
        })
        .and_then(|s| field(s, "name"))
}

/// The socket of `session`, running or stopped: the name was given, so the listing only maps
/// it to the socket the daemon must reach.
pub fn socket_of_session(list: &serde_json::Value, session: &str) -> Option<String> {
    list.get("sessions")?
        .as_array()?
        .iter()
        .find(|s| field(s, "name").is_some_and(|n| n == session))
        .and_then(|s| field(s, "socket_path"))
}

/// The error when neither the environment nor herdr can tell: `herdr session list` prints the
/// name and the socket of every session, running or stopped, so the step it names works.
fn unresolved(session: bool, socket: bool, why: &[String]) -> String {
    let mut missing = Vec::new();
    if !session {
        missing.push("HERDR_SESSION");
    }
    if !socket {
        missing.push("HERDR_SOCKET_PATH");
    }
    let them = if missing.len() == 1 { "it" } else { "them" };
    let tail = if why.is_empty() {
        String::new()
    } else {
        format!(" ({})", why.join("; "))
    };
    format!(
        "{} not set and herdr did not report {them}: run `herdr session list`, then \
         `HERDR_SESSION=<name> HERDR_SOCKET_PATH=<socket> quaqua daemon install`{tail}",
        missing.join(" and ")
    )
}

impl HerdrEnv {
    /// `HERDR_SESSION` and `HERDR_SOCKET_PATH` from the process env, else from herdr: what
    /// `daemon install` writes into the unit, and what `up` exports before installing.
    pub fn from_process() -> Result<Self, String> {
        Self::resolve(None, &ProcessFacts::new())
    }

    /// The session the daemon must reach. `up` passes the session its farm lives in as
    /// `preferred`: herdr's answer for that name wins and the env only fills a gap, so a pane
    /// of another farm cannot put its own pair in the unit or in the server's env. `None` (a
    /// plain `daemon install`) takes the env first and asks herdr for whatever it lacks.
    pub fn resolve(preferred: Option<&str>, facts: &dyn HerdrFacts) -> Result<Self, String> {
        let var = |k| std::env::var(k).ok();
        Self::resolve_with(
            var("HERDR_SESSION"),
            var("HERDR_SOCKET_PATH"),
            preferred,
            facts,
        )
    }

    /// [`HerdrEnv::resolve`] on the given env values: what a test drives, and what `up` runs
    /// with its farm's session as the preference.
    pub fn resolve_with(
        session: Option<String>,
        socket: Option<String>,
        preferred: Option<&str>,
        facts: &dyn HerdrFacts,
    ) -> Result<Self, String> {
        let (env_session, env_socket) = (clean(session), clean(socket));
        // A plain install with both values set needs neither herdr nor a clearer pair.
        if preferred.is_none() {
            if let (Some(session), Some(socket)) = (env_session.clone(), env_socket.clone()) {
                return Ok(HerdrEnv { session, socket });
            }
        }
        let mut why: Vec<String> = Vec::new();
        // A named session is the one to ask herdr about, and its answer is a pair: the socket
        // that belongs to it, never a socket left over from another session's env.
        let named = preferred
            .map(str::to_string)
            .or_else(|| env_session.clone());
        let server = facts.server(named.as_deref());
        if server.is_none() {
            why.push("`herdr status server --json` gave no answer".to_string());
        }
        let herdr_session = server.as_ref().and_then(|v| field(v, "session"));
        let herdr_socket = server.as_ref().and_then(|v| field(v, "socket"));
        let (mut session, mut socket) = if preferred.is_some() {
            (
                herdr_session.or_else(|| named.clone()),
                herdr_socket.or(env_socket),
            )
        } else {
            (env_session.or(herdr_session), env_socket.or(herdr_socket))
        };
        if session.is_none() {
            if let Some(p) = &socket {
                session = facts
                    .sessions()
                    .as_ref()
                    .and_then(|l| session_of_socket(l, p));
            }
        }
        if socket.is_none() {
            if let Some(s) = &session {
                socket = facts
                    .sessions()
                    .as_ref()
                    .and_then(|l| socket_of_session(l, s));
            }
        }
        match (session, socket) {
            (Some(session), Some(socket)) => Ok(HerdrEnv { session, socket }),
            (session, socket) => Err(unresolved(session.is_some(), socket.is_some(), &why)),
        }
    }
}

/// Unit text for the daemon, run from `exe` against workspace `home`, with `PATH` and the
/// herdr session as given.
pub fn render_unit(exe: &Path, home: &Path, path: &str, herdr: &HerdrEnv) -> String {
    let env = quote(&format!("QUAQUA_HOME={}", home.display()));
    let path_env = quote(&format!("PATH={path}"));
    let session_env = quote(&format!("HERDR_SESSION={}", herdr.session));
    let socket_env = quote(&format!("HERDR_SOCKET_PATH={}", herdr.socket));
    format!(
        "[Unit]\nDescription=quaqua farm daemon\n\n\
         [Service]\nExecStart={} daemon\nEnvironment={env}\nEnvironment={path_env}\nEnvironment={session_env}\nEnvironment={socket_env}\nRestart=on-failure\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n",
        quote(&exe.display().to_string())
    )
}

/// Where the user unit file goes.
pub fn unit_path(home: &Path, xdg_config: Option<&Path>) -> PathBuf {
    let base = xdg_config.map_or_else(|| home.join(".config"), Path::to_path_buf);
    base.join("systemd/user").join(UNIT_NAME)
}

/// Legacy `quaqua-auditor*` timer or service files in a user unit dir. The daemon's Supervisor
/// job owns the auditor now (#316), so these are duplicates. Reported, never deleted.
pub fn legacy_auditor_units(dir: &Path) -> Vec<PathBuf> {
    legacy_in(dir, |n| {
        n.starts_with(LEGACY_SYSTEMD) && (n.ends_with(".timer") || n.ends_with(".service"))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inhibit {
    Start,
    Stop,
    Keep,
}

/// A held lock that can say whether it still holds.
trait Liveness {
    fn alive(&mut self) -> bool;
}

/// Drops a held lock whose child already exited (logind refused, no D-Bus), so the next
/// decision sees "not held" and retries.
fn reap<T: Liveness>(held: &mut Option<T>) {
    if held.as_mut().is_some_and(|h| !h.alive()) {
        *held = None;
    }
}

/// What to do with the sleep inhibitor given the live worker count and whether it is held.
/// `None` is a failed store read: workers may be live, so keep the current state.
pub fn inhibit_decision(live: Option<usize>, held: bool) -> Inhibit {
    match (live.map(|n| n > 0), held) {
        (Some(true), false) => Inhibit::Start,
        (Some(false), true) => Inhibit::Stop,
        _ => Inhibit::Keep,
    }
}

/// A wall clock that runs ahead of the monotonic one by more than this is a sleep,
/// not scheduling jitter or an NTP nudge.
pub const WAKE_SLACK_MS: u64 = 10_000;

/// Detects a resume from sleep: the wall clock moved on more than the monotonic clock
/// (CLOCK_MONOTONIC stands still while the machine sleeps).
pub struct WakeDetector {
    last_wall: u64,
    last_mono: u64,
}

impl WakeDetector {
    pub fn new(wall_ms: u64, mono_ms: u64) -> Self {
        WakeDetector {
            last_wall: wall_ms,
            last_mono: mono_ms,
        }
    }

    /// Milliseconds slept since the last check, when it is a resume. The baseline always
    /// moves, so a wall clock stepped backwards is no wake and the next tick is quiet.
    pub fn check(&mut self, wall_ms: u64, mono_ms: u64) -> Option<u64> {
        let wall = wall_ms.saturating_sub(self.last_wall);
        let mono = mono_ms.saturating_sub(self.last_mono);
        self.last_wall = wall_ms;
        self.last_mono = mono_ms;
        let slept = wall.saturating_sub(mono);
        (slept > WAKE_SLACK_MS).then_some(slept)
    }
}

/// Body of the mail to cos after a resume.
/// `None` when there is nothing to report: no open agents, or the store read failed.
pub fn wake_body(slept_ms: u64, agents: Option<&[String]>) -> Option<String> {
    let agents = agents.filter(|a| !a.is_empty())?;
    Some(format!(
        "machine resumed from sleep after {}s; agents that were open and may have been cut: {}",
        slept_ms / 1000,
        agents.join(", ")
    ))
}

/// Which service manager runs the daemon. A runtime value: both are compiled everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Systemd,
    Launchd,
}

impl Platform {
    /// The only cfg in the module: Linux -> Systemd, macOS -> Launchd, else None.
    pub fn current() -> Option<Platform> {
        if cfg!(target_os = "linux") {
            Some(Platform::Systemd)
        } else if cfg!(target_os = "macos") {
            Some(Platform::Launchd)
        } else {
            None
        }
    }
}

/// The refusal on a target with no backend.
pub const NO_BACKEND: &str = "daemon install supports systemd (Linux) and launchd (macOS) only";
/// The launchd label when daemon.launchd_label is unset or unusable.
pub const LAUNCHD_LABEL: &str = "com.quaqua.daemon";
/// The agents dir under home when daemon.launchd_dir is unset or unusable.
pub const LAUNCHD_DIR: &str = "Library/LaunchAgents";
/// The launchd job's stdout and stderr, in the runs dir (systemd uses the journal).
pub const DAEMON_LOG: &str = "daemon.log";
const LEGACY_SYSTEMD: &str = "quaqua-auditor"; // + .timer | .service
const LEGACY_LAUNCHD: &str = "com.quaqua.auditor"; // + .plist

/// The resolved service: everything except the unit text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// The systemd unit name, or the launchd label.
    pub name: String,
    pub platform: Platform,
    /// The unit file, or the plist.
    pub file: PathBuf,
    /// The launchd domain is gui/<uid>; systemd ignores it.
    pub uid: u32,
}

/// What the unit or plist runs. The env values come from the installing process.
pub struct Spec<'a> {
    pub exe: &'a Path,
    /// QUAQUA_HOME
    pub workspace: &'a Path,
    /// PATH
    pub path: &'a str,
    /// HERDR_SESSION, HERDR_SOCKET_PATH
    pub herdr: &'a HerdrEnv,
    /// <runs_dir>/DAEMON_LOG, launchd only
    pub log: &'a Path,
}

/// One finished command. code is None when a signal killed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// Every systemctl and launchctl call goes through this. Tests implement it and record the calls.
pub trait Runner {
    fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<Ran>;
}

/// std::process::Command. Only ServiceHost::new constructs it.
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<Ran> {
        let out = std::process::Command::new(program).args(args).output()?;
        Ok(Ran {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// The strings up/daemon.rs shows: the unit or label, and the command a human runs to inspect it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    pub unit: String,
    pub status: String,
}

fn getuid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid takes no arguments and cannot fail.
    unsafe { getuid() }
}

/// Escapes one plist string value. '&' goes first so the other entities stay intact.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Plist text for the daemon under launchd, as the design note pins it.
pub fn render_plist(label: &str, spec: &Spec) -> String {
    let e = |s: &str| xml_escape(s);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>daemon</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>QUAQUA_HOME</key>
    <string>{workspace}</string>
    <key>PATH</key>
    <string>{path}</string>
    <key>HERDR_SESSION</key>
    <string>{session}</string>
    <key>HERDR_SOCKET_PATH</key>
    <string>{socket}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>5</integer>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        label = e(label),
        exe = e(&spec.exe.display().to_string()),
        workspace = e(&spec.workspace.display().to_string()),
        path = e(spec.path),
        session = e(&spec.herdr.session),
        socket = e(&spec.herdr.socket),
        log = e(&spec.log.display().to_string()),
    )
}

/// Legacy `com.quaqua.auditor*.plist` agents in a LaunchAgents dir. Reported, never deleted.
pub fn legacy_auditor_plists(dir: &Path) -> Vec<PathBuf> {
    legacy_in(dir, |n| {
        n.starts_with(LEGACY_LAUNCHD) && n.ends_with(".plist")
    })
}

fn legacy_in(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(&keep))
        .collect();
    found.sort();
    found
}

/// The command as shown in errors: `program arg arg`.
fn shown(program: &str, args: &[String]) -> String {
    format!("`{program} {}`", args.join(" "))
}

/// Runs one command through the runner. Ok(()) on exit 0; an Err names the command in
/// backticks, then its stderr, or "exited <code>" when stderr is empty, or the io error.
fn run_ok(r: &mut dyn Runner, program: &str, args: &[String]) -> Result<(), String> {
    let cmd = shown(program, args);
    let ran = r
        .run(program, args)
        .map_err(|e| format!("{cmd} failed: {e}"))?;
    if ran.ok() {
        return Ok(());
    }
    let why = ran.stderr.trim();
    if !why.is_empty() {
        return Err(format!("{cmd} failed: {why}"));
    }
    match ran.code {
        Some(c) => Err(format!("{cmd} failed: exited {c}")),
        None => Err(format!("{cmd} failed: killed by a signal")),
    }
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

impl Service {
    /// None -> Err(NO_BACKEND). Launchd label and dir fall back to their defaults when the
    /// config value is blank or unusable (see the design note, "Config keys").
    pub fn resolve(
        platform: Option<Platform>,
        home: &Path,
        xdg_config: Option<&Path>,
        cfg: &crate::config::LaunchdConfig,
        uid: u32,
    ) -> Result<Service, String> {
        let platform = platform.ok_or(NO_BACKEND)?;
        let (name, file) = match platform {
            Platform::Systemd => (UNIT_NAME.to_string(), unit_path(home, xdg_config)),
            Platform::Launchd => {
                let label = cfg
                    .label
                    .as_deref()
                    .map(str::trim)
                    .filter(|l| {
                        !l.is_empty() && !l.contains('/') && !l.contains(char::is_whitespace)
                    })
                    .unwrap_or(LAUNCHD_LABEL)
                    .to_string();
                let dir = cfg.dir.as_deref().map(str::trim).and_then(|d| {
                    if let Some(rest) = d.strip_prefix("~/") {
                        (!rest.is_empty()).then(|| home.join(rest))
                    } else {
                        Some(PathBuf::from(d)).filter(|p| p.is_absolute())
                    }
                });
                let dir = dir.unwrap_or_else(|| home.join(LAUNCHD_DIR));
                let file = dir.join(format!("{label}.plist"));
                (label, file)
            }
        };
        Ok(Service {
            name,
            platform,
            file,
            uid,
        })
    }

    /// Reads HOME, an absolute XDG_CONFIG_HOME, getuid(), the launchd config and the platform.
    pub fn for_process(runs_dir: &Path) -> Result<Service, String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        Service::resolve(
            Platform::current(),
            &home,
            xdg.as_deref(),
            &crate::config::launchd_config_in(runs_dir),
            getuid(),
        )
    }

    pub fn render(&self, spec: &Spec) -> String {
        match self.platform {
            Platform::Systemd => render_unit(spec.exe, spec.workspace, spec.path, spec.herdr),
            Platform::Launchd => render_plist(&self.name, spec),
        }
    }

    /// What install prints after "wrote <file>": how to load it. Ends with a newline.
    pub fn enable_hint(&self) -> String {
        match self.platform {
            Platform::Systemd => format!(
                "enable it with: systemctl --user enable --now {UNIT_NAME}\n"
            ),
            Platform::Launchd => format!(
                "load it with: launchctl bootstrap gui/{uid} {file}\n(if it is already loaded, run launchctl bootout gui/{uid}/{label} first)\n",
                uid = self.uid,
                file = self.file.display(),
                label = self.name,
            ),
        }
    }

    fn target(&self) -> String {
        format!("gui/{}/{}", self.uid, self.name)
    }

    pub fn names(&self) -> Names {
        match self.platform {
            Platform::Systemd => Names {
                unit: UNIT_NAME.to_string(),
                status: format!("systemctl --user status {UNIT_NAME}"),
            },
            Platform::Launchd => Names {
                unit: self.name.clone(),
                status: format!("launchctl print {}", self.target()),
            },
        }
    }

    /// Systemd: systemctl show LoadState not in {"", "not-found"}.
    /// Launchd: the plist exists, and no command runs.
    pub fn installed(&self, r: &mut dyn Runner) -> bool {
        match self.platform {
            Platform::Systemd => {
                let args = strings(&[
                    "--user",
                    "show",
                    "--property=LoadState",
                    "--value",
                    UNIT_NAME,
                ]);
                r.run("systemctl", &args).is_ok_and(|o| {
                    let state = o.stdout.trim();
                    o.ok() && !state.is_empty() && state != "not-found"
                })
            }
            Platform::Launchd => self.file.exists(),
        }
    }

    /// Starts the daemon. Launchd asks `print` first: loaded -> `kickstart` (never -k, a live
    /// daemon must not be killed), not loaded -> `bootstrap`. The engine never boots out.
    pub fn start(&self, r: &mut dyn Runner) -> Result<(), String> {
        match self.platform {
            Platform::Systemd => run_ok(r, "systemctl", &strings(&["--user", "start", UNIT_NAME])),
            Platform::Launchd => {
                let target = self.target();
                let print = strings(&["print", &target]);
                let loaded = r
                    .run("launchctl", &print)
                    .map_err(|e| format!("{} failed: {e}", shown("launchctl", &print)))?
                    .ok();
                if loaded {
                    run_ok(r, "launchctl", &strings(&["kickstart", &target]))
                } else {
                    let domain = format!("gui/{}", self.uid);
                    let file = self.file.display().to_string();
                    run_ok(r, "launchctl", &strings(&["bootstrap", &domain, &file]))
                }
            }
        }
    }

    /// Legacy auditor files next to the unit or plist, sorted.
    pub fn legacy_auditor(&self) -> Vec<PathBuf> {
        let Some(dir) = self.file.parent() else {
            return Vec::new();
        };
        match self.platform {
            Platform::Systemd => legacy_auditor_units(dir),
            Platform::Launchd => legacy_auditor_plists(dir),
        }
    }
}

/// `quaqua daemon install [--dry-run]`: writes the unit or plist and returns the text to print.
/// `--dry-run` returns the text alone and touches nothing. Runs no command: never loads or starts it.
pub fn install_at(svc: &Service, args: &[String], spec: &Spec) -> Result<String, String> {
    let mut dry = false;
    for a in args.iter().skip(1) {
        match a.as_str() {
            "--dry-run" => dry = true,
            other => {
                return Err(format!(
                    "unknown argument '{other}' (usage: daemon install [--dry-run])"
                ))
            }
        }
    }
    let text = svc.render(spec);
    if dry {
        return Ok(text);
    }
    let dir = svc.file.parent().ok_or("unit path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    std::fs::write(&svc.file, text).map_err(|e| format!("write {}: {e}", svc.file.display()))?;
    if svc.platform == Platform::Launchd {
        // launchctl wants no group or world write; a umask of 002 would break that.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&svc.file, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| format!("chmod {}: {e}", svc.file.display()))?;
    }
    let mut out = format!("wrote {}\n{}", svc.file.display(), svc.enable_hint());
    for legacy in svc.legacy_auditor() {
        match svc.platform {
            Platform::Systemd => out.push_str(&format!(
                "duplicate: {} is a legacy auditor unit; the daemon now runs the auditor. Disable and remove it yourself (not deleted)\n",
                legacy.display()
            )),
            Platform::Launchd => {
                let stem = legacy.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                out.push_str(&format!(
                    "duplicate: {} is a legacy auditor agent; the daemon now runs the auditor. Unload it with launchctl bootout gui/{}/{stem} and remove it yourself (not deleted)\n",
                    legacy.display(),
                    svc.uid
                ));
            }
        }
    }
    Ok(out)
}

/// `quaqua daemon install` for the running process: exe from `current_exe`, home from `HOME`,
/// the workspace is the parent of `runs_dir`, the herdr session from the env then herdr.
pub fn install(runs_dir: &Path, args: &[String]) -> Result<String, String> {
    let svc = Service::for_process(runs_dir)?;
    install_resolved(&svc, runs_dir, args, &ProcessFacts::new())
}

/// [`install`] with herdr injected: the same resolver `up` runs, with a test answering for
/// herdr instead of the binary answering.
pub fn install_resolved(
    svc: &Service,
    runs_dir: &Path,
    args: &[String],
    facts: &dyn HerdrFacts,
) -> Result<String, String> {
    let herdr = HerdrEnv::resolve(None, facts)?;
    let warn = not_running(&herdr, facts);
    let text = install_with(svc, runs_dir, args, &herdr)?;
    Ok(match warn {
        Some(w) if text.ends_with('\n') => format!("{text}{w}"),
        Some(w) => format!("{text}\n{w}"),
        None => text,
    })
}

/// A line for a session herdr reports as not running, so a unit naming it is written with a
/// warning rather than in silence: the daemon cannot reach herdr until that session runs.
/// `None` when herdr says nothing, or says it runs.
fn not_running(herdr: &HerdrEnv, facts: &dyn HerdrFacts) -> Option<String> {
    let status = facts.server(Some(&herdr.session))?;
    if status.get("running").and_then(serde_json::Value::as_bool) != Some(false) {
        return None;
    }
    let state = field(&status, "status").unwrap_or_else(|| "not running".to_string());
    Some(format!(
        "warning: herdr reports session {} as {state}: the unit cannot reach herdr until it runs (\
         `herdr --session {}`, or pick a running one from `herdr session list`)\n",
        herdr.session, herdr.session
    ))
}

/// [`install_at`] on a given service and herdr session, the rest of the spec (exe, PATH,
/// workspace, log) from the process: what `up` runs through its own [`Service`].
pub fn install_with(
    svc: &Service,
    runs_dir: &Path,
    args: &[String],
    herdr: &HerdrEnv,
) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own executable: {e}"))?;
    let workspace = runs_dir.parent().unwrap_or(runs_dir);
    let workspace = std::path::absolute(workspace).map_err(|e| e.to_string())?;
    // A service manager hands the daemon a bare PATH, so the unit carries the installer's own.
    let path = std::env::var("PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            let h = std::env::var("HOME").unwrap_or_default();
            format!("{h}/.local/bin:{h}/.cargo/bin:/usr/local/bin:/usr/bin:/bin")
        });
    let log = runs_dir.join(DAEMON_LOG);
    let spec = Spec {
        exe: &exe,
        workspace: &workspace,
        path: &path,
        herdr,
        log: &log,
    };
    install_at(svc, args, &spec)
}

// --- the daemon side: thin wrappers over the pure parts ---

/// The held sleep inhibitor: a `systemd-inhibit` child that lives while the daemon does.
/// `tail --pid` exits with the daemon, so a killed daemon leaks no inhibitor.
struct Inhibitor(std::process::Child);

impl Inhibitor {
    fn acquire() -> Option<Self> {
        std::process::Command::new("systemd-inhibit")
            .args([
                "--what=sleep",
                "--who=quaqua",
                "--why=workers are running",
                "--mode=block",
            ])
            .arg("tail")
            .arg(format!("--pid={}", std::process::id()))
            .args(["-f", "/dev/null"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()
            .map(Inhibitor)
    }
}

impl Liveness for Inhibitor {
    fn alive(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

impl Drop for Inhibitor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const TICK: std::time::Duration = std::time::Duration::from_secs(5);

/// Names of the open Lead and Peer agents: the live workers.
/// `None` when the store read fails.
fn live_agents(ctx: &super::Ctx) -> Option<Vec<String>> {
    ctx.store
        .with_conn(|c| {
            let mut st = c.prepare(
                "SELECT name FROM agent WHERE ended_ms IS NULL AND role IN ('lead', 'peer') ORDER BY name",
            )?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<Result<Vec<String>, _>>()
        })
        .ok()
}

/// Holds a sleep inhibitor while any worker is live and mails cos after a resume from
/// sleep. One cheap query per tick. Opt-in (`enabled`): only the CLI daemon command asks for it.
/// Linux only; `QQ_INHIBIT=0` turns it off.
pub fn spawn(
    enabled: bool,
    ctx: std::sync::Weak<super::Ctx>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    use std::sync::atomic::Ordering;
    if !enabled || !cfg!(target_os = "linux") || std::env::var("QQ_INHIBIT").as_deref() == Ok("0") {
        return None;
    }
    Some(std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let wall = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64)
        };
        let mono = || start.elapsed().as_millis() as u64;
        let mut wake = WakeDetector::new(wall(), mono());
        let mut held: Option<Inhibitor> = None;
        while !stop.load(Ordering::SeqCst) {
            if let Some(ctx) = ctx.upgrade() {
                let agents = live_agents(&ctx);
                reap(&mut held);
                match inhibit_decision(agents.as_ref().map(Vec::len), held.is_some()) {
                    Inhibit::Start => held = Inhibitor::acquire(),
                    Inhibit::Stop => held = None,
                    Inhibit::Keep => {}
                }
                if let Some(slept) = wake.check(wall(), mono()) {
                    if let Some(body) = wake_body(slept, agents.as_deref()) {
                        let _ = crate::bus::daemon_mail(&ctx, "cos", "state", &body, None);
                    }
                }
            } else {
                return;
            }
            let mut left = TICK;
            while !stop.load(Ordering::SeqCst) && !left.is_zero() {
                let d = left.min(std::time::Duration::from_millis(100));
                std::thread::sleep(d);
                left -= d;
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATH: &str = "/h/.cargo/bin:/usr/bin";

    fn herdr() -> HerdrEnv {
        HerdrEnv {
            session: "s1".into(),
            socket: "/h/.config/herdr/sessions/s1/herdr.sock".into(),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("qq-service-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn service_is_inert_unless_enabled() {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        assert!(spawn(false, std::sync::Weak::new(), stop).is_none());
    }

    #[test]
    fn unit_runs_daemon_with_home() {
        let u = render_unit(
            Path::new("opt/qq bin/quaqua"),
            Path::new("ws/quaqua"),
            PATH,
            &herdr(),
        );
        assert!(u.contains("[Service]"));
        assert!(u.contains("ExecStart=\"opt/qq bin/quaqua\" daemon\n"));
        assert!(u.contains("Environment=\"QUAQUA_HOME=ws/quaqua\"\n"));
        assert!(u.contains("Environment=\"PATH=/h/.cargo/bin:/usr/bin\"\n"));
        assert!(u.contains("Environment=\"HERDR_SESSION=s1\"\n"));
        assert!(u.contains(
            "Environment=\"HERDR_SOCKET_PATH=/h/.config/herdr/sessions/s1/herdr.sock\"\n"
        ));
        assert!(u.contains("Restart=on-failure"));
        assert!(u.contains("WantedBy=default.target"));
    }

    #[test]
    fn unit_path_prefers_xdg() {
        let n = "systemd/user/quaqua-daemon.service";
        assert_eq!(
            unit_path(Path::new("h"), None),
            Path::new("h/.config").join(n)
        );
        assert_eq!(
            unit_path(Path::new("h"), Some(Path::new("x"))),
            Path::new("x").join(n)
        );
    }

    struct Fake(bool);
    impl Liveness for Fake {
        fn alive(&mut self) -> bool {
            self.0
        }
    }

    #[test]
    fn a_dead_inhibitor_child_is_dropped_so_the_next_tick_retries() {
        let mut held = Some(Fake(false));
        reap(&mut held);
        assert!(held.is_none());
        assert_eq!(inhibit_decision(Some(2), held.is_some()), Inhibit::Start);
        let mut held = Some(Fake(true));
        reap(&mut held);
        assert!(held.is_some());
    }

    #[test]
    fn inhibitor_follows_live_workers() {
        assert_eq!(inhibit_decision(Some(1), false), Inhibit::Start);
        assert_eq!(inhibit_decision(Some(3), true), Inhibit::Keep);
        assert_eq!(inhibit_decision(Some(0), true), Inhibit::Stop);
        assert_eq!(inhibit_decision(Some(0), false), Inhibit::Keep);
    }

    #[test]
    fn a_store_error_keeps_the_current_state() {
        assert_eq!(inhibit_decision(None, true), Inhibit::Keep);
        assert_eq!(inhibit_decision(None, false), Inhibit::Keep);
    }

    #[test]
    fn wake_needs_a_wall_jump_beyond_the_monotonic_clock() {
        let mut w = WakeDetector::new(1_000_000, 5_000);
        // a normal tick: both clocks moved 2s
        assert_eq!(w.check(1_002_000, 7_000), None);
        // the lid closed for an hour: wall +1h2s, monotonic +2s
        assert_eq!(w.check(1_002_000 + 3_602_000, 9_000), Some(3_600_000));
        // the baseline moved: the next normal tick is quiet
        assert_eq!(w.check(1_002_000 + 3_602_000 + 2_000, 11_000), None);
    }

    #[test]
    fn small_drift_is_no_wake() {
        let mut w = WakeDetector::new(1_000_000, 5_000);
        assert_eq!(w.check(1_000_000 + 2_000 + 3_000, 7_000), None);
    }

    #[test]
    fn wall_clock_stepping_back_is_no_wake() {
        let mut w = WakeDetector::new(1_000_000, 5_000);
        assert_eq!(w.check(900_000, 7_000), None);
        // and the baseline follows it, so the next normal tick is quiet too
        assert_eq!(w.check(902_000, 9_000), None);
    }

    #[test]
    fn wake_body_names_the_agents() {
        let b = wake_body(90_000, Some(&["lead-a".into(), "peer-b".into()])).unwrap();
        assert!(b.contains("lead-a") && b.contains("peer-b"));
        assert!(b.contains("90s"));
    }

    #[test]
    fn no_open_agents_or_a_store_error_sends_no_wake_mail() {
        assert_eq!(wake_body(90_000, Some(&[])), None);
        assert_eq!(wake_body(90_000, None), None);
    }

    /// The pane in #376: herdr exports the socket to every pane and never the name.
    const PANE_SOCKET: &str = "/h/.config/herdr/sessions/quaqua/herdr.sock";

    /// A herdr that answers the way the real one does in a pane: `status server --json`
    /// reports the socket with a null session, `session list --json` ties that socket to
    /// the running session's name.
    fn fake_herdr(root: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join("fake-herdr");
        let running = format!(
            "{{\"status\":\"running\",\"running\":true,\"session\":null,\"socket\":\"{PANE_SOCKET}\"}}"
        );
        let list = format!(
            "{{\"sessions\":[\
             {{\"name\":\"default\",\"running\":false,\"socket_path\":\"/h/.config/herdr/herdr.sock\"}},\
             {{\"name\":\"quaqua\",\"running\":true,\"socket_path\":\"{PANE_SOCKET}\"}}]}}"
        );
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\ncase \"$*\" in\n*'status server'*) echo '{running}' ;;\n\
                 *'session list'*) echo '{list}' ;;\n\
                 *) echo \"unexpected: $*\" >&2; exit 1 ;;\nesac\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    /// #376: `daemon install` in a herdr pane must take the session herdr reports instead
    /// of refusing because the pane never exports `HERDR_SESSION`.
    #[test]
    fn install_takes_the_session_herdr_reports_when_the_env_lacks_it() {
        let ws = crate::testkit::TestWorkspace::new("service-install-pane");
        let bin = fake_herdr(ws.root());
        std::env::set_var("HERDR_BIN_PATH", &bin);
        std::env::set_var("HERDR_SOCKET_PATH", PANE_SOCKET);
        let got = HerdrEnv::from_process();
        ws.seal_herdr();
        let got = got.expect("daemon install must resolve the session herdr reports");
        assert_eq!(got.session, "quaqua");
        assert_eq!(got.socket, PANE_SOCKET);
    }

    /// Nothing in the env and a herdr that names no running session: the error must name a
    /// step that works, not the herdr pane that never exports `HERDR_SESSION`.
    #[test]
    fn unresolved_herdr_env_names_a_step_that_works() {
        let facts = Facts::default();
        let e = HerdrEnv::resolve_with(None, None, None, &facts).unwrap_err();
        assert!(
            e.contains("HERDR_SESSION") && e.contains("HERDR_SOCKET_PATH"),
            "{e}"
        );
        assert!(e.contains("herdr session list"), "{e}");
        assert!(!e.contains("from inside the herdr session"), "{e}");
        // A missing socket alone is named alone: the fix step still shows both exports.
        let e = HerdrEnv::resolve_with(Some("s".into()), None, None, &facts).unwrap_err();
        assert!(e.starts_with("HERDR_SOCKET_PATH not set"), "{e}");
    }

    /// herdr's two answers: the JSON `status server --json` prints and the one
    /// `session list --json` prints, with the name the resolver asked for recorded.
    #[derive(Default)]
    struct Facts {
        server: Option<serde_json::Value>,
        sessions: Option<serde_json::Value>,
        asked: std::cell::RefCell<Vec<Option<String>>>,
    }

    impl Facts {
        fn new(server: &str, sessions: &str) -> Self {
            Facts {
                server: serde_json::from_str(server).ok(),
                sessions: serde_json::from_str(sessions).ok(),
                asked: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn asked(&self) -> Vec<Option<String>> {
            self.asked.borrow().clone()
        }
    }

    impl HerdrFacts for Facts {
        fn server(&self, session: Option<&str>) -> Option<serde_json::Value> {
            self.asked.borrow_mut().push(session.map(str::to_string));
            self.server.clone()
        }

        fn sessions(&self) -> Option<serde_json::Value> {
            self.sessions.clone()
        }
    }

    /// What the real herdr prints when it is not asked by name: the socket, no session.
    fn pane_status() -> String {
        format!(r#"{{"status":"running","running":true,"session":null,"socket":"{PANE_SOCKET}"}}"#)
    }

    /// `herdr session list --json`: a stopped default session and the running one.
    fn session_list() -> String {
        format!(
            "{{\"sessions\":[\
             {{\"name\":\"default\",\"running\":false,\"socket_path\":\"/h/.config/herdr/herdr.sock\"}},\
             {{\"name\":\"quaqua\",\"running\":true,\"socket_path\":\"{PANE_SOCKET}\"}}]}}"
        )
    }

    #[test]
    fn resolve_names_the_session_of_the_sockets_owner() {
        let facts = Facts::new(&pane_status(), &session_list());
        let h = HerdrEnv::resolve_with(None, Some(PANE_SOCKET.into()), None, &facts).unwrap();
        assert_eq!(h.session, "quaqua");
        assert_eq!(h.socket, PANE_SOCKET);
    }

    #[test]
    fn resolve_fills_the_socket_when_the_env_has_only_the_session() {
        let facts = Facts::new(&pane_status(), &session_list());
        let h = HerdrEnv::resolve_with(Some("quaqua".into()), None, None, &facts).unwrap();
        assert_eq!(h.session, "quaqua");
        assert_eq!(h.socket, PANE_SOCKET);
        // Asked by name, because a named session answers with its own pair.
        assert_eq!(facts.asked(), vec![Some("quaqua".to_string())]);
    }

    #[test]
    fn resolve_keeps_the_env_pair_and_runs_no_herdr() {
        let facts = Facts::default();
        let h = HerdrEnv::resolve_with(Some("s1".into()), Some("/p".into()), None, &facts).unwrap();
        assert_eq!(h.session, "s1");
        assert_eq!(h.socket, "/p");
        assert!(facts.asked().is_empty(), "a full env asks herdr nothing");
        // A blank value is an unset one.
        let facts = Facts::new(&pane_status(), &session_list());
        let h = HerdrEnv::resolve_with(Some(String::new()), Some(PANE_SOCKET.into()), None, &facts)
            .unwrap();
        assert_eq!(h.session, "quaqua");
    }

    #[test]
    fn resolve_asks_herdr_by_name_for_the_preferred_session() {
        // `up` knows its farm's session: herdr answers that name with its own socket.
        let status = r#"{"running":true,"session":"quaqua","socket":"/h/quaqua.sock"}"#;
        let facts = Facts::new(status, &session_list());
        let h = HerdrEnv::resolve_with(None, None, Some("quaqua"), &facts).unwrap();
        assert_eq!(h.session, "quaqua");
        assert_eq!(h.socket, "/h/quaqua.sock");
        assert_eq!(facts.asked(), vec![Some("quaqua".to_string())]);
    }

    /// A named session's socket wins over an env socket: a pane of another farm must not mix
    /// its socket with this farm's name. This is the branch the reviewer's mutation M3 hit.
    #[test]
    fn a_preferred_session_ignores_a_foreign_env_pair() {
        let status = r#"{"running":true,"session":"farm-a","socket":"/h/farm-a.sock"}"#;
        let facts = Facts::new(status, &session_list());
        let h = HerdrEnv::resolve_with(
            Some("farm-b".into()),
            Some("/h/farm-b.sock".into()),
            Some("farm-a"),
            &facts,
        )
        .unwrap();
        assert_eq!(
            (h.session.as_str(), h.socket.as_str()),
            ("farm-a", "/h/farm-a.sock")
        );
        assert_eq!(facts.asked(), vec![Some("farm-a".to_string())]);
        let facts = Facts::new(status, &session_list());
        let h = HerdrEnv::resolve_with(None, Some("/h/farm-b.sock".into()), Some("farm-a"), &facts)
            .unwrap();
        assert_eq!(h.socket, "/h/farm-a.sock");
    }

    /// An explicit name that herdr reports as not running still installs, with a warning: the
    /// name was typed, and the unit is useless until that session starts.
    #[test]
    fn install_warns_when_the_named_session_is_not_running() {
        let ws = crate::testkit::TestWorkspace::new("service-install-stopped");
        std::env::set_var("HERDR_SESSION", "stopped-s1");
        let status = r#"{"status":"not_running","running":false,"session":"stopped-s1","socket":"/h/stopped.sock"}"#;
        let facts = Facts::new(status, &session_list());
        let out = install_resolved(
            &launchd(ws.root()),
            &ws.runs_dir(),
            &argv(&["install", "--dry-run"]),
            &facts,
        )
        .unwrap();
        assert!(
            out.contains("warning:") && out.contains("not_running"),
            "{out}"
        );
        assert!(
            out.contains("stopped-s1") && out.contains("/h/stopped.sock"),
            "{out}"
        );
    }

    #[test]
    fn a_stopped_session_socket_names_no_session() {
        let list: serde_json::Value = serde_json::from_str(&session_list()).unwrap();
        assert_eq!(
            session_of_socket(&list, "/h/.config/herdr/herdr.sock"),
            None
        );
        assert_eq!(
            session_of_socket(&list, &format!("{PANE_SOCKET}/")),
            Some("quaqua".to_string())
        );
        assert_eq!(
            socket_of_session(&list, "default"),
            Some("/h/.config/herdr/herdr.sock".to_string())
        );
    }

    /// `daemon install --dry-run` in a herdr pane: the text carries the session herdr names.
    #[test]
    fn install_dry_run_carries_the_session_herdr_names() {
        // The env lock, and a sealed pane: neither the socket nor the name is in the env.
        let ws = crate::testkit::TestWorkspace::new("service-install-resolved");
        let runs = ws.runs_dir();
        for svc in [launchd(ws.root()), systemd(ws.root())] {
            let facts = Facts::new(&pane_status(), &session_list());
            let out =
                install_resolved(&svc, &runs, &argv(&["install", "--dry-run"]), &facts).unwrap();
            assert!(
                out.contains("HERDR_SESSION") && out.contains("quaqua"),
                "{out}"
            );
            assert!(out.contains(PANE_SOCKET), "{out}");
            assert!(
                !out.contains("warning:"),
                "a running session warns about nothing: {out}"
            );
        }
        assert!(!ws.root().join("Library").exists() && !ws.root().join(".config").exists());
    }

    use crate::config::LaunchdConfig;

    struct FakeRunner {
        calls: Vec<Vec<String>>,
        answers: Vec<std::io::Result<Ran>>,
    }

    impl FakeRunner {
        fn new(answers: Vec<std::io::Result<Ran>>) -> Self {
            FakeRunner {
                calls: Vec::new(),
                answers,
            }
        }
    }

    impl Runner for FakeRunner {
        fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<Ran> {
            self.calls.push(
                std::iter::once(program.to_string())
                    .chain(args.iter().cloned())
                    .collect(),
            );
            self.answers.remove(0)
        }
    }

    fn exit(code: i32, stdout: &str, stderr: &str) -> std::io::Result<Ran> {
        Ok(Ran {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        })
    }

    fn ok() -> std::io::Result<Ran> {
        exit(0, "", "")
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        strings(parts)
    }

    fn launchd(home: &Path) -> Service {
        Service::resolve(
            Some(Platform::Launchd),
            home,
            None,
            &LaunchdConfig::default(),
            501,
        )
        .unwrap()
    }

    fn systemd(home: &Path) -> Service {
        Service::resolve(
            Some(Platform::Systemd),
            home,
            None,
            &LaunchdConfig::default(),
            501,
        )
        .unwrap()
    }

    fn spec<'a>(h: &'a HerdrEnv) -> Spec<'a> {
        Spec {
            exe: Path::new("bin/quaqua"),
            workspace: Path::new("ws"),
            path: PATH,
            herdr: h,
            log: Path::new("ws/runs/daemon.log"),
        }
    }

    const ARGS: &[&str] = &["install"];

    #[test]
    fn systemd_dry_run_prints_the_unit_and_writes_nothing() {
        let d = scratch("sd-dry");
        let h = herdr();
        let out = install_at(&systemd(&d), &argv(&["install", "--dry-run"]), &spec(&h)).unwrap();
        assert_eq!(
            out,
            render_unit(Path::new("bin/quaqua"), Path::new("ws"), PATH, &h)
        );
        assert!(!d.join(".config").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn launchd_dry_run_prints_the_plist_and_writes_nothing() {
        let d = scratch("ld-dry");
        let h = herdr();
        let out = install_at(&launchd(&d), &argv(&["install", "--dry-run"]), &spec(&h)).unwrap();
        assert_eq!(out, render_plist(LAUNCHD_LABEL, &spec(&h)));
        assert!(!d.join("Library").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn systemd_install_writes_the_unit_and_prints_the_enable_command() {
        let d = scratch("sd-real");
        let h = herdr();
        let svc = systemd(&d);
        let out = install_at(&svc, &argv(ARGS), &spec(&h)).unwrap();
        let p = unit_path(&d, None);
        assert_eq!(svc.file, p);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            render_unit(Path::new("bin/quaqua"), Path::new("ws"), PATH, &h)
        );
        assert!(out.contains(&p.display().to_string()));
        assert!(out.contains("systemctl --user enable --now quaqua-daemon.service"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn launchd_install_writes_the_plist_and_prints_the_bootstrap_command() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("ld-real");
        let h = herdr();
        let svc = launchd(&d);
        let out = install_at(&svc, &argv(ARGS), &spec(&h)).unwrap();
        let p = d.join("Library/LaunchAgents/com.quaqua.daemon.plist");
        assert_eq!(svc.file, p);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            render_plist(LAUNCHD_LABEL, &spec(&h))
        );
        assert!(out.contains(&p.display().to_string()));
        assert!(out.contains(&format!("launchctl bootstrap gui/501 {}", p.display())));
        assert!(out.contains("launchctl bootout gui/501/com.quaqua.daemon"));
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o022, 0, "mode {mode:o}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn install_refuses_an_unknown_flag() {
        let d = scratch("flag");
        let h = herdr();
        let e = install_at(&systemd(&d), &argv(&["install", "--now"]), &spec(&h));
        assert!(e.is_err());
        assert!(!d.join(".config").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn systemd_install_names_legacy_auditor_units_and_keeps_them() {
        let d = scratch("sd-legacy");
        let h = herdr();
        let svc = systemd(&d);
        let dir = svc.file.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        for n in [
            "quaqua-auditor.timer",
            "quaqua-auditor.service",
            "quaqua-backup.timer",
        ] {
            std::fs::write(dir.join(n), "x").unwrap();
        }
        let out = install_at(&svc, &argv(ARGS), &spec(&h)).unwrap();
        assert!(out.contains("duplicate:") && out.contains("quaqua-auditor.timer"));
        assert!(out.contains("quaqua-auditor.service") && !out.contains("quaqua-backup"));
        assert!(dir.join("quaqua-auditor.timer").exists());
        assert!(dir.join("quaqua-auditor.service").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn launchd_install_names_legacy_auditor_plists_and_keeps_them() {
        let d = scratch("ld-legacy");
        let h = herdr();
        let svc = launchd(&d);
        let dir = svc.file.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["com.quaqua.auditor.plist", "com.quaqua.backup.plist"] {
            std::fs::write(dir.join(n), "x").unwrap();
        }
        let out = install_at(&svc, &argv(ARGS), &spec(&h)).unwrap();
        assert!(out.contains("duplicate:") && out.contains("com.quaqua.auditor.plist"));
        assert!(out.contains("launchctl bootout gui/501/com.quaqua.auditor "));
        assert!(!out.contains("com.quaqua.backup"));
        assert!(dir.join("com.quaqua.auditor.plist").exists());
        assert!(dir.join("com.quaqua.backup.plist").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn launchd_start_kickstarts_a_loaded_label() {
        let mut r = FakeRunner::new(vec![ok(), ok()]);
        launchd(Path::new("/h")).start(&mut r).unwrap();
        assert_eq!(
            r.calls,
            vec![
                argv(&["launchctl", "print", "gui/501/com.quaqua.daemon"]),
                argv(&["launchctl", "kickstart", "gui/501/com.quaqua.daemon"]),
            ]
        );
    }

    #[test]
    fn launchd_start_bootstraps_an_unloaded_plist() {
        let mut r = FakeRunner::new(vec![exit(113, "", "not found"), ok()]);
        launchd(Path::new("/h")).start(&mut r).unwrap();
        assert_eq!(
            r.calls,
            vec![
                argv(&["launchctl", "print", "gui/501/com.quaqua.daemon"]),
                argv(&[
                    "launchctl",
                    "bootstrap",
                    "gui/501",
                    "/h/Library/LaunchAgents/com.quaqua.daemon.plist"
                ]),
            ]
        );
    }

    #[test]
    fn launchd_start_failure_names_the_command_and_stderr() {
        let mut r = FakeRunner::new(vec![
            exit(113, "", ""),
            exit(5, "", "Bootstrap failed: 5: Input/output error\n"),
        ]);
        let e = launchd(Path::new("/h")).start(&mut r).unwrap_err();
        assert!(e.contains("launchctl bootstrap gui/501"), "{e}");
        assert!(e.contains("Bootstrap failed: 5: Input/output error"), "{e}");
        let mut r = FakeRunner::new(vec![exit(113, "", ""), exit(5, "", "")]);
        let e = launchd(Path::new("/h")).start(&mut r).unwrap_err();
        assert!(e.contains("exited 5"), "{e}");
        let mut r = FakeRunner::new(vec![Err(std::io::Error::other("no launchctl"))]);
        let e = launchd(Path::new("/h")).start(&mut r).unwrap_err();
        assert!(
            e.contains("launchctl print") && e.contains("no launchctl"),
            "{e}"
        );
        assert_eq!(r.calls.len(), 1);
    }

    #[test]
    fn launchd_installed_is_the_plist_on_disk() {
        let d = scratch("ld-installed");
        let svc = launchd(&d);
        let mut r = FakeRunner::new(vec![]);
        assert!(!svc.installed(&mut r));
        std::fs::create_dir_all(svc.file.parent().unwrap()).unwrap();
        std::fs::write(&svc.file, "x").unwrap();
        assert!(svc.installed(&mut r));
        assert!(r.calls.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn systemd_start_runs_systemctl_start() {
        let mut r = FakeRunner::new(vec![ok()]);
        systemd(Path::new("/h")).start(&mut r).unwrap();
        assert_eq!(
            r.calls,
            vec![argv(&[
                "systemctl",
                "--user",
                "start",
                "quaqua-daemon.service"
            ])]
        );
        let mut r = FakeRunner::new(vec![exit(1, "", "Unit is masked.")]);
        let e = systemd(Path::new("/h")).start(&mut r).unwrap_err();
        assert!(
            e.contains("systemctl --user start") && e.contains("masked"),
            "{e}"
        );
    }

    #[test]
    fn systemd_installed_reads_load_state() {
        let svc = systemd(Path::new("/h"));
        let mut r = FakeRunner::new(vec![
            exit(0, "loaded\n", ""),
            exit(0, "masked\n", ""),
            exit(0, "not-found\n", ""),
            exit(0, "\n", ""),
            exit(1, "loaded\n", ""),
            Err(std::io::Error::other("no systemctl")),
        ]);
        let got: Vec<bool> = (0..6).map(|_| svc.installed(&mut r)).collect();
        assert_eq!(got, vec![true, true, false, false, false, false]);
        assert_eq!(
            r.calls[0],
            argv(&[
                "systemctl",
                "--user",
                "show",
                "--property=LoadState",
                "--value",
                "quaqua-daemon.service"
            ])
        );
    }

    #[test]
    fn platform_current_matches_the_target() {
        let want = if cfg!(target_os = "linux") {
            Some(Platform::Systemd)
        } else if cfg!(target_os = "macos") {
            Some(Platform::Launchd)
        } else {
            None
        };
        assert_eq!(Platform::current(), want);
    }

    #[test]
    fn no_backend_refuses() {
        let e = Service::resolve(None, Path::new("/h"), None, &LaunchdConfig::default(), 501)
            .unwrap_err();
        assert_eq!(e, NO_BACKEND);
    }

    #[test]
    fn names_show_the_unit_and_the_status_command() {
        assert_eq!(
            systemd(Path::new("/h")).names(),
            Names {
                unit: "quaqua-daemon.service".into(),
                status: "systemctl --user status quaqua-daemon.service".into()
            }
        );
        assert_eq!(
            launchd(Path::new("/h")).names(),
            Names {
                unit: "com.quaqua.daemon".into(),
                status: "launchctl print gui/501/com.quaqua.daemon".into()
            }
        );
    }

    #[test]
    fn plist_runs_daemon_with_env_and_keepalive() {
        let h = herdr();
        let p = render_plist("com.quaqua.daemon", &spec(&h));
        for want in [
            "<key>Label</key>\n  <string>com.quaqua.daemon</string>",
            "<string>bin/quaqua</string>\n    <string>daemon</string>",
            "<key>QUAQUA_HOME</key>\n    <string>ws</string>",
            "<key>PATH</key>\n    <string>/h/.cargo/bin:/usr/bin</string>",
            "<key>HERDR_SESSION</key>\n    <string>s1</string>",
            "<key>HERDR_SOCKET_PATH</key>\n    <string>/h/.config/herdr/sessions/s1/herdr.sock</string>",
            "<key>RunAtLoad</key>\n  <true/>",
            "<key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>",
            "<key>ThrottleInterval</key>\n  <integer>5</integer>",
            "<key>StandardOutPath</key>\n  <string>ws/runs/daemon.log</string>",
            "<key>StandardErrorPath</key>\n  <string>ws/runs/daemon.log</string>",
        ] {
            assert!(p.contains(want), "missing {want:?} in\n{p}");
        }
        assert!(p.ends_with("</plist>\n"));
    }

    #[test]
    fn plist_escapes_xml() {
        let h = herdr();
        let mut sp = spec(&h);
        sp.exe = Path::new("opt/q&a <x>/quaqua");
        let p = render_plist("l", &sp);
        assert!(p.contains("opt/q&amp;a &lt;x&gt;/quaqua"));
        assert_eq!(xml_escape("\"'"), "&quot;&apos;");
    }

    #[test]
    fn launchd_resolve_reads_label_and_dir() {
        let home = Path::new("/h");
        let r = |label: Option<&str>, dir: Option<&str>| {
            Service::resolve(
                Some(Platform::Launchd),
                home,
                None,
                &LaunchdConfig {
                    label: label.map(Into::into),
                    dir: dir.map(Into::into),
                },
                501,
            )
            .unwrap()
        };
        assert_eq!(
            r(None, Some("~/agents")).file,
            Path::new("/h/agents/com.quaqua.daemon.plist")
        );
        assert_eq!(
            r(None, Some("/abs")).file,
            Path::new("/abs/com.quaqua.daemon.plist")
        );
        let default = Path::new("/h/Library/LaunchAgents/com.quaqua.daemon.plist");
        assert_eq!(r(None, Some("rel/dir")).file, default);
        assert_eq!(r(None, Some("  ")).file, default);
        assert_eq!(r(Some("a/b"), None).name, LAUNCHD_LABEL);
        assert_eq!(r(Some("a b"), None).name, LAUNCHD_LABEL);
        assert_eq!(r(Some("  "), None).name, LAUNCHD_LABEL);
        let x = r(Some(" com.x.y "), None);
        assert_eq!(x.name, "com.x.y");
        assert_eq!(x.file, Path::new("/h/Library/LaunchAgents/com.x.y.plist"));
    }
}
