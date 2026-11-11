//! `quaqua init` boots a workspace from the template embedded in the binary:
//! the private dirs (`memory/`, `projects/`, `raw/`, `runs/`, `reports/`), the
//! managed template files, the seed files, `.quaqua/template.lock`, git hooks
//! on a repo target, and the CoS pane tag.
//!
//! `quaqua init [<dir>] [--update] [--dry-run] [--git-hooks <repo>]`:
//! - `<dir>` defaults to [`workspace_root`](crate::workspace::workspace_root).
//!   Nothing but the target is read from disk — the template is embedded
//!   (`build.rs` → `$OUT_DIR/template_table.rs`), so no checkout is needed.
//! - plain `init` writes what is missing and never touches an existing file.
//! - `--update` reconciles managed files against the lock (`.quaqua/template.lock`):
//!   missing → write; already equal → record; untouched since the lock → overwrite;
//!   changed locally (or no lock entry) → never overwrite — save the new template
//!   to `.quaqua/pending/<path>` and exit 3.
//! - `--dry-run` prints the decisions and writes nothing.
//! - `--git-hooks <repo>` installs only the embedded hooks (portable-paths
//!   pre-commit and the worker pre-push) into an existing git repo (no
//!   workspace sync, working tree untouched).
//! - `worker spawn` installs only the worker push guard (`pre-push`); the
//!   portable-paths pre-commit stays an explicit `init` opt-in.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::hash::md5_hex;
use crate::herdr::{Herdr, HerdrOps};
use crate::memory;
use crate::workspace::workspace_root;

const INSTALL_LINE: &str = "cargo install --path <checkout>/crates/quaqua --locked";

/// gitignored personal-state dirs (AGENTS.md '## Memory').
const PRIVATE_DIRS: &[&str] = &["memory", "projects", "raw", "runs", "reports"];

/// Seed files are copied to a workspace path that differs from their template
/// name; the source names live in `templates/seed/` (flat: the `.gitignore`
/// patterns `memory/`, `projects/` match at any depth — `rules/_all/git-hygiene.md`).
const SEED_TARGETS: &[(&str, &str)] = &[
    ("memory-rules.md", "memory/rules.md"),
    ("projects-INDEX.md", "projects/INDEX.md"),
    ("projects-_template.md", "projects/_template.md"),
    ("USER.example.md", "USER.md"),
];

const PRE_COMMIT_TEMPLATE: &str = r#"#!/usr/bin/env bash
# pre-commit — rules/_all/portable-paths.md: reject a commit whose ADDED lines
# carry a machine-specific absolute path (/home/<name>/…, /Users/<name>/…).
# The repo syncs across machines, so committed scripts, docs, settings and run
# reports must use repo-relative paths, ~/, $HOME or $(dirname "$0").
#
# Scope: only lines this commit adds. Existing tracked content is never
# re-judged — a checkout that already violates the rule still commits, which
# keeps the hook from bricking a legacy tree. A rename-with-modification adds
# its content, so a moved file is re-checked under its new name; a pure rename
# adds nothing and passes. Binary diffs carry no lines and pass.
#
# Allowlist: rules/_all/portable-paths.md (it names the pattern) and tests/**
# (fixtures must spell real machine paths). Excluded via pathspec, so the
# allowlist is exact — "tests-helper.sh" is still checked.
#
# Heuristic guardrail, like scripts/hooks/herdr-gate.sh: not a security
# boundary. Known holes — a user component outside [A-Za-z0-9._-], a path that
# only appears through a file marked `-diff` in .gitattributes (git then calls
# it binary), and commits made with --no-verify.
#
# The diff is parsed rather than diffed per file: `git diff -- <path>` re-adds
# every line when a rename is detected, which would re-judge untouched content.
# `--no-prefix` + `core.quotePath=false` make the "+++" line the literal path
# (a trailing TAB is added when the name contains a space, hence the strip).
#
# Installed by `quaqua init` via `core.hooksPath`.
# Exit 1 with each offending file:line on stderr; 0 when clean.
set -uo pipefail

path_re='/(home|Users)/[A-Za-z0-9._-]+'

diff=$(git -c core.quotePath=false diff --cached -U0 --no-prefix --no-ext-diff \
         --no-textconv --diff-filter=ACMR -- . \
         ':(exclude,literal)tests' \
         ':(exclude,literal)rules/_all/portable-paths.md' || true)
[[ -n $diff ]] || exit 0

