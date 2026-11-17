//! Shared test kit: a temp workspace whose team.toml and database paths are
//! private to the test. Public (not `cfg(test)`) so `tests/*.rs` can use it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// The herdr variables a live pane exports. A test workspace must never act
/// on that pane, so they are cleared while it lives.
const HERDR_KEYS: [&str; 7] = [
    "HERDR_PANE",
    "HERDR_PANE_ID",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_SESSION",
    "HERDR_SOCKET_PATH",
    "HERDR_BIN_PATH",
];

const HERDR_ENV: [&str; 6] = [
    "HERDR_PANE",
    "HERDR_PANE_ID",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_SESSION",
    "HERDR_SOCKET_PATH",
];

/// A temp workspace dir with `runs/`, a `team.toml` and a DB path (path only:
/// the schema is T32's, the kit never creates the file).
/// While alive it holds the process-wide env lock and points
/// `QUAQUA_TEAM_TOML` at its own team.toml; drop restores the old value and
/// removes the dir. It also seals the test off from the live herdr: the pane
/// variables are cleared and `HERDR_BIN_PATH` names a binary that does not
/// exist, so a command that reaches herdr by accident fails instead of acting
/// on the pane the suite runs in.
pub struct TestWorkspace {
    root: PathBuf,
    prev_team_toml: Option<OsString>,
    prev_herdr: Vec<(&'static str, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl TestWorkspace {
    pub fn new(tag: &str) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = std::env::temp_dir().join(format!("qq-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("runs")).expect("create temp workspace");
        let prev_herdr = HERDR_KEYS
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        let ws = TestWorkspace {
            prev_team_toml: std::env::var_os("QUAQUA_TEAM_TOML"),
            prev_herdr,
            root,
            _lock: lock,
        };
        ws.seal_herdr();
        std::fs::write(ws.team_toml(), "").expect("write team.toml");
        std::env::set_var("QUAQUA_TEAM_TOML", ws.team_toml());
        ws
    }

    /// Clears the pane variables and points `HERDR_BIN_PATH` at a missing
    /// binary. [`TestWorkspace::new`] does it; a test that sets herdr
    /// variables under the env lock seals again before running commands.
    pub fn seal_herdr(&self) {
        for k in HERDR_ENV {
            std::env::remove_var(k);
        }
        std::env::set_var("HERDR_BIN_PATH", self.root.join("no-herdr"));
    }

    /// `QUAQUA_TEAM_TOML` as it was before this workspace took the env lock.
    pub fn prev_team_toml(&self) -> Option<&OsString> {
        self.prev_team_toml.as_ref()
    }

    /// Drop the workspace and return `QUAQUA_TEAM_TOML` as read while the env
    /// lock is still held, so the value cannot be another test's.
    pub fn finish(self) -> Option<OsString> {
        self.restore();
        std::env::var_os("QUAQUA_TEAM_TOML")
    }

    fn restore(&self) {
        match &self.prev_team_toml {
            Some(v) => std::env::set_var("QUAQUA_TEAM_TOML", v),
            None => std::env::remove_var("QUAQUA_TEAM_TOML"),
        }
        for (k, v) in &self.prev_herdr {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    pub fn team_toml(&self) -> PathBuf {
        self.root.join("team.toml")
    }

    /// Where this workspace's database lives; the file is not created.
    pub fn db_path(&self) -> PathBuf {
        self.root.join("runs").join("qq.db")
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        self.restore();
    }
}
