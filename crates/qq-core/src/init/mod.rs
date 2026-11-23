//! `quaqua init [DIR] [--update] [--dry-run]`: writes the managed workspace template
//! (embedded in the binary, see `table.rs`) into a workspace.
//!
//! - plain `init` creates the private dirs and the files that are missing, and
//!   never touches a file that exists.
//! - `--update` reconciles against `.quaqua/template.lock` (md5 per file):
//!   missing → write; equal → record; untouched since the lock → overwrite;
//!   changed locally, or no lock entry → keep it, save the new template to
//!   `.quaqua/pending/<path>`, and exit 3.
//! - `--update --accept <path>` (repeatable) settles a conflict the operator
//!   resolved by hand: the lock base moves to the current template and the
//!   resolved local file's md5 is remembered (`accepted`), `.quaqua/pending/<path>`
//!   goes away. Later: same template + same file → quiet; the template moves or
//!   the file changes again → a conflict, never an overwrite.
//! - seed files (`memory/rules.md`, `projects/INDEX.md`, `projects/_template.md`,
//!   `USER.md`) are written only when absent, never overwritten.
//! - plain `init` in a git repo installs the portable-paths pre-commit gate
//!   (`core.hooksPath`); `--git-hooks <repo>` does only that.
//! - `--update` merges the memory hook entries into `.claude/settings.json`
//!   (marker `# qq-managed`, idempotent) and keeps the rest of the file.
//! - `--dry-run` reports the decisions and writes nothing.
//!
//! Idempotent: a second run finds every file equal to the template and writes
//! nothing. The lock format is the one the live workspaces already carry.

mod hooks;
mod md5;
mod seeds;
mod table;

pub use hooks::run_git_hooks;
pub use md5::md5_hex;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::{CliError, Command, Env, Install, Registry};
use crate::daemon::Daemon;

/// gitignored personal-state dirs of a workspace.
const PRIVATE_DIRS: &[&str] = &["memory", "projects", "raw", "runs", "reports"];
const ENGINE: &str = "quaqua";

/// The entry `cli::MODULES` lists.
pub const INSTALL: Install = install;

fn install(registry: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    registry.register(Command {
        name: "init",
        help: "init [DIR] [--update [--accept PATH]...] [--dry-run]  write the workspace template (exit 3 on conflict)",
        run: init_cmd,
    })
}

const USAGE: &str = "usage: quaqua init [DIR] [--update [--accept PATH]...] [--dry-run] | quaqua init --git-hooks <repo>";

pub fn init_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    let (mut dir, mut update, mut dry, mut git_hooks) = (None, false, false, false);
    let mut accept: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--accept" => accept.push(it.next().ok_or_else(|| CliError::new(2, USAGE))?.clone()),
            "--update" => update = true,
            "--dry-run" => dry = true,
            "--git-hooks" => git_hooks = true,
            s if s.starts_with("--") => return Err(CliError::new(2, USAGE)),
            s => dir = Some(PathBuf::from(s)),
        }
    }
    if git_hooks {
        let repo = dir.ok_or_else(|| CliError::new(2, USAGE))?;
        return run_git_hooks(&repo).map_err(|e| CliError::new(1, e));
    }
    if !accept.is_empty() && !update {
        return Err(CliError::new(2, USAGE));
    }
    let target = dir.or_else(|| env.runs_dir.parent().map(Path::to_path_buf));
    let target = target.ok_or_else(|| CliError::new(2, "init: no target directory"))?;
    let (mut report, conflicts) =
        run_with(&target, update, dry, &accept).map_err(|e| CliError::new(1, e))?;
    if !update && !dry {
        // The CoS pane of this workspace gets its tokens and identity record.
        let panes = crate::identity::cli_panes();
        let pane = crate::identity::pane_from_env();
        match crate::identity::session_retag(panes.as_ref(), pane.as_deref(), &target, &target) {
            Some(Ok(crate::identity::Retag::Tagged)) => report.push_str("\ntagged the CoS pane"),
            Some(Err(e)) => report.push_str(&format!("\ncos pane not tagged: {e}")),
            _ => {}
        }
    }
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
    /// md5 of a hand-resolved local file the operator accepted against `md5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accepted: Option<String>,
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
        self.files.insert(
            rel.to_string(),
            LockEntry {
                md5,
                mode,
                accepted: None,
            },
        );
    }
}

/// Runs init or update on `root`. Returns the report text and the number of
/// `--update` conflicts.
pub fn run(root: &Path, update: bool, dry: bool) -> Result<(String, usize), String> {
    run_with(root, update, dry, &[])
}