bad=0
cur=
while IFS= read -r line; do
  case $line in
    '+++ '*)
      cur=${line#+++ }
      cur=${cur%$'\t'}
      cur=${cur% }
      [[ $cur == /dev/null ]] && cur=
      ;;
    '+'*)
      [[ -n $cur ]] || continue
      if [[ $line =~ $path_re ]]; then
        printf '%s: %s\n' "$cur" "${line#+}" >&2
        bad=1
      fi
      ;;
  esac
done <<<"$diff"

(( bad )) || exit 0
{
  echo "pre-commit: machine-specific absolute path in the added lines above."
  echo "Use a repo-relative path, ~/, \$HOME or \$(dirname \"\$0\") instead"
  echo "— rules/_all/portable-paths.md"
} >&2
exit 1
"#;

const PRE_PUSH_TEMPLATE: &str = r#"#!/usr/bin/env bash
# pre-push — a worker must never push: pushes and merges belong to the
# Principal (issue #4). `quaqua worker spawn` exports QUAQUA_OWNER into the
# pane, so every process the worker starts carries it, git included. Run the
# push from a shell without QUAQUA_OWNER (the Lead/CoS) and it passes.
#
# Installed by `quaqua worker spawn` (push guard only, into the repo's
# effective hooks dir) and by `quaqua init` (beside the portable-paths hook).
if [[ -n "${QUAQUA_OWNER-}" ]]; then
  echo "pre-push: refused — QUAQUA_OWNER='$QUAQUA_OWNER' marks a worker; workers never push." >&2
  echo "Hand the branch to the Lead; it merges via the queue." >&2
  exit 1
fi
exit 0
"#;

/// One embedded template file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Path relative to the template subdir (`workspace/` or `seed/`).
    pub path: &'static str,
    pub kind: Kind,
    /// Permission bits of the source file (`0o755` stays executable).
    pub mode: u32,
    pub bytes: &'static [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Mirrors the workspace root; reconciled by `--update`.
    Managed,
    /// Copied only when missing; never updated.
    Seed,
}

// `ENTRIES` (managed + seed files, in path order) and `ENGINE`
// (`<CARGO_PKG_VERSION>+<short sha>`) written by build.rs.
include!(concat!(env!("OUT_DIR"), "/template_table.rs"));

/// `.quaqua/template.lock`: the engine that wrote the workspace and the hash of
/// every template file as last written. md5 is change detection, not security.
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

    fn save(&self, root: &Path) -> Result<(), String> {
        let path = lock_path(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("init: cannot create {}: {e}", parent.display()))?;
        }
        let mut lock = Lock {
            engine: self.engine.clone(),
            files: self.files.clone(),
        };
        lock.engine = ENGINE.to_string();
        let body = serde_json::to_string_pretty(&lock).map_err(|e| format!("init: {e}"))?;
        std::fs::write(&path, format!("{body}\n"))
            .map_err(|e| format!("init: cannot write {}: {e}", path.display()))
    }
}

fn lock_path(root: &Path) -> PathBuf {
    root.join(".quaqua/template.lock")
}

/// Entry point for `quaqua init [<dir>] [--update] [--dry-run]`.
/// Returns the process exit code: `0` ok, `3` at least one `--update` conflict.
pub fn run(target: Option<&Path>, update: bool, dry_run: bool) -> Result<i32, String> {
    let entries = template_entries()?;
    let root = resolve_target(target, dry_run)?;
    let mut lock = Lock::load(&root)?;

    if update {
        run_update(&root, &entries, &mut lock, dry_run)
    } else {
        run_init(&root, &entries, &mut lock, dry_run)?;
        Ok(0)
    }
}

/// Plain `init`: create what is missing, never touch an existing file.
fn run_init(root: &Path, entries: &[Entry], lock: &mut Lock, dry_run: bool) -> Result<(), String> {
    for d in PRIVATE_DIRS {
        let p = root.join(d);
        if p.is_dir() {
            continue;
        }
        if dry_run {
            println!("would create {d}/");
            continue;
        }
        std::fs::create_dir_all(&p).map_err(|e| format!("init: cannot create {d}/: {e}"))?;
        println!("created {d}/");
    }

    for e in entries {
        let rel = target_path(e);
        let dst = root.join(&rel);
        let want = md5_hex(e.bytes);
        match md5_file(&dst)? {
            None => {
                if dry_run {
                    println!("create {rel}");
                    continue;
                }
                write_file(&dst, e.bytes, e.mode)?;
                println!("created {rel}");
                lock.files.insert(
                    rel,
                    LockEntry {
                        md5: want,
                        mode: e.mode,
                    },
                );
            }
            Some(have) if have == want => {
                // already current — record it, restore the template mode
                if !dry_run {
                    apply_mode(&dst, e.mode)?;
                }
                lock.files.insert(
                    rel,
                    LockEntry {
                        md5: want,
                        mode: e.mode,
                    },
                );
            }
            // Existing and different: left byte-for-byte alone *and* left out of
            // the lock, so the first `--update` sees "no lock entry, W != T" and
            // reports a conflict instead of silently overwriting a workspace
            // that predates the template.
            Some(_) => {}
        }
    }

    install_memory_hooks(root, lock, dry_run)?;
    if dry_run {
        println!("dry run: nothing written");
        return Ok(());
    }
    wire_hooks(root)?;
    lock.save(root)?;
    tag_cos_pane(root);
    println!("workspace ready in {}", root.display());
    println!("{INSTALL_LINE}");
    Ok(())
}

