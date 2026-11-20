//! Git hooks of a workspace repo: the portable-paths pre-commit gate and
//! `quaqua init --git-hooks <repo>`. The hook lives in `<git-dir>/quaqua-hooks` and
//! `core.hooksPath` points at it; a repo that sets `core.hooksPath` elsewhere
//! is refused untouched. The worker pre-push guard is installed by the spawn
//! path, not here.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(super) const PRE_COMMIT_TEMPLATE: &str = r#"#!/usr/bin/env bash
# pre-commit — portable-paths gate: reject a commit whose ADDED lines
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
# Allowlist: tests/** (fixtures must spell real machine paths). Excluded via
# pathspec, so the allowlist is exact — "tests-helper.sh" is still checked.
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
         ':(exclude,literal)tests' || true)
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
  echo "— quaqua portable-paths pre-commit gate"
} >&2
exit 1
"#;

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
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// `<absolute git dir>/quaqua-hooks`: right for a checkout, a linked worktree
/// and a path inside a repo.
fn hooks_dir(root: &Path) -> PathBuf {
    git_output(root, &["rev-parse", "--absolute-git-dir"])
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(p).join("quaqua-hooks"))
        .unwrap_or_else(|| root.join(".git/quaqua-hooks"))
}

/// True when a `core.hooksPath` value already points at `dir`.
fn is_ours(have: &str, root: &Path, dir: &Path) -> bool {
    let want = dir.to_string_lossy();
    have == want.as_ref()
        || have == format!("{want}/")
        || have == ".git/quaqua-hooks"
        || have == ".git/quaqua-hooks/"
        || root.join(have) == dir
}

fn install_hook(dir: &Path, name: &str, body: &str) -> Result<(), String> {
    let path = dir.join(name);
    if std::fs::read_to_string(&path).is_ok_and(|cur| cur == body) {
        super::set_mode(&path, 0o755)?;
        return Ok(());
    }
    super::write_file(&path, body.as_bytes(), 0o755)
}

/// Installs the pre-commit gate into `root`'s repo and points `core.hooksPath`
/// at it. `Ok(None)` when `root` is no git repo; `Ok(Some(line))` otherwise
/// (empty line when nothing changed). A foreign `core.hooksPath` is an error.
pub(super) fn wire(root: &Path) -> Result<Option<String>, String> {
    if !git_ok(root, &["rev-parse", "--git-dir"]) {
        return Ok(None);
    }
    let dir = hooks_dir(root);
    let have = git_output(root, &["config", "--get", "core.hooksPath"]).unwrap_or_default();
    if !have.is_empty() && !is_ours(&have, root, &dir) {
        return Err(format!(
            "core.hooksPath is already '{have}' in {} — left unchanged (unset it to install the hook)",
            root.display()
        ));
    }
    install_hook(&dir, "pre-commit", PRE_COMMIT_TEMPLATE)?;
    if have.is_empty() {
        let want = dir.to_string_lossy().to_string();
        if !git_ok(root, &["config", "core.hooksPath", &want]) {
            return Err(format!("failed to set core.hooksPath to {want}"));
        }
        return Ok(Some(format!("git hooks: core.hooksPath={want}")));
    }
    Ok(Some(String::new()))
}

/// `quaqua init --git-hooks <repo>`: the pre-commit gate only — no template, no lock.
pub fn run_git_hooks(repo: &Path) -> Result<String, String> {
    let repo = std::fs::canonicalize(repo)
        .map_err(|e| format!("init --git-hooks: cannot resolve {}: {e}", repo.display()))?;
    match wire(&repo).map_err(|e| format!("init --git-hooks: {e}"))? {
        None => Err(format!(
            "init --git-hooks: {} is not a git repository",
            repo.display()
        )),
        Some(_) => Ok(format!(
            "git hooks: {} -> {}",
            repo.display(),
            hooks_dir(&repo).display()
        )),
    }
}
