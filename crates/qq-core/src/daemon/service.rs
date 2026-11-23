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

/// Unit text for the daemon, run from `exe` against workspace `home`, with `PATH` as given.
pub fn render_unit(exe: &Path, home: &Path, path: &str) -> String {
    let env = quote(&format!("QUAQUA_HOME={}", home.display()));
    let path_env = quote(&format!("PATH={path}"));
    format!(
        "[Unit]\nDescription=quaqua farm daemon\n\n\
         [Service]\nExecStart={} daemon\nEnvironment={env}\nEnvironment={path_env}\nRestart=on-failure\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n",
        quote(&exe.display().to_string())
    )
}

/// Where the user unit file goes.
pub fn unit_path(home: &Path, xdg_config: Option<&Path>) -> PathBuf {
    let base = xdg_config.map_or_else(|| home.join(".config"), Path::to_path_buf);
    base.join("systemd/user").join(UNIT_NAME)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inhibit {
    Start,
    Stop,
    Keep,
}

/// What to do with the sleep inhibitor given the live worker count and whether it is held.
pub fn inhibit_decision(live: usize, held: bool) -> Inhibit {
    match (live > 0, held) {
        (true, false) => Inhibit::Start,
        (false, true) => Inhibit::Stop,
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
pub fn wake_body(slept_ms: u64, agents: &[String]) -> String {
    let secs = slept_ms / 1000;
    if agents.is_empty() {
        return format!("machine resumed from sleep after {secs}s; no open agents");
    }
    format!(
        "machine resumed from sleep after {secs}s; agents that were open and may have been cut: {}",
        agents.join(", ")
    )
}

/// `quaqua daemon install [--dry-run]`: writes the user unit and returns the text to print.
/// `--dry-run` returns the unit alone and touches nothing. Never enables or starts it.
pub fn install_at(
    args: &[String],
    exe: &Path,
    workspace: &Path,
    home: &Path,
    xdg_config: Option<&Path>,
    path: &str,
) -> Result<String, String> {
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
    if !cfg!(target_os = "linux") {
        return Err(String::from(
            "daemon install supports systemd on Linux only",
        ));
    }
    let unit = render_unit(exe, workspace, path);
    if dry {
        return Ok(unit);
    }
    let path = unit_path(home, xdg_config);
    let dir = path.parent().ok_or("unit path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    std::fs::write(&path, unit).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(format!(
        "wrote {}\nenable it with: systemctl --user enable --now {UNIT_NAME}\n",
        path.display()
    ))
}

/// `quaqua daemon install` for the running process: exe from `current_exe`, home from `HOME`,
/// the workspace is the parent of `runs_dir`.
pub fn install(runs_dir: &Path, args: &[String]) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own executable: {e}"))?;
    let workspace = runs_dir.parent().unwrap_or(runs_dir);
    let workspace = std::path::absolute(workspace).map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    // A systemd user unit gets a bare PATH, so the unit carries the installer's own.
    let path = std::env::var("PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            let h = home.display();
            format!("{h}/.local/bin:{h}/.cargo/bin:/usr/local/bin:/usr/bin:/bin")
        });
    install_at(args, &exe, &workspace, &home, xdg.as_deref(), &path)
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

impl Drop for Inhibitor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const TICK: std::time::Duration = std::time::Duration::from_secs(5);

/// Names of the open Lead and Peer agents: the live workers.
fn live_agents(ctx: &super::Ctx) -> Vec<String> {
    ctx.store
        .with_conn(|c| {
            let mut st = c.prepare(
                "SELECT name FROM agent WHERE ended_ms IS NULL AND role IN ('lead', 'peer') ORDER BY name",
            )?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<Result<Vec<String>, _>>()
        })
        .unwrap_or_default()
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
                match inhibit_decision(agents.len(), held.is_some()) {
                    Inhibit::Start => held = Inhibitor::acquire(),
                    Inhibit::Stop => held = None,
                    Inhibit::Keep => {}
                }
                if let Some(slept) = wake.check(wall(), mono()) {
                    let _ = crate::bus::daemon_mail(
                        &ctx,
                        "cos",
                        "state",
                        &wake_body(slept, &agents),
                        None,
                    );
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
        let u = render_unit(Path::new("opt/qq bin/quaqua"), Path::new("ws/quaqua"), PATH);
        assert!(u.contains("[Service]"));
        assert!(u.contains("ExecStart=\"opt/qq bin/quaqua\" daemon\n"));
        assert!(u.contains("Environment=\"QUAQUA_HOME=ws/quaqua\"\n"));
        assert!(u.contains("Environment=\"PATH=/h/.cargo/bin:/usr/bin\"\n"));
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
        assert_eq!(inhibit_decision(1, false), Inhibit::Start);
        assert_eq!(inhibit_decision(3, true), Inhibit::Keep);
        assert_eq!(inhibit_decision(0, true), Inhibit::Stop);
        assert_eq!(inhibit_decision(0, false), Inhibit::Keep);
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
        let b = wake_body(90_000, &["lead-a".into(), "peer-b".into()]);
        assert!(b.contains("lead-a") && b.contains("peer-b"));
        assert!(b.contains("90s"));
        assert!(wake_body(90_000, &[]).contains("no open agents"));
    }

    #[test]
    fn dry_run_prints_the_unit_and_writes_nothing() {
        let d = scratch("dry");
        let out = install_at(
            &["install".into(), "--dry-run".into()],
            Path::new("bin/quaqua"),
            Path::new("ws"),
            &d,
            None,
            PATH,
        )
        .unwrap();
        assert_eq!(
            out,
            render_unit(Path::new("bin/quaqua"), Path::new("ws"), PATH)
        );
        assert!(!d.join(".config").exists());
    }

    #[test]
    fn install_writes_the_unit_and_prints_the_enable_command() {
        let d = scratch("real");
        let out = install_at(
            &["install".into()],
            Path::new("bin/quaqua"),
            Path::new("ws"),
            &d,
            None,
            PATH,
        )
        .unwrap();
        let p = unit_path(&d, None);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            render_unit(Path::new("bin/quaqua"), Path::new("ws"), PATH)
        );
        assert!(out.contains(&p.display().to_string()));
        assert!(out.contains("systemctl --user enable --now quaqua-daemon.service"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn install_refuses_an_unknown_flag() {
        let d = scratch("flag");
        let e = install_at(
            &["install".into(), "--now".into()],
            Path::new("q"),
            Path::new("w"),
            &d,
            None,
            PATH,
        );
        assert!(e.is_err());
        assert!(!d.join(".config").exists());
    }
}
