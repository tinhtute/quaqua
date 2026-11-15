//! Workspace root, default database paths and the live-database guard.
//!
//! The new database is `<runs>/qq.db`; the old one, `<runs>/quaqua.db`, is
//! only ever read. Under test mode neither live file may be touched.

use std::path::{Path, PathBuf};

pub const DB_FILE: &str = "qq.db";
pub const OLD_DB_FILE: &str = "quaqua.db";

/// `QUAQUA_HOME`, then its alias `QUAQUA_ROOT`, then `<home>/quaqua`, else `.`.
/// Pure: no process env.
pub fn resolve_workspace_root(
    quaqua_home: Option<&Path>,
    quaqua_root: Option<&Path>,
    home: Option<&Path>,
) -> PathBuf {
    if let Some(p) = quaqua_home.or(quaqua_root) {
        p.to_path_buf()
    } else if let Some(h) = home {
        h.join("quaqua")
    } else {
        PathBuf::from(".")
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// The workspace root from the process env.
pub fn workspace_root() -> PathBuf {
    resolve_workspace_root(
        env_path("QUAQUA_HOME").as_deref(),
        env_path("QUAQUA_ROOT").as_deref(),
        env_path("HOME").as_deref(),
    )
}

#[cfg(unix)]
fn home_from_passwd() -> Option<PathBuf> {
    use std::ffi::CStr;

    #[cfg(target_os = "macos")]
    #[repr(C)]
    struct Passwd {
        pw_name: *mut std::ffi::c_char,
        pw_passwd: *mut std::ffi::c_char,
        pw_uid: u32,
        pw_gid: u32,
        pw_change: i64,
        pw_class: *mut std::ffi::c_char,
        pw_gecos: *mut std::ffi::c_char,
        pw_dir: *mut std::ffi::c_char,
        pw_shell: *mut std::ffi::c_char,
        pw_expire: i64,
    }

    #[cfg(not(target_os = "macos"))]
    #[repr(C)]
    struct Passwd {
        pw_name: *mut std::ffi::c_char,
        pw_passwd: *mut std::ffi::c_char,
        pw_uid: u32,
        pw_gid: u32,
        pw_gecos: *mut std::ffi::c_char,
        pw_dir: *mut std::ffi::c_char,
        pw_shell: *mut std::ffi::c_char,
    }

    extern "C" {
        fn getuid() -> u32;
        fn getpwuid(uid: u32) -> *const Passwd;
    }

    // SAFETY: getpwuid returns null or a pointer to a static passwd record
    // whose pw_dir is a NUL-terminated string; both are checked before use.
    unsafe {
        let pwd = getpwuid(getuid());
        if pwd.is_null() {
            return None;
        }
        let dir = (*pwd).pw_dir;
        if dir.is_null() {
            return None;
        }
        std::str::from_utf8(CStr::from_ptr(dir).to_bytes())
            .ok()
            .map(PathBuf::from)
    }
}

/// The real user home from the passwd database (independent of `$HOME`),
/// falling back to `$HOME`.
pub fn system_user_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Some(h) = home_from_passwd() {
            return Some(h);
        }
    }
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The owner's live runs directory, `<passwd home>/quaqua/runs`.
pub fn live_runs_dir() -> Option<PathBuf> {
    system_user_home().map(|h| h.join("quaqua/runs"))
}

/// Both live database files: the new `qq.db` and the old `quaqua.db`.
pub fn live_db_files() -> Vec<PathBuf> {
    live_runs_dir()
        .map(|r| vec![r.join(DB_FILE), r.join(OLD_DB_FILE)])
        .unwrap_or_default()
}

/// Cargo sets `CARGO_BIN_EXE_<name>` for integration tests (the package has a
/// bin target), where `cfg!(test)` is false for the library.
pub fn is_test_environment() -> bool {
    cfg!(test)
        || std::env::var("QUAQUA_TEST").as_deref() == Ok("1")
        || std::env::vars_os().any(|(k, _)| k.to_string_lossy().starts_with("CARGO_BIN_EXE_"))
}

/// Canonical form of `p`, also for a file that does not exist yet.
fn normalize(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    match (p.parent().and_then(|d| d.canonicalize().ok()), p.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => p.to_path_buf(),
    }
}

/// True when `path` is one of `live` (canonicalized compare). Pure: no env.
pub fn is_live_db(path: &Path, live: &[PathBuf]) -> bool {
    let want = normalize(path);
    live.iter().any(|l| path == l || want == normalize(l))
}

/// True when the process is in test mode and `path` is a live database file.
/// Every opener calls this before its first filesystem access.
pub fn live_db_open_refused(path: &Path) -> bool {
    if !is_test_environment() {
        return false;
    }
    let tmp = std::env::temp_dir();
    if path.starts_with(&tmp) || normalize(path).starts_with(normalize(&tmp)) {
        return false;
    }
    is_live_db(path, &live_db_files())
}

/// `QUAQUA_RUNS_DIR`, else `<workspace>/runs`. In test mode a result that
/// holds a live database is replaced by a per-process temp dir.
pub fn default_runs_dir() -> PathBuf {
    if let Some(dir) = env_path("QUAQUA_RUNS_DIR") {
        return dir;
    }
    let runs = workspace_root().join("runs");
    if is_test_environment()
        && (live_db_open_refused(&runs.join(DB_FILE)) || live_db_open_refused(&runs.join(OLD_DB_FILE)))
    {
        let dir = std::env::temp_dir().join(format!("qq-test-runs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    runs
}

/// The new database: `<runs>/qq.db`.
pub fn default_db_path() -> PathBuf {
    default_runs_dir().join(DB_FILE)
}

/// The old database, read-only: `<runs>/quaqua.db`.
pub fn old_db_path() -> PathBuf {
    default_runs_dir().join(OLD_DB_FILE)
}
