//! The cleanup journal over a temp workspace (never ~/quaqua): raw copy, memory
//! line, idempotent rerun, missing report, watchdog case line.

use std::path::{Path, PathBuf};

use qq_core::memory::journal;

fn ws(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("qq-cj-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let runs = root.join("runs");
    let repo = root.join("code").join("engine");
    for d in [&runs, &root.join("memory"), &root.join("projects"), &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    (root, runs, repo)
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

const REPORT: &str =
    "Outcome: COMPLETE\n## Changes\nx\n## Verify\ny\n## Risks\nnone\n## Lessons\nnone\n";

#[test]
fn cleanup_journal_copies_report_and_appends_line() {
    let (root, runs, repo) = ws("copy");
    std::fs::write(runs.join("sude-thing.md"), REPORT).unwrap();
    let j = journal::cleanup(&runs, &repo, "sude-thing", "abc1234", "2026-11-19");
    assert_eq!(j.raw.as_deref(), Some("2026-11-19-engine-thing.md"));
    assert_eq!(read(&root.join("raw/2026-11-19-engine-thing.md")), REPORT);
    assert_eq!(
        read(&root.join("memory/2026-11-19.md")),
        "- [engine] sude-thing cleanup: COMPLETE abc1234 · raw/2026-11-19-engine-thing.md\n"
    );
}

#[test]
fn cleanup_journal_uses_the_index_slug_when_one_row_matches() {
    let (root, runs, repo) = ws("slug");
    std::fs::write(
        root.join("projects/INDEX.md"),
        format!(
            "| slug | repo |\n|---|---|\n| eng-proj | {} | cos |\n",
            repo.display()
        ),
    )
    .unwrap();
    std::fs::write(runs.join("sude-thing.md"), REPORT).unwrap();
    journal::cleanup(&runs, &repo, "sude-thing", "abc1234", "2026-11-19");
    assert!(read(&root.join("memory/2026-11-19.md")).starts_with("- [eng-proj] sude-thing"));
    assert!(root.join("raw/2026-11-19-eng-proj-thing.md").is_file());
}

#[test]
fn cleanup_journal_rerun_writes_nothing_new() {
    let (root, runs, repo) = ws("rerun");
    std::fs::write(runs.join("sude-thing.md"), REPORT).unwrap();
    journal::cleanup(&runs, &repo, "sude-thing", "abc1234", "2026-11-19");
    let again = journal::cleanup(&runs, &repo, "sude-thing", "abc1234", "2026-11-19");
    assert_eq!(again.line, None);
    // even on a later day: same bytes reuse the raw copy, the line is already there
    let later = journal::cleanup(&runs, &repo, "sude-thing", "abc1234", "2026-11-20");
    assert_eq!(later.line, None);
    assert_eq!(later.raw.as_deref(), Some("2026-11-19-engine-thing.md"));
    assert_eq!(std::fs::read_dir(root.join("raw")).unwrap().count(), 1);
    assert_eq!(read(&root.join("memory/2026-11-19.md")).lines().count(), 1);
    assert!(!root.join("memory/2026-11-20.md").exists());
}

#[test]
fn cleanup_journal_never_overwrites_a_different_raw() {
    let (root, runs, repo) = ws("nooverwrite");
    std::fs::create_dir_all(root.join("raw")).unwrap();
    std::fs::write(root.join("raw/2026-11-19-engine-thing.md"), "older report").unwrap();
    std::fs::write(runs.join("sude-thing.md"), REPORT).unwrap();
    let j = journal::cleanup(&runs, &repo, "sude-thing", "abc1234", "2026-11-19");
    assert_eq!(j.raw.as_deref(), Some("2026-11-19-engine-thing-2.md"));
    assert_eq!(
        read(&root.join("raw/2026-11-19-engine-thing.md")),
        "older report"
    );
}

#[test]
fn cleanup_journal_missing_report_writes_no_raw_and_says_so() {
    let (root, runs, repo) = ws("missing");
    let j = journal::cleanup(&runs, &repo, "sude-thing", "abc1234", "2026-11-19");
    assert_eq!(j.raw, None);
    assert!(!root.join("raw").exists());
    assert_eq!(
        read(&root.join("memory/2026-11-19.md")),
        "- [engine] sude-thing cleanup: no report abc1234\n"
    );
    assert_eq!(
        journal::cleanup(&runs, &repo, "sude-thing", "abc1234", "2026-11-19").line,
        None
    );
}

#[test]
fn cleanup_journal_skips_without_a_workspace_memory_dir() {
    let dir = std::env::temp_dir().join(format!("qq-cj-bare-{}/runs", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let j = journal::cleanup(&dir, Path::new("/x/repo"), "sude-thing", "-", "2026-11-19");
    assert!(j.skipped.is_some() && j.line.is_none());
}

#[test]
fn cleanup_journal_watchdog_case_line_for_failure_exits_only() {
    let (root, runs, _) = ws("case");
    for code in [3, 4, 5, 7, 8] {
        assert!(
            journal::watchdog_case(&runs, "sude-thing", code).is_some(),
            "exit {code}"
        );
    }
    for code in [0, 1, 2, 6] {
        assert_eq!(
            journal::watchdog_case(&runs, "sude-thing", code),
            None,
            "exit {code}"
        );
    }
    let mem = read(&root.join(format!("memory/{}.md", journal::today())));
    assert_eq!(mem.lines().count(), 5);
    assert!(mem.contains("- [case] sude-thing exit 3: stalled"));
    // the same stall twice in a day is one line
    assert_eq!(journal::watchdog_case(&runs, "sude-thing", 3), None);
}

#[test]
fn cleanup_journal_slug_picks_the_single_active_row_of_a_shared_repo() {
    let (root, runs, repo) = ws("multirow");
    let idx = |rows: &str| {
        std::fs::write(
            root.join("projects/INDEX.md"),
            format!("| slug | repo | owner | status |\n|---|---|---|---|\n{rows}"),
        )
        .unwrap()
    };
    let r = repo.display();
    std::fs::write(runs.join("sude-thing.md"), REPORT).unwrap();
    idx(&format!("| old | {r} | cos | done: x |\n| live | {r} | cos | active 2026-11-19: y |\n| idle1 | {r} | cos | idle |\n"));
    journal::cleanup(&runs, &repo, "sude-thing", "abc1234", "2026-11-19");
    assert!(read(&root.join("memory/2026-11-19.md")).starts_with("- [live] sude-thing"));
    // two active rows, or none: the repo dir name
    for (tag, rows) in [
        (
            "2",
            format!("| a | {r} | cos | active |\n| b | {r} | cos | active |\n"),
        ),
        (
            "3",
            format!("| a | {r} | cos | idle |\n| b | {r} | cos | done |\n"),
        ),
    ] {
        idx(&rows);
        let name = format!("sude-t{tag}");
        std::fs::write(runs.join(format!("{name}.md")), REPORT).unwrap();
        journal::cleanup(&runs, &repo, &name, "abc1234", "2026-11-19");
        assert!(
            read(&root.join("memory/2026-11-19.md"))
                .contains(&format!("- [engine] {name} cleanup")),
            "{tag}"
        );
    }
}
