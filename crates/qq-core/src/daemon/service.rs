//! Service glue: the systemd user unit, the sleep inhibitor and wake detection.
//! Pure decisions live in free functions; the daemon thread and `install` are thin wrappers.

use std::path::{Path, PathBuf};

/// Unit file name under `systemd/user/`.
pub const UNIT_NAME: &str = "quaqua-daemon.service";

/// Unit text for the daemon, run from `exe` against workspace `home`.
pub fn render_unit(_exe: &Path, _home: &Path) -> String {
    String::new()
}

/// Where the user unit file goes.
pub fn unit_path(_home: &Path, _xdg_config: Option<&Path>) -> PathBuf {
    PathBuf::new()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inhibit {
    Start,
    Stop,
    Keep,
}

/// What to do with the sleep inhibitor given the live worker count and whether it is held.
pub fn inhibit_decision(_live: usize, _held: bool) -> Inhibit {
    Inhibit::Keep
}

/// Detects a resume from sleep: the wall clock moved on more than the monotonic clock.
pub struct WakeDetector {
    last_wall: u64,
    last_mono: u64,
}

impl WakeDetector {
    pub fn new(wall_ms: u64, mono_ms: u64) -> Self {
        WakeDetector { last_wall: wall_ms, last_mono: mono_ms }
    }

    /// Milliseconds slept since the last check, when it is a resume.
    pub fn check(&mut self, _wall_ms: u64, _mono_ms: u64) -> Option<u64> {
        None
    }
}

/// Body of the mail to cos after a resume.
pub fn wake_body(_slept_ms: u64, _agents: &[String]) -> String {
    String::new()
}

/// `quaqua daemon install [--dry-run]`: writes the user unit and returns the text to print.
/// `--dry-run` returns the unit alone and touches nothing. Never enables or starts it.
pub fn install_at(
    _args: &[String],
    _exe: &Path,
    _workspace: &Path,
    _home: &Path,
    _xdg_config: Option<&Path>,
) -> Result<String, String> {
    Err(String::from("not written yet"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("qq-service-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unit_runs_daemon_with_home() {
        let u = render_unit(Path::new("opt/qq bin/quaqua"), Path::new("ws/quaqua"));
        assert!(u.contains("[Service]"));
        assert!(u.contains("ExecStart=\"opt/qq bin/quaqua\" daemon\n"));
        assert!(u.contains("Environment=\"QUAQUA_HOME=ws/quaqua\"\n"));
        assert!(u.contains("Restart=on-failure"));
        assert!(u.contains("WantedBy=default.target"));
    }

    #[test]
    fn unit_path_prefers_xdg() {
        let n = "systemd/user/quaqua-daemon.service";
        assert_eq!(unit_path(Path::new("h"), None), Path::new("h/.config").join(n));
        assert_eq!(unit_path(Path::new("h"), Some(Path::new("x"))), Path::new("x").join(n));
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
        )
        .unwrap();
        assert_eq!(out, render_unit(Path::new("bin/quaqua"), Path::new("ws")));
        assert!(!d.join(".config").exists());
    }

    #[test]
    fn install_writes_the_unit_and_prints_the_enable_command() {
        let d = scratch("real");
        let out = install_at(&["install".into()], Path::new("bin/quaqua"), Path::new("ws"), &d, None)
            .unwrap();
        let p = unit_path(&d, None);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), render_unit(Path::new("bin/quaqua"), Path::new("ws")));
        assert!(out.contains(&p.display().to_string()));
        assert!(out.contains("systemctl --user enable --now quaqua-daemon.service"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn install_refuses_an_unknown_flag() {
        let d = scratch("flag");
        let e = install_at(&["install".into(), "--now".into()], Path::new("q"), Path::new("w"), &d, None);
        assert!(e.is_err());
        assert!(!d.join(".config").exists());
    }
}
