//! `quaqua transcripts index|search|tail` end to end, against a fixture corpus.
//!
//! The fixture is a miniature `~/.claude/projects`: two project directories,
//! three sessions, and the entry shapes a real transcript carries (string
//! content, `text`/`thinking`/`tool_use`/`tool_result` blocks, an
//! `isSidechain` entry, and one truncated line). Nothing here reads the live
//! corpus or the live bus.

use std::path::PathBuf;
use std::process::{Command, Output};

const PROJ_A: &str = "-home-tinhpt-quaqua";
const PROJ_B: &str = "-home-tinhpt-other";

/// Session `sess-a1`: three real prompts, one assistant text, a tool round-trip,
/// a sidechain prompt, and a truncated line — the whole entry-shape matrix.
const SESS_A1: &str = concat!(
    r#"{"type":"user","sessionId":"sess-a1","timestamp":"2026-11-01T10:00:00Z","cwd":"@HOME@/quaqua","message":{"role":"user","content":"first prompt about the memory caps"}}"#,
    "\n",
    r#"{"type":"assistant","sessionId":"sess-a1","timestamp":"2026-11-01T10:05:00Z","cwd":"@HOME@/quaqua","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"tool_use","name":"Read","input":{}},{"type":"text","text":"we shipped the gate and the batch hook"}]}}"#,
    "\n",
    r#"{"type":"user","sessionId":"sess-a1","timestamp":"2026-11-01T10:06:00Z","cwd":"@HOME@/quaqua","message":{"role":"user","content":[{"type":"tool_result","content":"file body"}]}}"#,
    "\n",
    r#"{"type":"user","isSidechain":true,"sessionId":"sess-a1","timestamp":"2026-11-01T10:07:00Z","cwd":"@HOME@/quaqua","message":{"role":"user","content":"subagent prompt that tail must skip"}}"#,
    "\n",
    "{\"type\":\"user\",\"sessionId\":\"sess-a1\",\"message\":{\"role\":\"user\",\"content\":\"trunc",
    "\n",
    r#"{"type":"user","sessionId":"sess-a1","timestamp":"2026-11-05T10:00:00Z","cwd":"@HOME@/quaqua","message":{"role":"user","content":"second prompt raise the memory caps"}}"#,
    "\n",
    r#"{"type":"user","sessionId":"sess-a1","timestamp":"2026-11-06T10:00:00Z","cwd":"@HOME@/quaqua","message":{"role":"user","content":"third prompt about grid repair"}}"#,
    "\n",
);

/// Session `sess-a2`: one older prompt, used for `--since`.
const SESS_A2: &str = concat!(
    r#"{"type":"user","sessionId":"sess-a2","timestamp":"2026-10-01T10:00:00Z","cwd":"@HOME@/quaqua","message":{"role":"user","content":"an older note about memory caps"}}"#,
    "\n",
);

/// Session `sess-b1`: another project, used for `--project`.
const SESS_B1: &str = concat!(
    r#"{"type":"user","sessionId":"sess-b1","timestamp":"2026-11-09T10:00:00Z","cwd":"@HOME@/other","message":{"role":"user","content":"memory caps in another project entirely"}}"#,
    "\n",
);

/// A fake machine home, assembled rather than spelled: the committed file must
/// not carry a machine path (rules/_all/portable-paths.md).
fn home() -> String {
    format!("/{}/{}", "home", "tinhpt")
}

