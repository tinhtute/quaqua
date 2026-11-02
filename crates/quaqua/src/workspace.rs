//! The one workspace root.
//!
//! Every workspace path the binary resolves (rules, runs, team.toml) goes
//! through [`workspace_root`]: `QUAQUA_HOME` first (with `QUAQUA_ROOT` kept as
//! an alias for one release), else `$HOME/quaqua`. There is no discovery from
//! the cwd or the executable path. Code that needs a real workspace calls
//! [`workspace_root_checked`], which requires `projects/INDEX.md`.

use std::path::{Path, PathBuf};

/// Pure resolver, no process env — testable without `set_var`.
/// `QUAQUA_HOME` wins, else `$HOME/quaqua`, else the current directory.
pub fn resolve_workspace_root(quaqua_home: Option<&Path>, home: Option<&Path>) -> PathBuf {
    if let Some(p) = quaqua_home {
        p.to_path_buf()
    } else if let Some(h) = home {
        h.join("quaqua")
    } else {
        PathBuf::from(".")
    }
}

/// Returns true if the process is running in a test environment.
pub fn is_test_environment() -> bool {
    if cfg!(test) {
        return true;
    }
    std::env::var("QUAQUA_TEST").as_deref() == Ok("1") || is_running_in_cargo_test()
}

fn is_running_in_cargo_test() -> bool {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            if parent.ends_with("deps") {
                return true;
            }
        }
    }
    false
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

    unsafe {
        let pwd = getpwuid(getuid());
        if pwd.is_null() {
            return None;
        }
        let dir = (*pwd).pw_dir;
        if dir.is_null() {
            return None;
        }
        let bytes = CStr::from_ptr(dir).to_bytes();
        std::str::from_utf8(bytes).ok().map(PathBuf::from)
    }
}

/// Returns the real user home directory from the system passwd database (independent of HOME env var),
/// falling back to the HOME environment variable if passwd lookup is unavailable.
pub fn system_user_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Some(h) = home_from_passwd() {
            return Some(h);
        }
    }
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Returns the live workspace DB path at `<system_user_home>/quaqua/runs/quaqua.db`.
pub fn live_workspace_db() -> Option<PathBuf> {
    system_user_home().map(|h| h.join("quaqua/runs/quaqua.db"))
}

/// Returns the live workspace runs directory at `<system_user_home>/quaqua/runs`.
pub fn live_workspace_runs() -> Option<PathBuf> {
    system_user_home().map(|h| h.join("quaqua/runs"))
}

/// Returns true if `path` points to the live workspace DB at `$HOME/quaqua/runs/quaqua.db`.
pub fn is_live_workspace_db(path: &Path) -> bool {
    // Any path inside the system temp directory is a test path, never the live workspace.
    let tmp = std::env::temp_dir();
    if path.starts_with(&tmp) {
        return false;
    }
    if let (Ok(p_canon), Ok(tmp_canon)) = (path.canonicalize(), tmp.canonicalize()) {
        if p_canon.starts_with(&tmp_canon) {
            return false;
        }
    }
    if path.starts_with("/tmp")
        || path.starts_with("/private/tmp")
        || path.starts_with("/private/var")
    {
        return false;
    }

    if let Some(live) = live_workspace_db() {
        if path == live {
            return true;
        }
        if let (Ok(p1), Ok(p2)) = (path.canonicalize(), live.canonicalize()) {
            if p1 == p2 {
                return true;
            }
        }
    }
    false
}

/// Returns true if `path` points to the live workspace runs directory at `$HOME/quaqua/runs`.
pub fn is_live_workspace_runs(path: &Path) -> bool {
    let db = path.join("quaqua.db");
    is_live_workspace_db(&db)
}

/// True when a test must not touch `path` at all: it is the owner's live workspace DB.
///
/// Every SQLite opener calls this before its first filesystem access, so a test
/// can never create, migrate, or read the live journal through a default path or
/// an env override.
pub fn live_db_open_refused(path: &Path) -> bool {
    is_test_environment() && is_live_workspace_db(path)
}

