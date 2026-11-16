//! `qq init [DIR] [--update] [--dry-run]`: writes the managed workspace template
//! (embedded in the binary, see `table.rs`) into a workspace.
//!
//! - plain `init` creates the private dirs and the files that are missing, and
//!   never touches a file that exists.
//! - `--update` reconciles against `.quaqua/template.lock` (md5 per file):
//!   missing → write; equal → record; untouched since the lock → overwrite;
//!   changed locally, or no lock entry → keep it, save the new template to
//!   `.quaqua/pending/<path>`, and exit 3.
//! - `--dry-run` reports the decisions and writes nothing.
//!
//! Idempotent: a second run finds every file equal to the template and writes
//! nothing. The lock format is the one the live workspaces already carry.

mod md5;
mod table;

pub use md5::md5_hex;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::{CliError, Command, Env, Install, Registry};
use crate::daemon::Daemon;

/// gitignored personal-state dirs of a workspace.
const PRIVATE_DIRS: &[&str] = &["memory", "projects", "raw", "runs", "reports"];
const ENGINE: &str = "qq";

/// The entry `cli::MODULES` lists.
pub const INSTALL: Install = install;

fn install(registry: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    registry.register(Command {
        name: "init",
        help:
            "init [DIR] [--update] [--dry-run]  write the workspace template (exit 3 on conflict)",
        run: init_cmd,
    })
}

fn init_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    let (mut dir, mut update, mut dry) = (None, false, false);
    for a in args {
        match a.as_str() {
            "--update" => update = true,
            "--dry-run" => dry = true,
            s if s.starts_with("--") => {
                return Err(CliError::new(
                    2,
                    "usage: qq init [DIR] [--update] [--dry-run]",
                ))
            }
            s => dir = Some(PathBuf::from(s)),
        }
    }
    let target = dir.or_else(|| env.runs_dir.parent().map(Path::to_path_buf));
    let target = target.ok_or_else(|| CliError::new(2, "init: no target directory"))?;
    let (report, conflicts) = run(&target, update, dry).map_err(|e| CliError::new(1, e))?;
    if conflicts > 0 {
        Err(CliError::new(3, report))
    } else {
        Ok(report)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Lock {
    engine: String,
    files: BTreeMap<String, LockEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockEntry {
    md5: String,
    mode: u32,
}

fn lock_path(root: &Path) -> PathBuf {
    root.join(".quaqua/template.lock")
}

impl Lock {
    fn load(root: &Path) -> Result<Self, String> {
        let path = lock_path(root);
        if !path.is_file() {
            return Ok(Self::default());
        }
        let body = std::fs::read_to_string(&path)
            .map_err(|e| format!("init: cannot read {}: {e}", path.display()))?;
        serde_json::from_str(&body).map_err(|e| {
            format!(
                "init: corrupt {}: {e} (delete it to re-baseline)",
                path.display()
            )
        })
    }

    fn save(&mut self, root: &Path) -> Result<(), String> {
        self.engine = ENGINE.to_string();
        let body = serde_json::to_string_pretty(self).map_err(|e| format!("init: {e}"))?;
        write_file(&lock_path(root), format!("{body}\n").as_bytes(), 0o644)
    }

    fn record(&mut self, rel: &str, md5: String, mode: u32) {
        self.files.insert(rel.to_string(), LockEntry { md5, mode });
    }
}

/// Runs init or update on `root`. Returns the report text and the number of
/// `--update` conflicts.
pub fn run(root: &Path, update: bool, dry: bool) -> Result<(String, usize), String> {
    let mut log: Vec<String> = Vec::new();
    let mut lock = Lock::load(root)?;
    let mut conflicts = 0usize;

    if !update {
        for d in PRIVATE_DIRS {
            if root.join(d).is_dir() {
                continue;
            }
            if !dry {
                std::fs::create_dir_all(root.join(d))
                    .map_err(|e| format!("init: cannot create {d}/: {e}"))?;
            }
            log.push(format!(
                "{} {d}/",
                if dry { "would create" } else { "created" }
            ));
        }
    }

    for (rel, mode, bytes) in table::TABLE {
        let dst = root.join(rel);
        let want = md5_hex(bytes);
        let have = md5_file(&dst)?;
        let locked = lock.files.get(*rel).map(|e| e.md5.clone());
        // Missing → write; equal → record; untouched since the lock → overwrite
        // (update only); otherwise keep the file.
        let write = match &have {
            None => true,
            Some(h) if *h == want => false,
            Some(h) => update && Some(h) == locked.as_ref(),
        };
        if write {
            let verb = match (&have, update) {
                (None, false) => "create",
                (None, true) => "write",
                _ => "overwrite",
            };
            log.push(format!("{verb} {rel}"));
            if !dry {
                write_file(&dst, bytes, *mode)?;
                lock.record(rel, want, *mode);
            }
        } else if have.as_deref() == Some(want.as_str()) {
            if !dry {
                set_mode(&dst, *mode)?;
                lock.record(rel, want, *mode);
            }
        } else if update {
            conflicts += 1;
            let pending = format!(".quaqua/pending/{rel}");
            log.push(format!("conflict {rel} -> {pending}"));
            if !dry {
                write_file(&root.join(&pending), bytes, *mode)?;
            }
        }
        // Plain init, existing and different: left alone and out of the lock,
        // so the first `--update` reports it instead of overwriting it.
    }

    if update {
        for rel in lock.files.keys() {
            if !table::TABLE.iter().any(|(r, ..)| r == rel) {
                log.push(format!("removed upstream {rel} (kept)"));
            }
        }
    }
    if dry {
        log.push("dry run: nothing written".to_string());
    } else {
        lock.save(root)?;
        if conflicts > 0 {
            log.push(format!("{conflicts} conflicts — see .quaqua/pending/"));
        } else if log.is_empty() {
            log.push("workspace up to date".to_string());
        }
    }
    Ok((log.join("\n"), conflicts))
}

fn md5_file(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(md5_hex(&b))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("init: cannot read {}: {e}", path.display())),
    }
}

fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("init: cannot create {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("qq-tmp");
    std::fs::write(&tmp, bytes)
        .map_err(|e| format!("init: cannot write {}: {e}", path.display()))?;
    set_mode(&tmp, mode)?;
    std::fs::rename(&tmp, path).map_err(|e| format!("init: cannot write {}: {e}", path.display()))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("init: cannot chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded table is the template directory, file for file.
    /// Regenerate `table.rs` when this fails: every file under
    /// `templates/workspace/`, sorted, with `0o755` for executables.
    #[test]
    fn table_matches_directory() {
        fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/workspace");
        let mut on_disk = Vec::new();
        walk(&base, &base, &mut on_disk);
        on_disk.sort();
        let embedded: Vec<String> = table::TABLE.iter().map(|(r, ..)| r.to_string()).collect();
        assert_eq!(on_disk, embedded);
        for (rel, _, bytes) in table::TABLE {
            assert_eq!(&std::fs::read(base.join(rel)).unwrap(), bytes, "{rel}");
        }
    }
}