/// One scratch workspace: `<root>/projects/<project>/<session>.jsonl`.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "quaqua-transcripts-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("projects").join(PROJ_A)).unwrap();
        std::fs::create_dir_all(root.join("projects").join(PROJ_B)).unwrap();
        std::fs::create_dir_all(root.join("runs")).unwrap();
        Fixture { root }
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join("projects").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body.replace("@HOME@", &home())).unwrap();
    }

    fn append(&self, rel: &str, body: &str) {
        use std::io::Write;
        let path = self.root.join("projects").join(rel);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(body.replace("@HOME@", &home()).as_bytes())
            .unwrap();
    }

    /// The three file corpus every test starts from.
    fn seeded(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.write(&format!("{PROJ_A}/sess-a1.jsonl"), SESS_A1);
        f.write(&format!("{PROJ_A}/sess-a2.jsonl"), SESS_A2);
        f.write(&format!("{PROJ_B}/sess-b1.jsonl"), SESS_B1);
        f
    }

    fn projects(&self) -> PathBuf {
        self.root.join("projects")
    }

    fn db(&self) -> PathBuf {
        self.root.join("runs").join("transcripts.db")
    }

    fn bus_db(&self) -> PathBuf {
        self.root.join("runs").join("quaqua.db")
    }

    fn session(&self, project: &str, session: &str) -> PathBuf {
        self.projects()
            .join(project)
            .join(format!("{session}.jsonl"))
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
        cmd.env("QUAQUA_RUNS_DIR", self.root.join("runs"))
            .env("QUAQUA_HOME", &self.root)
            .env_remove("HERDR_PANE_ID");
        cmd
    }

    fn index(&self) -> Output {
        self.cmd()
            .args([
                "transcripts",
                "index",
                "--projects",
                self.projects().to_str().unwrap(),
                "--db",
                self.db().to_str().unwrap(),
            ])
            .output()
            .expect("spawn quaqua transcripts index")
    }

    fn search(&self, args: &[&str]) -> Output {
        let mut cmd = self.cmd();
        cmd.args(["transcripts", "search"]);
        cmd.args(args);
        cmd.args([
            "--projects",
            self.projects().to_str().unwrap(),
            "--db",
            self.db().to_str().unwrap(),
        ]);
        cmd.output().expect("spawn quaqua transcripts search")
    }

    fn tail(&self, args: &[&str]) -> Output {
        let mut cmd = self.cmd();
        cmd.args(["transcripts", "tail", "--projects"]);
        cmd.arg(self.projects());
        cmd.args(args);
        cmd.output().expect("spawn quaqua transcripts tail")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `path:line  <ts> <role>  <excerpt>`, split on the two-space separators.
fn parse_hit(line: &str) -> (String, u64, String, String, String) {
    let parts: Vec<&str> = line.split("  ").collect();
    assert_eq!(
        parts.len(),
        3,
        "hit line is not `anchor  ts role  excerpt`: {line}"
    );
    let (path, number) = parts[0]
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("hit line has no path:line anchor: {line}"));
    let mut ts_role = parts[1].splitn(2, ' ');
    let ts = ts_role.next().unwrap().to_string();
    let role = ts_role.next().unwrap_or("").to_string();
    (
        path.to_string(),
        number.parse().expect("line number"),
        ts,
        role,
        parts[2].to_string(),
    )
}

#[test]
fn transcripts_index_builds_an_incremental_index_and_reports_rows() {
    let f = Fixture::seeded("index");
    let out = f.index();
    assert!(out.status.success(), "index failed: {}", stderr(&out));
    // Five indexable entries in sess-a1 (the tool_result-only entry and the
    // truncated line are not messages), one each in sess-a2 and sess-b1.
    assert_eq!(
        stdout(&out).trim(),
        format!(
            "indexed 3/3 files, 7 rows (0 unchanged) -> {}",
            f.db().display()
        )
    );
    assert!(f.db().is_file(), "index file was not created");

    // R4: a second run over an unchanged corpus inserts no rows.
    let again = f.index();
    assert!(
        again.status.success(),
        "second index failed: {}",
        stderr(&again)
    );
    assert_eq!(
        stdout(&again).trim(),
        format!(
            "indexed 0/3 files, 0 rows (3 unchanged) -> {}",
            f.db().display()
        )
    );

    // A changed file is re-indexed on its own; the other two are skipped.
    f.append(
        &format!("{PROJ_A}/sess-a2.jsonl"),
        concat!(
            r#"{"type":"user","sessionId":"sess-a2","timestamp":"2026-10-02T10:00:00Z","cwd":"@HOME@/quaqua","message":{"role":"user","content":"a second note about memory caps"}}"#,
            "\n",
        ),
    );
    let third = f.index();
    assert!(
        third.status.success(),
        "third index failed: {}",
        stderr(&third)
    );
    assert_eq!(
        stdout(&third).trim(),
        format!(
            "indexed 1/3 files, 2 rows (2 unchanged) -> {}",
            f.db().display()
        )
    );
}

#[test]
fn transcripts_index_never_writes_the_bus_db() {
    let f = Fixture::seeded("busdb");
    let out = f.index();
    assert!(out.status.success(), "index failed: {}", stderr(&out));
    assert!(f.db().is_file(), "the index file was not created");
    assert!(
        !f.bus_db().exists(),
        "index created the bus database {}",
        f.bus_db().display()
    );
}