/// `--update`: the lock table from the plan's "init --update rule".
fn run_update(
    root: &Path,
    entries: &[Entry],
    lock: &mut Lock,
    dry_run: bool,
) -> Result<i32, String> {
    let mut conflicts = 0usize;
    let mut targets: BTreeSet<String> = BTreeSet::new();

    for e in entries {
        let rel = target_path(e);
        targets.insert(rel.clone());
        let dst = root.join(&rel);
        let want = md5_hex(e.bytes);
        let have = md5_file(&dst)?;

        // Seed files: copied only when missing, never updated.
        if e.kind == Kind::Seed {
            match have {
                None => {
                    if dry_run {
                        println!("create {rel} (seed)");
                        continue;
                    }
                    write_file(&dst, e.bytes, e.mode)?;
                    println!("created {rel} (seed)");
                    lock.files.insert(
                        rel,
                        LockEntry {
                            md5: want,
                            mode: e.mode,
                        },
                    );
                }
                Some(h) if h == want => {
                    lock.files.insert(
                        rel,
                        LockEntry {
                            md5: want,
                            mode: e.mode,
                        },
                    );
                }
                Some(_) => println!("keep {rel} (seed, never updated)"),
            }
            continue;
        }

        let locked = lock.files.get(&rel).map(|x| x.md5.clone());

        match have {
            // row 1: missing in the workspace -> write T, record
            None => {
                if dry_run {
                    println!("write {rel}");
                    continue;
                }
                write_file(&dst, e.bytes, e.mode)?;
                println!("write {rel}");
                lock.files.insert(
                    rel,
                    LockEntry {
                        md5: want,
                        mode: e.mode,
                    },
                );
            }
            // row 2: W == T -> record only
            Some(have) if have == want => {
                if !dry_run {
                    apply_mode(&dst, e.mode)?;
                }
                println!("record {rel}");
                lock.files.insert(
                    rel,
                    LockEntry {
                        md5: want,
                        mode: e.mode,
                    },
                );
            }
            // row 3: W == L, T != L (untouched locally) -> overwrite with T
            Some(have) if Some(&have) == locked.as_ref() => {
                if dry_run {
                    println!("overwrite {rel}");
                    continue;
                }
                write_file(&dst, e.bytes, e.mode)?;
                println!("overwrite {rel}");
                lock.files.insert(
                    rel,
                    LockEntry {
                        md5: want,
                        mode: e.mode,
                    },
                );
            }
            // row 4: changed locally, or no lock entry and W != T -> never
            // overwrite: pending copy + diff, keep L
            Some(_) => {
                conflicts += 1;
                let pending_rel = format!(".quaqua/pending/{rel}");
                if dry_run {
                    println!("conflict {rel} -> {pending_rel}");
                    continue;
                }
                let pending = root.join(&pending_rel);
                write_file(&pending, e.bytes, e.mode)?;
                println!("conflict {rel} -> {pending_rel}");
                print_diff(&dst, &pending);
            }
        }
    }

    // row 5: in the lock, gone from the template -> never delete
    for rel in lock.files.keys() {
        if !targets.contains(rel) {
            println!("removed upstream {rel} (kept)");
        }
    }

    install_memory_hooks(root, lock, dry_run)?;

    if !dry_run {
        lock.save(root)?;
    } else {
        println!("dry run: nothing written");
    }

    if conflicts > 0 {
        println!("{conflicts} conflicts — see .quaqua/pending/");
        Ok(3)
    } else {
        Ok(0)
    }
}

/// The workspace-relative destination of a template entry.
fn target_path(e: &Entry) -> String {
    if e.kind == Kind::Seed {
        if let Some((_, dst)) = SEED_TARGETS.iter().find(|(src, _)| *src == e.path) {
            return (*dst).to_string();
        }
    }
    e.path.to_string()
}