/// `run` plus `accept`: managed paths whose hand-resolved local file the
/// operator accepts (`--update --accept <path>`).
pub fn run_with(
    root: &Path,
    update: bool,
    dry: bool,
    accept: &[String],
) -> Result<(String, usize), String> {
    let mut log: Vec<String> = Vec::new();
    let mut lock = Lock::load(root)?;
    let mut conflicts = 0usize;

    for rel in accept {
        let (_, mode, bytes) = table::TABLE
            .iter()
            .find(|(r, ..)| r == rel)
            .ok_or_else(|| format!("init: --accept {rel}: not a managed template file"))?;
        let have = md5_file(&root.join(rel))?
            .ok_or_else(|| format!("init: --accept {rel}: no local file to accept"))?;
        let want = md5_hex(bytes);
        lock.record(rel, want.clone(), *mode);
        if have != want {
            if let Some(e) = lock.files.get_mut(rel.as_str()) {
                e.accepted = Some(have);
            }
        }
        log.push(format!(
            "{} {rel}",
            if dry { "would accept" } else { "accept" }
        ));
        if !dry {
            let pending = root.join(".quaqua/pending").join(rel);
            if pending.is_file() {
                std::fs::remove_file(&pending)
                    .map_err(|e| format!("init: cannot remove {}: {e}", pending.display()))?;
            }
        }
    }

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
        // Accepted: same template as at accept time, same file the operator resolved.
        let resolved = update
            && locked.as_deref() == Some(want.as_str())
            && have.is_some()
            && lock.files.get(*rel).and_then(|e| e.accepted.as_ref()) == have.as_ref();
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
        } else if resolved {
            // Settled by `--accept`; nothing to report.
        } else if update {
            conflicts += 1;
            let pending = format!(".quaqua/pending/{rel}");
            let differ = differing_lines(&dst, bytes);
            log.push(format!(
                "conflict {rel} -> {pending} ({differ} lines differ; diff {rel} {pending})"
            ));
            if !dry {
                write_file(&root.join(&pending), bytes, *mode)?;
            }
        }
        // Plain init, existing and different: left alone and out of the lock,
        // so the first `--update` reports it instead of overwriting it.
    }

    for (rel, body) in seeds::SEEDS {
        if root.join(rel).exists() {
            continue;
        }
        log.push(format!(
            "{} seed {rel}",
            if dry { "would write" } else { "write" }
        ));
        if !dry {
            write_file(&root.join(rel), body.as_bytes(), 0o644)?;
        }
    }

    if !update && !dry {
        match hooks::wire(root) {
            Ok(Some(line)) if !line.is_empty() => log.push(line),
            Ok(_) => {}
            Err(e) => log.push(format!("git hooks not installed: {e}")),
        }
    }

    if update {
        match merge_memory_hooks(root, dry, &mut lock) {
            Ok(Some(line)) => log.push(line),
            Ok(None) => {}
            Err(e) => log.push(e),
        }
    }

    if update {
        let removed = lock
            .files
            .keys()
            .filter(|rel| !table::TABLE.iter().any(|(r, ..)| r == *rel))
            .count();
        if removed > 0 {
            let s = if removed == 1 { "" } else { "s" };
            log.push(format!("{removed} file{s} removed upstream, kept"));
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

/// Lines of `new` that are not line-for-line the same as the file at `path`.
fn differing_lines(path: &Path, new: &[u8]) -> usize {
    let old = std::fs::read_to_string(path).unwrap_or_default();
    let new = String::from_utf8_lossy(new);
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    (0..a.len().max(b.len()))
        .filter(|i| a.get(*i) != b.get(*i))
        .count()
}

const SETTINGS_REL: &str = ".claude/settings.json";
/// Trailing shell comment that marks a handler `quaqua init --update` added.
const MARKER: &str = "# qq-managed";

/// `(event, matcher, needle, command)`: a handler whose command holds `needle`
/// (any binary path, marker or not) counts as present.
fn memory_hooks() -> [(&'static str, Option<&'static str>, &'static str, String); 2] {
    [
        (
            "PreToolUse",
            Some("Write|Edit|MultiEdit"),
            "memory hook pre",
            format!("quaqua memory hook pre {MARKER}"),
        ),
        (
            "PostToolBatch",
            None,
            "memory hook batch",
            format!("quaqua memory hook batch {MARKER}"),
        ),
    ]
}

fn has_command(groups: &[serde_json::Value], needle: &str) -> bool {
    groups.iter().any(|g| {
        g.get("hooks")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|hs| {
                hs.iter().any(|h| {
                    h.get("command")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|c| c.contains(needle))
                })
            })
    })
}

/// Adds the handler to `hooks[event]` (the group with `matcher`, or a new one)
/// unless one running `needle` is there. True when the document changed.
fn ensure_hook(
    doc: &mut serde_json::Value,
    (event, matcher, needle, command): &(&str, Option<&str>, &str, String),
) -> bool {
    use serde_json::{json, Map, Value};
    let Some(hooks) = doc
        .as_object_mut()
        .map(|o| o.entry("hooks").or_insert_with(|| json!({})))
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    let Some(groups) = hooks
        .entry(*event)
        .or_insert_with(|| json!([]))
        .as_array_mut()
    else {
        return false;
    };
    if has_command(groups, needle) {
        return false;
    }
    let handler = json!({ "type": "command", "command": command });
    let slot = groups.iter_mut().find(|g| match matcher {
        Some(m) => g.get("matcher").and_then(Value::as_str) == Some(*m),
        None => g.get("matcher").is_none(),
    });
    match slot
        .and_then(|g| g.get_mut("hooks"))
        .and_then(Value::as_array_mut)
    {
        Some(handlers) => handlers.push(handler),
        None => {
            let mut group = Map::new();
            if let Some(m) = matcher {
                group.insert("matcher".into(), json!(m));
            }
            group.insert("hooks".into(), json!([handler]));
            groups.push(Value::Object(group));
        }
    }
    true
}

/// `--update`: puts the memory hook entries into a settings file the user
/// modified, so a kept (conflicting) file still runs them. Idempotent; a file
/// that is not a JSON object is reported and left alone.
fn merge_memory_hooks(root: &Path, dry: bool, lock: &mut Lock) -> Result<Option<String>, String> {
    let path = root.join(SETTINGS_REL);
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let mut doc: serde_json::Value = serde_json::from_str(&body)
        .ok()
        .filter(serde_json::Value::is_object)
        .ok_or_else(|| format!("{SETTINGS_REL} is not a JSON object — memory hooks not merged"))?;
    let mut changed = false;
    for h in memory_hooks() {
        changed |= ensure_hook(&mut doc, &h);
    }
    if !changed {
        return Ok(None);
    }
    if dry {
        return Ok(Some(format!(
            "would merge memory hooks into {SETTINGS_REL}"
        )));
    }
    let mut out = serde_json::to_string_pretty(&doc).map_err(|e| format!("init: {e}"))?;
    out.push('\n');
    write_file(&path, out.as_bytes(), 0o644)?;
    // Only a file still equal to its lock entry was ours; recording the merge
    // keeps the next `--update` from calling it a local edit. A user-modified
    // file keeps its old lock entry, so the next `--update` still reports it
    // and never overwrites it.
    if let Some(e) = lock.files.get_mut(SETTINGS_REL) {
        let old = md5_hex(body.as_bytes());
        if e.md5 == old {
            e.md5 = md5_hex(out.as_bytes());
        }
        if e.accepted.as_deref() == Some(old.as_str()) {
            e.accepted = Some(md5_hex(out.as_bytes()));
        }
    }
    Ok(Some(format!("merged memory hooks into {SETTINGS_REL}")))
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
        for (rel, mode, bytes) in table::TABLE {
            assert_eq!(&std::fs::read(base.join(rel)).unwrap(), bytes, "{rel}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let disk = std::fs::metadata(base.join(rel))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(disk & 0o111 != 0, mode & 0o111 != 0, "{rel}: exec bit");
            }
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("qq-init-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn set_lock_md5(root: &Path, rel: &str, md5: &str) {
        let p = lock_path(root);
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        v["files"][rel]["md5"] = serde_json::json!(md5);
        std::fs::write(&p, v.to_string()).unwrap();
    }

    /// A conflict the operator resolved by hand stays resolved; a later template
    /// change or a later local edit is a conflict again.
    #[test]
    fn init_accept_resolves_a_conflict_until_the_template_or_file_moves() {
        let root = scratch("accept");
        let acc = ["AGENTS.md".to_string()];
        run(&root, false, false).unwrap();
        let agents = root.join("AGENTS.md");
        // The template moved on since the lock, and the operator edited the file.
        set_lock_md5(&root, "AGENTS.md", &md5_hex(b"older template\n"));
        std::fs::write(&agents, "upstream + my block\n").unwrap();
        let (r, c) = run(&root, true, false).unwrap();
        assert_eq!(c, 1, "{r}");
        assert!(root.join(".quaqua/pending/AGENTS.md").is_file());

        // Without accept it is flagged forever.
        assert_eq!(run(&root, true, false).unwrap().1, 1);

        // Accept: this update reports 0 conflicts, drops the pending copy, keeps the file.
        let (r, c) = run_with(&root, true, false, &acc).unwrap();
        assert_eq!(c, 0, "{r}");
        assert!(!root.join(".quaqua/pending/AGENTS.md").exists());
        assert_eq!(
            std::fs::read_to_string(&agents).unwrap(),
            "upstream + my block\n"
        );
        // And it stays quiet.
        let (r, c) = run(&root, true, false).unwrap();
        assert_eq!((c, r.as_str()), (0, "workspace up to date"));

        // A later template change (lock base differs from the template) is a conflict again.
        set_lock_md5(&root, "AGENTS.md", &md5_hex(b"older template\n"));
        assert_eq!(run(&root, true, false).unwrap().1, 1);
        run_with(&root, true, false, &acc).unwrap();

        // A later local edit is a conflict again, and is never overwritten.
        std::fs::write(&agents, "edited again\n").unwrap();
        assert_eq!(run(&root, true, false).unwrap().1, 1);
        assert_eq!(std::fs::read_to_string(&agents).unwrap(), "edited again\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `--dry-run --accept` writes nothing; `--accept` needs `--update` and a managed path.
    #[test]
    fn init_accept_dry_run_and_usage() {
        let root = scratch("accept-dry");
        run(&root, false, false).unwrap();
        set_lock_md5(&root, "AGENTS.md", &md5_hex(b"older template\n"));
        std::fs::write(root.join("AGENTS.md"), "mine\n").unwrap();
        run(&root, true, false).unwrap();
        let before = std::fs::read(lock_path(&root)).unwrap();
        let acc = ["AGENTS.md".to_string()];
        let (r, c) = run_with(&root, true, true, &acc).unwrap();
        assert_eq!(c, 0, "{r}");
        assert!(r.contains("would accept AGENTS.md"), "{r}");
        assert_eq!(std::fs::read(lock_path(&root)).unwrap(), before);
        assert!(root.join(".quaqua/pending/AGENTS.md").is_file());
        assert!(run_with(&root, true, false, &["nope.md".to_string()]).is_err());
        let env = Env {
            runs_dir: root.join("runs"),
        };
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let d = root.to_str().unwrap();
        assert_eq!(
            init_cmd(&env, &a(&[d, "--accept", "AGENTS.md"]))
                .unwrap_err()
                .code,
            2
        );
        assert_eq!(
            init_cmd(&env, &a(&[d, "--update", "--accept"]))
                .unwrap_err()
                .code,
            2
        );
        assert!(init_cmd(&env, &a(&[d, "--update", "--accept", "AGENTS.md"])).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Files the lock holds but the template dropped are one count line, not a
    /// line each, so a dry run's real conflicts stay visible.
    #[test]
    fn update_dry_run_counts_removed_upstream_files_in_one_line() {
        let root = scratch("removed-upstream");
        run(&root, false, false).unwrap();
        let p = lock_path(&root);
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        for n in ["old/a.md", "old/b.md", "old/c.md"] {
            v["files"][n] = serde_json::json!({"md5": "x", "mode": 420});
        }
        std::fs::write(&p, v.to_string()).unwrap();
        set_lock_md5(&root, "AGENTS.md", &md5_hex(b"older template\n"));
        std::fs::write(root.join("AGENTS.md"), "mine\n").unwrap();
        let (r, c) = run(&root, true, true).unwrap();
        assert_eq!(c, 1, "{r}");
        assert!(r.contains("conflict AGENTS.md"), "{r}");
        assert!(
            r.contains("3 files removed upstream, kept: old/a.md, old/b.md, old/c.md"),
            "{r}"
        );
        assert!(!r.contains("removed upstream old/"), "{r}");
        assert_eq!(r.matches("removed upstream").count(), 1, "{r}");
        for n in ["old/a.md", "old/b.md", "old/c.md"] {
            assert_eq!(r.matches(n).count(), 1, "{n} once: {r}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// More than five kept files: five names, then "+N more".
    #[test]
    fn update_removed_upstream_names_are_capped() {
        let root = scratch("removed-upstream-cap");
        run(&root, false, false).unwrap();
        let p = lock_path(&root);
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        for i in 1..=7 {
            v["files"][format!("old/f{i}.md")] = serde_json::json!({"md5": "x", "mode": 420});
        }
        std::fs::write(&p, v.to_string()).unwrap();
        let (r, _) = run(&root, true, true).unwrap();
        assert!(
            r.contains(
                "7 files removed upstream, kept: old/f1.md, old/f2.md, old/f3.md, old/f4.md, old/f5.md, +2 more"
            ),
            "{r}"
        );
        assert!(!r.contains("old/f6.md"), "{r}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