/// A dedicated per-process temporary runs directory under test.
pub fn test_default_runs_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quaqua-test-runs-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// A non-empty env var as a path; an empty value counts as unset.
pub(crate) fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// [`workspace_root`] from an already-read env snapshot: `QUAQUA_HOME`, then
/// its alias `QUAQUA_ROOT`, then `$HOME/quaqua`. Pure — testable without
/// `set_var`.
pub(crate) fn workspace_root_from_env(
    quaqua_home: Option<&Path>,
    quaqua_root: Option<&Path>,
    home: Option<&Path>,
) -> PathBuf {
    resolve_workspace_root(quaqua_home.or(quaqua_root), home)
}

/// Read the process env once: `QUAQUA_HOME`, then its alias `QUAQUA_ROOT`,
/// then `$HOME/quaqua`.
pub fn workspace_root() -> PathBuf {
    workspace_root_from_env(
        env_path("QUAQUA_HOME").as_deref(),
        env_path("QUAQUA_ROOT").as_deref(),
        env_path("HOME").as_deref(),
    )
}

/// [`workspace_root`], required to be an actual workspace: `projects/INDEX.md`
/// must exist. A missing index is an error — never a cwd fallback.
pub fn workspace_root_checked() -> Result<PathBuf, (i32, String)> {
    ensure_workspace_root(workspace_root())
}

fn ensure_workspace_root(root: PathBuf) -> Result<PathBuf, (i32, String)> {
    if root.join("projects/INDEX.md").is_file() {
        Ok(root)
    } else {
        Err((
            1,
            format!(
                "no quaqua workspace at {}: projects/INDEX.md is missing (set QUAQUA_HOME or QUAQUA_ROOT)\n",
                root.display()
            ),
        ))
    }
}

/// Sanitizes a string to `[a-z0-9_-]` (lowercase, everything else -> `-`).
pub fn sanitize_farm_id(name: &str) -> String {
    let lower = name.to_lowercase();
    let sanitized: String = lower
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

/// Derives the farm id from a workspace root: the last path component
/// of the canonicalized root, sanitized to `[a-z0-9_-]`.
pub fn farm_id_of(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let last = canonical
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("quaqua");
    sanitize_farm_id(last)
}

/// Derives the local farm id from `workspace_root()`.
pub fn farm_id() -> String {
    farm_id_of(&workspace_root())
}

/// Derives the farm id for a given runs directory.
/// If `runs_dir` ends with `runs`, resolves from its parent workspace root.
pub fn farm_of_runs_dir(runs_dir: &Path) -> String {
    let target = if runs_dir.file_name().and_then(|s| s.to_str()) == Some("runs") {
        runs_dir.parent().unwrap_or(runs_dir)
    } else {
        runs_dir
    };
    farm_id_of(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quaqua_home_wins_over_home() {
        assert_eq!(
            resolve_workspace_root(Some(Path::new("target/qa")), Some(Path::new("target/user"))),
            PathBuf::from("target/qa"),
        );
    }

    #[test]
    fn default_is_home_quaqua() {
        assert_eq!(
            resolve_workspace_root(None, Some(Path::new("target/user"))),
            PathBuf::from("target/user/quaqua"),
        );
    }

    #[test]
    fn no_env_falls_back_to_cwd() {
        assert_eq!(resolve_workspace_root(None, None), PathBuf::from("."));
    }

    #[test]
    fn checked_root_requires_projects_index() {
        let dir =
            std::env::temp_dir().join(format!("quaqua-workspace-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let err = ensure_workspace_root(dir.clone()).unwrap_err();
        assert_eq!(err.0, 1);
        assert!(err.1.contains("projects/INDEX.md"), "{}", err.1);

        std::fs::create_dir_all(dir.join("projects")).unwrap();
        std::fs::write(dir.join("projects/INDEX.md"), "# Projects\n").unwrap();
        assert_eq!(ensure_workspace_root(dir.clone()).unwrap(), dir);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_sanitize_farm_id() {
        assert_eq!(sanitize_farm_id("quaqua"), "quaqua");
        assert_eq!(sanitize_farm_id("TTDVKH"), "ttdvkh");
        assert_eq!(sanitize_farm_id("My Farm.v1"), "my-farm-v1");
        assert_eq!(sanitize_farm_id("farm_2"), "farm_2");
    }

    #[test]
    fn test_farm_of_runs_dir() {
        let runs = Path::new("/path/to/myworkspace/runs");
        assert_eq!(farm_of_runs_dir(runs), "myworkspace");

        let standalone = Path::new("/tmp/my-standalone-test");
        assert_eq!(farm_of_runs_dir(standalone), "my-standalone-test");
    }
}