/// The template list: the embedded table, or — for tests — a fixture tree that
/// mirrors `<repo>/templates/{workspace,seed}`.
///
/// Why an env override and not a second build: the integration tests drive the
/// built binary, so a template has to be injectable at run time. `QUAQUA_TEMPLATE_DIR`
/// is an explicit absolute path (no cwd ancestors, no shim guess), unset in every
/// normal run, and a directory with neither `workspace/` nor `seed/` fails closed.
/// It exercises the same `Entry` list and the same update algorithm as the
/// embedded table instead of duplicating the rule in the tests.
fn template_entries() -> Result<Vec<Entry>, String> {
    if let Some(dir) = std::env::var_os("QUAQUA_TEMPLATE_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return load_fixture(&dir);
        }
    }
    if ENTRIES.is_empty() {
        return Err(
            "init: this binary has no embedded template (templates/ was missing at build time)"
                .to_string(),
        );
    }
    Ok(ENTRIES.to_vec())
}

fn load_fixture(dir: &Path) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    for (sub, kind) in [("workspace", Kind::Managed), ("seed", Kind::Seed)] {
        let base = dir.join(sub);
        if !base.is_dir() {
            continue;
        }
        load_dir(&base, &base, kind, &mut out)?;
    }
    if out.is_empty() {
        return Err(format!(
            "init: QUAQUA_TEMPLATE_DIR={} holds no workspace/ or seed/ files",
            dir.display()
        ));
    }
    out.sort_by(|a, b| a.path.cmp(b.path));
    Ok(out)
}

fn load_dir(base: &Path, dir: &Path, kind: Kind, out: &mut Vec<Entry>) -> Result<(), String> {
    let rd =
        std::fs::read_dir(dir).map_err(|e| format!("init: cannot read {}: {e}", dir.display()))?;
    for ent in rd {
        let path = ent
            .map_err(|e| format!("init: cannot read {}: {e}", dir.display()))?
            .path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name == ".git" || name == ".DS_Store" {
            continue;
        }
        let meta = std::fs::metadata(&path)
            .map_err(|e| format!("init: cannot stat {}: {e}", path.display()))?;
        if meta.is_dir() {
            load_dir(base, &path, kind, out)?;
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(base)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or(name);
        let bytes = std::fs::read(&path)
            .map_err(|e| format!("init: cannot read {}: {e}", path.display()))?;
        // The process is short-lived (one CLI run, one test binary): leaking the
        // fixture keeps a single `Entry` shape for both sources.
        let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        let rel: &'static str = Box::leak(rel.into_boxed_str());
        out.push(Entry {
            path: rel,
            kind,
            mode: mode_of(&meta),
            bytes,
        });
    }
    Ok(())
}

/// `<dir>` (relative paths resolve against the cwd) or the workspace root.
fn resolve_target(target: Option<&Path>, dry_run: bool) -> Result<PathBuf, String> {
    let raw = match target {
        Some(p) if !p.as_os_str().is_empty() => {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir()
                    .map_err(|e| format!("init: cannot read the cwd: {e}"))?
                    .join(p)
            }
        }
        _ => workspace_root(),
    };
    if raw.is_file() {
        return Err(format!(
            "init: {} is a file, not a directory",
            raw.display()
        ));
    }
    if !dry_run {
        std::fs::create_dir_all(&raw)
            .map_err(|e| format!("init: cannot create {}: {e}", raw.display()))?;
    }
    Ok(std::fs::canonicalize(&raw).unwrap_or(raw))
}

fn md5_file(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(md5_hex(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("init: cannot read {}: {e}", path.display())),
    }
}

fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("init: cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes)
        .map_err(|e| format!("init: cannot write {}: {e}", path.display()))?;
    apply_mode(path, mode)
}

