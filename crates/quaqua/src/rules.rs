//! Rules dump and audience filtering.
//!
//! Rule files live under `<root>/rules/_all/*.md` and `<root>/rules/<repo>/*.md`.
//! A rule file is orchestrator-only if its first line is `<!-- audience: orchestrator -->`.
//! Absent marker means all audiences.

use std::path::{Path, PathBuf};

/// Target audience for rules dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Worker: receives unmarked rules from `rules/_all` and `rules/<repo>`.
    Worker,
    /// Orchestrator / Lead: receives all rules from `rules/_all` and `rules/<repo>`.
    Orchestrator,
}

/// The in-band marker that flags a rule file as orchestrator-only.
pub const ORCHESTRATOR_MARKER: &str = "<!-- audience: orchestrator -->";

/// Check if the given rule file content starts with the orchestrator-only marker.
pub fn is_orchestrator_only(content: &str) -> bool {
    content
        .lines()
        .next()
        .map(|line| line.trim_start_matches('\u{feff}').trim() == ORCHESTRATOR_MARKER)
        .unwrap_or(false)
}

/// Resolve the clean repository name from a repo path or name string,
/// handling trailing slashes, relative/absolute paths, and edge cases.
fn resolve_repo_name(repo: &str) -> &str {
    let clean = repo.trim_end_matches('/');
    if clean.is_empty() {
        return "repo";
    }
    Path::new(clean)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(clean)
}

/// Collect and append markdown rule files from `dir` into `out`,
/// filtering out orchestrator-only files when `audience` is [`Audience::Worker`].
fn append_rules_from_dir(dir: &Path, audience: Audience, out: &mut String) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("md"))
            .collect();
        paths.sort();
        for p in paths {
            if let Ok(c) = std::fs::read_to_string(&p) {
                if audience == Audience::Worker && is_orchestrator_only(&c) {
                    continue;
                }
                out.push_str(&c);
            }
        }
    }
}

/// Dump concatenated rules for the given audience.
///
/// Worker receives `rules/_all` minus orchestrator-only files, plus `rules/<repo>`.
/// Orchestrator receives all `rules/_all` plus `rules/<repo>`.
pub fn dump(root: &Path, repo: &str, audience: Audience) -> String {
    let mut out = String::new();
    let all_rules = root.join("rules/_all");
    append_rules_from_dir(&all_rules, audience, &mut out);

    let repo_name = resolve_repo_name(repo);
    if repo_name != "_all" {
        let repo_rules = root.join("rules").join(repo_name);
        append_rules_from_dir(&repo_rules, audience, &mut out);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_test_dir(tag: &str) -> TestDir {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("quaqua-rules-{tag}-{}-{nanos}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TestDir(dir)
    }

    #[test]
    fn rules_dump_audience() {
        let td = temp_test_dir("audience");
        let root = &td.0;

        let all_dir = root.join("rules/_all");
        let my_repo_dir = root.join("rules/my-repo");
        let other_repo_dir = root.join("rules/other-repo");

        std::fs::create_dir_all(&all_dir).unwrap();
        std::fs::create_dir_all(&my_repo_dir).unwrap();
        std::fs::create_dir_all(&other_repo_dir).unwrap();

        // 1. Unmarked file in rules/_all -> both Worker and Lead see this
        std::fs::write(
            all_dir.join("01-common.md"),
            "# Common Rule\nShared instruction for all.\n",
        )
        .unwrap();

        // 2. Marked file in rules/_all -> only Orchestrator sees this
        std::fs::write(
            all_dir.join("02-orch.md"),
            "<!-- audience: orchestrator -->\n# Orchestrator Rule\nOnly for Lead / CoS.\n",
        )
        .unwrap();

        // 3. Unmarked file in rules/<repo> -> both Worker and Lead for my-repo see this
        std::fs::write(
            my_repo_dir.join("01-repo-common.md"),
            "# Repo Common Rule\nSpecific to my-repo.\n",
        )
        .unwrap();

        // 4. Marked file in rules/<repo> -> only Orchestrator for my-repo sees this
        std::fs::write(
            my_repo_dir.join("02-repo-orch.md"),
            "<!-- audience: orchestrator -->\n# Repo Orch Rule\nLead-only for my-repo.\n",
        )
        .unwrap();

        // 5. File in other-repo -> neither Worker nor Lead for my-repo sees this
        std::fs::write(
            other_repo_dir.join("01-other.md"),
            "# Other Repo Rule\nShould not be leaked.\n",
        )
        .unwrap();

        // --- Verify Worker Audience ---
        let worker_dump = dump(root, "my-repo", Audience::Worker);

        assert!(
            worker_dump.contains("Common Rule"),
            "Worker must see unmarked rules in _all"
        );
        assert!(
            !worker_dump.contains("Orchestrator Rule"),
            "Worker must NOT see marked orchestrator rules in _all"
        );
        assert!(
            !worker_dump.contains(ORCHESTRATOR_MARKER),
            "Worker must not see orchestrator marker"
        );
        assert!(
            worker_dump.contains("Repo Common Rule"),
            "Worker must see unmarked rules in repo"
        );
        assert!(
            !worker_dump.contains("Repo Orch Rule"),
            "Worker must NOT see marked orchestrator rules in repo"
        );
        assert!(
            !worker_dump.contains("Other Repo Rule"),
            "Worker must not see other repo rules"
        );

        // --- Verify Orchestrator Audience ---
        let lead_dump = dump(root, "my-repo", Audience::Orchestrator);

        assert!(
            lead_dump.contains("Common Rule"),
            "Lead must see unmarked rules in _all"
        );
        assert!(
            lead_dump.contains("Orchestrator Rule"),
            "Lead must see marked orchestrator rules in _all"
        );
        assert!(
            lead_dump.contains("Repo Common Rule"),
            "Lead must see unmarked rules in repo"
        );
        assert!(
            lead_dump.contains("Repo Orch Rule"),
            "Lead must see marked orchestrator rules in repo"
        );
        assert!(
            !lead_dump.contains("Other Repo Rule"),
            "Lead must not see other repo rules"
        );

        // --- Verify Path & Edge Cases ---
        // Trailing slash resolution
        let worker_dump_slash = dump(root, "/some/path/my-repo/", Audience::Worker);
        assert_eq!(
            worker_dump, worker_dump_slash,
            "Trailing slash in repo path must resolve identically"
        );

        // Unknown repo gets _all rules cleanly
        let unknown_dump = dump(root, "nonexistent-repo", Audience::Worker);
        assert!(unknown_dump.contains("Common Rule"));
        assert!(!unknown_dump.contains("Repo Common Rule"));

        // When repo is _all, it should not duplicate _all rules
        let worker_all = dump(root, "_all", Audience::Worker);
        assert_eq!(worker_all.matches("Common Rule").count(), 1);
    }
}
