//! The cleanup journal over a temp workspace (never ~/quaqua): raw copy, memory
//! line, idempotent rerun, missing report, watchdog case line.

use std::path::{Path, PathBuf};

use qq_core::memory::journal;

/// A temp workspace root, removed when the test ends (pass or fail).
struct Root(PathBuf);

impl std::ops::Deref for Root {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ws(tag: &str) -> (Root, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("qq-cj-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let runs = root.join("runs");
    let repo = root.join("code").join("engine");
    for d in [&runs, &root.join("memory"), &root.join("projects"), &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    (Root(root), runs, repo)
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
    let bare = Root(std::env::temp_dir().join(format!("qq-cj-bare-{}", std::process::id())));
    let dir = bare.join("runs");
    std::fs::create_dir_all(&dir).unwrap();
    let j = journal::cleanup(&dir, Path::new("/x/repo"), "sude-thing", "-", "2026-11-19");
    assert!(j.skipped.is_some() && j.line.is_none());
}

#[test]
fn cleanup_journal_watchdog_case_line_for_failure_exits_only() {
    let (root, runs, _) = ws("case");
    let day = journal::today();
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
    let mem = read(&root.join(format!("memory/{day}.md")));
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

const DAY: &str = "2026-11-19";

#[test]
fn cleanup_journal_non_utf8_day_file_keeps_its_prior_bytes() {
    // P1: one bad byte used to turn the whole day file into the new line.
    let (root, runs, repo) = ws("p1");
    let file = root.join(format!("memory/{DAY}.md"));
    let prior: &[u8] = b"- first line\n- caf\xE9 second line\n";
    std::fs::write(&file, prior).unwrap();
    std::fs::write(runs.join("sude-thing.md"), REPORT).unwrap();
    let j = journal::cleanup(&runs, &repo, "sude-thing", "abc1234", DAY);
    assert!(j.line.is_some());
    let after = std::fs::read(&file).unwrap();
    assert!(after.starts_with(prior), "prior bytes rewritten: {after:?}");
    assert_eq!(after.len(), prior.len() + j.line.unwrap().len() + 1);
    // a file with no trailing newline gets one before the new line, not a joined line
    std::fs::write(&file, b"- no newline \xFF").unwrap();
    std::fs::write(runs.join("sude-two.md"), REPORT).unwrap();
    journal::cleanup(&runs, &repo, "sude-two", "abc1234", DAY);
    let after = std::fs::read(&file).unwrap();
    assert!(after.starts_with(b"- no newline \xFF\n- [engine] sude-two"));
}

#[test]
fn cleanup_journal_concurrent_appender_loses_nothing() {
    // P2: a read-modify-write journal used to drop lines an O_APPEND writer
    // added between its read and its write. One shared day file, 500 writes a
    // side (a few KB on disk); the temp root goes with the `Root` guard.
    use std::io::Write;
    const N: usize = 500;
    let (root, runs, _) = ws("p2");
    let day = journal::today();
    let file = root.join(format!("memory/{day}.md"));
    std::fs::write(&file, "- seed\n").unwrap();
    let cos = std::thread::spawn({
        let file = file.clone();
        move || {
            for i in 0..N {
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&file)
                    .unwrap();
                f.write_all(format!("- cos line {i}\n").as_bytes()).unwrap();
                std::thread::sleep(std::time::Duration::from_micros(20));
            }
        }
    });
    for i in 0..N {
        assert!(journal::watchdog_case(&runs, &format!("sude-w{i}"), 3).is_some());
    }
    cos.join().unwrap();
    let text = read(&file);
    let kept = |p: &str| text.lines().filter(|l| l.starts_with(p)).count();
    assert_eq!(kept("- seed"), 1);
    assert_eq!(kept("- cos line "), N, "CoS lines lost");
    assert_eq!(kept("- [case] "), N, "case lines lost");
    assert!(text.len() < 1_000_000);
}

#[test]
fn cleanup_journal_secret_in_outcome_is_dropped_not_written() {
    let (root, runs, repo) = ws("secret");
    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
    std::fs::write(
        runs.join("sude-thing.md"),
        format!("Outcome: COMPLETE {token}\n"),
    )
    .unwrap();
    let j = journal::cleanup(&runs, &repo, "sude-thing", "abc1234", DAY);
    let line = j.line.expect("line still written");
    assert_eq!(
        line,
        "- [engine] sude-thing cleanup: abc1234 · raw/2026-11-19-engine-thing.md"
    );
    assert!(!read(&root.join(format!("memory/{DAY}.md"))).contains("ghp_"));
}

#[test]
fn cleanup_journal_long_outcome_keeps_the_raw_cite() {
    let (root, runs, repo) = ws("long");
    let name = format!("sude-{}", "x".repeat(60));
    std::fs::write(
        runs.join(format!("{name}.md")),
        format!("Outcome: {}\n", "y".repeat(80)),
    )
    .unwrap();
    let j = journal::cleanup(&runs, &repo, &name, "abc1234", DAY);
    let line = j.line.unwrap();
    assert!(line.chars().count() <= qq_core::memory::LINE_CAP, "{line}");
    assert!(line.ends_with(&format!("raw/{}", j.raw.unwrap())), "{line}");
    assert_eq!(
        journal::cleanup(&runs, &repo, &name, "abc1234", DAY).line,
        None,
        "rerun dedupes on the cite"
    );
    let _ = root;
}

#[test]
fn cleanup_journal_reused_name_without_a_raw_cite_still_gets_its_line() {
    // P4: same worker name, no report, two days: both days keep a line.
    let (root, runs, repo) = ws("reuse");
    let first = journal::cleanup(&runs, &repo, "sude-c", "-", "2026-11-01");
    assert!(first.line.is_some());
    let second = journal::cleanup(&runs, &repo, "sude-c", "-", DAY);
    assert!(second.line.is_some());
    assert!(root.join("memory/2026-11-19.md").is_file());
    // same day twice is still one line
    assert_eq!(
        journal::cleanup(&runs, &repo, "sude-c", "-", DAY).line,
        None
    );
}

#[test]
fn cleanup_journal_raw_copy_leaves_no_temp_file_and_failure_leaves_no_partial() {
    let (root, runs, repo) = ws("rawtmp");
    std::fs::write(runs.join("sude-thing.md"), REPORT).unwrap();
    journal::cleanup(&runs, &repo, "sude-thing", "abc1234", DAY);
    let names: Vec<_> = std::fs::read_dir(root.join("raw"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["2026-11-19-engine-thing.md"]);
    // raw/ is a file: nothing is created, the line says so
    let (root2, runs2, repo2) = ws("rawfile");
    std::fs::write(root2.join("raw"), "x").unwrap();
    std::fs::write(runs2.join("sude-thing.md"), REPORT).unwrap();
    let j = journal::cleanup(&runs2, &repo2, "sude-thing", "abc1234", DAY);
    assert_eq!(j.raw, None);
    assert!(j.line.unwrap().contains("report not copied"));
}

/// `worker.cleanup` over a live daemon: a repo with a clean worker worktree and
/// an agent row, inside a TestWorkspace (removed on drop).
fn daemon_cleanup(tag: &str, prep: impl Fn(&Path)) -> serde_json::Value {
    use qq_core::daemon::{Config, Daemon};
    use qq_core::herdr::FakeHerdr;
    use qq_core::rpc::{self, Request};
    use qq_core::testkit::TestWorkspace;
    use std::process::Command;
    let ws = TestWorkspace::new(tag);
    let git = |dir: &Path, args: &[&str]| {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@local")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@local")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    };
    let code = ws.root().join("code");
    let repo = code.join("engine");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a"), "a").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let wt = code.join(".wt").join("w1");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "quaqua/w1",
            &wt.to_string_lossy(),
        ],
    );
    let mut conn = qq_core::db::open(&ws.db_path()).unwrap();
    let team = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
    )
    .unwrap();
    qq_core::config::seed(&mut conn, &team).unwrap();
    conn.execute(
        "INSERT INTO project (slug, repo_path, created_ms) VALUES ('engine', ?1, 1)",
        [repo.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent (name, role, kind, project_id, started_ms) VALUES ('w1', 'peer', 'tieude', ?1, 1)",
        [conn.last_insert_rowid()],
    )
    .unwrap();
    drop(conn);
    std::fs::write(ws.runs_dir().join("w1.md"), REPORT).unwrap();
    prep(ws.root());
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: std::sync::Arc::new(FakeHerdr::new()),
    });
    qq_core::task::cleanup::register(&mut d).unwrap();
    let h = d.spawn().unwrap();
    let r = rpc::call(
        &rpc::socket_path(&ws.runs_dir()),
        &Request::new("worker.cleanup", serde_json::json!({"name": "w1"})),
    )
    .unwrap()
    .into_result()
    .unwrap_or_else(|e| panic!("worker.cleanup refused: {e}"));
    let _ = h.stop();
    r
}