fn apply_mode(path: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("init: cannot chmod {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u32 {
    0o644
}

/// Show the local file against the pending template, best effort: a missing
/// `diff` binary must not fail the sync (the pending copy is the contract).
fn print_diff(local: &Path, pending: &Path) {
    let _ = Command::new("diff")
        .arg("-u")
        .arg(local)
        .arg(pending)
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .status();
}

/// Wire `core.hooksPath` -> `<git-dir>/quaqua-hooks` on a real git repo, generating
/// the embedded `pre-commit` and `pre-push` hooks if missing or refreshing them.
/// An existing different value (local or global) is left in place with a warning.
/// If `core.hooksPath` is legacy `<root>/scripts/hooks`, it is migrated to the new dir.
/// A failed write fails the run, like the bash script's `set -e`.
fn install_hook(hooks_dir: &Path, name: &str, template: &str) -> Result<(), String> {
    let hook_file = hooks_dir.join(name);
    let existed = hook_file.exists();
    if existed && std::fs::read_to_string(&hook_file).unwrap_or_default() == template {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&hook_file, std::fs::Permissions::from_mode(0o755));
        }
        return Ok(());
    }
    std::fs::write(&hook_file, template)
        .map_err(|e| format!("init-workspace: cannot write {}: {e}", hook_file.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_file, std::fs::Permissions::from_mode(0o755)).map_err(
            |e| {
                format!(
                    "init-workspace: failed to chmod {}: {e}",
                    hook_file.display()
                )
            },
        )?;
    }
    if existed {
        println!("hooks: refreshed {}", hook_file.display());
    }
    Ok(())
}

/// `worker spawn`: install ONLY the worker push guard (`pre-push`) on the
/// target repo. The portable-paths `pre-commit` stays an explicit
/// `quaqua init` opt-in — a spawn must not police the repo's commits.
///
/// Where the guard goes is git's own answer, never a config rewrite:
/// - `core.hooksPath` unset → git's default `<common-git-dir>/hooks`, which
///   every linked worktree shares, so a push from the worker's worktree is
///   guarded without touching any config;
/// - `core.hooksPath` pointing at the quaqua-owned dir → that dir.
///
/// A foreign `core.hooksPath` and an existing non-quaqua `pre-push` are left
/// untouched with a warning: spawn never redirects or overwrites a hook setup
/// it did not create. `Ok(())` when there is nothing safe to do.
pub fn ensure_worker_push_guard(root: &Path) -> Result<(), String> {
    if !git_ok(root, &["rev-parse", "--git-dir"]) {
        return Ok(());
    }

    let Some(hooks_dir) = worker_hooks_dir(root) else {
        return Ok(());
    };

    let hook_file = hooks_dir.join("pre-push");
    match std::fs::read_to_string(&hook_file) {
        Ok(body) if body == PRE_PUSH_TEMPLATE => {
            apply_mode(&hook_file, 0o755)?;
            return Ok(());
        }
        Ok(_) => {
            eprintln!(
                "worker: {} already exists and is not the quaqua push guard — left unchanged",
                hook_file.display()
            );
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            eprintln!(
                "worker: cannot read {}: {e} — push guard not installed",
                hook_file.display()
            );
            return Ok(());
        }
    }

    write_file(&hook_file, PRE_PUSH_TEMPLATE.as_bytes(), 0o755)
        .map_err(|e| format!("worker: cannot install the push guard: {e}"))?;
    println!("worker: push guard installed at {}", hook_file.display());
    Ok(())
}

/// The directory `ensure_worker_push_guard` may write to, or `None` with a
/// warning when the repo's hook setup belongs to someone else. `git rev-parse
/// --git-path hooks` is the single source of truth: it resolves
/// `core.hooksPath` when set and otherwise the shared `<common-git-dir>/hooks`
/// (linked worktrees included).
fn worker_hooks_dir(root: &Path) -> Option<PathBuf> {
    let effective = git_hooks_path(root);
    if let Some(have) = git_output(root, &["config", "--get", "core.hooksPath"]) {
        if !have.is_empty() {
            let ours = quaqua_hooks_dir(root);
            let owned = hooks_path_is_ours(&have, root, &ours)
                || effective.as_deref() == Some(ours.as_path());
            if !owned {
                eprintln!(
                    "worker: core.hooksPath is already set to '{have}' — left untouched; \
                     the worker push guard was not installed"
                );
                return None;
            }
        }
    }
    match effective {
        Some(dir) => Some(dir),
        None => {
            eprintln!(
                "worker: git cannot name the repository's hooks directory — push guard not installed"
            );
            None
        }
    }
}

/// The hooks directory git will actually run: `core.hooksPath` when set, else
/// `<common-git-dir>/hooks` (shared by linked worktrees). `--path-format` needs
/// git 2.31+; the plain form is retried and anchored at the repo root.
fn git_hooks_path(root: &Path) -> Option<PathBuf> {
    git_path(
        root,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )
    .or_else(|| git_path(root, &["rev-parse", "--git-path", "hooks"]))
}

