//! Git execution helpers and GitOps trait.

use std::path::Path;
use std::process::{Command, Output};

pub trait GitOps {
    fn run_in(&self, dir: &Path, args: &[&str]) -> std::io::Result<Output>;
    fn run(&self, args: &[&str]) -> std::io::Result<Output>;
    fn rev_parse_commit(&self, repo: &Path, base: &str) -> bool;
    fn worktree_list_porcelain(&self, repo: &Path) -> std::io::Result<Vec<String>>;
    fn worktree_add(
        &self,
        repo: &Path,
        branch: &str,
        wt: &Path,
        base: Option<&str>,
    ) -> std::io::Result<Output>;
    fn branch_exists(&self, repo: &Path, branch: &str) -> bool;
    fn worktree_add_existing(
        &self,
        repo: &Path,
        branch: &str,
        wt: &Path,
    ) -> std::io::Result<Output>;
    fn worktree_remove(&self, repo: &Path, wt: &Path) -> std::io::Result<Output>;
    fn remote_list(&self, repo: &Path) -> std::io::Result<Vec<String>>;
    fn is_core_bare(&self, repo: &Path) -> bool;
    fn has_core_worktree(&self, repo: &Path) -> bool;
    fn enable_worktree_config(&self, repo: &Path) -> std::io::Result<Output>;
    fn set_worktree_pushurl_blocked(&self, wt: &Path, remote: &str) -> std::io::Result<()>;
    fn is_git_dir(&self, cwd: &Path) -> bool;
    fn diff_and_status(&self, cwd: &Path) -> (String, String);
}

#[derive(Debug, Clone, Default)]
pub struct Git;

impl Git {
    pub fn new() -> Self {
        Self
    }
}

impl GitOps for Git {
    fn run_in(&self, dir: &Path, args: &[&str]) -> std::io::Result<Output> {
        Command::new("git").arg("-C").arg(dir).args(args).output()
    }

    fn run(&self, args: &[&str]) -> std::io::Result<Output> {
        Command::new("git").args(args).output()
    }

    fn rev_parse_commit(&self, repo: &Path, base: &str) -> bool {
        let commit_ref = format!("{}^{{commit}}", base);
        match self.run_in(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &commit_ref,
            ],
        ) {
            Ok(out) => out.status.success(),
            Err(_) => false,
        }
    }

    fn worktree_list_porcelain(&self, repo: &Path) -> std::io::Result<Vec<String>> {
        let out = self.run_in(repo, &["worktree", "list", "--porcelain"])?;
        if !out.status.success() {
            return Err(std::io::Error::other(
                String::from_utf8_lossy(&out.stderr).to_string(),
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut list = Vec::new();
        for line in text.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                list.push(path.trim().to_string());
            }
        }
        Ok(list)
    }

    fn worktree_add(
        &self,
        repo: &Path,
        branch: &str,
        wt: &Path,
        base: Option<&str>,
    ) -> std::io::Result<Output> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(repo)
            .arg("worktree")
            .arg("add")
            .arg("-b")
            .arg(branch)
            .arg(wt);
        if let Some(b) = base {
            cmd.arg(b);
        }
        cmd.output()
    }

    fn branch_exists(&self, repo: &Path, branch: &str) -> bool {
        let ref_name = format!("refs/heads/{branch}");
        match self.run_in(repo, &["show-ref", "--verify", "--quiet", &ref_name]) {
            Ok(out) => out.status.success(),
            Err(_) => false,
        }
    }

    fn worktree_add_existing(
        &self,
        repo: &Path,
        branch: &str,
        wt: &Path,
    ) -> std::io::Result<Output> {
        // Reuse a branch cleanup-worker.sh kept for the merge (same-name retry):
        // `worktree add -b` would die with "a branch named ... already exists".
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("worktree")
            .arg("add")
            .arg(wt)
            .arg(branch)
            .output()
    }

    fn worktree_remove(&self, repo: &Path, wt: &Path) -> std::io::Result<Output> {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("worktree")
            .arg("remove")
            .arg(wt)
            .output()
    }

    fn remote_list(&self, repo: &Path) -> std::io::Result<Vec<String>> {
        let out = self.run_in(repo, &["remote"])?;
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect())
    }

    fn is_core_bare(&self, repo: &Path) -> bool {
        match self.run_in(repo, &["config", "--local", "--bool", "--get", "core.bare"]) {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout).trim().to_lowercase();
                text == "true"
            }
            Err(_) => false,
        }
    }

    fn has_core_worktree(&self, repo: &Path) -> bool {
        match self.run_in(repo, &["config", "--local", "--list"]) {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                text.lines().any(|l| l.starts_with("core.worktree="))
            }
            Err(_) => false,
        }
    }

    fn enable_worktree_config(&self, repo: &Path) -> std::io::Result<Output> {
        self.run_in(repo, &["config", "extensions.worktreeConfig", "true"])
    }

    fn set_worktree_pushurl_blocked(&self, wt: &Path, remote: &str) -> std::io::Result<()> {
        let key = format!("remote.{}.pushurl", remote);
        let _ = self.run_in(wt, &["config", "--worktree", "--replace-all", &key, ""])?;
        let _ = self.run_in(
            wt,
            &[
                "config",
                "--worktree",
                "--add",
                &key,
                "PUSH DISABLED - task workers never push (AGENTS.md handoff rule)",
            ],
        )?;
        Ok(())
    }

    fn is_git_dir(&self, cwd: &Path) -> bool {
        match self.run_in(cwd, &["rev-parse", "--git-dir"]) {
            Ok(out) => out.status.success(),
            Err(_) => false,
        }
    }

    fn diff_and_status(&self, cwd: &Path) -> (String, String) {
        let st_out = self.run_in(cwd, &["status", "--porcelain=v1"]).ok();
        let diff_out = self.run_in(cwd, &["diff", "HEAD", "--stat"]).ok();
        let short_out = self.run_in(cwd, &["diff", "HEAD", "--shortstat"]).ok();

        let st_text = st_out
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let diff_text = diff_out
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        let combined = format!("{}{}", st_text, diff_text);
        let dh = crate::hash::md5_hex(combined.as_bytes());

        let short = short_out
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let untracked = st_text.lines().filter(|l| l.starts_with("??")).count();

        let mut diffstat = if short.is_empty() {
            "clean".to_string()
        } else {
            short
        };
        if untracked > 0 {
            diffstat.push_str(&format!(", {} untracked", untracked));
        }

        (dh, diffstat)
    }
}