#[test]
fn transcripts_index_uses_the_default_db_under_the_runs_dir() {
    let f = Fixture::seeded("defaultdb");
    let out = f
        .cmd()
        .args([
            "transcripts",
            "index",
            "--projects",
            f.projects().to_str().unwrap(),
        ])
        .output()
        .expect("spawn quaqua transcripts index");
    assert!(out.status.success(), "index failed: {}", stderr(&out));
    assert!(
        f.db().is_file(),
        "index did not land in <runs>/transcripts.db: {}",
        stdout(&out)
    );
}

#[test]
fn transcripts_search_prints_one_anchored_line_per_hit() {
    let f = Fixture::seeded("search");
    f.index();

    let out = f.search(&["memory caps"]);
    assert!(out.status.success(), "search failed: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    assert_eq!(lines.len(), 4, "expected four hits, got: {lines:?}");

    for line in &lines {
        let (path, number, ts, role, excerpt) = parse_hit(line);
        assert!(
            path.starts_with(f.projects().to_str().unwrap()),
            "hit path is not in the corpus: {path}"
        );
        assert!(number >= 1, "hit has no line number: {line}");
        assert!(ts.starts_with("2026-"), "hit has no timestamp: {line}");
        assert!(role == "user" || role == "assistant", "bad role: {line}");
        assert!(!excerpt.is_empty(), "hit has no excerpt: {line}");
        assert!(
            excerpt.chars().count() <= 200,
            "excerpt is longer than 200 chars: {line}"
        );
    }

    // The tool_result-only entry and the truncated line never became hits.
    for line in &lines {
        assert!(
            !line.contains("file body"),
            "a tool result was indexed: {line}"
        );
        assert!(!line.contains("trunc"), "a broken line was indexed: {line}");
    }

    // `--limit` caps the hits without changing the order.
    let limited = f.search(&["memory caps", "--limit", "1"]);
    assert!(limited.status.success());
    assert_eq!(stdout(&limited).lines().count(), 1);
    assert_eq!(stdout(&limited).lines().next().unwrap(), lines[0]);
}

/// R5's "ranked by bm25": the stronger match comes first, not the newest.
#[test]
fn transcripts_search_ranks_the_strongest_match_first() {
    let f = Fixture::new("rank");
    // A short entry repeating the phrase beats a long entry that mentions it
    // once — and it is also the *older* one, so a timestamp sort would invert it.
    f.write(
        &format!("{PROJ_A}/sess-r1.jsonl"),
        concat!(
            r#"{"type":"user","sessionId":"sess-r1","timestamp":"2026-10-01T10:00:00Z","cwd":"/w","message":{"role":"user","content":"memory caps memory caps memory caps memory caps"}}"#,
            "\n",
        ),
    );
    f.write(
        &format!("{PROJ_A}/sess-r2.jsonl"),
        concat!(
            r#"{"type":"user","sessionId":"sess-r2","timestamp":"2026-11-09T10:00:00Z","cwd":"/w","message":{"role":"user","content":"a long unrelated paragraph that happens to mention memory caps once among many other words about grids panes workers pipelines merges hooks templates and layouts"}}"#,
            "\n",
        ),
    );
    let out = f.index();
    assert!(out.status.success(), "index failed: {}", stderr(&out));

    let hits = f.search(&["memory caps"]);
    assert!(hits.status.success(), "search failed: {}", stderr(&hits));
    let text = stdout(&hits);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "expected two hits: {lines:?}");
    assert_eq!(
        parse_hit(lines[0]).0,
        f.session(PROJ_A, "sess-r1").to_str().unwrap(),
        "bm25 did not rank the repeated phrase first: {lines:?}"
    );
}