/// A `git rev-parse` answer that names a path. Empty output (a git that cannot
/// answer, as in the test stubs) is not a path, and a relative answer is
/// anchored at the repo root — never at the process cwd.
fn git_path(root: &Path, args: &[&str]) -> Option<PathBuf> {
    let out = git_output(root, args)?;
    if out.is_empty() {
        return None;
    }
    let path = PathBuf::from(out);
    Some(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

fn wire_hooks(root: &Path) -> Result<(), String> {
    if !git_ok(root, &["rev-parse", "--git-dir"]) {
        return Ok(());
    }

    let hooks_dir = quaqua_hooks_dir(root);

    std::fs::create_dir_all(&hooks_dir)
        .map_err(|e| format!("init-workspace: cannot create {}: {e}", hooks_dir.display()))?;

    for (name, template) in [
        ("pre-commit", PRE_COMMIT_TEMPLATE),
        ("pre-push", PRE_PUSH_TEMPLATE),
    ] {
        install_hook(&hooks_dir, name, template)?;
    }

    let want_str = hooks_dir.to_string_lossy().to_string();

    match git_output(root, &["config", "--get", "core.hooksPath"]) {
        Some(have) if hooks_path_is_ours(&have, root, &hooks_dir) => {
            // already ours — silent, like the rest of this command
        }
        Some(have) if hooks_path_is_legacy(&have, root) => {
            if !git_ok(root, &["config", "core.hooksPath", &want_str]) {
                return Err(format!(
                    "init-workspace: failed to set core.hooksPath to {want_str}"
                ));
            }
            println!("hooks: migrated core.hooksPath from '{have}' to {want_str}");
        }
        Some(have) if !have.is_empty() => {
            eprintln!(
                "init-workspace: core.hooksPath already set to '{have}' — left unchanged \
                 (unset it to use {want_str})"
            );
        }
        _ => {
            if !git_ok(root, &["config", "core.hooksPath", &want_str]) {
                return Err(format!(
                    "init-workspace: failed to set core.hooksPath to {want_str}"
                ));
            }
            println!("hooks: core.hooksPath={want_str}");
        }
    }
    Ok(())
}

/// The workspace settings file the memory hooks are merged into.
const SETTINGS_REL: &str = ".claude/settings.json";

/// The `PreCompact` command `quaqua init` installs (G4): the pre-compaction
/// snapshot. Claude Code appends a PreCompact hook's stdout to the compact
/// instructions, so this is a steer, not a block.
///
/// The `[ -x ]` guard keeps a workspace whose script is missing silent instead
/// of printing an error on every compaction; `${CLAUDE_PROJECT_DIR:-.}` keeps
/// the entry working when the variable is unset.
pub const COMPACT_PRE_COMMAND: &str = "d=${CLAUDE_PROJECT_DIR:-.}; [ -x \"$d/scripts/hooks/memory-compact.sh\" ] || exit 0; exec \"$d/scripts/hooks/memory-compact.sh\" --pre-compact";

/// The `SessionStart` command `quaqua init` installs: injects the newest
/// pre-compaction snapshot after a compaction, silent otherwise.
pub const COMPACT_SESSION_COMMAND: &str = "d=${CLAUDE_PROJECT_DIR:-.}; [ -x \"$d/scripts/hooks/memory-compact.sh\" ] || exit 0; exec \"$d/scripts/hooks/memory-compact.sh\" --session-start";

/// Install the CoS memory hooks into `<root>/.claude/settings.json`.
///
/// This is the one home for the four hook commands ([`memory::HOOK_PRE_COMMAND`],
/// [`memory::HOOK_BATCH_COMMAND`], [`COMPACT_PRE_COMMAND`],
/// [`COMPACT_SESSION_COMMAND`]): the template ships the rest of the settings
/// file, and this step adds what the engine owns. Idempotent — a handler that
/// already carries the command is left alone, so re-running `init` writes
/// nothing and a hand-edited settings file keeps its other hooks.
///
/// The `SessionStart` handler joins the group the template already gives the
/// herdr-gate hook (no matcher); the script decides on `source` itself.
///
/// A settings file that is not a JSON object is reported and left untouched:
/// `init` is a boot step, not a repair step, and the gate is a guardrail.
fn install_memory_hooks(root: &Path, lock: &mut Lock, dry_run: bool) -> Result<(), String> {
    let path = root.join(SETTINGS_REL);
    let mut doc: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(body) if body.trim().is_empty() => serde_json::json!({}),
        Ok(body) => match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "init: {} is not valid JSON ({e}) — memory hooks not installed",
                    path.display()
                );
                return Ok(());
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => {
            eprintln!(
                "init: cannot read {} ({e}) — memory hooks not installed",
                path.display()
            );
            return Ok(());
        }
    };
    if !doc.is_object() {
        eprintln!(
            "init: {} is not a JSON object — memory hooks not installed",
            path.display()
        );
        return Ok(());
    }

    let mut changed = false;
    changed |= ensure_hook(
        &mut doc,
        "PreToolUse",
        Some(memory::HOOK_PRE_MATCHER),
        memory::HOOK_PRE_COMMAND,
    );
    changed |= ensure_hook(&mut doc, "PostToolBatch", None, memory::HOOK_BATCH_COMMAND);
    changed |= ensure_hook(&mut doc, "PreCompact", None, COMPACT_PRE_COMMAND);
    changed |= ensure_hook(&mut doc, "SessionStart", None, COMPACT_SESSION_COMMAND);
    if !changed {
        return Ok(());
    }
    if dry_run {
        println!("would install memory hooks in {SETTINGS_REL}");
        return Ok(());
    }

    let mut body = serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("init: cannot serialize {}: {e}", path.display()))?;
    body.push('\n');
    write_file(&path, body.as_bytes(), 0o644)?;
    println!("memory hooks: installed in {SETTINGS_REL}");
    // The lock records what quaqua last wrote. Recording the merged hash keeps
    // the next `--update` on the "untouched locally" row, so it reconciles the
    // file instead of reporting a conflict against the shipped template.
    if let Some(entry) = lock.files.get_mut(SETTINGS_REL) {
        if let Ok(Some(md5)) = md5_file(&path) {
            entry.md5 = md5;
        }
    }
    Ok(())
}