#[test]
fn cleanup_journal_daemon_reply_carries_journal_raw_and_skipped_keys() {
    let ok = daemon_cleanup("djok", |root| {
        std::fs::create_dir_all(root.join("memory")).unwrap();
    });
    assert_eq!(ok["cleaned"], "w1");
    let day = journal::today();
    assert!(ok["journal"]
        .as_str()
        .unwrap()
        .starts_with("- [engine] w1 cleanup: COMPLETE "));
    assert_eq!(ok["raw"], format!("{day}-engine-w1.md"));
    assert!(ok["journal_skipped"].is_null());
    // no memory/ beside runs/: the verb says why, the cleanup still succeeded
    let bare = daemon_cleanup("djbare", |_| {});
    assert_eq!(bare["cleaned"], "w1");
    assert!(bare["journal"].is_null() && bare["raw"].is_null());
    assert!(bare["journal_skipped"]
        .as_str()
        .unwrap()
        .contains("no workspace memory/"));
}

#[test]
fn cleanup_journal_failing_journal_does_not_fail_the_cleanup() {
    // today's memory file is a directory, so the append cannot open it
    let r = daemon_cleanup("djfail", |root| {
        let day = journal::today();
        std::fs::create_dir_all(root.join(format!("memory/{day}.md"))).unwrap();
    });
    assert_eq!(r["cleaned"], "w1");
    assert!(r["journal"].is_null());
    assert!(r["raw"].is_string());
}