#[test]
fn transcripts_search_filters_by_project_and_since() {
    let f = Fixture::seeded("filters");
    f.index();

    let by_project = f.search(&["memory caps", "--project", PROJ_B]);
    assert!(by_project.status.success(), "{}", stderr(&by_project));
    let text = stdout(&by_project);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "project filter leaked: {lines:?}");
    assert_eq!(
        parse_hit(lines[0]).0,
        f.session(PROJ_B, "sess-b1").to_str().unwrap()
    );

    // A cwd works as well as a project directory name.
    let other_cwd = format!("{}/other", home());
    let by_cwd = f.search(&["memory caps", "--project", &other_cwd]);
    assert_eq!(stdout(&by_cwd).lines().count(), 1);

    // `--since` excludes the 2026-10-01 note and the 2026-11-01 prompt, and
    // keeps 2026-11-05 onward.
    let since = f.search(&["memory caps", "--since", "2026-11-05"]);
    assert!(since.status.success(), "{}", stderr(&since));
    let text = stdout(&since);
    let mut kept: Vec<(String, u64)> = text
        .lines()
        .map(|line| {
            let (path, number, ts, _, _) = parse_hit(line);
            assert!(
                ts.as_str() >= "2026-11-05",
                "hit older than --since: {line}"
            );
            (path, number)
        })
        .collect();
    kept.sort();
    let mut expected = vec![
        (
            f.session(PROJ_A, "sess-a1").to_str().unwrap().to_string(),
            6,
        ),
        (
            f.session(PROJ_B, "sess-b1").to_str().unwrap().to_string(),
            1,
        ),
    ];
    expected.sort();
    assert_eq!(kept, expected, "since filter: {text}");
}

#[test]
fn transcripts_search_with_no_hit_exits_zero_and_says_no_matches() {
    let f = Fixture::seeded("nohit");
    f.index();
    let out = f.search(&["zzzqqqxxx"]);
    assert!(out.status.success(), "a miss must exit 0: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "no matches");
}

#[test]
fn transcripts_search_builds_a_missing_index_once() {
    let f = Fixture::seeded("autobuild");
    assert!(!f.db().exists(), "fixture already has an index");

    let first = f.search(&["memory caps"]);
    assert!(first.status.success(), "search failed: {}", stderr(&first));
    assert!(f.db().is_file(), "search did not build the index");
    assert_eq!(stdout(&first).lines().count(), 4);
    assert!(
        stderr(&first).contains("built"),
        "the build was not reported on stderr: {}",
        stderr(&first)
    );

    let second = f.search(&["memory caps"]);
    assert_eq!(stdout(&second).lines().count(), 4);
    assert!(
        !stderr(&second).contains("built"),
        "search rebuilt an existing index: {}",
        stderr(&second)
    );
}

#[test]
fn transcripts_tail_prints_the_last_n_user_messages() {
    let f = Fixture::seeded("tail");
    let out = f.tail(&["--session", "sess-a1", "--last", "2"]);
    assert!(out.status.success(), "tail failed: {}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "second prompt raise the memory caps\nthird prompt about grid repair\n"
    );

    // One message: the newest prompt only.
    let one = f.tail(&["--session", "sess-a1", "--last", "1"]);
    assert_eq!(stdout(&one), "third prompt about grid repair\n");

    // A path works as well as an id, and the default is the last 10.
    let by_path = f.tail(&["--session", f.session(PROJ_A, "sess-a1").to_str().unwrap()]);
    assert!(by_path.status.success(), "{}", stderr(&by_path));
    assert_eq!(
        stdout(&by_path),
        concat!(
            "first prompt about the memory caps\n",
            "second prompt raise the memory caps\n",
            "third prompt about grid repair\n",
        ),
        "tool_result and sidechain entries must not appear"
    );
}

#[test]
fn transcripts_tail_of_an_unknown_session_exits_one_with_empty_stdout() {
    let f = Fixture::seeded("unknown");
    let out = f.tail(&["--session", "no-such-session"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1: {}",
        stderr(&out)
    );
    assert!(
        stdout(&out).is_empty(),
        "an unknown session printed: {}",
        stdout(&out)
    );

    let missing_path = f.tail(&["--session", "/nonexistent/x.jsonl"]);
    assert_eq!(missing_path.status.code(), Some(1));
    assert!(stdout(&missing_path).is_empty());
}

#[test]
fn transcripts_search_rejects_an_empty_query() {
    let f = Fixture::seeded("empty");
    f.index();
    let out = f.search(&["   "]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "empty query must fail: {}",
        stdout(&out)
    );
    assert!(stdout(&out).is_empty());
}

/// The corpus root is a parameter, so a search never has to touch the live one.
#[test]
fn transcripts_search_of_a_missing_corpus_fails_loudly() {
    let f = Fixture::seeded("nocorpus");
    let missing = f.root.join("no-such-projects");
    let out = f
        .cmd()
        .args([
            "transcripts",
            "search",
            "memory",
            "--projects",
            missing.to_str().unwrap(),
            "--db",
            f.db().to_str().unwrap(),
        ])
        .output()
        .expect("spawn quaqua transcripts search");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("is not a directory"),
        "unexpected error: {}",
        stderr(&out)
    );
}