/// True when some handler under `groups` already runs `command`.
fn has_command(groups: &[serde_json::Value], command: &str) -> bool {
    groups.iter().any(|g| {
        g.get("hooks")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|hs| {
                hs.iter()
                    .any(|h| h.get("command").and_then(serde_json::Value::as_str) == Some(command))
            })
    })
}

/// Ensure `hooks[event]` carries a command handler running `command`, adding it
/// to the group with the same matcher (or a new group) only when absent.
/// Returns true when the document changed.
fn ensure_hook(
    doc: &mut serde_json::Value,
    event: &str,
    matcher: Option<&str>,
    command: &str,
) -> bool {
    use serde_json::{Map, Value};

    let Some(root) = doc.as_object_mut() else {
        return false;
    };
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        return false;
    };
    let groups = hooks
        .entry(event)
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(groups) = groups.as_array_mut() else {
        return false;
    };
    if has_command(groups, command) {
        return false;
    }

    let handler = serde_json::json!({ "type": "command", "command": command });
    let slot = groups.iter_mut().find(|g| match matcher {
        Some(m) => g.get("matcher").and_then(Value::as_str) == Some(m),
        None => g.get("matcher").is_none(),
    });
    match slot {
        Some(group) => match group.get_mut("hooks").and_then(Value::as_array_mut) {
            Some(handlers) => handlers.push(handler),
            None => group["hooks"] = Value::Array(vec![handler]),
        },
        None => {
            let mut group = Map::new();
            if let Some(m) = matcher {
                group.insert("matcher".to_string(), Value::String(m.to_string()));
            }
            group.insert("hooks".to_string(), Value::Array(vec![handler]));
            groups.push(Value::Object(group));
        }
    }
    true
}

/// `quaqua init --git-hooks <repo>`: install the embedded portable-paths
/// pre-commit and worker pre-push into an existing git repo and stop there —
/// no workspace dirs, no template sync, no lock, no CoS tag. The repo's working
/// tree is untouched; only `<git-dir>/quaqua-hooks/{pre-commit,pre-push}` and
/// `core.hooksPath` change.
///
/// A repo whose `core.hooksPath` points somewhere else is refused untouched;
/// re-running against a repo we already own is a no-op.
pub fn run_git_hooks(repo: &Path) -> Result<i32, String> {
    let repo = std::fs::canonicalize(repo)
        .map_err(|e| format!("init --git-hooks: cannot resolve {}: {e}", repo.display()))?;
    if !repo.is_dir() {
        return Err(format!(
            "init --git-hooks: {} is not a directory",
            repo.display()
        ));
    }
    if !git_ok(&repo, &["rev-parse", "--git-dir"]) {
        return Err(format!(
            "init --git-hooks: {} is not a git repository",
            repo.display()
        ));
    }

    let hooks_dir = quaqua_hooks_dir(&repo);
    if let Some(have) = git_output(&repo, &["config", "--get", "core.hooksPath"]) {
        if !have.is_empty() && !hooks_path_is_ours(&have, &repo, &hooks_dir) {
            return Err(format!(
                "init --git-hooks: {} already sets core.hooksPath='{have}' — refusing to \
                 overwrite it; unset it to install the quaqua hook instead",
                repo.display()
            ));
        }
    }

    wire_hooks(&repo)?;
    println!("git hooks: {} -> {}", repo.display(), hooks_dir.display());
    Ok(0)
}

/// The directory quaqua owns for git hooks: `<absolute git dir>/quaqua-hooks`.
/// Works for a normal checkout, a linked worktree, and a path inside a repo.
fn quaqua_hooks_dir(root: &Path) -> PathBuf {
    git_path(root, &["rev-parse", "--absolute-git-dir"])
        .map(|p| p.join("quaqua-hooks"))
        .unwrap_or_else(|| root.join(".git/quaqua-hooks"))
}

/// True when a `core.hooksPath` value already points at `hooks_dir` — the
/// absolute path quaqua writes, the legacy relative form, or an equivalent
/// spelling of either.
fn hooks_path_is_ours(have: &str, root: &Path, hooks_dir: &Path) -> bool {
    let want = hooks_dir.to_string_lossy();
    have == want.as_ref()
        || have == format!("{want}/")
        || have == ".git/quaqua-hooks"
        || have == ".git/quaqua-hooks/"
        || Path::new(have) == hooks_dir
        || root.join(have) == hooks_dir
}

/// True when a `core.hooksPath` value is the legacy `<root>/scripts/hooks`
/// location that `quaqua init` migrates to the quaqua-owned directory.
fn hooks_path_is_legacy(have: &str, root: &Path) -> bool {
    let legacy = root.join("scripts/hooks");
    let legacy_str = legacy.to_string_lossy();
    have == legacy_str.as_ref()
        || have == "scripts/hooks"
        || have == format!("{legacy_str}/")
        || Path::new(have) == legacy
        || root.join(have) == legacy
}

fn git_ok(root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// herdr-quaqua: tag the CoS orchestrator pane (R5) — but ONLY that pane. The
/// caller's pane is checked against herdr, not assumed: the CoS is the unnamed
/// Claude agent in this checkout. Anything else (another harness, a named
/// agent, another directory, a herdr that cannot answer) fails closed and is
/// left untagged.
fn tag_cos_pane(root: &Path) {
    if !herdr_available() {
        return;
    }
    let cos_pane = std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            herdr_json(&["pane", "current"]).and_then(|v| {
                v.pointer("/result/pane/pane_id")
                    .and_then(|p| p.as_str())
                    .map(String::from)
            })
        });
    let Some(pane) = cos_pane else {
        return;
    };
    if !is_cos_pane(&pane, root) {
        return;
    }
    let farm_tok = format!("grid_farm={}", crate::workspace::farm_id_of(root));
    let _ = Herdr::new().pane_report_metadata(
        &pane,
        &[
            crate::cos::COS_ROLE_TOKEN,
            crate::cos::COS_OWNER_TOKEN,
            &farm_tok,
        ],
    );
}

/// is_cos_pane <pane>: true only for the CoS pane itself — a `claude` agent, in
/// this checkout root, not registered in `herdr agent list`.
fn is_cos_pane(pane: &str, root: &Path) -> bool {
    let info = match herdr_json(&["pane", "get", pane]) {
        Some(v) => v,
        None => return false,
    };
    let agent = info
        .pointer("/result/pane/agent")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let cwd = info
        .pointer("/result/pane/cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent != "claude" || cwd.is_empty() {
        return false;
    }
    let real = std::fs::canonicalize(cwd).ok();
    if real.as_deref() != Some(root) {
        return false;
    }
    let list = match herdr_json(&["agent", "list"]) {
        Some(v) => v,
        None => return false,
    };
    let name = list
        .get("result")
        .and_then(|r| r.get("agents"))
        .and_then(|a| a.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|e| e.get("pane_id").and_then(|p| p.as_str()) == Some(pane))
        })
        .and_then(|e| e.get("name").and_then(|n| n.as_str()))
        .unwrap_or("");
    name.is_empty()
}

fn herdr_json(args: &[&str]) -> Option<serde_json::Value> {
    let out = Herdr::new().run(args).ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// `command -v` on the herdr binary the exec sites resolve (see
/// `herdr::bin_path`): `HERDR_BIN_PATH` when set, else the first `herdr` on
/// `PATH` — checked, never invoked (no side effects for a stubbed test
/// `herdr`).
fn herdr_available() -> bool {
    let bin = crate::herdr::bin_path();
    if bin.contains('/') {
        return Path::new(&bin).is_file();
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if !dir.is_empty() && Path::new(dir).join(&bin).is_file() {
                return true;
            }
        }
    }
    false
}
